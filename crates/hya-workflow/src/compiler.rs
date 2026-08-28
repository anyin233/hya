//! Frontmatter and restricted flowchart compilation.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::WorkflowSource;
use crate::error::WorkflowCompileError;
use crate::model::{
    CompiledWorkflow, FailurePolicy, StageMode, VerifySpec, WorkflowDefinition, WorkflowLevel,
    WorkflowPlan, WorkflowRevision, WorkflowStage,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorWorkflow {
    kind: String,
    name: String,
    description: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    #[serde(default)]
    on_failure: FailurePolicy,
    nodes: BTreeMap<String, AuthorNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorNode {
    #[serde(default)]
    title: Option<String>,
    agent: String,
    directive: String,
    #[serde(default)]
    mode: StageMode,
    #[serde(default)]
    verify: Option<VerifySpec>,
    #[serde(default)]
    actor: Option<String>,
}

#[derive(Clone, Copy)]
struct SourcePoint {
    line: usize,
    column: usize,
}

struct GraphEdge {
    from: String,
    to: String,
    from_location: SourcePoint,
    to_location: SourcePoint,
}

struct GraphEndpoint<'a> {
    id: &'a str,
    location: SourcePoint,
}

struct ParsedGraph {
    order: Vec<String>,
    node_locations: BTreeMap<String, SourcePoint>,
    edges: Vec<GraphEdge>,
}

pub(crate) fn compile_source(
    source: WorkflowSource<'_>,
) -> Result<CompiledWorkflow, WorkflowCompileError> {
    let lines: Vec<&str> = source.text().lines().collect();
    let (frontmatter, graph_start) = split_frontmatter(source.name(), &lines)?;
    let node_locations =
        scan_frontmatter_nodes(source.name(), &lines[1..graph_start.saturating_sub(1)], 2)?;
    let author: AuthorWorkflow = serde_norway::from_str(&frontmatter).map_err(|error| {
        WorkflowCompileError::frontmatter(
            source.name(),
            2,
            1,
            format!("invalid Workflow frontmatter: {error}"),
        )
    })?;
    validate_author(source.name(), &author, &node_locations)?;
    let graph = parse_graph(source.name(), &lines[graph_start..], graph_start + 1)?;
    normalize(source.name(), author, node_locations, graph)
}

fn split_frontmatter(
    source: &str,
    lines: &[&str],
) -> Result<(String, usize), WorkflowCompileError> {
    if lines.first().map(|line| line.trim_end_matches('\r')) != Some("---") {
        return Err(WorkflowCompileError::frontmatter(
            source,
            1,
            1,
            "Workflow document must start with YAML frontmatter",
        ));
    }
    let Some(closing) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim_end_matches('\r') == "---").then_some(index))
    else {
        return Err(WorkflowCompileError::frontmatter(
            source,
            1,
            1,
            "Workflow frontmatter has no closing `---` fence",
        ));
    };
    Ok((lines[1..closing].join("\n"), closing + 1))
}

fn scan_frontmatter_nodes(
    source: &str,
    lines: &[&str],
    first_line_number: usize,
) -> Result<BTreeMap<String, SourcePoint>, WorkflowCompileError> {
    let mut in_nodes = false;
    let mut locations = BTreeMap::new();
    for (offset, raw) in lines.iter().enumerate() {
        let line_number = first_line_number + offset;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indentation = raw.bytes().take_while(|byte| *byte == b' ').count();
        if !in_nodes {
            if indentation == 0 && trimmed == "nodes:" {
                in_nodes = true;
            }
            continue;
        }
        if indentation == 0 {
            break;
        }
        if indentation != 2 || !trimmed.ends_with(':') {
            continue;
        }
        let id = trimmed.trim_end_matches(':');
        let location = SourcePoint {
            line: line_number,
            column: 3,
        };
        if locations.insert(id.to_string(), location).is_some() {
            return Err(WorkflowCompileError::frontmatter(
                source,
                location.line,
                location.column,
                format!("duplicate node `{id}`"),
            ));
        }
    }
    Ok(locations)
}

