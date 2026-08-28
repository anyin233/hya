//! Workflow graph planning: placeholder closure, cycle detection, and
//! topological levelization into parallel batches.
//!
//! Levels are the fan-out unit: every stage whose `needs` are satisfied at the
//! same level runs as ONE governed team batch at execution time.

use std::collections::{BTreeMap, BTreeSet};

use super::{WorkflowDef, WorkflowError};

/// A validated workflow graph ready for execution.
#[derive(Clone, Debug)]
pub struct WorkflowPlan {
    levels: Vec<Vec<usize>>,
}

impl WorkflowPlan {
    /// Topological levels; each holds indices into
    /// [`WorkflowDef::stages`] in declaration order.
    #[must_use]
    pub fn levels(&self) -> &[Vec<usize>] {
        &self.levels
    }
}

/// Validate the stage graph and levelize it into parallel batches.
///
/// # Errors
/// [`WorkflowError::Invalid`] when `needs` reference unknown stages (or the
/// stage itself), a cycle exists, a template placeholder escapes its upstream
/// closure, or an `inputs.` placeholder names an undeclared input.
pub fn build_plan(def: &WorkflowDef) -> Result<WorkflowPlan, WorkflowError> {
    def.validate()?;
    let index_of = |id: &str| def.stages.iter().position(|s| s.id == id);

    for stage in &def.stages {
        for dep in &stage.needs {
            match index_of(dep) {
                Some(dep_index) if dep_index != position(def, &stage.id) => {}
                Some(_) => {
                    return Err(invalid(
                        def,
                        format!("stage `{}` depends on itself", stage.id),
                    ));
                }
                None => {
                    return Err(invalid(
                        def,
                        format!("stage `{}` depends on unknown stage `{}`", stage.id, dep),
                    ));
                }
            }
        }
    }

    // Kahn levelization preserving declaration order within each level.
    let mut remaining: BTreeSet<usize> = (0..def.stages.len()).collect();
    let mut done_ids: BTreeSet<&str> = BTreeSet::new();
    let mut levels: Vec<Vec<usize>> = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| {
                def.stages[i]
                    .needs
                    .iter()
                    .all(|dep| done_ids.contains(dep.as_str()))
            })
            .collect();
        if ready.is_empty() {
            let stuck: Vec<String> = remaining
                .iter()
                .map(|&i| def.stages[i].id.clone())
                .collect();
            return Err(invalid(
                def,
                format!("dependency cycle among stages {stuck:?}"),
            ));
        }
        for &i in &ready {
            done_ids.insert(def.stages[i].id.as_str());
            remaining.remove(&i);
        }
        levels.push(ready);
    }

    // Placeholder closure: `{{inputs.*}}` and `{{upstream}}` references must be
    // declared / strictly-upstream of the referencing stage.
    for (stage_position, stage) in def.stages.iter().enumerate() {
        let upstream = upstream_closure(def, stage_position);
        for token in scan_placeholders(&stage.prompt).map_err(|detail| invalid(def, detail))? {
            if let Some(key) = token.strip_prefix("inputs.") {
                if !def.inputs.contains_key(key) {
                    return Err(invalid(
                        def,
                        format!("stage `{}` references undeclared input `{key}`", stage.id),
                    ));
                }
            } else if !upstream.contains(token.as_str()) {
                return Err(invalid(
                    def,
                    format!(
                        "stage `{}` references `{token}`, which is not an upstream stage",
                        stage.id
                    ),
                ));
            }
        }
    }

    Ok(WorkflowPlan { levels })
}

fn invalid(workflow: &WorkflowDef, detail: String) -> WorkflowError {
    WorkflowError::Invalid {
        workflow: workflow.name.clone(),
        detail,
    }
}

fn position(def: &WorkflowDef, id: &str) -> usize {
    def.stages
        .iter()
        .position(|s| s.id == id)
        .unwrap_or(usize::MAX)
}

/// All transitive upstream stage ids of `at`.
fn upstream_closure(def: &WorkflowDef, at: usize) -> BTreeSet<&str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: Vec<&str> = def.stages[at].needs.iter().map(String::as_str).collect();
    while let Some(id) = queue.pop() {
        if seen.insert(id)
            && let Some(i) = def.stages.iter().position(|s| s.id == id)
        {
            queue.extend(def.stages[i].needs.iter().map(String::as_str));
        }
    }
    seen
}

/// Scan `{{...}}` placeholders in declaration order; unclosed fences are errors.
fn scan_placeholders(template: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("unclosed placeholder near {:?}", &rest[start..]));
        };
        let token = after[..end].trim().to_string();
        if token.is_empty() {
            return Err("empty placeholder {{}}".to_string());
        }
        tokens.push(token);
        rest = &after[end + 2..];
    }
    Ok(tokens)
}

