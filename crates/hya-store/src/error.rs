use thiserror::Error;

use hya_proto::OperationId;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OPERATION_ID_CONFLICT: immutable request differs for {operation_id}")]
    OperationIdConflict { operation_id: OperationId },
    #[error("admission operation not found: {operation_id}")]
    AdmissionNotFound { operation_id: OperationId },
    #[error("admission transition conflict for {operation_id}: {from} -> {to}")]
    AdmissionTransitionConflict {
        operation_id: OperationId,
        from: &'static str,
        to: &'static str,
    },
    #[error("admission journal: {0}")]
    AdmissionData(String),
}