fn validate_author(
    source: &str,
    author: &AuthorWorkflow,
    node_locations: &BTreeMap<String, SourcePoint>,
) -> Result<(), WorkflowCompileError> {
    if author.kind != "Workflow" {
        return Err(WorkflowCompileError::new(
            source,
            2,
            1,
            "frontmatter `kind` must be `Workflow`",
        ));
    }
    if !valid_identifier(&author.name) {
        return Err(WorkflowCompileError::new(
            source,
            2,
            1,
            format!("invalid Workflow name `{}`", author.name),
        ));
    }
    if author.description.trim().is_empty() {
        return Err(WorkflowCompileError::new(
            source,
            2,
            1,
            "Workflow description must not be empty",
        ));
    }
    if author.nodes.is_empty() {
        return Err(WorkflowCompileError::new(
            source,
            2,
            1,
            "Workflow must declare at least one node",
        ));
    }
    for input in author.inputs.keys() {
        if !valid_identifier(input) {
            return Err(WorkflowCompileError::new(
                source,
                2,
                1,
                format!("invalid input identifier `{input}`"),
            ));
        }
    }
    let mut actor_agents: BTreeMap<&str, &str> = BTreeMap::new();
    for (id, node) in &author.nodes {
        let location = node_locations
            .get(id)
            .copied()
            .unwrap_or(SourcePoint { line: 2, column: 1 });
        if !valid_identifier(id) {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("invalid node identifier `{id}`"),
            ));
        }
        if node.agent.trim().is_empty() {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("node `{id}` must declare an Agent"),
            ));
        }
        if node.directive.trim().is_empty() {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("node `{id}` must declare a directive"),
            ));
        }
        validate_input_template(source, id, &node.directive, &author.inputs, location)?;
        match (node.mode, node.verify.as_ref()) {
            (StageMode::Loop, None) => {
                return Err(WorkflowCompileError::new(
                    source,
                    location.line,
                    location.column,
                    format!("node `{id}` mode `loop` requires a verifier"),
                ));
            }
            (StageMode::Once, Some(_)) => {
                return Err(WorkflowCompileError::new(
                    source,
                    location.line,
                    location.column,
                    format!("node `{id}` declares verify but mode is `once`"),
                ));
            }
            (StageMode::Loop, Some(verify)) => {
                if verify.agent.trim().is_empty() || verify.until.trim().is_empty() {
                    return Err(WorkflowCompileError::new(
                        source,
                        location.line,
                        location.column,
                        format!("node `{id}` verifier requires Agent and until"),
                    ));
                }
                if verify.max_iterations == 0 {
                    return Err(WorkflowCompileError::new(
                        source,
                        location.line,
                        location.column,
                        format!("node `{id}` verifier max_iterations must be at least 1"),
                    ));
                }
                validate_input_template(source, id, &verify.until, &author.inputs, location)?;
            }
            (StageMode::Once, None) => {}
        }
        if node.actor.is_some() && node.mode == StageMode::Loop {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("node `{id}` cannot combine actor and loop modes"),
            ));
        }
        if let Some(actor) = node.actor.as_deref() {
            if !valid_identifier(actor) {
                return Err(WorkflowCompileError::new(
                    source,
                    location.line,
                    location.column,
                    format!("node `{id}` has invalid actor key `{actor}`"),
                ));
            }
            if let Some(previous_agent) = actor_agents.insert(actor, &node.agent)
                && previous_agent != node.agent
            {
                return Err(WorkflowCompileError::new(
                    source,
                    location.line,
                    location.column,
                    format!(
                        "actor key `{actor}` targets both `{previous_agent}` and `{}`",
                        node.agent
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_input_template(
    source: &str,
    stage: &str,
    template: &str,
    inputs: &BTreeMap<String, String>,
    location: SourcePoint,
) -> Result<(), WorkflowCompileError> {
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("node `{stage}` has an unclosed input placeholder"),
            ));
        };
        let token = after[..end].trim();
        let Some(input) = token.strip_prefix("input.") else {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!(
                    "node `{stage}` uses `{{{{{token}}}}}`; only `{{{{input.<name>}}}}` placeholders are public"
                ),
            ));
        };
        if !valid_identifier(input) || !inputs.contains_key(input) {
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("node `{stage}` references undeclared input `{input}`"),
            ));
        }
        rest = &after[end + 2..];
    }
    if rest.contains("}}") {
        return Err(WorkflowCompileError::new(
            source,
            location.line,
            location.column,
            format!("node `{stage}` has an unmatched placeholder terminator"),
        ));
    }
    Ok(())
}

