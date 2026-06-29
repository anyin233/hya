use hya_proto::SessionId;

use super::SessionEngine;
use crate::error::CoreError;
use crate::title;

impl SessionEngine {
    pub async fn cleanup_empty_unnamed_session(
        &self,
        session: SessionId,
    ) -> Result<bool, CoreError> {
        let projection = self.read_projection(session).await?;
        if !title::is_empty_unnamed_session(&projection) {
            return Ok(false);
        }
        self.delete_session(session).await
    }
}
