#[derive(Debug, PartialEq, Eq)]
pub enum RpcRequest {
    Prompt { text: String },
    Quit,
}

#[must_use]
pub fn parse_rpc(line: &str) -> Option<RpcRequest> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    match value.get("type").and_then(serde_json::Value::as_str)? {
        "prompt" => Some(RpcRequest::Prompt {
            text: value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "quit" => Some(RpcRequest::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prompt_quit_and_rejects_garbage() {
        assert_eq!(
            parse_rpc(r#"{"type":"prompt","text":"hi"}"#),
            Some(RpcRequest::Prompt {
                text: "hi".to_string()
            })
        );
        assert_eq!(parse_rpc(r#"{"type":"quit"}"#), Some(RpcRequest::Quit));
        assert_eq!(parse_rpc(""), None);
        assert_eq!(parse_rpc("not json"), None);
        assert_eq!(parse_rpc(r#"{"type":"unknown"}"#), None);
    }
}
