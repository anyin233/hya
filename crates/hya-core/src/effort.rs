use hya_proto::ModelRef;
use hya_provider::ReasoningEffort;

/// Origin of an effective reasoning effort selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffortSource {
    /// Explicit `#variant` model suffix.
    Suffix,
    /// The Agent's own default effort (see [`AgentEffortSource`]).
    Agent,
    /// Persisted per-model user preference.
    Preference,
    /// Configured model default.
    ModelDefault,
    /// Configured global default.
    GlobalDefault,
    /// No effort will be sent.
    None,
}

/// Effective effort and the layer that supplied it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectiveEffort {
    /// Typed effort, or `None` when the request omits effort.
    pub effort: Option<ReasoningEffort>,
    /// Precedence layer that supplied `effort`.
    pub source: EffortSource,
}

/// Which Agent-level layer chose an Agent's default effort, highest first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentEffortSource {
    /// Set by the user at runtime (durable, per Agent).
    Preference,
    /// `agents.<id>.reasoning` in the user's configuration file.
    Configured,
    /// The bundle Agent's authored `model_policy.reasoning`.
    Authored,
}

/// Pick an Agent's default effort: runtime preference > configuration >
/// authored policy. `None` when the Agent has no effort of its own.
#[must_use]
pub fn agent_effort(
    preference: Option<ReasoningEffort>,
    configured: Option<ReasoningEffort>,
    authored: Option<ReasoningEffort>,
) -> Option<(ReasoningEffort, AgentEffortSource)> {
    preference
        .map(|effort| (effort, AgentEffortSource::Preference))
        .or_else(|| configured.map(|effort| (effort, AgentEffortSource::Configured)))
        .or_else(|| authored.map(|effort| (effort, AgentEffortSource::Authored)))
}

/// Resolve the effective effort using suffix, agent, preference, model-default,
/// and global-default precedence.
///
/// `variants` contains the advertised labels for the base model. An invalid
/// selected effort is omitted and logged rather than falling through.
#[must_use]
pub fn resolve_effort(
    model: &ModelRef,
    agent: Option<ReasoningEffort>,
    preference: Option<ReasoningEffort>,
    model_default: Option<ReasoningEffort>,
    global_default: Option<ReasoningEffort>,
    variants: Option<&[String]>,
) -> EffectiveEffort {
    let valid = |effort: ReasoningEffort| {
        effort == ReasoningEffort::Off
            || variants.is_none_or(|labels| {
                labels.iter().any(|label| {
                    ReasoningEffort::parse(label).is_some_and(|candidate| candidate == effort)
                })
            })
    };
    let invalid = || {
        tracing::warn!(model = %model, "invalid reasoning effort for model; omitting effort");
        EffectiveEffort {
            effort: None,
            source: EffortSource::None,
        }
    };

    if let Some((_, suffix)) = model.as_str().rsplit_once('#') {
        let Some(effort) = ReasoningEffort::parse(suffix) else {
            return invalid();
        };
        return if valid(effort) {
            EffectiveEffort {
                effort: Some(effort),
                source: EffortSource::Suffix,
            }
        } else {
            invalid()
        };
    }
    for (candidate, source) in [
        (agent, EffortSource::Agent),
        (preference, EffortSource::Preference),
        (model_default, EffortSource::ModelDefault),
        (global_default, EffortSource::GlobalDefault),
    ] {
        if let Some(effort) = candidate {
            return if valid(effort) {
                EffectiveEffort {
                    effort: Some(effort),
                    source,
                }
            } else {
                invalid()
            };
        }
    }
    EffectiveEffort {
        effort: None,
        source: EffortSource::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ModelRef {
        ModelRef::new(name)
    }
    fn all() -> Vec<String> {
        vec!["low".into(), "medium".into(), "high".into()]
    }

    #[test]
    fn precedence_each_layer_wins_over_lower_layers() {
        let variants = all();
        let cases = [
            (
                "m#high",
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::Medium),
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Low),
                EffortSource::Suffix,
                Some(ReasoningEffort::High),
            ),
            (
                "m",
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::Medium),
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Low),
                EffortSource::Agent,
                Some(ReasoningEffort::High),
            ),
            (
                "m",
                None,
                Some(ReasoningEffort::Medium),
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Low),
                EffortSource::Preference,
                Some(ReasoningEffort::Medium),
            ),
            (
                "m",
                None,
                None,
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Medium),
                EffortSource::ModelDefault,
                Some(ReasoningEffort::Low),
            ),
            (
                "m",
                None,
                None,
                None,
                Some(ReasoningEffort::Medium),
                EffortSource::GlobalDefault,
                Some(ReasoningEffort::Medium),
            ),
            ("m", None, None, None, None, EffortSource::None, None),
        ];
        for (name, agent, preference, model_default, global_default, source, effort) in cases {
            let got = resolve_effort(
                &model(name),
                agent,
                preference,
                model_default,
                global_default,
                Some(&variants),
            );
            assert_eq!(got.source, source, "{name}");
            assert_eq!(got.effort, effort, "{name}");
        }
    }

    #[test]
    fn invalid_suffix_or_layer_value_resolves_to_none() {
        let variants = all();
        let got = resolve_effort(
            &model("m#invalid"),
            Some(ReasoningEffort::High),
            None,
            None,
            None,
            Some(&variants),
        );
        assert_eq!(
            got,
            EffectiveEffort {
                effort: None,
                source: EffortSource::None
            }
        );
        let got = resolve_effort(
            &model("m"),
            Some(ReasoningEffort::Max),
            None,
            None,
            None,
            Some(&variants),
        );
        assert_eq!(
            got,
            EffectiveEffort {
                effort: None,
                source: EffortSource::None
            }
        );
    }
}
