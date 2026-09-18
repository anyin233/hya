//! Stable error model shared by the HTTP and gRPC v1 bindings.
//!
//! Every failed v1 call surfaces one stable code from [`Code`]. The two
//! bindings render the same code identically:
//!
//! - HTTP: `{"error": {"code": ..., "message": ...}}` with the status from
//!   [`Code::http_status`].
//! - gRPC: a `tonic::Status` with the code from [`Code::grpc_code`] and the
//!   stable string carried in the message/details.

use std::fmt;

/// Stable machine-readable error code for the v1 contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// Request body or parameters are invalid.
    InvalidArgument,
    /// Requested resource does not exist.
    NotFound,
    /// Session id does not exist (or was deleted).
    SessionNotFound,
    /// Caller is not authorized for the operation.
    PermissionDenied,
    /// Another run already owns the session's single admission slot.
    SessionBusy,
    /// The operation conflicts with the current state (stale revisions,
    /// idempotency replays, terminal rewrites).
    Conflict,
    /// A required runtime capability is not configured (for example no
    /// summarizer for compact/summarize).
    Unavailable,
    /// Unhandled internal failure.
    Internal,
}

impl Code {
    /// Stable wire string for the code.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::SessionNotFound => "session_not_found",
            Self::PermissionDenied => "permission_denied",
            Self::SessionBusy => "session_busy",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    /// Canonical HTTP status for the code.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidArgument => 400,
            Self::NotFound | Self::SessionNotFound => 404,
            Self::PermissionDenied => 403,
            Self::SessionBusy | Self::Conflict => 409,
            Self::Unavailable => 503,
            Self::Internal => 500,
        }
    }

    /// Canonical gRPC status code for the error code.
    #[must_use]
    pub fn grpc_code(&self) -> tonic::Code {
        match self {
            Self::InvalidArgument => tonic::Code::InvalidArgument,
            Self::NotFound | Self::SessionNotFound => tonic::Code::NotFound,
            Self::PermissionDenied => tonic::Code::PermissionDenied,
            Self::SessionBusy | Self::Conflict => tonic::Code::FailedPrecondition,
            Self::Unavailable => tonic::Code::Unavailable,
            Self::Internal => tonic::Code::Internal,
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One failed v1 call: stable code plus human-readable message.
#[derive(Clone, Debug)]
pub struct ApiError {
    code: Code,
    message: String,
}

impl ApiError {
    /// Create an error from a stable code and message.
    #[must_use]
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Stable code for the error.
    #[must_use]
    pub fn code(&self) -> Code {
        self.code
    }

    /// Human-readable message for the error.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Render the canonical gRPC status for the error.
    #[must_use]
    pub fn grpc_status(&self) -> tonic::Status {
        tonic::Status::new(self.code.grpc_code(), self.message.clone())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::Code;

    #[test]
    fn codes_map_consistently_across_transports() {
        assert_eq!(Code::SessionBusy.http_status(), 409);
        assert_eq!(
            Code::SessionBusy.grpc_code(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(Code::SessionNotFound.http_status(), 404);
        assert_eq!(Code::SessionNotFound.grpc_code(), tonic::Code::NotFound);
        assert_eq!(Code::InvalidArgument.as_str(), "invalid_argument");
    }

    #[test]
    fn grpc_status_carries_code_and_message() {
        let error = super::ApiError::new(Code::Unavailable, "no summarizer configured");
        let status = error.grpc_status();
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "no summarizer configured");
    }
}
