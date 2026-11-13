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
    /// Workflow command syntax is invalid.
    WorkflowSyntax,
    /// Workflow inputs or source fail validation.
    WorkflowInvalidInput,
    /// Workflow source fails validation.
    WorkflowInvalidSource,
    /// Requested Workflow is missing.
    WorkflowNotFound,
    /// No Workflow is selected.
    WorkflowNotSelected,
    /// Workflow access is denied.
    WorkflowUnauthorized,
    /// Workflow admission is busy.
    WorkflowBusy,
    /// Workflow operation conflicts with a prior operation.
    WorkflowOperationConflict,
    /// Workflow revision is stale.
    WorkflowStaleRevision,
    /// Workflow runtime is unavailable.
    WorkflowRuntimeUnavailable,
    /// Workflow live capacity is currently exhausted.
    WorkflowOverloaded,
    /// Requested resource does not exist.
    NotFound,
    /// Session id does not exist (or was deleted).
    SessionNotFound,
    /// Caller is not authorized for the operation.
    PermissionDenied,
    /// Another run already owns the session's single admission slot.
    SessionBusy,
    /// The operation is refused in the resource's current state (for
    /// example deleting a Project that still has live sessions).
    FailedPrecondition,
    /// The operation conflicts with the current state (stale revisions,
    /// idempotency replays, terminal rewrites).
    Conflict,
    /// A required runtime capability is not configured (for example no
    /// summarizer for compact/summarize).
    Unavailable,
    /// Unhandled internal failure.
    Internal,
    /// No published bundle endpoint matches the request (unknown bundle, a
    /// bundle without endpoints, or no template of the scope matches the
    /// path under any method).
    BundleApiNotFound,
    /// The path matches a bundle endpoint of the scope, but not under the
    /// requested method.
    BundleApiMethodNotAllowed,
    /// The bundle API request is malformed (non-JSON or oversized body, bad
    /// path escape, unknown method, unparsable query).
    BundleApiBadRequest,
    /// The bundle process failed, timed out, or answered malformed data
    /// while serving an endpoint.
    BundleApiFailed,
}

impl Code {
    /// Stable wire string for the code.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::WorkflowSyntax => "WORKFLOW_SYNTAX",
            Self::WorkflowInvalidInput => "WORKFLOW_INVALID_INPUT",
            Self::WorkflowInvalidSource => "WORKFLOW_INVALID_SOURCE",
            Self::WorkflowNotFound => "WORKFLOW_NOT_FOUND",
            Self::WorkflowNotSelected => "WORKFLOW_NOT_SELECTED",
            Self::WorkflowUnauthorized => "WORKFLOW_UNAUTHORIZED",
            Self::WorkflowBusy => "WORKFLOW_BUSY",
            Self::WorkflowOperationConflict => "WORKFLOW_OPERATION_CONFLICT",
            Self::WorkflowStaleRevision => "WORKFLOW_STALE_REVISION",
            Self::WorkflowRuntimeUnavailable => "WORKFLOW_RUNTIME_UNAVAILABLE",
            Self::WorkflowOverloaded => "WORKFLOW_OVERLOADED",

