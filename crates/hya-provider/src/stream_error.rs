//! Classification of provider error frames delivered inside an established
//! (HTTP 200) SSE body.
//!
//! Upstreams and gateways report throttling and outages in-band — Anthropic
//! sends `{"type":"error","error":{"type":"overloaded_error",…}}`, Gemini
//! sends `{"error":{"code":429,"status":"RESOURCE_EXHAUSTED",…}}`, Responses
//! sends `response.failed` with `response.error.code`, and gateways often send
//! only `{"error":{"message":"… please retry later"}}`. Recognized classes
//! become the [`ProviderError::HttpStatus`] their out-of-band HTTP response
//! would have carried, so the existing retryability rules
//! ([`ProviderError::is_retryable_before_stream`]), `Retry-After` handling, and
//! downstream failure classes apply unchanged. Unrecognized frames stay
//! [`ProviderError::Http`] with the upstream message (never retried).

use std::time::Duration;

use serde_json::Value;

use crate::ProviderError;

/// Longest in-band retry hint honored, matching the `Retry-After` header cap.
const MAX_IN_STREAM_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Classify an in-stream error payload.
///
/// `error` is the error object (or string) itself; `frame` is the whole SSE
/// frame, consulted for a top-level retry hint. `fallback` is the message used
/// when the payload carries none.
pub(crate) fn classify_error_frame(error: &Value, frame: &Value, fallback: &str) -> ProviderError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .or_else(|| frame.get("message").and_then(Value::as_str))
        .unwrap_or(fallback);
    let labels = ["type", "code", "status"]
        .into_iter()
        .filter_map(|key| error.get(key).and_then(Value::as_str));
    let mut label = None;
    let mut status = None;
    for candidate in labels {
        if let Some(mapped) = status_for_label(candidate) {
            label = Some(candidate);
            status = Some(mapped);
            break;
        }
    }
    let status = status
        .or_else(|| numeric_status(error))
        .or_else(|| status_for_message(message));
    let Some(status) = status else {
        return ProviderError::Http(message.to_string());
    };
    let message = match label {
        Some(label) => format!("in-stream error ({label}): {message}"),
        None => format!("in-stream error: {message}"),
    };
    ProviderError::HttpStatus {
        status,
        message: message.chars().take(500).collect(),
        retry_after: retry_after(error).or_else(|| retry_after(frame)),
    }
}

/// Map a provider error `type` / `code` / `status` label to its HTTP status.
fn status_for_label(label: &str) -> Option<u16> {
    let status = match label.to_ascii_lowercase().as_str() {
        "rate_limit_error"
        | "rate_limit_exceeded"
        | "rate_limited"
        | "too_many_requests"
        | "resource_exhausted" => 429,
        "overloaded_error" | "overloaded" => 529,
        "api_error" | "server_error" | "internal_error" | "internal_server_error" | "internal" => {
            500
        }
        "service_unavailable" | "unavailable" => 503,
        "timeout_error" | "timeout" | "deadline_exceeded" | "gateway_timeout" => 504,
        "invalid_request_error"
        | "invalid_request"
        | "invalid_argument"
        | "bad_request"
        | "failed_precondition" => 400,
        "authentication_error" | "unauthenticated" | "invalid_api_key" => 401,
        "permission_error" | "permission_denied" => 403,
        "not_found_error" | "not_found" | "model_not_found" => 404,
        "request_too_large" => 413,
        _ => return None,
    };
    Some(status)
}

/// A numeric `code` / `status` in the HTTP error range (Gemini, gateways).
fn numeric_status(error: &Value) -> Option<u16> {
    ["code", "status"]
        .into_iter()
        .filter_map(|key| error.get(key).and_then(Value::as_u64))
        .filter_map(|code| u16::try_from(code).ok())
        .find(|code| (400..=599).contains(code))
}

/// Last resort for untyped gateway frames: transient-sounding messages.
fn status_for_message(message: &str) -> Option<u16> {
    let message = message.to_ascii_lowercase();
    let has = |needle: &str| message.contains(needle);
    if has("overloaded") {
        Some(529)
    } else if has("rate limit")
        || has("rate-limit")
        || has("too many requests")
        || has("concurrency limit")
        || has("retry later")
        || has("try again later")
    {
        Some(429)
    } else if has("temporarily unavailable") || has("service unavailable") {
        Some(503)
    } else {
        None
    }
}

/// In-band retry hint in seconds (`retry_after`), bounded like `Retry-After`.
fn retry_after(value: &Value) -> Option<Duration> {
    let hint = value.get("retry_after")?;
    let seconds = hint
        .as_f64()
        .or_else(|| hint.as_str().and_then(|s| s.trim().parse::<f64>().ok()))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        seconds.min(MAX_IN_STREAM_RETRY_AFTER.as_secs_f64()),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn classify(frame: &Value) -> ProviderError {
        let error = frame.get("error").unwrap_or(frame);
        classify_error_frame(error, frame, "provider returned an error")
    }

    fn status(error: &ProviderError) -> Option<u16> {
        match error {
            ProviderError::HttpStatus { status, .. } => Some(*status),
            _ => None,
        }
    }

    #[test]
    fn classifies_in_stream_error_frames() {
        let cases = [
            (
                json!({"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}),
                Some(429),
                true,
            ),
            (
                json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
                Some(529),
                true,
            ),
            (
                json!({"type":"error","error":{"type":"api_error","message":"boom"}}),
                Some(500),
                true,
            ),
            (
                json!({"error":{"message":"Concurrency limit exceeded for account, please retry later"}}),
                Some(429),
                true,
            ),
            (
                json!({"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota"}}),
                Some(429),
                true,
            ),
            (
                json!({"error":{"code":503,"message":"backend down"}}),
                Some(503),
                true,
            ),
            (
                json!({"type":"error","code":"rate_limit_exceeded","message":"slow"}),
                Some(429),
                true,
            ),
            (
                json!({"type":"error","error":{"type":"invalid_request_error","message":"please retry later"}}),
                Some(400),
                false,
            ),
            (
                json!({"type":"error","error":{"type":"authentication_error","message":"bad key"}}),
                Some(401),
                false,
            ),
            (
                json!({"type":"error","error":{"type":"permission_error","message":"no"}}),
                Some(403),
                false,
            ),
            (json!({"error":{"message":"quota exhausted"}}), None, false),
            (json!({"error":"something odd"}), None, false),
        ];
        for (frame, expected_status, retryable) in cases {
            let error = classify(&frame);
            assert_eq!(status(&error), expected_status, "{frame}: {error:?}");
            assert_eq!(
                error.is_retryable_before_stream(),
                retryable,
                "{frame}: {error:?}"
            );
        }
    }

    #[test]
    fn keeps_the_upstream_message_and_label() {
        let error = classify(&json!({"error":{"type":"rate_limit_error","message":"slow down"}}));
        assert!(
            matches!(&error, ProviderError::HttpStatus { message, .. } if message == "in-stream error (rate_limit_error): slow down"),
            "{error:?}"
        );
        let error = classify(&json!({"error":{"message":"quota exhausted"}}));
        assert!(
            matches!(&error, ProviderError::Http(message) if message == "quota exhausted"),
            "{error:?}"
        );
    }

    #[test]
    fn honors_a_bounded_in_band_retry_hint() {
        let error =
            classify(&json!({"error":{"type":"rate_limit_error","message":"x","retry_after":2}}));
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
        let error = classify(
            &json!({"retry_after":"3600","error":{"type":"overloaded_error","message":"x"}}),
        );
        assert_eq!(error.retry_after(), Some(MAX_IN_STREAM_RETRY_AFTER));
    }
}
