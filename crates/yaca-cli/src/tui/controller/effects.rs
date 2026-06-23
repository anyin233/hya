use super::super::block_action::SelectedBlockAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiEffect {
    None,
    Exit,
    Interrupt,
    Submit(String),
    SubmitConfigured {
        prompt: String,
        agent: Option<String>,
        model: Option<String>,
    },
    SelectModel {
        model: String,
        provider: Option<String>,
    },
    SelectAgent(String),
    SelectReasoning(String),
    ResumeSession(String),
    NewSession,
    CompactTranscript,
    InitProject,
    ExportTranscript,
    SelectedBlock(SelectedBlockAction),
    SystemMessage(String),
}
