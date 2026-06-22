use yaca_proto::{AgentName, Event, MessageId, ModelRef, PartId, SessionId, ToolPartState};

use super::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    pub async fn switch_agent(
        &self,
        session: SessionId,
        agent: AgentName,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::AgentSwitched {
                session,
                message: Some(MessageId::new()),
                agent,
            },
        )
        .await
    }

    pub async fn switch_model(&self, session: SessionId, model: ModelRef) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ModelSwitched {
                session,
                message: Some(MessageId::new()),
                model,
            },
        )
        .await
    }

    pub async fn set_title(&self, session: SessionId, title: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionTitled { session, title })
            .await
    }

    pub async fn set_workdir(&self, session: SessionId, workdir: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionMoved { session, workdir })
            .await
    }

    pub async fn set_metadata(
        &self,
        session: SessionId,
        metadata: serde_json::Value,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::SessionMetadataSet { session, metadata })
            .await
    }

    pub async fn set_permission(
        &self,
        session: SessionId,
        permission: Vec<serde_json::Value>,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::SessionPermissionSet {
                session,
                permission,
            },
        )
        .await
    }

    pub async fn set_archived(
        &self,
        session: SessionId,
        archived: serde_json::Number,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::SessionArchived { session, archived })
            .await
    }

    pub async fn set_share(&self, session: SessionId, url: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionShareSet { session, url })
            .await
    }

    pub async fn clear_share(&self, session: SessionId) -> Result<(), CoreError> {
        self.emit(session, Event::SessionShareCleared { session })
            .await
    }

    pub async fn delete_message(
        &self,
        session: SessionId,
        message: MessageId,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::MessageDeleted { session, message })
            .await
    }

    pub async fn delete_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::PartDeleted {
                session,
                message,
                part,
            },
        )
        .await
    }

    pub async fn replace_text_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::TextReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    pub async fn replace_reasoning_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ReasoningReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    pub async fn update_tool_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        state: ToolPartState,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ToolPartUpdated {
                session,
                message,
                part,
                state,
            },
        )
        .await
    }
}
