use yaca_proto::{Event, MessageId, SessionId};

pub(super) fn event_message_id(event: &Event) -> Option<MessageId> {
    match event {
        Event::MessageStarted { message, .. }
        | Event::MessageFinished { message, .. }
        | Event::StepStarted { message, .. }
        | Event::StepFinished { message, .. }
        | Event::TextStart { message, .. }
        | Event::TextDelta { message, .. }
        | Event::TextEnd { message, .. }
        | Event::ReasoningStart { message, .. }
        | Event::ReasoningDelta { message, .. }
        | Event::ReasoningEnd { message, .. }
        | Event::ToolInputStart { message, .. }
        | Event::ToolInputDelta { message, .. }
        | Event::ToolCallRequested { message, .. }
        | Event::ToolResult { message, .. }
        | Event::ToolError { message, .. } => Some(*message),
        Event::SessionCreated { .. } | Event::SessionTitled { .. } | Event::Error { .. } => None,
    }
}

pub(super) fn rewrite_session(event: &Event, session: SessionId) -> Event {
    match event {
        Event::SessionCreated {
            parent,
            agent,
            model,
            workdir,
            ..
        } => Event::SessionCreated {
            session,
            parent: *parent,
            agent: agent.clone(),
            model: model.clone(),
            workdir: workdir.clone(),
        },
        Event::SessionTitled { title, .. } => Event::SessionTitled {
            session,
            title: title.clone(),
        },
        Event::MessageStarted { message, role, .. } => Event::MessageStarted {
            session,
            message: *message,
            role: *role,
        },
        Event::MessageFinished {
            message, finish, ..
        } => Event::MessageFinished {
            session,
            message: *message,
            finish: *finish,
        },
        Event::StepStarted { message, step, .. } => Event::StepStarted {
            session,
            message: *message,
            step: *step,
        },
        Event::StepFinished { message, step, .. } => Event::StepFinished {
            session,
            message: *message,
            step: *step,
        },
        Event::TextStart { message, part, .. } => Event::TextStart {
            session,
            message: *message,
            part: *part,
        },
        Event::TextDelta {
            message,
            part,
            delta,
            ..
        } => Event::TextDelta {
            session,
            message: *message,
            part: *part,
            delta: delta.clone(),
        },
        Event::TextEnd { message, part, .. } => Event::TextEnd {
            session,
            message: *message,
            part: *part,
        },
        Event::ReasoningStart { message, part, .. } => Event::ReasoningStart {
            session,
            message: *message,
            part: *part,
        },
        Event::ReasoningDelta {
            message,
            part,
            delta,
            ..
        } => Event::ReasoningDelta {
            session,
            message: *message,
            part: *part,
            delta: delta.clone(),
        },
        Event::ReasoningEnd { message, part, .. } => Event::ReasoningEnd {
            session,
            message: *message,
            part: *part,
        },
        Event::ToolInputStart {
            message,
            part,
            call,
            name,
            ..
        } => Event::ToolInputStart {
            session,
            message: *message,
            part: *part,
            call: *call,
            name: name.clone(),
        },
        Event::ToolInputDelta {
            message,
            part,
            call,
            delta,
            ..
        } => Event::ToolInputDelta {
            session,
            message: *message,
            part: *part,
            call: *call,
            delta: delta.clone(),
        },
        Event::ToolCallRequested {
            message,
            part,
            call,
            name,
            input,
            ..
        } => Event::ToolCallRequested {
            session,
            message: *message,
            part: *part,
            call: *call,
            name: name.clone(),
            input: input.clone(),
        },
        Event::ToolResult {
            message,
            part,
            call,
            output,
            time_ms,
            ..
        } => Event::ToolResult {
            session,
            message: *message,
            part: *part,
            call: *call,
            output: output.clone(),
            time_ms: *time_ms,
        },
        Event::ToolError {
            message,
            part,
            call,
            message_text,
            ..
        } => Event::ToolError {
            session,
            message: *message,
            part: *part,
            call: *call,
            message_text: message_text.clone(),
        },
        Event::Error { code, message, .. } => Event::Error {
            session: Some(session),
            code: code.clone(),
            message: message.clone(),
        },
    }
}