fn parse_graph(
    source: &str,
    lines: &[&str],
    first_line_number: usize,
) -> Result<ParsedGraph, WorkflowCompileError> {
    let mut header_seen = false;
    let mut order = Vec::new();
    let mut node_locations = BTreeMap::new();
    let mut edges = Vec::new();
    let mut seen_edges = BTreeSet::new();

    for (offset, raw) in lines.iter().enumerate() {
        let line_number = first_line_number + offset;
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !header_seen {
            if line != "flowchart TD" {
                return Err(WorkflowCompileError::graph(
                    source,
                    line_number,
                    first_non_whitespace_column(raw),
                    "first graph line must be `flowchart TD`",
                ));
            }
            header_seen = true;
            continue;
        }
        let Some(arrow_offset) = raw.find("-->") else {
            if valid_identifier(line) {
                record_node(
                    line,
                    SourcePoint {
                        line: line_number,
                        column: first_non_whitespace_column(raw),
                    },
                    &mut node_locations,
                    &mut order,
                );
                continue;
            }
            return Err(WorkflowCompileError::graph(
                source,
                line_number,
                first_non_whitespace_column(raw),
                "expected a standalone identifier or `a --> b` edge",
            ));
        };
        if raw[arrow_offset + 3..].contains("-->") {
            return Err(WorkflowCompileError::graph(
                source,
                line_number,
                first_non_whitespace_column(raw),
                "one graph line may contain only one `-->` edge operator",
            ));
        }
        let from = parse_endpoint_group(source, line_number, &raw[..arrow_offset], 0)?;
        let to = parse_endpoint_group(
            source,
            line_number,
            &raw[arrow_offset + 3..],
            arrow_offset + 3,
        )?;
        if from.len() > 1 && to.len() > 1 {
            return Err(WorkflowCompileError::graph(
                source,
                line_number,
                first_non_whitespace_column(raw),
                "fan sugar may appear on only one side of an edge",
            ));
        }
        for endpoint in from.iter().chain(to.iter()) {
            record_node(
                endpoint.id,
                endpoint.location,
                &mut node_locations,
                &mut order,
            );
        }
        for source_endpoint in &from {
            for target_endpoint in &to {
                if seen_edges.insert((
                    source_endpoint.id.to_string(),
                    target_endpoint.id.to_string(),
                )) {
                    edges.push(GraphEdge {
                        from: source_endpoint.id.to_string(),
                        to: target_endpoint.id.to_string(),
                        from_location: source_endpoint.location,
                        to_location: target_endpoint.location,
                    });
                }
            }
        }
    }

    if !header_seen {
        return Err(WorkflowCompileError::graph(
            source,
            first_line_number,
            1,
            "Workflow body must contain `flowchart TD`",
        ));
    }
    if order.is_empty() {
        return Err(WorkflowCompileError::graph(
            source,
            first_line_number,
            1,
            "Workflow graph must contain at least one node",
        ));
    }
    Ok(ParsedGraph {
        order,
        node_locations,
        edges,
    })
}

fn parse_endpoint_group<'a>(
    source: &str,
    line_number: usize,
    value: &'a str,
    base_offset: usize,
) -> Result<Vec<GraphEndpoint<'a>>, WorkflowCompileError> {
    let mut endpoints = Vec::new();
    let mut cursor = 0;
    for part in value.split('&') {
        let leading = part.len().saturating_sub(part.trim_start().len());
        endpoints.push(GraphEndpoint {
            id: part.trim(),
            location: SourcePoint {
                line: line_number,
                column: base_offset + cursor + leading + 1,
            },
        });
        cursor += part.len() + 1;
    }
    if let Some(endpoint) = endpoints
        .iter()
        .find(|endpoint| !valid_identifier(endpoint.id))
    {
        return Err(WorkflowCompileError::graph(
            source,
            endpoint.location.line,
            endpoint.location.column,
            "edge endpoints must be stable identifiers",
        ));
    }
    let mut unique = BTreeSet::new();
    if let Some(endpoint) = endpoints
        .iter()
        .find(|endpoint| !unique.insert(endpoint.id))
    {
        return Err(WorkflowCompileError::graph(
            source,
            endpoint.location.line,
            endpoint.location.column,
            "an endpoint group cannot repeat a node",
        ));
    }
    Ok(endpoints)
}

