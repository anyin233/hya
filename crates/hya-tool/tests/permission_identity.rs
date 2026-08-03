use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{
    Action, Decision, InvocationPolicy, InvocationRule, Mode, PermissionInterceptor,
    PermissionModel, PermissionPlane, PermissionRules, PermissionTarget, Resource, Rule,
};

struct UnidentifiedInterceptor;

struct IdentifiedInterceptor {
    identity: [u8; 32],
}

#[async_trait]
impl PermissionInterceptor for UnidentifiedInterceptor {
    async fn intercept(
        &self,
        _session: Option<SessionId>,
        _action: Action,
        _resource: &Resource,
    ) -> Option<Decision> {
        None
    }
}

#[async_trait]
impl PermissionInterceptor for IdentifiedInterceptor {
    async fn intercept(
        &self,
        _session: Option<SessionId>,
        _action: Action,
        _resource: &Resource,
    ) -> Option<Decision> {
        None
    }

    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        Some(self.identity)
    }
}

fn fixture(
    model: PermissionModel,
    read_pattern: &str,
    invocation_selector: &str,
) -> PermissionPlane {
    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, read_pattern, Mode::Allow),
        Rule::new(Action::Bash, "git status", Mode::Ask),
    ]);
    let Ok(policy) = InvocationPolicy::compile(
        model,
        vec![
            InvocationRule::new(PermissionTarget::Tool, invocation_selector, Mode::Ask),
            InvocationRule::new(PermissionTarget::Command, "^git status$", Mode::Deny),
        ],
    ) else {
        panic!("permission identity fixture selector must compile");
    };
    PermissionPlane::new_with_policy(rules, policy).0
}

#[test]
fn permission_semantic_identity_tracks_immutable_policy_semantics() {
    let baseline = fixture(PermissionModel::Default, "/workspace/**", "^write$");
    let Some(baseline_identity) = baseline.semantic_identity_v1() else {
        panic!("permission identity fixture must have a representable policy");
    };
    assert_ne!(baseline_identity, [0; 32]);

    let same = fixture(PermissionModel::Default, "/workspace/**", "^write$");
    assert_eq!(same.semantic_identity_v1(), Some(baseline_identity));

    let changed_rule = fixture(PermissionModel::Default, "/other/**", "^write$");
    assert_ne!(changed_rule.semantic_identity_v1(), Some(baseline_identity));

    let changed_model = fixture(PermissionModel::Allow, "/workspace/**", "^write$");
    assert_ne!(
        changed_model.semantic_identity_v1(),
        Some(baseline_identity)
    );

    let changed_selector = fixture(PermissionModel::Default, "/workspace/**", "^edit$");
    assert_ne!(
        changed_selector.semantic_identity_v1(),
        Some(baseline_identity)
    );
}

#[test]
fn unidentified_permission_interceptor_makes_semantic_identity_unavailable() {
    let plane = fixture(PermissionModel::Default, "/workspace/**", "^write$")
        .with_interceptor(Arc::new(UnidentifiedInterceptor));

    assert_eq!(plane.semantic_identity_v1(), None);
}

#[test]
fn identified_permission_interceptor_contributes_to_semantic_identity() {
    let first = fixture(PermissionModel::Default, "/workspace/**", "^write$")
        .with_interceptor(Arc::new(IdentifiedInterceptor { identity: [1; 32] }));
    let Some(first_identity) = first.semantic_identity_v1() else {
        panic!("identified interceptor must contribute an identity");
    };
    assert_ne!(first_identity, [0; 32]);

    let same = fixture(PermissionModel::Default, "/workspace/**", "^write$")
        .with_interceptor(Arc::new(IdentifiedInterceptor { identity: [1; 32] }));
    assert_eq!(same.semantic_identity_v1(), Some(first_identity));

    let changed = fixture(PermissionModel::Default, "/workspace/**", "^write$")
        .with_interceptor(Arc::new(IdentifiedInterceptor { identity: [2; 32] }));
    assert_ne!(changed.semantic_identity_v1(), Some(first_identity));
}
