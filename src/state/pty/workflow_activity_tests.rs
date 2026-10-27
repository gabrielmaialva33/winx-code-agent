use super::*;

#[test]
fn native_workflow_echo_quarantine_covers_work_approval_and_multiline_reflow() {
    for hint in ["claude", "auto"] {
        let recognizer = crate::state::turn::recognizer_for(hint);
        for text in [
            "esc to interrupt",
            "would you like",
            "please explain esc to\ninterrupt",
            "would\r\nyou\tlike to review this",
        ] {
            assert!(
                prompt_text_mimics_activity(recognizer.as_ref(), text),
                "{hint} must treat activity-looking prompt text as ambiguous: {text:?}"
            );
        }
    }
}

#[test]
fn native_workflow_ordinary_prompt_text_keeps_activity_detection_enabled() {
    for hint in ["claude", "auto"] {
        let recognizer = crate::state::turn::recognizer_for(hint);
        for text in ["hello", "review these changes", "summarize\nthe result"] {
            assert!(!prompt_text_mimics_activity(recognizer.as_ref(), text));
        }
    }
}
