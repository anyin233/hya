//! Deterministic event-script generator for projection equivalence tests.
//!
//! `script(seed, ...)` produces a plausible single-session event log mixing
//! transcript streaming, usage records (per-round and legacy), message/part
//! deletion (revert, compaction), file changes, transcript revert/unrevert/
//! commit, archive/unarchive (including legacy zero stamps), ephemeral
//! marks, compaction
//! markers, todo lists, forks, Workflow runs
//! (including a re-emitted `WorkflowRunStarted` for an already-seen run, which
//! only replay-only reducer state can deduplicate), and team roster/mail
//! traffic. The same seed always yields the same events, so a failing seed is
//! reproducible. Shared with `hya-store` tests through a `#[path]` include.

#![allow(dead_code)]

use hya_proto::{
    AgentName, CompactionStrategy, Event, FileChange, FileRestore, FileState, FinishReason,
    MailEndpoint, MailKind, MemberId, MemberRunStatus, MessageId, ModelRef, OwnerRunId, PartId,
    Role, RosterStatus, SessionId, SubagentMode, TodoItem, TodoStatus, TokenUsage, ToolCallId,
    UsagePurpose, WorkflowIdentity, WorkflowRevision, WorkflowRunId, WorkflowRunStatus,
    WorkflowSourceId, WorkflowStagePlan,
};
use uuid::Uuid;

/// SplitMix64: tiny, dependency-free, and stable across platforms.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[derive(Clone, Copy)]
enum PartKind {
    Text,
    Reasoning,
    Tool(ToolCallId),
}

struct Script {
    rng: Rng,
    session: SessionId,
    ids: u128,
    messages: Vec<(MessageId, Vec<(PartId, PartKind)>)>,
    runs: Vec<WorkflowRunId>,
    handles: Vec<String>,
    members: Vec<MemberId>,
}

impl Script {
    fn uuid(&mut self) -> Uuid {
        self.ids += 1;
        Uuid::from_u128((u128::from(self.rng.next()) << 64) | self.ids)
    }

    fn message(&mut self) -> Option<(MessageId, Vec<(PartId, PartKind)>)> {
        if self.messages.is_empty() {
            return None;
        }
        // Mostly the newest message, sometimes an older one.
        let index = if self.rng.chance(75) {
            self.messages.len() - 1
        } else {
            self.rng.below(self.messages.len())
        };
        Some(self.messages[index].clone())
    }

    fn tokens(&mut self) -> TokenUsage {
        TokenUsage {
            input: self.rng.next() % 5_000,
            output: self.rng.next() % 800,
            reasoning: self.rng.next() % 300,
            cache_read: self.rng.next() % 2_000,
            cache_write: self.rng.next() % 100,
            reasoning_unknown: self.rng.chance(20),
        }
    }

    fn identity(&mut self) -> WorkflowIdentity {
        let revision = (self.rng.next() % 4) as u8;
        WorkflowIdentity {
            source: WorkflowSourceId::new("project:scripted"),
            name: "scripted".to_string(),
            revision: WorkflowRevision::from_bytes([revision; 32]),
        }
    }

