#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSelectionStep {
    Previous,
    Next,
}

#[must_use]
pub fn next_selected_message(
    current: Option<usize>,
    message_count: usize,
    step: MessageSelectionStep,
) -> Option<usize> {
    if message_count == 0 {
        return None;
    }
    match step {
        MessageSelectionStep::Previous => {
            Some(current.map_or(message_count - 1, |idx| idx.saturating_sub(1)))
        }
        MessageSelectionStep::Next => {
            Some(current.map_or(0, |idx| (idx + 1).min(message_count - 1)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageSelectionStep, next_selected_message};

    #[test]
    fn previous_starts_at_latest_message_and_saturates_at_first() {
        assert_eq!(
            next_selected_message(None, 3, MessageSelectionStep::Previous),
            Some(2)
        );
        assert_eq!(
            next_selected_message(Some(2), 3, MessageSelectionStep::Previous),
            Some(1)
        );
        assert_eq!(
            next_selected_message(Some(0), 3, MessageSelectionStep::Previous),
            Some(0)
        );
    }

    #[test]
    fn next_starts_at_first_message_and_saturates_at_latest() {
        assert_eq!(
            next_selected_message(None, 3, MessageSelectionStep::Next),
            Some(0)
        );
        assert_eq!(
            next_selected_message(Some(0), 3, MessageSelectionStep::Next),
            Some(1)
        );
        assert_eq!(
            next_selected_message(Some(2), 3, MessageSelectionStep::Next),
            Some(2)
        );
    }

    #[test]
    fn empty_transcript_clears_selection() {
        assert_eq!(
            next_selected_message(Some(1), 0, MessageSelectionStep::Next),
            None
        );
    }
}