fn record_node(
    id: &str,
    location: SourcePoint,
    locations: &mut BTreeMap<String, SourcePoint>,
    order: &mut Vec<String>,
) {
    if !locations.contains_key(id) {
        locations.insert(id.to_string(), location);
        order.push(id.to_string());
    }
}

fn normalize(
    source: &str,
    author: AuthorWorkflow,
    frontmatter_locations: BTreeMap<String, SourcePoint>,
    graph: ParsedGraph,
) -> Result<CompiledWorkflow, WorkflowCompileError> {
    for id in &graph.order {
        if !author.nodes.contains_key(id) {
            let location = graph
                .node_locations
                .get(id)
                .copied()
                .unwrap_or(SourcePoint { line: 1, column: 1 });
            return Err(WorkflowCompileError::new(
                source,
                location.line,
                location.column,
                format!("graph node `{id}` has no frontmatter definition"),
            ));
        }
    }
    if let Some(id) = author
        .nodes
        .keys()
        .find(|id| !graph.order.iter().any(|graph_id| graph_id == *id))
    {
        let location = frontmatter_locations
            .get(id)
            .copied()
            .unwrap_or(SourcePoint { line: 1, column: 1 });
        return Err(WorkflowCompileError::new(
            source,
            location.line,
            location.column,
            format!("frontmatter node `{id}` does not occur in the graph"),
        ));
    }

    let index_by_id: BTreeMap<&str, usize> = graph
        .order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut predecessors = vec![Vec::new(); graph.order.len()];
    let mut successors = vec![Vec::new(); graph.order.len()];
    for edge in &graph.edges {
        let Some(&from_index) = index_by_id.get(edge.from.as_str()) else {
            return Err(WorkflowCompileError::new(
                source,
                edge.from_location.line,
                edge.from_location.column,
                format!("unknown graph node `{}`", edge.from),
            ));
        };
        let Some(&to_index) = index_by_id.get(edge.to.as_str()) else {
            return Err(WorkflowCompileError::new(
                source,
                edge.to_location.line,
                edge.to_location.column,
                format!("unknown graph node `{}`", edge.to),
            ));
        };
        if from_index == to_index {
            return Err(WorkflowCompileError::new(
                source,
                edge.to_location.line,
                edge.to_location.column,
                format!("node `{}` cannot depend on itself", edge.from),
            ));
        }
        predecessors[to_index].push(from_index);
        successors[from_index].push(to_index);
    }

    let cycle_location = graph
        .edges
        .last()
        .map(|edge| edge.from_location)
        .unwrap_or(SourcePoint { line: 1, column: 1 });
    let levels = topological_levels(source, cycle_location, &predecessors, &successors)?;
    let mut stage_levels = vec![0; graph.order.len()];
    for (level_index, level) in levels.iter().enumerate() {
        for &stage_index in &level.stage_indices {
            stage_levels[stage_index] = level_index;
        }
    }
    for level in &levels {
        let mut actors = BTreeSet::new();
        for &stage_index in &level.stage_indices {
            let id = &graph.order[stage_index];
            let Some(actor) = author.nodes.get(id).and_then(|node| node.actor.as_deref()) else {
                continue;
            };
            if !actors.insert(actor) {
                let location = frontmatter_locations
                    .get(id)
                    .copied()
                    .unwrap_or(SourcePoint { line: 1, column: 1 });
                return Err(WorkflowCompileError::new(
                    source,
                    location.line,
                    location.column,
                    format!("same actor key `{actor}` in one level is ambiguous"),
                ));
            }
        }
    }
    let mut stages = Vec::with_capacity(graph.order.len());
    for (index, id) in graph.order.iter().enumerate() {
        let Some(node) = author.nodes.get(id) else {
            return Err(WorkflowCompileError::new(
                source,
                1,
                1,
                format!("graph node `{id}` has no frontmatter definition"),
            ));
        };
        stages.push(WorkflowStage {
            id: id.clone(),
            title: node.title.clone(),
            agent: node.agent.clone(),
            directive: node.directive.clone(),
            mode: node.mode,
            verify: node.verify.clone(),
            actor: node.actor.clone(),
            level: stage_levels[index],
            predecessor_indices: predecessors[index].clone(),
        });
    }

    let definition = WorkflowDefinition {
        name: author.name,
        description: author.description,
        inputs: author.inputs,
        on_failure: author.on_failure,
    };
    let plan = WorkflowPlan { stages, levels };
    let revision = canonical_revision(&definition, &plan);
    Ok(CompiledWorkflow {
        definition,
        plan,
        revision,
    })
}