    fn step(&mut self) -> Event {
        let session = self.session;
        match self.rng.below(29) {
            0 | 1 => {
                let message = MessageId::from_uuid(self.uuid());
                self.messages.push((message, Vec::new()));
                let assistant = self.rng.chance(70);
                let (agent, model) = if assistant {
                    let pick = self.rng.below(2);
                    (
                        Some(AgentName::new(["build", "plan"][pick])),
                        Some(ModelRef::new(["fake/alpha", "fake/beta"][pick])),
                    )
                } else {
                    (None, None)
                };
                Event::MessageStarted {
                    session,
                    message,
                    role: if assistant {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    agent,
                    model,
                }
            }
            2 => {
                let Some((message, _)) = self.message() else {
                    return self.title();
                };
                let part = PartId::from_uuid(self.uuid());
                self.push_part(message, part, PartKind::Text);
                Event::TextStart {
                    session,
                    message,
                    part,
                }
            }
            3 => {
                let Some((message, _)) = self.message() else {
                    return self.title();
                };
                let part = PartId::from_uuid(self.uuid());
                self.push_part(message, part, PartKind::Reasoning);
                Event::ReasoningStart {
                    session,
                    message,
                    part,
                    reason: self.rng.chance(30).then(|| "plan".to_string()),
                }
            }
            4 => {
                let Some((message, _)) = self.message() else {
                    return self.title();
                };
                let part = PartId::from_uuid(self.uuid());
                let call = ToolCallId::from_uuid(self.uuid());
                self.push_part(message, part, PartKind::Tool(call));
                Event::ToolInputStart {
                    session,
                    message,
                    part,
                    call,
                    name: "read".into(),
                }
            }
            5..=8 => self.part_event(),
            9 => {
                let Some((message, _)) = self.message() else {
                    return self.title();
                };
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Stop,
                    tokens: self.rng.chance(60).then(|| self.tokens()),
                    cause: None,
                }
            }
            10 | 11 => {
                let message = if self.rng.chance(70) {
                    self.message().map(|(message, _)| message)
                } else {
                    None
                };
                let purpose = match self.rng.below(3) {
                    0 => UsagePurpose::Turn,
                    1 => UsagePurpose::Title,
                    _ => UsagePurpose::Compaction,
                };
                let model = if self.rng.chance(50) {
                    "fake/a"
                } else {
                    "fake/b"
                };
                Event::UsageRecorded {
                    session,
                    message,
                    step: Some(self.rng.below(5) as u32),
                    model: model.into(),
                    purpose,
                    tokens: self.tokens(),
                }
            }
            12 => {
                // Revert / compaction removal of a whole message.
                if self.messages.is_empty() {
                    return self.title();
                }
                let index = self.rng.below(self.messages.len());
                let (message, _) = self.messages.remove(index);
                Event::MessageDeleted { session, message }
            }
            13 => {
                let Some((message, parts)) = self.message() else {
                    return self.title();
                };
                let Some((part, _)) = parts.first().copied() else {
                    return self.title();
                };
                if let Some(entry) = self.messages.iter_mut().find(|(id, _)| *id == message) {
                    entry.1.retain(|(id, _)| *id != part);
                }
                Event::PartDeleted {
                    session,
                    message,
                    part,
                }
            }
            14 => {
                let Some((message, _)) = self.message() else {
                    return self.title();
                };
                Event::ContextCompacted {
                    session,
                    message,
                    strategy: CompactionStrategy::LocalSummarizer,
                    from_message: message,
                    to_message: message,
                    folded_count: 3,
                    input_tokens_est: 10_000,
                    threshold: 8_000,
                }
            }
            15 => {
                // New run, or a re-emitted start for an already-seen run id.
                let run = if !self.runs.is_empty() && self.rng.chance(40) {
                    self.runs[self.rng.below(self.runs.len())]
                } else {
                    let run = WorkflowRunId::from_uuid(self.uuid());
                    self.runs.push(run);
                    run
                };
                Event::WorkflowRunStarted {
                    session,
                    run,
                    workflow: self.identity(),
                    request_hash: format!("hash-{}", self.rng.below(3)),
                    owner: OwnerRunId::from_storage(Uuid::from_u128(7)),
                    stages: vec![WorkflowStagePlan {
                        id: "plan".to_string(),
                        title: None,
                        agent: AgentName::new("planner"),
                        mode: "once".to_string(),
                        level: 0,
                        worker_model: None,
                        selected_worker_model: None,
                        verifier_model: None,
                        selected_verifier_model: None,
                    }],
                }
            }
            16 => {
                let Some(run) = self.runs.last().copied() else {
                    return self.title();
                };
                Event::WorkflowRunFinished {
                    session,
                    run,
                    status: if self.rng.chance(50) {
                        WorkflowRunStatus::Completed
                    } else {
                        WorkflowRunStatus::Failed
                    },
                    error: None,
                }
            }
            17 => {
                let handle = format!("worker-{}", self.handles.len() + 1);
                self.handles.push(handle.clone());
                Event::AgentRegistered {
                    session,
                    agent_session: SessionId::from_uuid(self.uuid()),
                    handle,
                    parent: None,
                    agent_type: AgentName::new("hya-worker"),
                    mode: SubagentMode::Resident,
                }
            }
            18 => {
                let to = if self.handles.is_empty() {
                    "main".to_string()
                } else {
                    self.handles[self.rng.below(self.handles.len())].clone()
                };
                Event::MailSent {
                    session,
                    from: "main".to_string(),
                    to: MailEndpoint::Handle(to),
                    kind: MailKind::Message,
                    body: format!("mail {}", self.rng.below(100)),
                }
            }
            19 => {
                if self.handles.is_empty() || self.rng.chance(50) {
                    let member = MemberId::from_uuid(self.uuid());
                    self.members.push(member);
                    Event::MemberSpawned {
                        session,
                        member,
                        child: Some(SessionId::from_uuid(self.uuid())),
                        subagent_type: AgentName::new("hya-worker"),
                        description: "scripted member".to_string(),
                        depth: 1,
                        directive: String::new(),
                        tool_call: None,
                    }
                } else {
                    let handle = self.handles[self.rng.below(self.handles.len())].clone();
                    Event::AgentActivityChanged {
                        session,
                        handle,
                        status: RosterStatus::Busy,
                        current_task: Some("scripted".to_string()),
                    }
                }
            }
            20 => {
                let Some(member) = self.members.last().copied() else {
                    return self.title();
                };
                Event::MemberFinished {
                    session,
                    member,
                    status: MemberRunStatus::Done,
                    summary: "done".to_string(),
                    child: None,
                }
            }
            22 => Event::Error {
                session: Some(session),
                code: "provider_error".to_string(),
                message: "http status 400: rejected".to_string(),
                failed_message: self.message().map(|(message, _)| message),
            },
            21 => Event::SessionPermissionModeSet {
                session,
                mode: ["manual", "yolo", "acme/approver/careful"][self.rng.below(3)].to_string(),
            },
            23 => {
                let count = self.rng.below(4);
                let todos = (0..count)
                    .map(|index| TodoItem {
                        id: (index + 1).to_string(),
                        content: format!("todo {}", self.rng.below(100)),
                        status: [
                            TodoStatus::Pending,
                            TodoStatus::InProgress,
                            TodoStatus::Blocked,
                            TodoStatus::Completed,
                        ][self.rng.below(4)],
                    })
                    .collect();
                Event::TodosUpdated { session, todos }
            }
            25 => match self.message() {
                Some((message, _)) => Event::FilesChanged {
                    session,
                    message,
                    call: self
                        .rng
                        .chance(80)
                        .then(|| ToolCallId::from_uuid(self.uuid())),
                    files: vec![FileChange {
                        path: format!("/tmp/scripted/f{}", self.rng.below(4)),
                        before: self.file_state(),
                    }],
                },
                None => self.title(),
            },
            26 => match self.message() {
                Some((message, _)) => Event::SessionReverted {
                    session,
                    message,
                    files: vec![FileRestore {
                        path: format!("/tmp/scripted/f{}", self.rng.below(4)),
                        restored: self.file_state(),
                        saved: self.file_state(),
                        error: self.rng.chance(10).then(|| "denied".to_string()),
                    }],
                },
                None => self.title(),
            },
            27 => Event::SessionUnreverted {
                session,
                files: Vec::new(),
            },
            24 => {
                if self.rng.chance(60) {
                    Event::SessionArchived {
                        session,
                        // Mostly real stamps, sometimes the legacy zero.
                        archived: serde_json::Number::from(if self.rng.chance(80) {
                            1_700_000_000_000 + self.rng.below(1_000) as u64
                        } else {
                            0
                        }),
                    }
                } else {
                    Event::SessionUnarchived { session }
                }
            }
            28 => Event::SessionEphemeralSet {
                session,
                ephemeral: self.rng.chance(50),
            },
            _ => self.title(),
        }
    }

