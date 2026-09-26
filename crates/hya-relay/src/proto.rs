//! The `hya.relay.v1` rendezvous protocol.
//!
//! One set of messages serves both bindings: the gRPC `Relay` service and the
//! WebSocket routes `<prefix>/hya.relay.v1/ws/{host,accept,open}`, whose
//! binary frames are the same encoded messages.

/// Generated protobuf code for `hya.relay.v1`.
///
/// The `#![allow]` inner attributes keep generated code outside the
/// hand-written lint contract.
mod generated {
    #![allow(missing_docs, clippy::all)]
    include!("gen/hya.relay.v1.rs");
}

pub use generated::*;

/// Domain-separation prefix of the host registration signature. The host
/// signs `REGISTER_SIGNING_CONTEXT || Challenge.nonce ||
/// Register.open_token_hash` with its Ed25519 key.
///
/// Versioned: the first protocol revision signed
/// `"hya.relay.v1/register\0" || nonce` (no open token hash). The contexts
/// differ at byte 21 (`\0` vs `/`), so a signature over one message can
/// never verify as the other.
pub const REGISTER_SIGNING_CONTEXT: &[u8] = b"hya.relay.v1/register/v2\0";

/// Domain-separation prefix of the `UpdateOpenToken` signature: the host
/// signs `UPDATE_OPEN_TOKEN_SIGNING_CONTEXT || Challenge.nonce ||
/// UpdateOpenToken.open_token_hash`.
pub const UPDATE_OPEN_TOKEN_SIGNING_CONTEXT: &[u8] = b"hya.relay.v1/update-open-token/v1\0";

/// The exact bytes a host signs to answer a registration challenge.
#[must_use]
pub fn register_signing_message(nonce: &[u8], open_token_hash: &[u8]) -> Vec<u8> {
    signing_message(REGISTER_SIGNING_CONTEXT, nonce, open_token_hash)
}

/// The exact bytes a host signs to replace its open token hash on the
/// control stream whose challenge was `nonce`.
#[must_use]
pub fn update_open_token_signing_message(nonce: &[u8], open_token_hash: &[u8]) -> Vec<u8> {
    signing_message(UPDATE_OPEN_TOKEN_SIGNING_CONTEXT, nonce, open_token_hash)
}

fn signing_message(context: &[u8], nonce: &[u8], hash: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(context.len() + nonce.len() + hash.len());
    message.extend_from_slice(context);
    message.extend_from_slice(nonce);
    message.extend_from_slice(hash);
    message
}

/// Map a relay error code to the gRPC status code it mirrors.
///
/// `Unspecified` maps to [`tonic::Code::Unknown`].
#[must_use]
pub fn relay_error_code_to_grpc(code: RelayErrorCode) -> tonic::Code {
    match code {
        RelayErrorCode::Unspecified | RelayErrorCode::Unknown => tonic::Code::Unknown,
        RelayErrorCode::Cancelled => tonic::Code::Cancelled,
        RelayErrorCode::InvalidArgument => tonic::Code::InvalidArgument,
        RelayErrorCode::DeadlineExceeded => tonic::Code::DeadlineExceeded,
        RelayErrorCode::NotFound => tonic::Code::NotFound,
        RelayErrorCode::AlreadyExists => tonic::Code::AlreadyExists,
        RelayErrorCode::PermissionDenied => tonic::Code::PermissionDenied,
        RelayErrorCode::ResourceExhausted => tonic::Code::ResourceExhausted,
        RelayErrorCode::FailedPrecondition => tonic::Code::FailedPrecondition,
        RelayErrorCode::Internal => tonic::Code::Internal,
        RelayErrorCode::Unavailable => tonic::Code::Unavailable,
        RelayErrorCode::Unauthenticated => tonic::Code::Unauthenticated,
    }
}

/// Map a gRPC status code to the relay error code with the same meaning.
///
/// Codes the relay never produces (`OK`, `ABORTED`, `OUT_OF_RANGE`,
/// `UNIMPLEMENTED`, `DATA_LOSS`) map to `Unknown`.
#[must_use]
pub fn relay_error_code_from_grpc(code: tonic::Code) -> RelayErrorCode {
    match code {
        tonic::Code::Cancelled => RelayErrorCode::Cancelled,
        tonic::Code::InvalidArgument => RelayErrorCode::InvalidArgument,
        tonic::Code::DeadlineExceeded => RelayErrorCode::DeadlineExceeded,
        tonic::Code::NotFound => RelayErrorCode::NotFound,
        tonic::Code::AlreadyExists => RelayErrorCode::AlreadyExists,
        tonic::Code::PermissionDenied => RelayErrorCode::PermissionDenied,
        tonic::Code::ResourceExhausted => RelayErrorCode::ResourceExhausted,
        tonic::Code::FailedPrecondition => RelayErrorCode::FailedPrecondition,
        tonic::Code::Internal => RelayErrorCode::Internal,
        tonic::Code::Unavailable => RelayErrorCode::Unavailable,
        tonic::Code::Unauthenticated => RelayErrorCode::Unauthenticated,
        _ => RelayErrorCode::Unknown,
    }
}

impl RelayError {
    /// Build a relay error frame payload.
    #[must_use]
    pub fn new(code: RelayErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code as i32,
            message: message.into(),
        }
    }

    /// The decoded error code; unknown wire values read as `Unknown`.
    #[must_use]
    pub fn error_code(&self) -> RelayErrorCode {
        RelayErrorCode::try_from(self.code).unwrap_or(RelayErrorCode::Unknown)
    }
}

impl From<RelayError> for tonic::Status {
    fn from(error: RelayError) -> Self {
        tonic::Status::new(relay_error_code_to_grpc(error.error_code()), error.message)
    }
}

impl From<&tonic::Status> for RelayError {
    fn from(status: &tonic::Status) -> Self {
        RelayError::new(
            relay_error_code_from_grpc(status.code()),
            status.message().to_owned(),
        )
    }
}
