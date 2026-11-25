use std::collections::{BTreeMap, BTreeSet};

use hya_workflow::{WorkflowSource, compile};

use super::decode::digest_bytes;
use super::parse::normalize_source_path;
use crate::error::BundleError;
use crate::model::{Bundle, PreparedAgent, PreparedWorkflow};

pub(super) fn prepare_workflow(
    bundle_id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    workflow: crate::source::SourceWorkflow,
    agents: &[PreparedAgent],
) -> Result<PreparedWorkflow, BundleError> {
    let workflow_path = normalize_source_path(bundle_id, &workflow.path)?;
    if !is_canonical_workflow_path(&workflow_path) {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "Workflow source path must match `workflows/*.hya.md`".to_string(),
        });
    }
    let workflow_bytes =
        files
            .get(&workflow_path)
            .ok_or_else(|| BundleError::MissingReference {
                bundle_id: bundle_id.to_string(),
                path: workflow_path.clone(),
            })?;
    let workflow_source =
        std::str::from_utf8(workflow_bytes).map_err(|error| BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!("Workflow source `{workflow_path}` is not UTF-8: {error}"),
        })?;
    let compiled =
        compile(WorkflowSource::new(&workflow_path, workflow_source)).map_err(|error| {
            BundleError::WorkflowCompile {
                bundle_id: bundle_id.to_string(),
                source_path: workflow_path.clone(),
                detail: error.to_string(),
            }
        })?;
    if workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id: bundle_id.to_string(),
            manifest_id: workflow.id,
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    validate_workflow_agent_closure(bundle_id, &compiled, agents)?;
    Ok(PreparedWorkflow {
        id: compiled.definition().name().to_string(),
        source_path: workflow_path,
        source: workflow_source.to_string(),
        source_digest: digest_bytes(workflow_bytes),
        compiler_revision: compiled.revision().to_string(),
    })
}

fn validate_workflow_agent_closure(
    bundle_id: &str,
    workflow: &hya_workflow::CompiledWorkflow,
    agents: &[PreparedAgent],
) -> Result<(), BundleError> {
    let available = agents
        .iter()
        .map(|agent| (agent.id.as_str(), agent))
        .collect::<BTreeMap<_, _>>();
    let mut required = BTreeSet::new();
    let mut queue = Vec::new();
    for stage in workflow.plan().stages() {
        require_workflow_agent(
            bundle_id,
            stage.agent(),
            &format!("stage:{}", stage.id()),
            &available,
            &mut required,
            &mut queue,
        )?;
        if let Some(verify) = stage.verify() {
            require_workflow_agent(
                bundle_id,
                verify.agent(),
                &format!("verifier:{}", stage.id()),
                &available,
                &mut required,
                &mut queue,
            )?;
        }
    }
    while let Some(agent_id) = queue.pop() {
        let Some(agent) = available.get(agent_id.as_str()) else {
            continue;
        };
        for target in &agent.can_spawn {
            let target_id = target.as_str();
            require_workflow_agent(
                bundle_id,
                target_id,
                &format!("agent:{agent_id}"),
                &available,
                &mut required,
                &mut queue,
            )?;
        }
    }
    for agent in agents {
        if !required.contains(agent.id.as_str()) {
            return Err(BundleError::WorkflowAgentUnreachable {
                bundle_id: bundle_id.to_string(),
                agent_id: agent.id.as_str().to_string(),
            });
        }
    }
    Ok(())
}

fn require_workflow_agent<'a>(
    bundle_id: &str,
    agent_id: &str,
    reference: &str,
    available: &BTreeMap<&'a str, &'a PreparedAgent>,
    required: &mut BTreeSet<String>,
    queue: &mut Vec<String>,
) -> Result<(), BundleError> {
    if !available.contains_key(agent_id) {
        return Err(BundleError::WorkflowAgentMissing {
            bundle_id: bundle_id.to_string(),
            agent_id: agent_id.to_string(),
            reference: reference.to_string(),
        });
    }
    if required.insert(agent_id.to_string()) {
        queue.push(agent_id.to_string());
    }
    Ok(())
}

pub(super) fn validate_prepared_workflow(
    bundle: &Bundle,
    workflow: &PreparedWorkflow,
) -> Result<(), BundleError> {
    let compiled =
        compile(WorkflowSource::new(&workflow.source_path, &workflow.source)).map_err(|error| {
            BundleError::WorkflowCompile {
                bundle_id: bundle.identity.id.clone(),
                source_path: workflow.source_path.clone(),
                detail: error.to_string(),
            }
        })?;
    if workflow.id != compiled.definition().name() {
        return Err(BundleError::WorkflowIdMismatch {
            bundle_id: bundle.identity.id.clone(),
            manifest_id: workflow.id.clone(),
            compiled_id: compiled.definition().name().to_string(),
        });
    }
    if workflow.compiler_revision != compiled.revision().to_string() {
        return Err(BundleError::PreparedContentDigestMismatch {
            bundle_id: bundle.identity.id.clone(),
            source_path: workflow.source_path.clone(),
        });
    }
    validate_workflow_agent_closure(&bundle.identity.id, &compiled, &bundle.agents)
}

pub(super) fn is_canonical_workflow_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("workflows/") else {
        return false;
    };
    !name.is_empty() && !name.contains('/') && name.ends_with(".hya.md")
}
