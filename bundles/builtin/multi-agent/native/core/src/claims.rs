//! Actor lease/fence claim and state persistence via [`HostBundleStorage`].
//!
//! All actor lease fencing and team supervisor checkpoints flow exclusively
//! through `HostRuntime::bundle_storage()` under bundle id `"hya/multi-agent"`,
//! backed by `bundle_claim` and `bundle_state` in `hya-store` v2.

use hya_proto::{ActorClaim, ActorEpoch, OwnerRunId, SessionId};
use hya_runtime_abi::{BundleClaimLease, BundleClaimRequest, HostError, HostRuntime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Canonical bundle id owning multi-agent storage claims and extension events.
pub const MULTI_AGENT_BUNDLE_ID: &str = hya_tool::extension::BUNDLE_MULTI_AGENT;

/// Active actor claim lease paired with its typed [`ActorClaim`] fence token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FencedActorLease {
    /// Owning team root session id.
    pub root: SessionId,
    /// Typed fencing claim (`actor_id`, `epoch`, `owner_run_id`).
    pub claim: ActorClaim,
    /// Underlying `bundle_claim` lease for renewals and release.
    pub lease: BundleClaimLease,
}

/// Persisted checkpoint for a supervised team stored in `bundle_state`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamCheckpoint {
    /// Team root session id.
    pub root: SessionId,
    /// Runtime owner that last updated this checkpoint.
    pub owner_run_id: String,
    /// Total resident turns recorded for budget tracking.
    pub resident_turns: u64,
    /// Total mail messages observed for budget tracking.
    pub team_messages: u64,
}

/// Convert a [`BundleClaimLease`] into a typed [`ActorClaim`].
///
/// # Errors
/// Returns [`HostError::Storage`] if the lease's resource key, owner id, or
/// epoch is malformed.
pub fn lease_to_actor_claim(lease: &BundleClaimLease) -> Result<ActorClaim, HostError> {
    let actor_id: SessionId = lease.resource_key.parse().map_err(|error| {
        HostError::Storage(format!(
            "invalid actor_id `{}` in bundle_claim: {error}",
            lease.resource_key
        ))
    })?;
    let owner_run_id: OwnerRunId = serde_json::from_value(Value::String(lease.owner_id.clone()))
        .map_err(|error| {
            HostError::Storage(format!(
                "invalid owner_run_id `{}` in bundle_claim: {error}",
                lease.owner_id
            ))
        })?;
    if lease.epoch == 0 {
        return Err(HostError::Storage(
            "bundle_claim actor epoch must be >= 1".to_string(),
        ));
    }
    Ok(ActorClaim {
        actor_id,
        epoch: ActorEpoch::from_storage(lease.epoch),
        owner_run_id,
    })
}

/// Extract the `root_session_id` metadata field from a [`BundleClaimLease`].
#[must_use]
pub fn lease_root_session(lease: &BundleClaimLease) -> Option<SessionId> {
    lease
        .metadata
        .get("root_session_id")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse().ok())
}

/// Acquire a fenced actor claim in `bundle_claim` under `"hya/multi-agent"`.
///
/// Passing `ttl_millis = 0` requests an indefinite claim (released explicitly
/// on archive or taken over by a newer `OwnerRunId` after restart).
pub async fn acquire_actor_claim(
    runtime: &dyn HostRuntime,
    root: SessionId,
    actor: SessionId,
    owner: OwnerRunId,
    ttl_millis: u64,
) -> Result<FencedActorLease, HostError> {
    let lease = runtime
        .bundle_storage()
        .acquire_claim(BundleClaimRequest {
            bundle: MULTI_AGENT_BUNDLE_ID.to_string(),
            resource_key: actor.to_string(),
            owner_id: owner.as_uuid().to_string(),
            ttl_millis,
            metadata: json!({ "root_session_id": root.to_string() }),
        })
        .await?;
    let claim = lease_to_actor_claim(&lease)?;
    Ok(FencedActorLease { root, claim, lease })
}

/// Renew an active actor claim lease, returning [`HostError::StaleClaim`] if superseded.
pub async fn renew_actor_claim(
    runtime: &dyn HostRuntime,
    fenced: &FencedActorLease,
    ttl_millis: u64,
) -> Result<FencedActorLease, HostError> {
    let lease = runtime
        .bundle_storage()
        .renew_claim(&fenced.lease, ttl_millis)
        .await?;
    let claim = lease_to_actor_claim(&lease)?;
    Ok(FencedActorLease {
        root: fenced.root,
        claim,
        lease,
    })
}

