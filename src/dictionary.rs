//! Wispr-Flow-style personal dictionary.
//!
//! File format (one entry per line):
//!   xAI                       -> bias recognition + enforce this exact casing
//!   gee pee you -> GPU        -> bias "gee pee you", replace it with "GPU"
//!   # lines starting with # are comments

pub struct Entry {
    pub matcher: String,
    pub output: String,
}

pub struct Dictionary {
    pub entries: Vec<Entry>,
}

pub const DEFAULT_DICTIONARY: &str = "\
# FnType dictionary - one entry per line.
# `Word` biases recognition toward the word and enforces its exact casing.
# `spoken form -> Written Form` also replaces the spoken form in the output.
xAI
Grok
FnType
gee pee you -> GPU
open whisper -> OpenWispr
";

impl Dictionary {
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (matcher, output) = match line.split_once("->") {
                Some((m, o)) => (m.trim().to_string(), o.trim().to_string()),
                None => (line.to_string(), line.to_string()),
            };
            if matcher.is_empty() || output.is_empty() {
                continue;
            }
            entries.push(Entry { matcher, output });
        }
        Dictionary { entries }
    }

    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, DEFAULT_DICTIONARY);
                Self::parse(DEFAULT_DICTIONARY)
            }
        }
    }

    /// Terms sent to xAI as `keyterm` bias parameters (max 100, each <= 50 chars).
    pub fn bias_terms(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut terms = Vec::new();
        for entry in &self.entries {
            for term in [&entry.matcher, &entry.output] {
                let term = term.trim();
                if term.is_empty() || term.len() > 50 {
                    continue;
                }
                if seen.insert(term.to_lowercase()) {
                    terms.push(term.to_string());
                }
                if terms.len() >= 100 {
                    return terms;
                }
            }
        }
        terms
    }

    /// Case-insensitive, word-boundary replacement of every entry in `text`.
    pub fn apply(&self, text: &str) -> String {
        let mut result = text.to_string();
        for entry in &self.entries {
            result = replace_word(&result, &entry.matcher, &entry.output);
        }
        result
    }
}

fn replace_word(haystack: &str, needle: &str, replacement: &str) -> String {
    let hay: Vec<char> = haystack.chars().collect();
    let ndl: Vec<char> = needle.to_lowercase().chars().collect();
    if ndl.is_empty() || hay.len() < ndl.len() {
        return haystack.to_string();
    }
    let hay_lower: Vec<char> = haystack.to_lowercase().chars().collect();
    // to_lowercase can change length for exotic chars; bail out to be safe.
    if hay_lower.len() != hay.len() {
        return haystack.to_string();
    }

    let boundary = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while i < hay.len() {
        let end = i + ndl.len();
        let matches = end <= hay.len()
            && hay_lower[i..end] == ndl[..]
            && boundary(if i == 0 { None } else { hay.get(i - 1) })
            && boundary(hay.get(end));
        if matches {
            out.push_str(replacement);
            i = end;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(text: &str) -> Dictionary {
        Dictionary::parse(text)
    }

    #[test]
    fn enforces_casing_for_plain_entries() {
        let d = dict("OpenWispr\nxAI");
        assert_eq!(d.apply("openwispr uses xai"), "OpenWispr uses xAI");
    }

    #[test]
    fn replaces_spoken_forms() {
        let d = dict("gee pee you -> GPU");
        assert_eq!(d.apply("My gee pee you is fast."), "My GPU is fast.");
    }

    #[test]
    fn respects_word_boundaries() {
        let d = dict("cat -> dog");
        assert_eq!(
            d.apply("concatenate the cat, please"),
            "concatenate the dog, please"
        );
    }

    #[test]
    fn handles_punctuation_adjacent_matches() {
        let d = dict("openwispr -> OpenWispr");
        assert_eq!(
            d.apply("like openwispr, but faster"),
            "like OpenWispr, but faster"
        );
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let d = dict("# comment\n\nGrok\n");
        assert_eq!(d.entries.len(), 1);
        assert_eq!(d.bias_terms(), vec!["Grok".to_string()]);
    }

    #[test]
    fn bias_terms_deduplicates_and_caps_length() {
        let d = dict("GPU -> GPU\ngee pee you -> GPU");
        assert_eq!(
            d.bias_terms(),
            vec!["GPU".to_string(), "gee pee you".to_string()]
        );
    }
}