            Self::NotFound => "not_found",
            Self::SessionNotFound => "session_not_found",
            Self::PermissionDenied => "permission_denied",
            Self::SessionBusy => "session_busy",
            Self::FailedPrecondition => "failed_precondition",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
            Self::BundleApiNotFound => "bundle_api_not_found",
            Self::BundleApiMethodNotAllowed => "bundle_api_method_not_allowed",
            Self::BundleApiBadRequest => "bundle_api_bad_request",
            Self::BundleApiFailed => "bundle_api_failed",
        }
    }

    /// Look up a stable wire code shared by transport adapters.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "invalid_argument" => Some(Self::InvalidArgument),
            "WORKFLOW_SYNTAX" => Some(Self::WorkflowSyntax),
            "WORKFLOW_INVALID_INPUT" => Some(Self::WorkflowInvalidInput),
            "WORKFLOW_INVALID_SOURCE" => Some(Self::WorkflowInvalidSource),
            "WORKFLOW_NOT_FOUND" => Some(Self::WorkflowNotFound),
            "WORKFLOW_NOT_SELECTED" => Some(Self::WorkflowNotSelected),
            "WORKFLOW_UNAUTHORIZED" => Some(Self::WorkflowUnauthorized),
            "WORKFLOW_BUSY" => Some(Self::WorkflowBusy),
            "WORKFLOW_OPERATION_CONFLICT" => Some(Self::WorkflowOperationConflict),
            "WORKFLOW_STALE_REVISION" => Some(Self::WorkflowStaleRevision),
            "WORKFLOW_RUNTIME_UNAVAILABLE" => Some(Self::WorkflowRuntimeUnavailable),
            "WORKFLOW_OVERLOADED" => Some(Self::WorkflowOverloaded),
            "not_found" => Some(Self::NotFound),
            "session_not_found" => Some(Self::SessionNotFound),
            "permission_denied" => Some(Self::PermissionDenied),
            "session_busy" => Some(Self::SessionBusy),
            "failed_precondition" => Some(Self::FailedPrecondition),
            "conflict" => Some(Self::Conflict),
            "unavailable" => Some(Self::Unavailable),
            "internal" => Some(Self::Internal),
            "bundle_api_not_found" => Some(Self::BundleApiNotFound),
            "bundle_api_method_not_allowed" => Some(Self::BundleApiMethodNotAllowed),
            "bundle_api_bad_request" => Some(Self::BundleApiBadRequest),
            "bundle_api_failed" => Some(Self::BundleApiFailed),
            _ => None,
        }
    }

    /// Canonical HTTP status for the code.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::WorkflowSyntax => 400,
            Self::WorkflowInvalidInput | Self::WorkflowInvalidSource => 422,
            Self::WorkflowNotFound | Self::WorkflowNotSelected => 404,
            Self::WorkflowUnauthorized => 403,
            Self::WorkflowBusy | Self::WorkflowOperationConflict | Self::WorkflowStaleRevision => {
                409
            }
            Self::WorkflowRuntimeUnavailable | Self::WorkflowOverloaded => 503,

            Self::InvalidArgument | Self::BundleApiBadRequest => 400,
            Self::NotFound | Self::SessionNotFound | Self::BundleApiNotFound => 404,
            Self::BundleApiMethodNotAllowed => 405,
            Self::PermissionDenied => 403,
            Self::SessionBusy | Self::Conflict | Self::FailedPrecondition => 409,
            Self::Unavailable => 503,
            Self::Internal => 500,
            Self::BundleApiFailed => 502,
        }
    }

    /// Canonical gRPC status code for the error code.
    #[must_use]
    pub fn grpc_code(&self) -> tonic::Code {
        match self {
            Self::WorkflowSyntax | Self::WorkflowInvalidInput | Self::WorkflowInvalidSource => {
                tonic::Code::InvalidArgument
            }
            Self::WorkflowNotFound | Self::WorkflowNotSelected => tonic::Code::NotFound,
            Self::WorkflowUnauthorized => tonic::Code::PermissionDenied,
            Self::WorkflowBusy | Self::WorkflowOperationConflict | Self::WorkflowStaleRevision => {
                tonic::Code::FailedPrecondition
            }
            Self::WorkflowRuntimeUnavailable => tonic::Code::Unavailable,
            Self::WorkflowOverloaded => tonic::Code::ResourceExhausted,

            Self::InvalidArgument | Self::BundleApiBadRequest => tonic::Code::InvalidArgument,
            Self::NotFound | Self::SessionNotFound | Self::BundleApiNotFound => {
                tonic::Code::NotFound
            }
            Self::BundleApiMethodNotAllowed => tonic::Code::Unimplemented,
            Self::PermissionDenied => tonic::Code::PermissionDenied,
            Self::SessionBusy | Self::Conflict | Self::FailedPrecondition => {
                tonic::Code::FailedPrecondition
            }
            Self::Unavailable | Self::BundleApiFailed => tonic::Code::Unavailable,
            Self::Internal => tonic::Code::Internal,
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
        assert_eq!(Code::FailedPrecondition.as_str(), "failed_precondition");
        assert_eq!(Code::FailedPrecondition.http_status(), 409);
        assert_eq!(
            Code::FailedPrecondition.grpc_code(),
            tonic::Code::FailedPrecondition
        );
        assert_eq!(Code::SessionNotFound.http_status(), 404);
        assert_eq!(Code::SessionNotFound.grpc_code(), tonic::Code::NotFound);
        assert_eq!(Code::InvalidArgument.as_str(), "invalid_argument");
        for (code, name, http, grpc) in [
            (
                Code::BundleApiNotFound,
                "bundle_api_not_found",
                404,
                tonic::Code::NotFound,
            ),
            (
                Code::BundleApiMethodNotAllowed,
                "bundle_api_method_not_allowed",
                405,
                tonic::Code::Unimplemented,
            ),
            (
                Code::BundleApiBadRequest,
                "bundle_api_bad_request",
                400,
                tonic::Code::InvalidArgument,
            ),
            (
                Code::BundleApiFailed,
                "bundle_api_failed",
                502,
                tonic::Code::Unavailable,
            ),
        ] {
            assert_eq!(code.as_str(), name);
            assert_eq!(code.http_status(), http);
            assert_eq!(code.grpc_code(), grpc);
        }
    }
}

#[cfg(test)]
mod workflow_codes {
    use super::Code;

    #[test]
    fn workflow_errors_keep_their_intended_status() {
        for (code, status) in [
            (Code::WorkflowSyntax, 400),
            (Code::WorkflowInvalidInput, 422),
            (Code::WorkflowInvalidSource, 422),
            (Code::WorkflowNotFound, 404),
            (Code::WorkflowNotSelected, 404),
            (Code::WorkflowUnauthorized, 403),
            (Code::WorkflowBusy, 409),
            (Code::WorkflowOperationConflict, 409),
            (Code::WorkflowStaleRevision, 409),
            (Code::WorkflowRuntimeUnavailable, 503),
            (Code::WorkflowOverloaded, 503),
        ] {
            assert_eq!(code.http_status(), status);
            assert_ne!(code.grpc_code(), tonic::Code::Internal);
        }
    }
}
