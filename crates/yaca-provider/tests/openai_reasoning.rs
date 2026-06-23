use yaca_proto::ModelRef;
use yaca_provider::{CompletionRequest, OpenAiChatProtocol, Protocol, ReasoningEffort};

#[test]
fn openai_encodes_max_reasoning_as_supported_xhigh() {
    // Given
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5.5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::Max),
    };

    // When
    let body = match OpenAiChatProtocol.encode(&req) {
        Ok(body) => body,
        Err(err) => panic!("OpenAI encoding failed: {err}"),
    };

    // Then
    assert_eq!(body["reasoning_effort"], "xhigh");
}
