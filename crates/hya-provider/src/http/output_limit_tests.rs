//! Per-model output limits shape every protocol's max-tokens field.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::ModelRef;
use serde_json::Value;

use super::{HttpProvider, ModelLimitOverride, ProviderKind};
use crate::{CompletionRequest, Provider as _, ReasoningEffort};

const LIMITED: &str = "glm-5.3-flash";
const UNLIMITED: &str = "glm-5.3";

fn route(kind: ProviderKind, output: u32) -> HttpProvider {
    HttpProvider::new(
        "12th",
        kind,
        "https://api.12th.day/v1",
        Some("key".to_string()),
        [LIMITED.to_string(), UNLIMITED.to_string()],
    )
    .unwrap()
    .with_model_limits([(
        LIMITED.to_string(),
        ModelLimitOverride {
            context: 1_048_576,
            output,
        },
    )])
}

fn request(model: &str, max: Option<u32>, reasoning: Option<ReasoningEffort>) -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new(model),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: max,
        reasoning,
        headers: Default::default(),
    }
}

fn body(route: &HttpProvider, req: CompletionRequest) -> Value {
    route.prepare_request(req).unwrap().1
}

#[test]
fn anthropic_defaults_max_tokens_to_the_known_output_limit() {
    let route = route(ProviderKind::Anthropic, 131_072);
    let body = body(&route, request("12th/glm-5.3-flash", None, None));
    assert_eq!(body["max_tokens"], 131_072);
    assert_eq!(body["model"], LIMITED);
}

#[test]
fn anthropic_clamps_an_explicit_request_to_the_known_output_limit() {
    let route = route(ProviderKind::Anthropic, 131_072);
    let over = body(&route, request(LIMITED, Some(200_000), None));
    assert_eq!(over["max_tokens"], 131_072);
    let under = body(&route, request(LIMITED, Some(1_000), None));
    assert_eq!(under["max_tokens"], 1_000, "smaller explicit values win");
}

#[test]
fn anthropic_without_a_known_limit_keeps_the_4096_fallback() {
    let route = route(ProviderKind::Anthropic, 131_072);
    let body = body(&route, request(UNLIMITED, None, None));
    assert_eq!(body["max_tokens"], 4096);
    let context_only = self::route(ProviderKind::Anthropic, 0);
    let body = self::body(&context_only, request(LIMITED, None, None));
    assert_eq!(
        body["max_tokens"], 4096,
        "a context-only limit is not an output limit"
    );
}

#[test]
fn anthropic_thinking_budget_stays_below_the_known_output_limit() {
    // A large limit leaves the budget untouched.
    let wide = route(ProviderKind::Anthropic, 131_072);
    let body = body(&wide, request(LIMITED, None, Some(ReasoningEffort::Max)));
    assert_eq!(body["max_tokens"], 131_072);
    assert_eq!(body["thinking"]["budget_tokens"], 31_999);

    // An explicit small request is raised above the budget but never past the limit.
    let body = self::body(
        &wide,
        request(LIMITED, Some(1_024), Some(ReasoningEffort::High)),
    );
    assert_eq!(body["max_tokens"], 16_000 + 4096);
    assert_eq!(body["thinking"]["budget_tokens"], 16_000);

    // A limit below the budget shrinks the budget to leave room for an answer.
    let narrow = route(ProviderKind::Anthropic, 8_192);
    let body = self::body(&narrow, request(LIMITED, None, Some(ReasoningEffort::High)));
    assert_eq!(body["max_tokens"], 8_192);
    assert_eq!(body["thinking"]["budget_tokens"], 4_096);

    // A limit too small for the minimum 1024-token budget drops thinking.
    let tiny = route(ProviderKind::Anthropic, 1_500);
    let body = self::body(&tiny, request(LIMITED, None, Some(ReasoningEffort::Medium)));
    assert_eq!(body["max_tokens"], 1_500);
    assert!(body.get("thinking").is_none(), "got {body}");

    // Without a known limit the historical raise is unchanged.
    let body = self::body(&wide, request(UNLIMITED, None, Some(ReasoningEffort::High)));
    assert_eq!(body["max_tokens"], 16_000 + 4096);
    assert_eq!(body["thinking"]["budget_tokens"], 16_000);
}

#[test]
fn openai_compatible_sends_max_tokens_only_when_the_limit_is_known() {
    let route = route(ProviderKind::OpenAiCompatible, 131_072);
    assert_eq!(
        body(&route, request(LIMITED, None, None))["max_tokens"],
        131_072
    );
    assert_eq!(
        body(&route, request(LIMITED, Some(500_000), None))["max_tokens"],
        131_072
    );
    let unknown = body(&route, request(UNLIMITED, None, None));
    assert!(unknown.get("max_tokens").is_none(), "got {unknown}");
    assert_eq!(
        body(&route, request(UNLIMITED, Some(64), None))["max_tokens"],
        64
    );
}

#[test]
fn responses_and_google_follow_the_same_limit_rule() {
    let responses = route(ProviderKind::OpenAiResponse, 131_072);
    assert_eq!(
        body(&responses, request(LIMITED, None, None))["max_output_tokens"],
        131_072
    );
    let unknown = body(&responses, request(UNLIMITED, None, None));
    assert!(unknown.get("max_output_tokens").is_none(), "got {unknown}");

    let google = route(ProviderKind::Google, 65_536);
    assert_eq!(
        body(&google, request(LIMITED, Some(100_000), None))["generationConfig"]["maxOutputTokens"],
        65_536
    );
    let unknown = body(&google, request(UNLIMITED, None, None));
    assert!(unknown.get("generationConfig").is_none(), "got {unknown}");
}

#[test]
fn catalog_rows_advertise_the_known_output_limit() {
    let route = route(ProviderKind::Anthropic, 131_072);
    let caps = route
        .capabilities(&ModelRef::new("12th/glm-5.3-flash"))
        .unwrap();
    assert_eq!(caps.max_output, 131_072);
    assert_eq!(caps.max_context, 1_048_576);
    let row = route
        .catalog()
        .into_iter()
        .find(|row| row.model_id == LIMITED)
        .unwrap();
    assert_eq!(row.capabilities.max_output, 131_072);
}

#[test]
fn configured_identity_covers_model_limits() {
    let identity = |output: u32| route(ProviderKind::Anthropic, output).configured_identity_v1();
    assert!(identity(131_072).is_some());
    assert_eq!(identity(131_072), identity(131_072));
    assert_ne!(
        identity(131_072),
        identity(8_192),
        "changing only an output limit changes the request shape"
    );
}