fn canonical_revision(definition: &WorkflowDefinition, plan: &WorkflowPlan) -> WorkflowRevision {
    let mut hash = Sha256::new();
    hash.update(b"hya.workflow.revision.v1\0");
    hash_string(&mut hash, &definition.name);
    hash_string(&mut hash, &definition.description);
    hash.update([match definition.on_failure {
        FailurePolicy::FailFast => 0,
        FailurePolicy::CollectAll => 1,
    }]);
    hash_usize(&mut hash, definition.inputs.len());
    for (name, description) in &definition.inputs {
        hash_string(&mut hash, name);
        hash_string(&mut hash, description);
    }
    hash_usize(&mut hash, plan.stages.len());
    for stage in &plan.stages {
        hash_string(&mut hash, &stage.id);
        hash_optional_string(&mut hash, stage.title.as_deref());
        hash_string(&mut hash, &stage.agent);
        hash_string(&mut hash, &stage.directive);
        hash_usize(&mut hash, stage.level);
        hash.update([match stage.mode {
            StageMode::Once => 0,
            StageMode::Loop => 1,
        }]);
        match &stage.verify {
            Some(verify) => {
                hash.update([1]);
                hash_string(&mut hash, &verify.agent);
                hash_string(&mut hash, &verify.until);
                hash.update(verify.max_iterations.to_be_bytes());
            }
            None => hash.update([0]),
        }
        hash_optional_string(&mut hash, stage.actor.as_deref());
        hash_usize(&mut hash, stage.predecessor_indices.len());
        for &predecessor in &stage.predecessor_indices {
            hash_usize(&mut hash, predecessor);
        }
    }
    WorkflowRevision(hash.finalize().into())
}

fn hash_optional_string(hash: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash.update([1]);
            hash_string(hash, value);
        }
        None => hash.update([0]),
    }
}

fn hash_string(hash: &mut Sha256, value: &str) {
    hash_usize(hash, value.len());
    hash.update(value.as_bytes());
}

fn hash_usize(hash: &mut Sha256, value: usize) {
    hash.update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
}

fn topological_levels(
    source: &str,
    cycle_location: SourcePoint,
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
) -> Result<Vec<WorkflowLevel>, WorkflowCompileError> {
    let mut indegree: Vec<usize> = predecessors.iter().map(Vec::len).collect();
    let mut ready: Vec<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, &count)| (count == 0).then_some(index))
        .collect();
    let mut visited = 0_usize;
    let mut levels = Vec::new();
    while !ready.is_empty() {
        let current = ready;
        visited = visited.saturating_add(current.len());
        let mut next = Vec::new();
        for &index in &current {
            for &successor in &successors[index] {
                indegree[successor] = indegree[successor].saturating_sub(1);
                if indegree[successor] == 0 {
                    next.push(successor);
                }
            }
        }
        next.sort_unstable();
        levels.push(WorkflowLevel {
            stage_indices: current,
        });
        ready = next;
    }
    if visited != predecessors.len() {
        return Err(WorkflowCompileError::new(
            source,
            cycle_location.line,
            cycle_location.column,
            "Workflow graph contains a cycle",
        ));
    }
    Ok(levels)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn first_non_whitespace_column(line: &str) -> usize {
    line.char_indices()
        .find_map(|(index, character)| (!character.is_whitespace()).then_some(index + 1))
        .unwrap_or(1)
}