    fn file_state(&mut self) -> FileState {
        match self.rng.below(3) {
            0 => FileState::Absent,
            1 => FileState::Stored {
                hash: format!("{:064x}", self.rng.next()),
                size: self.rng.next() % 4_096,
            },
            _ => FileState::Omitted {
                size: self.rng.next() % 9_000_000,
                reason: "too_large".to_string(),
            },
        }
    }

    fn title(&mut self) -> Event {
        Event::SessionTitled {
            session: self.session,
            title: format!("title {}", self.rng.below(1_000)),
        }
    }

    fn push_part(&mut self, message: MessageId, part: PartId, kind: PartKind) {
        if let Some(entry) = self.messages.iter_mut().find(|(id, _)| *id == message) {
            entry.1.push((part, kind));
        }
    }

    fn part_event(&mut self) -> Event {
        let session = self.session;
        let Some((message, parts)) = self.message() else {
            return self.title();
        };
        if parts.is_empty() {
            return self.title();
        }
        let (part, kind) = parts[self.rng.below(parts.len())];
        let word = format!("w{} ", self.rng.below(50));
        match kind {
            PartKind::Text if self.rng.chance(15) => Event::TextReplace {
                session,
                message,
                part,
                text: word,
            },
            PartKind::Text => Event::TextDelta {
                session,
                message,
                part,
                delta: word,
            },
            PartKind::Reasoning if self.rng.chance(15) => Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: Some(serde_json::json!({ "signature": word })),
            },
            PartKind::Reasoning => Event::ReasoningDelta {
                session,
                message,
                part,
                delta: word,
            },
            PartKind::Tool(call) => match self.rng.below(4) {
                0 => Event::ToolInputDelta {
                    session,
                    message,
                    part,
                    call,
                    name: "read".into(),
                    delta: word,
                },
                1 => Event::ToolCallRequested {
                    session,
                    message,
                    part,
                    call,
                    name: "read".into(),
                    input: serde_json::json!({ "path": word, "n": 1.5 }),
                },
                2 => Event::ToolResult {
                    session,
                    message,
                    part,
                    call,
                    output: serde_json::json!({ "ok": true, "text": word }),
                    time_ms: 3,
                },
                _ => Event::ToolError {
                    session,
                    message,
                    part,
                    call,
                    message_text: "boom".to_string(),
                    value: None,
                },
            },
        }
    }
}

/// Generate `len` events for `session` (after its `SessionCreated`).
///
/// With `fork_of`, the log starts like a fork: `SessionCreated` then
/// `SessionForked`, so legacy `MessageFinished.tokens` never count.
pub fn script(seed: u64, session: SessionId, fork_of: Option<SessionId>, len: usize) -> Vec<Event> {
    let mut script = Script {
        rng: Rng::new(seed),
        session,
        ids: 0,
        messages: Vec::new(),
        runs: Vec::new(),
        handles: Vec::new(),
        members: Vec::new(),
    };
    let mut events = vec![Event::SessionCreated {
        session,
        parent: None,
        agent: "build".into(),
        model: "fake/a".into(),
        workdir: "/tmp/scripted".into(),
    }];
    if let Some(source) = fork_of {
        events.push(Event::SessionForked {
            session,
            source,
            before_message: None,
        });
    }
    while events.len() < len {
        events.push(script.step());
    }
    events
}
