use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub language: String,
    pub auto_submit: bool,
    pub allow_terminal_submit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: "en".into(),
            auto_submit: false,
            allow_terminal_submit: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("FnType")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn dictionary_path() -> PathBuf {
    config_dir().join("dictionary.txt")
}

pub fn log_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/Logs/FnType")
}

pub fn lock_path() -> PathBuf {
    config_dir().join("fntype.lock")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(settings_path(), json);
        }
    }
}
