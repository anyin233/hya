//! Stable error model shared by the HTTP and gRPC v2 bindings.
//!
//! HTTP exposes the stable string in the JSON error envelope and uses
//! [Code::http_status] for its status. gRPC uses [Code::grpc_code] and
//! sends the human-readable diagnostic.

use std::fmt;

macro_rules! wire_codes {
    ($($(#[$attr:meta])* $variant:ident => $wire:literal),+ $(,)?) => {
        /// Stable machine-readable error code for the v2 contract.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code {
            $($(#[$attr])* $variant),+
        }

        impl Code {
            /// Stable wire string for the code.
            #[must_use]
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            /// Look up a stable wire code shared by transport adapters.
            #[must_use]
            pub fn from_wire(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

wire_codes! {
    /// Request body or parameters are invalid.
    InvalidArgument => "invalid_argument",
    /// Requested resource does not exist.
    NotFound => "not_found",
    /// Session id does not exist (or was deleted).
    SessionNotFound => "session_not_found",
    /// Caller is not authorized for the operation.
    PermissionDenied => "permission_denied",
    /// Another run already owns the session's single admission slot.
    SessionBusy => "session_busy",
    /// The operation is refused in the resource's current state (for
    /// example deleting a Project that still has live sessions).
    FailedPrecondition => "failed_precondition",
    /// The operation conflicts with the current state (stale revisions,
    /// idempotency replays, terminal rewrites).
    Conflict => "conflict",
    /// A required runtime capability is not configured (for example no
    /// summarizer for compact/summarize).
    Unavailable => "unavailable",
    /// Unhandled internal failure.
    Internal => "internal",
    /// No published bundle endpoint matches the request (unknown bundle, a
    /// bundle without endpoints, or no template of the scope matches the
    /// path under any method).
    BundleApiNotFound => "bundle_api_not_found",
    /// The path matches a bundle endpoint of the scope, but not under the
    /// requested method.
    BundleApiMethodNotAllowed => "bundle_api_method_not_allowed",
    /// The bundle API request is malformed (non-JSON or oversized body, bad
    /// path escape, unknown method, unparsable query).
    BundleApiBadRequest => "bundle_api_bad_request",
    /// The bundle process failed, timed out, or answered malformed data
    /// while serving an endpoint.
    BundleApiFailed => "bundle_api_failed",
}

impl Code {
    /// Canonical HTTP status for the code.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
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
    fn stable_wire_codes_round_trip() {
        for (code, name) in [
            (Code::InvalidArgument, "invalid_argument"),
            (Code::NotFound, "not_found"),
            (Code::SessionNotFound, "session_not_found"),
            (Code::PermissionDenied, "permission_denied"),
            (Code::SessionBusy, "session_busy"),
            (Code::FailedPrecondition, "failed_precondition"),
            (Code::Conflict, "conflict"),
            (Code::Unavailable, "unavailable"),
            (Code::Internal, "internal"),
            (Code::BundleApiNotFound, "bundle_api_not_found"),
            (
                Code::BundleApiMethodNotAllowed,
                "bundle_api_method_not_allowed",
            ),
            (Code::BundleApiBadRequest, "bundle_api_bad_request"),
            (Code::BundleApiFailed, "bundle_api_failed"),
        ] {
            assert_eq!(code.as_str(), name);
            assert_eq!(Code::from_wire(name), Some(code));
        }
        assert_eq!(Code::from_wire("unknown"), None);
    }

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
