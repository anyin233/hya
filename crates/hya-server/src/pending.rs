mod permission;
mod question;
mod saved_permission;

pub(crate) use permission::{PermissionReply, PermissionRequestView, PermissionRequests};
pub(crate) use question::{QuestionRequestView, QuestionRequests};

#[derive(Clone)]
pub(crate) enum PendingEvent {
    PermissionRequested(PermissionRequestView),
    QuestionRequested(QuestionRequestView),
    Resolved {
        session: Option<hya_proto::SessionId>,
        id: String,
    },
}