/// Render a prompt template with input values and completed upstream sections.
///
/// Upstream results render as bounded markdown sections whose header carries
/// the stage id and terminal status, so the joining model sees exactly what ran
/// and how it ended. Ordering follows the consumer's `needs` declaration order
/// (falling back to declaration order), which keeps replay deterministic.
#[allow(dead_code)] // exercised by run.rs; kept public for cross-module reuse
pub(crate) fn render_stage_section(report: &super::run::StageReport) -> String {
    let head = format!("## upstream stage `{}` ({})", report.stage, report.agent);
    match report.status {
        super::run::StageStatus::Done => {
            format!("{head}\n{}\n", render_bounded(&report.output))
        }
        super::run::StageStatus::Failed => {
            format!("{head} FAILED\n{}\n", render_bounded(&report.output))
        }
    }
}

/// Clamp oversized handoff text so joins stay bounded.
fn render_bounded(text: &str) -> &str {
    // Character-boundary safe truncation without panicking on multibyte input.
    if text.len() <= super::run::MAX_STAGE_OUTPUT_CHARS {
        return text;
    }
    let mut end = super::run::MAX_STAGE_OUTPUT_CHARS;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Substitute placeholders in `template` using declared inputs and finished
/// upstream stages.
///
/// # Errors
/// [`WorkflowError::Invalid`] when a required input is missing or an upstream
/// section cannot be resolved (a planning bug guard).
pub(crate) fn render_template(
    workflow: &str,
    template: &str,
    inputs: &BTreeMap<String, String>,
    outputs: &BTreeMap<String, StageSection>,
) -> Result<String, WorkflowError> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("unclosed placeholder near {:?}", &rest[start..]),
            });
        };
        let token = after[..end].trim();
        rendered.push_str(&rest[..start]);
        if let Some(key) = token.strip_prefix("inputs.") {
            let value = inputs.get(key).ok_or_else(|| WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("input `{key}` was not provided for this run"),
            })?;
            rendered.push_str(value);
        } else {
            let section = outputs.get(token).ok_or_else(|| WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("placeholder `{token}` has no completed upstream"),
            })?;
            rendered.push_str(section.text());
        }
        rest = &after[end + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

/// Upstream material available to a consuming template.
#[derive(Clone, Debug)]
pub(crate) struct StageSection {
    /// Rendered markdown body (bounded).
    text: String,
}

impl StageSection {
    /// Build a section from a finished stage report.
    #[must_use]
    pub(crate) fn from_report(report: &super::run::StageReport) -> Self {
        Self {
            text: render_stage_section(report),
        }
    }

    /// Rendered markdown body.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::WorkflowDef;

    fn plan(def_yaml: &str) -> Result<WorkflowPlan, WorkflowError> {
        let d = serde_norway::from_str::<WorkflowDef>(def_yaml)
            .map_err(|e| WorkflowError::Parse(e.to_string()))?;
        build_plan(&d)
    }

    #[test]
    fn levelization_fans_out_same_level_stages() {
        let levels = plan(
            r#"
name: t
description: t
stages:
  - id: plan
    agent: a
    prompt: p
  - id: a
    agent: a
    prompt: a
    needs: [plan]
  - id: b
    agent: a
    prompt: b
    needs: [plan]
  - id: join
    agent: a
    prompt: j
    needs: [a, b]
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(levels.levels(), &[vec![0], vec![1, 2], vec![3]]);
    }

    #[test]
    fn cycles_are_rejected() {
        let error = plan(
            r#"
name: t
description: t
stages:
  - id: a
    agent: x
    prompt: p
    needs: [b]
  - id: b
    agent: x
    prompt: p
    needs: [a]
"#,
        )
        .err()
        .unwrap_or_else(|| panic!("cycle must fail"));
        assert!(error.to_string().contains("cycle"), "{error}");
    }

    #[test]
    fn placeholders_must_reference_declared_inputs_and_upstream() {
        let valid = plan(
            r#"
name: t
description: t
inputs:
  k: desc
stages:
  - id: src
    agent: a
    prompt: "s {{inputs.k}}"
  - id: sink
    agent: a
    prompt: "{{src}}"
    needs: [src]
"#,
        );
        assert!(valid.is_ok(), "valid closure must plan");

        let forward = plan(
            r#"
name: t
description: t
stages:
  - id: src
    agent: a
    prompt: "{{sink}}"
  - id: sink
    agent: a
    prompt: s
"#,
        );
        assert!(forward.is_err(), "forward references must fail");

        let undeclared = plan(
            r#"
name: t
description: t
stages:
  - id: src
    agent: a
    prompt: "{{inputs.missing}}"
"#,
        );
        assert!(undeclared.is_err());
    }

    #[test]
    fn self_dependency_is_rejected() {
        let error = plan(
            r#"
name: t
description: t
stages:
  - id: a
    agent: x
    prompt: p
    needs: [a]
"#,
        )
        .err()
        .unwrap_or_else(|| panic!("self dependency must fail"));
        assert!(error.to_string().contains("itself"), "{error}");
    }
}