/// Release an active actor claim lease, returning [`HostError::StaleClaim`] if superseded.
pub async fn release_actor_claim(
    runtime: &dyn HostRuntime,
    fenced: &FencedActorLease,
) -> Result<(), HostError> {
    runtime.bundle_storage().release_claim(&fenced.lease).await
}

/// Read the active actor claim for `actor`, if one exists.
pub async fn read_actor_claim(
    runtime: &dyn HostRuntime,
    actor: SessionId,
) -> Result<Option<FencedActorLease>, HostError> {
    let Some(lease) = runtime
        .bundle_storage()
        .read_claim(MULTI_AGENT_BUNDLE_ID, &actor.to_string())
        .await?
    else {
        return Ok(None);
    };
    let root = lease_root_session(&lease).unwrap_or(actor);
    let claim = lease_to_actor_claim(&lease)?;
    Ok(Some(FencedActorLease { root, claim, lease }))
}

/// List all active actor claims owned by `"hya/multi-agent"`, optionally
/// filtered to one team root.
pub async fn list_active_actor_claims(
    runtime: &dyn HostRuntime,
    root_filter: Option<SessionId>,
) -> Result<Vec<FencedActorLease>, HostError> {
    let leases = runtime
        .bundle_storage()
        .list_claims(MULTI_AGENT_BUNDLE_ID)
        .await?;
    let mut out = Vec::new();
    for lease in leases {
        let root = lease_root_session(&lease);
        if let Some(expected) = root_filter
            && root != Some(expected)
        {
            continue;
        }
        let claim = lease_to_actor_claim(&lease)?;
        out.push(FencedActorLease {
            root: root.unwrap_or(claim.actor_id),
            claim,
            lease,
        });
    }
    Ok(out)
}

/// Count live claimed members belonging to `root`.
pub async fn live_member_claim_count(
    runtime: &dyn HostRuntime,
    root: SessionId,
) -> Result<u64, HostError> {
    let claims = list_active_actor_claims(runtime, Some(root)).await?;
    Ok(claims.len() as u64)
}

/// Take over stale claims from previous process runs, re-acquiring each under
/// `new_owner` (bumping its fencing epoch).
pub async fn recover_stale_actor_claims(
    runtime: &dyn HostRuntime,
    new_owner: OwnerRunId,
) -> Result<Vec<(FencedActorLease, ActorEpoch)>, HostError> {
    let existing = list_active_actor_claims(runtime, None).await?;
    let mut recovered = Vec::new();
    for entry in existing {
        if entry.claim.owner_run_id == new_owner {
            continue;
        }
        let previous_epoch = entry.claim.epoch;
        let fresh =
            acquire_actor_claim(runtime, entry.root, entry.claim.actor_id, new_owner, 0).await?;
        recovered.push((fresh, previous_epoch));
    }
    Ok(recovered)
}

/// Persist a [`TeamCheckpoint`] into `bundle_state` under `"hya/multi-agent"`.
pub async fn save_team_checkpoint(
    runtime: &dyn HostRuntime,
    checkpoint: &TeamCheckpoint,
) -> Result<(), HostError> {
    let value = serde_json::to_value(checkpoint)
        .map_err(|error| HostError::Internal(format!("serialize team checkpoint: {error}")))?;
    let _ = runtime
        .bundle_storage()
        .put_state(
            MULTI_AGENT_BUNDLE_ID,
            "team",
            &checkpoint.root.to_string(),
            value,
            None,
        )
        .await?;
    Ok(())
}

/// Read a persisted [`TeamCheckpoint`] from `bundle_state`, if present.
pub async fn load_team_checkpoint(
    runtime: &dyn HostRuntime,
    root: SessionId,
) -> Result<Option<TeamCheckpoint>, HostError> {
    let Some(entry) = runtime
        .bundle_storage()
        .get_state(MULTI_AGENT_BUNDLE_ID, "team", &root.to_string())
        .await?
    else {
        return Ok(None);
    };
    let checkpoint = serde_json::from_value(entry.value)
        .map_err(|error| HostError::Storage(format!("decode team checkpoint: {error}")))?;
    Ok(Some(checkpoint))
}
