#![allow(dead_code)]

use hya_proto::{AgentName, SessionId, ToolCallId};
use hya_tool::{InlineAgent, SpawnMember, ToolOperation};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SPAWN_INTENT_DOMAIN_V1: &[u8] = b"hya.app.spawn-intent";
const SPAWN_INTENT_INTEGRITY_DOMAIN_V1: &[u8] = b"hya.app.spawn-intent-integrity";
const SPAWN_INTENT_FORMAT_VERSION: u32 = 1;
const SPAWN_INTENT_RUNTIME_FINGERPRINT_VERSION: u32 = 1;
const SPAWN_INTENT_ADMISSION_BINDING_FINGERPRINT_VERSION: u32 = 1;
const SPAWN_INTENT_RESOLVER_VERSION: u32 = 1;
const SPAWN_INTENT_INTEGRITY_WIDTH: usize = 32;
const MAX_SPAWN_INTENT_BYTES_V1: usize = hya_store::MAX_ADMISSION_INTENT_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PriorStartV1 {
    NeverStarted,
    PreviouslyStarted,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum SpawnIntentError {
    #[error("spawn intent integrity mismatch")]
    IntegrityMismatch,
    #[error("unsupported spawn intent {field} version {found}")]
    UnsupportedVersion { field: &'static str, found: u32 },
    #[error("non-canonical spawn intent")]
    NonCanonical,
    #[error("spawn intent encoded size {encoded} exceeds limit {limit}")]
    EncodedSizeExceeded { encoded: usize, limit: usize },
    #[error("spawn intent length overflow")]
    LengthOverflow,
}

#[derive(Clone, Debug)]
pub(crate) struct SpawnIntentInputV1 {
    pub(crate) member: SpawnMember,
    pub(crate) parent: SessionId,
    pub(crate) stable_target: AgentName,
    pub(crate) background: bool,
    pub(crate) operation: ToolOperation,
    pub(crate) member_ordinal: u32,
    pub(crate) batch_cardinality: u32,
    pub(crate) prior_start: PriorStartV1,
    pub(crate) runtime_fingerprint: [u8; 32],
    pub(crate) admission_binding_fingerprint: [u8; 32],
    pub(crate) diagnostic_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SpawnIntentV1 {
    member: SpawnMember,
    parent: SessionId,
    stable_target: AgentName,
    background: bool,
    source_tool_call_id: ToolCallId,
    operation_id: String,
    member_ordinal: u32,
    batch_cardinality: u32,
    prior_start: PriorStartV1,
    runtime_fingerprint: [u8; 32],
    admission_binding_fingerprint: [u8; 32],
    diagnostic_generation: u64,
}

impl SpawnIntentV1 {
    pub(crate) fn new(input: SpawnIntentInputV1) -> Result<Self, SpawnIntentError> {
        let SpawnIntentInputV1 {
            member,
            parent,
            stable_target,
            background,
            operation,
            member_ordinal,
            batch_cardinality,
            prior_start,
            runtime_fingerprint,
            admission_binding_fingerprint,
            diagnostic_generation,
        } = input;
        if stable_target.as_str().is_empty()
            || batch_cardinality == 0
            || member_ordinal >= batch_cardinality
        {
            return Err(SpawnIntentError::NonCanonical);
        }

        let source_tool_call_id = operation.source_tool_call_id();
        let operation_id = operation_id_for(source_tool_call_id);
        if operation.operation_id().to_string() != operation_id {
            return Err(SpawnIntentError::NonCanonical);
        }

        Ok(Self {
            member,
            parent,
            stable_target,
            background,
            source_tool_call_id,
            operation_id,
            member_ordinal,
            batch_cardinality,
            prior_start,
            runtime_fingerprint,
            admission_binding_fingerprint,
            diagnostic_generation,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, SpawnIntentError> {
        if self.stable_target.as_str().is_empty()
            || self.batch_cardinality == 0
            || self.member_ordinal >= self.batch_cardinality
        {
            return Err(SpawnIntentError::NonCanonical);
        }
        if self.operation_id != operation_id_for(self.source_tool_call_id) {
            return Err(SpawnIntentError::NonCanonical);
        }

        let mut bytes = SpawnIntentByteSinkV1::new();
        bytes.slice(SPAWN_INTENT_DOMAIN_V1)?;
        push_u32(&mut bytes, SPAWN_INTENT_FORMAT_VERSION)?;
        push_u32(&mut bytes, SPAWN_INTENT_RUNTIME_FINGERPRINT_VERSION)?;
        bytes.slice(&self.runtime_fingerprint)?;
        push_u32(
            &mut bytes,
            SPAWN_INTENT_ADMISSION_BINDING_FINGERPRINT_VERSION,
        )?;
        bytes.slice(&self.admission_binding_fingerprint)?;
        push_u32(&mut bytes, SPAWN_INTENT_RESOLVER_VERSION)?;
        push_u64(&mut bytes, self.diagnostic_generation)?;

        encode_member(&mut bytes, &self.member)?;
        push_string(&mut bytes, &self.parent.to_string())?;
        push_string(&mut bytes, self.stable_target.as_str())?;
        push_bool(&mut bytes, self.background)?;
        push_string(&mut bytes, &self.source_tool_call_id.to_string())?;
        push_string(&mut bytes, &self.operation_id)?;
        push_u32(&mut bytes, self.member_ordinal)?;
        push_u32(&mut bytes, self.batch_cardinality)?;
        push_prior_start(&mut bytes, self.prior_start)?;

        let integrity = integrity_digest(bytes.as_slice());
        bytes.slice(&integrity)?;
        Ok(bytes.finish())
    }

    pub(crate) fn into_admission_intent(
        self,
    ) -> Result<hya_store::AdmissionIntent, SpawnIntentError> {
        let spawn_intent = self.encode()?;
        Ok(hya_store::AdmissionIntent {
            runtime_fingerprint_version: SPAWN_INTENT_RUNTIME_FINGERPRINT_VERSION,
            runtime_fingerprint: self.runtime_fingerprint,
            admission_binding_fingerprint_version:
                SPAWN_INTENT_ADMISSION_BINDING_FINGERPRINT_VERSION,
            admission_binding_fingerprint: self.admission_binding_fingerprint,
            spawn_intent,
        })
    }

    pub(crate) fn decode_admission_launch(
        launch: &hya_store::AdmissionLaunch,
    ) -> Result<Self, SpawnIntentError> {
        let intent = Self::decode(&launch.intent.spawn_intent)?;
        if launch.intent.runtime_fingerprint_version != SPAWN_INTENT_RUNTIME_FINGERPRINT_VERSION
            || launch.intent.admission_binding_fingerprint_version
                != SPAWN_INTENT_ADMISSION_BINDING_FINGERPRINT_VERSION
            || launch.intent.runtime_fingerprint != intent.runtime_fingerprint
            || launch.intent.admission_binding_fingerprint != intent.admission_binding_fingerprint
            || launch.record.state != hya_store::AdmissionState::Accepted
            || launch.record.admission_units != 1
            || launch.record.logical_released
            || launch.record.terminal_reason.is_some()
            || launch.record.source_tool_call_id != intent.source_tool_call_id
            || launch.record.operation_id
                != ToolOperation::from_tool_call(intent.source_tool_call_id).operation_id()
            || launch.record.member_ordinal != intent.member_ordinal
            || launch.record.batch_size != intent.batch_cardinality
            || intent.prior_start != PriorStartV1::NeverStarted
        {
            return Err(SpawnIntentError::NonCanonical);
        }
        Ok(intent)
    }

    fn decode(bytes: &[u8]) -> Result<Self, SpawnIntentError> {
        let encoded = checked_encoded_end_v1(0, bytes.len())?;
        if encoded > MAX_SPAWN_INTENT_BYTES_V1 {
            return Err(SpawnIntentError::EncodedSizeExceeded {
                encoded,
                limit: MAX_SPAWN_INTENT_BYTES_V1,
            });
        }
        if bytes.len() < SPAWN_INTENT_INTEGRITY_WIDTH {
            return Err(SpawnIntentError::NonCanonical);
        }
        let integrity_offset = bytes.len() - SPAWN_INTENT_INTEGRITY_WIDTH;
        let expected_integrity = integrity_digest(&bytes[..integrity_offset]);
        if bytes[integrity_offset..] != expected_integrity {
            return Err(SpawnIntentError::IntegrityMismatch);
        }

        let mut cursor = Cursor::new(&bytes[..integrity_offset]);
        if cursor.take(SPAWN_INTENT_DOMAIN_V1.len())? != SPAWN_INTENT_DOMAIN_V1 {
            return Err(SpawnIntentError::NonCanonical);
        }
        let format_version = cursor.u32()?;
        if format_version != SPAWN_INTENT_FORMAT_VERSION {
            return Err(SpawnIntentError::UnsupportedVersion {
                field: "format",
                found: format_version,
            });
        }
        let runtime_version = cursor.u32()?;
        if runtime_version != SPAWN_INTENT_RUNTIME_FINGERPRINT_VERSION {
            return Err(SpawnIntentError::UnsupportedVersion {
                field: "runtime_fingerprint",
                found: runtime_version,
            });
        }
        let runtime_fingerprint = cursor.fixed::<32>()?;
        let admission_version = cursor.u32()?;
        if admission_version != SPAWN_INTENT_ADMISSION_BINDING_FINGERPRINT_VERSION {
            return Err(SpawnIntentError::UnsupportedVersion {
                field: "admission_binding_fingerprint",
                found: admission_version,
            });
        }
        let admission_binding_fingerprint = cursor.fixed::<32>()?;
        let resolver_version = cursor.u32()?;
        if resolver_version != SPAWN_INTENT_RESOLVER_VERSION {
            return Err(SpawnIntentError::UnsupportedVersion {
                field: "resolver",
                found: resolver_version,
            });
        }
        let diagnostic_generation = cursor.u64()?;

        let member = decode_member(&mut cursor)?;
        let parent_raw = cursor.string()?;
        let parent = parent_raw
            .parse::<SessionId>()
            .map_err(|_| SpawnIntentError::NonCanonical)?;
        if parent.to_string() != parent_raw {
            return Err(SpawnIntentError::NonCanonical);
        }
        let stable_target_raw = cursor.string()?;
        let stable_target = AgentName::new(stable_target_raw);
        let background = cursor.bool()?;
        let source_tool_call_raw = cursor.string()?;
        let source_tool_call_id = source_tool_call_raw
            .parse::<ToolCallId>()
            .map_err(|_| SpawnIntentError::NonCanonical)?;
        if source_tool_call_id.to_string() != source_tool_call_raw {
            return Err(SpawnIntentError::NonCanonical);
        }
        let operation_id = cursor.string()?;
        if operation_id != operation_id_for(source_tool_call_id) {
            return Err(SpawnIntentError::NonCanonical);
        }
        let member_ordinal = cursor.u32()?;
        let batch_cardinality = cursor.u32()?;
        let prior_start = cursor.prior_start()?;
        if !cursor.is_empty() {
            return Err(SpawnIntentError::NonCanonical);
        }

        let operation = ToolOperation::from_tool_call(source_tool_call_id);
        let intent = Self::new(SpawnIntentInputV1 {
            member,
            parent,
            stable_target,
            background,
            operation,
            member_ordinal,
            batch_cardinality,
            prior_start,
            runtime_fingerprint,
            admission_binding_fingerprint,
            diagnostic_generation,
        })?;
        if intent.encode()?.as_slice() != bytes {
            return Err(SpawnIntentError::NonCanonical);
        }
        Ok(intent)
    }

    pub(crate) fn raw_member(&self) -> &SpawnMember {
        &self.member
    }

    pub(crate) fn parent(&self) -> SessionId {
        self.parent
    }

    pub(crate) fn stable_target(&self) -> &AgentName {
        &self.stable_target
    }
}

fn encode_spawn_intent_batch_v1(
    intents: &[SpawnIntentV1],
) -> Result<Vec<Vec<u8>>, SpawnIntentError> {
    intents.iter().map(SpawnIntentV1::encode).collect()
}

impl PartialEq for SpawnIntentV1 {
    fn eq(&self, other: &Self) -> bool {
        members_equal(&self.member, &other.member)
            && self.parent == other.parent
            && self.stable_target == other.stable_target
            && self.background == other.background
            && self.source_tool_call_id == other.source_tool_call_id
            && self.operation_id == other.operation_id
            && self.member_ordinal == other.member_ordinal
            && self.batch_cardinality == other.batch_cardinality
            && self.prior_start == other.prior_start
            && self.runtime_fingerprint == other.runtime_fingerprint
            && self.admission_binding_fingerprint == other.admission_binding_fingerprint
            && self.diagnostic_generation == other.diagnostic_generation
    }
}

impl Eq for SpawnIntentV1 {}

fn operation_id_for(source_tool_call_id: ToolCallId) -> String {
    ToolOperation::from_tool_call(source_tool_call_id)
        .operation_id()
        .to_string()
}

fn members_equal(left: &SpawnMember, right: &SpawnMember) -> bool {
    left.description == right.description
        && left.prompt == right.prompt
        && left.subagent_type == right.subagent_type
        && left.task_id == right.task_id
        && left.model == right.model
        && left.category == right.category
        && left.resident == right.resident
        && inline_agents_equal(left.inline_agent.as_ref(), right.inline_agent.as_ref())
}

fn inline_agents_equal(left: Option<&InlineAgent>, right: Option<&InlineAgent>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.name == right.name
                && left.prompt == right.prompt
                && left.description == right.description
                && left.category == right.category
                && left.model == right.model
                && left.resident == right.resident
        }
        _ => false,
    }
}

fn checked_encoded_end_v1(current: usize, additional: usize) -> Result<usize, SpawnIntentError> {
    current
        .checked_add(additional)
        .ok_or(SpawnIntentError::LengthOverflow)
}

struct SpawnIntentByteSinkV1 {
    bytes: Vec<u8>,
}

impl SpawnIntentByteSinkV1 {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn byte(&mut self, value: u8) -> Result<(), SpawnIntentError> {
        self.slice(&[value])
    }

    fn slice(&mut self, value: &[u8]) -> Result<(), SpawnIntentError> {
        let end = checked_encoded_end_v1(self.bytes.len(), value.len())?;
        if end > MAX_SPAWN_INTENT_BYTES_V1 {
            return Err(SpawnIntentError::EncodedSizeExceeded {
                encoded: end,
                limit: MAX_SPAWN_INTENT_BYTES_V1,
            });
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn encode_member(
    bytes: &mut SpawnIntentByteSinkV1,
    member: &SpawnMember,
) -> Result<(), SpawnIntentError> {
    push_string(bytes, &member.description)?;
    push_string(bytes, &member.prompt)?;
    push_string(bytes, &member.subagent_type)?;
    push_option_string(bytes, member.task_id.as_deref())?;
    push_option_string(bytes, member.model.as_deref())?;
    push_option_string(bytes, member.category.as_deref())?;
    match member.inline_agent.as_ref() {
        None => bytes.byte(0)?,
        Some(inline) => {
            bytes.byte(1)?;
            push_string(bytes, &inline.name)?;
            push_string(bytes, &inline.prompt)?;
            push_option_string(bytes, inline.description.as_deref())?;
            push_option_string(bytes, inline.category.as_deref())?;
            push_option_string(bytes, inline.model.as_deref())?;
            push_option_bool(bytes, inline.resident)?;
        }
    }
    push_bool(bytes, member.resident)
}

fn decode_member(cursor: &mut Cursor<'_>) -> Result<SpawnMember, SpawnIntentError> {
    let description = cursor.string()?;
    let prompt = cursor.string()?;
    let subagent_type = cursor.string()?;
    let task_id = cursor.option_string()?;
    let model = cursor.option_string()?;
    let category = cursor.option_string()?;
    let inline_agent = match cursor.tag()? {
        0 => None,
        1 => Some(InlineAgent {
            name: cursor.string()?,
            prompt: cursor.string()?,
            description: cursor.option_string()?,
            category: cursor.option_string()?,
            model: cursor.option_string()?,
            resident: cursor.option_bool()?,
        }),
        _ => return Err(SpawnIntentError::NonCanonical),
    };
    let resident = cursor.bool()?;
    Ok(SpawnMember {
        description,
        prompt,
        subagent_type,
        task_id,
        model,
        category,
        inline_agent,
        resident,
    })
}

fn push_u32(bytes: &mut SpawnIntentByteSinkV1, value: u32) -> Result<(), SpawnIntentError> {
    bytes.slice(&value.to_be_bytes())
}

fn push_u64(bytes: &mut SpawnIntentByteSinkV1, value: u64) -> Result<(), SpawnIntentError> {
    bytes.slice(&value.to_be_bytes())
}

fn push_string(bytes: &mut SpawnIntentByteSinkV1, value: &str) -> Result<(), SpawnIntentError> {
    let length = u32::try_from(value.len()).map_err(|_| SpawnIntentError::LengthOverflow)?;
    push_u32(bytes, length)?;
    bytes.slice(value.as_bytes())
}

fn push_option_string(
    bytes: &mut SpawnIntentByteSinkV1,
    value: Option<&str>,
) -> Result<(), SpawnIntentError> {
    match value {
        None => bytes.byte(0)?,
        Some(value) => {
            bytes.byte(1)?;
            push_string(bytes, value)?;
        }
    }
    Ok(())
}

fn push_option_bool(
    bytes: &mut SpawnIntentByteSinkV1,
    value: Option<bool>,
) -> Result<(), SpawnIntentError> {
    match value {
        None => bytes.byte(0)?,
        Some(value) => {
            bytes.byte(1)?;
            push_bool(bytes, value)?;
        }
    }
    Ok(())
}

fn push_bool(bytes: &mut SpawnIntentByteSinkV1, value: bool) -> Result<(), SpawnIntentError> {
    bytes.byte(u8::from(value))
}

fn push_prior_start(
    bytes: &mut SpawnIntentByteSinkV1,
    value: PriorStartV1,
) -> Result<(), SpawnIntentError> {
    bytes.byte(match value {
        PriorStartV1::NeverStarted => 0,
        PriorStartV1::PreviouslyStarted => 1,
    })
}

fn integrity_digest(payload: &[u8]) -> [u8; SPAWN_INTENT_INTEGRITY_WIDTH] {
    let mut hasher = Sha256::new();
    hasher.update(SPAWN_INTENT_INTEGRITY_DOMAIN_V1);
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut integrity = [0_u8; SPAWN_INTENT_INTEGRITY_WIDTH];
    integrity.copy_from_slice(&digest);
    integrity
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SpawnIntentError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SpawnIntentError::NonCanonical)?;
        if end > self.bytes.len() {
            return Err(SpawnIntentError::NonCanonical);
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, SpawnIntentError> {
        Ok(self.take(1)?[0])
    }

    fn tag(&mut self) -> Result<u8, SpawnIntentError> {
        self.byte()
    }

    fn u32(&mut self) -> Result<u32, SpawnIntentError> {
        let mut value = [0_u8; 4];
        value.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(value))
    }

    fn u64(&mut self) -> Result<u64, SpawnIntentError> {
        let mut value = [0_u8; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(value))
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], SpawnIntentError> {
        let mut value = [0_u8; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }

    fn string(&mut self) -> Result<String, SpawnIntentError> {
        let length = usize::try_from(self.u32()?).map_err(|_| SpawnIntentError::NonCanonical)?;
        let value = self.take(length)?;
        let value = std::str::from_utf8(value).map_err(|_| SpawnIntentError::NonCanonical)?;
        Ok(value.to_string())
    }

    fn option_string(&mut self) -> Result<Option<String>, SpawnIntentError> {
        match self.tag()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            _ => Err(SpawnIntentError::NonCanonical),
        }
    }

    fn bool(&mut self) -> Result<bool, SpawnIntentError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SpawnIntentError::NonCanonical),
        }
    }

    fn option_bool(&mut self) -> Result<Option<bool>, SpawnIntentError> {
        match self.tag()? {
            0 => Ok(None),
            1 => Ok(Some(self.bool()?)),
            _ => Err(SpawnIntentError::NonCanonical),
        }
    }

    fn prior_start(&mut self) -> Result<PriorStartV1, SpawnIntentError> {
        match self.tag()? {
            0 => Ok(PriorStartV1::NeverStarted),
            1 => Ok(PriorStartV1::PreviouslyStarted),
            _ => Err(SpawnIntentError::NonCanonical),
        }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use hya_proto::{AgentName, SessionId, ToolCallId};
    use hya_tool::{InlineAgent, SpawnMember, ToolOperation};
    use sha2::{Digest, Sha256};

    use super::{
        MAX_SPAWN_INTENT_BYTES_V1, PriorStartV1, SPAWN_INTENT_DOMAIN_V1,
        SPAWN_INTENT_INTEGRITY_DOMAIN_V1, SpawnIntentError, SpawnIntentInputV1, SpawnIntentV1,
        checked_encoded_end_v1, encode_spawn_intent_batch_v1,
    };

    #[test]
    fn spawn_intent_v1_round_trips_raw_fields_and_rejects_tampering() {
        const U32_WIDTH: usize = std::mem::size_of::<u32>();
        const FINGERPRINT_WIDTH: usize = 32;
        const U64_WIDTH: usize = std::mem::size_of::<u64>();
        const INTEGRITY_WIDTH: usize = 32;

        let parent: SessionId = "ses_018f032a3d2f7a21a05c2e61fc57dced"
            .parse()
            .expect("deterministic parent session id");
        let source_tool_call: ToolCallId = "tc_018f032a3d2f7a21a05c2e61fc57dced"
            .parse()
            .expect("deterministic source tool call id");
        let operation = ToolOperation::from_tool_call(source_tool_call);
        let stable_target = "stable-target-alpha";
        let raw_task_prompt = "raw-task-prompt-sentinel-6b7e4d2c";
        let raw_inline_prompt = "raw-inline-prompt-sentinel-94c1a8e3";
        let member = SpawnMember {
            description: "raw-description-sentinel-3f8a1c7d".to_string(),
            prompt: raw_task_prompt.to_string(),
            subagent_type: "  raw-subagent-type  ".to_string(),
            task_id: Some("  raw-task-id:not-a-session  ".to_string()),
            model: Some("  raw-model  ".to_string()),
            category: Some("  raw-category  ".to_string()),
            inline_agent: Some(InlineAgent {
                name: "  raw-inline-name  ".to_string(),
                prompt: raw_inline_prompt.to_string(),
                description: None,
                category: Some("  raw-inline-category  ".to_string()),
                model: Some("  raw-inline-model  ".to_string()),
                resident: Some(true),
            }),
            resident: true,
        };
        let runtime_fingerprint = [0x11; FINGERPRINT_WIDTH];
        let admission_binding_fingerprint = [0x22; FINGERPRINT_WIDTH];
        let intent = SpawnIntentV1::new(SpawnIntentInputV1 {
            member: member.clone(),
            parent,
            stable_target: AgentName::new(stable_target),
            background: true,
            operation,
            member_ordinal: 2,
            batch_cardinality: 5,
            prior_start: PriorStartV1::NeverStarted,
            runtime_fingerprint,
            admission_binding_fingerprint,
            diagnostic_generation: 42,
        })
        .expect("valid deterministic spawn intent");

        let encoded = intent.encode().expect("canonical intent encoding");
        let decoded = SpawnIntentV1::decode(&encoded).expect("canonical intent decoding");
        assert_eq!(decoded, intent);
        assert_eq!(
            decoded.encode().expect("re-encoding decoded intent"),
            encoded
        );

        let recovered = decoded.raw_member();
        assert_eq!(recovered.description, member.description);
        assert_eq!(recovered.prompt, member.prompt);
        assert_eq!(recovered.subagent_type, member.subagent_type);
        assert_eq!(recovered.task_id, member.task_id);
        assert_eq!(recovered.model, member.model);
        assert_eq!(recovered.category, member.category);
        assert_eq!(recovered.resident, member.resident);
        let recovered_inline = recovered
            .inline_agent
            .as_ref()
            .expect("inline raw overlay preserved");
        let expected_inline = member
            .inline_agent
            .as_ref()
            .expect("fixture inline raw overlay");
        assert_eq!(recovered_inline.name, expected_inline.name);
        assert_eq!(recovered_inline.prompt, expected_inline.prompt);
        assert_eq!(recovered_inline.description, expected_inline.description);
        assert_eq!(recovered_inline.category, expected_inline.category);
        assert_eq!(recovered_inline.model, expected_inline.model);
        assert_eq!(recovered_inline.resident, expected_inline.resident);

        let contains = |needle: &[u8]| encoded.windows(needle.len()).any(|window| window == needle);
        assert!(contains(raw_task_prompt.as_bytes()));
        assert!(contains(raw_inline_prompt.as_bytes()));
        for derived in [
            "effective-model-sentinel-0aa1bb2c",
            "effective-system-prompt-sentinel-1dd2ee3f",
            "effective-reasoning-sentinel-4aa5bb6c",
            "chosen-category-candidate-sentinel-7dd8ee9f",
            "resolved-agent-spec-sentinel-a1b2c3d4",
            "provider-catalog-resource-object-sentinel-e5f6a7b8",
            "credential-secret-sentinel-c9d0e1f2",
            "guidance-sentinel-33445566",
            "cancellation-state-sentinel-778899aa",
            "reply-channel-sentinel-bbccddee",
            "runtime-handle-sentinel-ff001122",
        ] {
            assert!(
                !contains(derived.as_bytes()),
                "derived value leaked: {derived}"
            );
        }

        let format_version_offset = SPAWN_INTENT_DOMAIN_V1.len();
        let runtime_version_offset = format_version_offset + U32_WIDTH;
        let runtime_fingerprint_offset = runtime_version_offset + U32_WIDTH;
        let admission_version_offset = runtime_fingerprint_offset + FINGERPRINT_WIDTH;
        let admission_fingerprint_offset = admission_version_offset + U32_WIDTH;
        let resolver_version_offset = admission_fingerprint_offset + FINGERPRINT_WIDTH;
        let diagnostic_generation_offset = resolver_version_offset + U32_WIDTH;
        let raw_member_offset = diagnostic_generation_offset + U64_WIDTH;
        let raw_task_offset = raw_member_offset + U32_WIDTH + member.description.len() + U32_WIDTH;

        let mut task_tampered = encoded.clone();
        task_tampered[raw_task_offset] ^= 0x01;
        assert_eq!(
            SpawnIntentV1::decode(&task_tampered),
            Err(SpawnIntentError::IntegrityMismatch)
        );

        let rewrite_integrity = |bytes: &mut Vec<u8>| {
            let integrity_offset = bytes
                .len()
                .checked_sub(INTEGRITY_WIDTH)
                .expect("encoded intent includes integrity bytes");
            let mut hasher = Sha256::new();
            hasher.update(SPAWN_INTENT_INTEGRITY_DOMAIN_V1);
            hasher.update(&bytes[..integrity_offset]);
            let digest = hasher.finalize();
            bytes[integrity_offset..].copy_from_slice(&digest);
        };

        for (field, offset) in [
            ("format", format_version_offset),
            ("runtime_fingerprint", runtime_version_offset),
            ("admission_binding_fingerprint", admission_version_offset),
            ("resolver", resolver_version_offset),
        ] {
            let mut unsupported = encoded.clone();
            unsupported[offset..offset + U32_WIDTH].copy_from_slice(&2_u32.to_be_bytes());
            rewrite_integrity(&mut unsupported);
            assert_eq!(
                SpawnIntentV1::decode(&unsupported),
                Err(SpawnIntentError::UnsupportedVersion { field, found: 2 })
            );
        }

        let mut trailing = encoded.clone();
        let integrity_offset = trailing.len() - INTEGRITY_WIDTH;
        trailing.insert(integrity_offset, 0xa5);
        rewrite_integrity(&mut trailing);
        assert_eq!(
            SpawnIntentV1::decode(&trailing),
            Err(SpawnIntentError::NonCanonical)
        );

        for offset in [
            runtime_fingerprint_offset,
            admission_fingerprint_offset,
            diagnostic_generation_offset,
        ] {
            let mut identity_tampered = encoded.clone();
            identity_tampered[offset] ^= 0x01;
            assert_eq!(
                SpawnIntentV1::decode(&identity_tampered),
                Err(SpawnIntentError::IntegrityMismatch)
            );
        }
    }

    #[test]
    fn spawn_intent_v1_enforces_exact_size_and_batch_preparation() {
        const FINGERPRINT_WIDTH: usize = 32;

        let parent: SessionId = "ses_018f032a3d2f7a21a05c2e61fc57dced"
            .parse()
            .expect("deterministic parent session id");
        let source_tool_call: ToolCallId = "tc_018f032a3d2f7a21a05c2e61fc57dced"
            .parse()
            .expect("deterministic source tool call id");
        let fixture = |prompt: String, member_ordinal: u32| {
            SpawnIntentV1::new(SpawnIntentInputV1 {
                member: SpawnMember {
                    prompt,
                    ..SpawnMember::default()
                },
                parent,
                stable_target: AgentName::new("stable-target-alpha"),
                background: false,
                operation: ToolOperation::from_tool_call(source_tool_call),
                member_ordinal,
                batch_cardinality: 2,
                prior_start: PriorStartV1::NeverStarted,
                runtime_fingerprint: [0x11; FINGERPRINT_WIDTH],
                admission_binding_fingerprint: [0x22; FINGERPRINT_WIDTH],
                diagnostic_generation: 42,
            })
            .expect("valid deterministic spawn intent fixture")
        };

        let empty = fixture(String::new(), 0);
        let baseline_encoded = empty.encode().expect("empty-prompt fixture encoding");
        let prompt_len = MAX_SPAWN_INTENT_BYTES_V1
            .checked_sub(baseline_encoded.len())
            .expect("baseline encoding leaves prompt capacity");
        let exact = fixture("x".repeat(prompt_len), 0);
        let exact_encoded = exact.encode().expect("exact-sized row encoding");
        assert_eq!(exact_encoded.len(), 1_048_576);

        let oversized = fixture("x".repeat(prompt_len + 1), 1);
        let expected_size_error = SpawnIntentError::EncodedSizeExceeded {
            encoded: 1_048_577,
            limit: 1_048_576,
        };
        assert_eq!(oversized.encode(), Err(expected_size_error.clone()));
        assert_eq!(
            SpawnIntentV1::decode(&vec![0_u8; 1_048_577]),
            Err(expected_size_error.clone())
        );
        assert_eq!(
            checked_encoded_end_v1(usize::MAX, 1),
            Err(SpawnIntentError::LengthOverflow)
        );
        assert_eq!(
            encode_spawn_intent_batch_v1(&[exact.clone(), oversized]),
            Err(expected_size_error)
        );

        let exact_batch =
            encode_spawn_intent_batch_v1(&[exact]).expect("one-member exact batch encoding");
        assert_eq!(exact_batch.len(), 1);
        assert_eq!(exact_batch[0].len(), 1_048_576);
    }
}
