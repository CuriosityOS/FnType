use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub struct SttEvent {
    #[serde(rename = "type")]
    pub typ: String,
    pub text: Option<String>,
    #[serde(rename = "is_final")]
    pub is_final: Option<bool>,
    #[serde(rename = "speech_final")]
    pub speech_final: Option<bool>,
    pub message: Option<String>,
}

pub fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
pub struct Accumulator {
    completed: Vec<String>,
    interim: String,
}

impl Accumulator {
    pub fn reset(&mut self) {
        self.completed.clear();
        self.interim.clear();
    }

    pub fn ingest(&mut self, event: &SttEvent) {
        let cleaned = clean(event.text.as_deref().unwrap_or(""));
        if cleaned.is_empty() {
            return;
        }
        if event.typ == "transcript.done" || event.speech_final == Some(true) {
            // xAI's utterance-final event is the complete stitched transcript, so
            // it supersedes the chunk finals accumulated while speech was active.
            self.completed.clear();
            self.completed.push(cleaned);
            self.interim.clear();
        } else if event.is_final == Some(true) {
            if self.completed.last() != Some(&cleaned) {
                self.completed.push(cleaned);
            }
            self.interim.clear();
        } else {
            self.interim = cleaned;
        }
    }

    pub fn preview(&self) -> String {
        let mut parts = self.completed.clone();
        if !self.interim.is_empty() {
            parts.push(self.interim.clone());
        }
        parts.join(" ")
    }

    pub fn best_text(&self) -> String {
        self.preview()
    }
}

pub fn should_press_return(
    transcript: &str,
    has_target: bool,
    target_is_terminal: bool,
    auto_submit: bool,
    allow_terminal_submit: bool,
    secure_field: bool,
) -> bool {
    if clean(transcript).is_empty() || !has_target || !auto_submit || secure_field {
        return false;
    }
    if target_is_terminal && !allow_terminal_submit {
        return false;
    }
    true
}

pub fn is_terminal_app(name: &str, bundle_id: &str) -> bool {
    let haystack = format!("{name} {bundle_id}").to_lowercase();
    [
        "terminal",
        "iterm",
        "warp",
        "ghostty",
        "kitty",
        "alacritty",
        "wezterm",
        "rio",
    ]
    .iter()
    .any(|t| haystack.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(text: &str, is_final: bool, speech_final: bool) -> SttEvent {
        SttEvent {
            typ: "transcript.partial".into(),
            text: Some(text.into()),
            is_final: Some(is_final),
            speech_final: Some(speech_final),
            message: None,
        }
    }

    #[test]
    fn event_decodes_from_json() {
        let event: SttEvent = serde_json::from_str(
            r#"{"type":"transcript.partial","text":"hello world","is_final":true,"speech_final":true}"#,
        )
        .unwrap();
        assert_eq!(event.typ, "transcript.partial");
        assert_eq!(event.text.as_deref(), Some("hello world"));
        assert_eq!(event.speech_final, Some(true));
    }

    #[test]
    fn accumulator_keeps_final_segments_and_cleans_whitespace() {
        let mut acc = Accumulator::default();
        acc.ingest(&ev("  hello   world ", false, false));
        assert_eq!(acc.preview(), "hello world");
        acc.ingest(&ev("hello world", true, true));
        acc.ingest(&ev("hello world", true, true));
        assert_eq!(acc.best_text(), "hello world");
    }

    #[test]
    fn utterance_final_replaces_chunk_finals_with_stitched_text() {
        let mut acc = Accumulator::default();
        acc.ingest(&ev("first chunk", true, false));
        acc.ingest(&ev("second chunk", true, false));
        assert_eq!(acc.best_text(), "first chunk second chunk");

        acc.ingest(&ev("first chunk second chunk", true, true));
        assert_eq!(acc.best_text(), "first chunk second chunk");

        let done = SttEvent {
            typ: "transcript.done".into(),
            text: Some("corrected full transcript".into()),
            is_final: None,
            speech_final: None,
            message: None,
        };
        acc.ingest(&done);
        assert_eq!(acc.best_text(), "corrected full transcript");
    }

    #[test]
    fn best_text_falls_back_to_interim_on_timeout() {
        let mut acc = Accumulator::default();
        acc.ingest(&ev("still useful", false, false));
        assert_eq!(acc.best_text(), "still useful");
    }

    #[test]
    fn never_returns_for_empty_or_secure_text() {
        assert!(!should_press_return("", true, false, true, true, false));
        assert!(!should_press_return("hello", true, false, true, true, true));
        assert!(!should_press_return(
            "hello", false, false, true, true, false
        ));
    }

    #[test]
    fn terminal_needs_explicit_permission() {
        assert!(!should_press_return("ls", true, true, true, false, false));
        assert!(should_press_return("ls", true, true, true, true, false));
        assert!(is_terminal_app("iTerm2", "com.googlecode.iterm2"));
        assert!(!is_terminal_app("TextEdit", "com.apple.TextEdit"));
    }
}
