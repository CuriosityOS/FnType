use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

const SERVICE: &str = "com.curiosityos.fntype";
const ACCOUNT: &str = "xai-api-key";

fn private_key_path() -> std::path::PathBuf {
    crate::config::config_dir().join(".xai-key")
}

pub fn load() -> Option<String> {
    let keychain = get_generic_password(SERVICE, ACCOUNT)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    keychain
        .or_else(|| std::fs::read_to_string(private_key_path()).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() >= 40)
}

pub fn save(key: &str) -> anyhow::Result<()> {
    let trimmed = key.trim();
    if trimmed.len() < 40 {
        anyhow::bail!("that does not look like a complete xAI API key");
    }

    // Prefer macOS Keychain. If Keychain access is unavailable, use a local-only
    // 0600 fallback without putting the key in UserDefaults, logs, or source code.
    if set_generic_password(SERVICE, ACCOUNT, trimmed.as_bytes()).is_ok() {
        let _ = std::fs::remove_file(private_key_path());
        return Ok(());
    }

    let path = private_key_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(trimmed.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[allow(dead_code)]
pub fn delete() {
    let _ = delete_generic_password(SERVICE, ACCOUNT);
    let _ = std::fs::remove_file(private_key_path());
}
