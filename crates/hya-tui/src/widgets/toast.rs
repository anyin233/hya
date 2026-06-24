use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: Option<String>,
    pub message: String,
    pub variant: ToastVariant,
    pub created_at: Instant,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToastState {
    current: Option<Toast>,
}

impl Toast {
    #[must_use]
    pub fn new(message: impl Into<String>, variant: ToastVariant, created_at: Instant) -> Self {
        Self {
            title: None,
            message: message.into(),
            variant,
            created_at,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.created_at) >= self.timeout
    }
}

impl ToastState {
    pub fn show(&mut self, toast: Toast) {
        self.current = Some(toast);
    }

    pub fn expire(&mut self, now: Instant) {
        if self
            .current
            .as_ref()
            .is_some_and(|toast| toast.is_expired(now))
        {
            self.current = None;
        }
    }

    #[must_use]
    pub fn current(&self) -> Option<&Toast> {
        self.current.as_ref()
    }
}
