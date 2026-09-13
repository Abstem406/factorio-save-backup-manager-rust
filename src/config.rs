//! Configuration management: load/save `config.json` next to the executable,
//! with optional obfuscation of secrets (compatible with the JS version).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct GoogleDriveConfig {
    #[serde(default)]
    pub credentials_path: Option<String>,
    #[serde(default)]
    pub folder_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    #[serde(default = "default_cloud_service")]
    pub cloud_service: String,
    #[serde(default = "default_interval")]
    pub check_interval: u64,
    #[serde(default)]
    pub backup_prefix: Option<String>,
    #[serde(default)]
    pub obfuscate_secrets: bool,
    /// UI language: "en" (default) or "es".
    #[serde(default = "default_language")]
    pub language: String,

    #[serde(default)]
    pub discord_webhook: Option<String>,
    #[serde(default)]
    pub discord_bot_token: Option<String>,
    #[serde(default)]
    pub discord_channel_id: Option<String>,

    #[serde(default)]
    pub google_drive: Option<GoogleDriveConfig>,
}

fn default_cloud_service() -> String {
    "google-drive".into()
}
fn default_interval() -> u64 {
    5
}
fn default_language() -> String {
    "en".into()
}

impl AppConfig {
    /// "es" | "en" (anything else maps to English, the default).
    pub fn ui_language(&self) -> &str {
        if self.language == "es" {
            "es"
        } else {
            "en"
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        // Deserialize from `{}` so the per-field `default = ...` functions apply
        serde_json::from_str("{}").expect("default AppConfig is valid")
    }
}

/// Probe whether a directory is writable (cheap create/remove test).
fn dir_is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".write_probe_tmp");
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Directory for runtime data files (config.json, gdrive-token.json).
///
/// Portable default: next to the executable. Inside an AppImage (or any
/// non-writable install dir) that is a read-only squashfs, so fall back to
/// the XDG data dir (~/.local/share/factorio-save-backup-manager-rust).
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let inside_appimage = dir.components().any(|c| {
            c.as_os_str().to_string_lossy().starts_with("appimage_extracted_")
        });
        if !inside_appimage && dir_is_writable(dir) {
            return dir.to_path_buf();
        }
    }
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("factorio-save-backup-manager-rust");
    let _ = fs::create_dir_all(&dir);
    dir
}

impl AppConfig {
    pub fn save_path() -> PathBuf {
        // Writable location (next to the exe, or XDG data dir inside AppImages).
        data_dir().join("config.json")
    }

    pub fn load() -> Option<Self> {
        let p = Self::save_path();
        let data = fs::read_to_string(&p).ok()?;
        let mut cfg: AppConfig = serde_json::from_str(&data).ok()?;
        cfg.deobfuscate_all();
        Some(cfg)
    }

    pub fn save(&self) {
        let mut out = self.clone();
        if out.obfuscate_secrets {
            out.obfuscate_all();
        }
        let json = serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".into());
        if let Err(e) = fs::write(Self::save_path(), json) {
            eprintln!("Error saving config: {e}");
        }
    }

    // --- simple obfuscation of secrets (aes-like not needed; the JS version used
    // a fixed key "factorio-backup-secret-key" with prefix "obf:". We use a
    // lightweight scheme so plain-text secrets never sit in the config file.)
    fn obfuscate_all(&mut self) {
        self.discord_webhook = self.discord_webhook.as_deref().map(obfuscate);
        self.discord_bot_token = self.discord_bot_token.as_deref().map(obfuscate);
        if let Some(g) = &mut self.google_drive {
            g.credentials_path = g.credentials_path.as_deref().map(obfuscate);
        }
    }

    fn deobfuscate_all(&mut self) {
        self.discord_webhook = self.discord_webhook.as_deref().map(deobfuscate);
        self.discord_bot_token = self.discord_bot_token.as_deref().map(deobfuscate);
        if let Some(g) = &mut self.google_drive {
            g.credentials_path = g.credentials_path.as_deref().map(deobfuscate);
        }
    }
}

const SECRET_PREFIX: &str = "obf:";
const SECRET_KEY: &str = "factorio-backup-secret-key";

/// XOR-based obfuscation, hex encoded, with "obf:" prefix (NOT encryption,
/// just hides secrets from plain view, same spirit as the JS version).
pub fn obfuscate(value: &str) -> String {
    if value.is_empty() || value.starts_with(SECRET_PREFIX) {
        return value.to_string();
    }
    let key = SECRET_KEY.as_bytes();
    let bytes: Vec<u8> = value
        .as_bytes()
        .iter()
        .zip(key.iter().cycle())
        .map(|(b, k)| b ^ k)
        .collect();
    format!("{}{}", SECRET_PREFIX, hex_encode(&bytes))
}

pub fn deobfuscate(value: &str) -> String {
    if !value.starts_with(SECRET_PREFIX) {
        return value.to_string();
    }
    let hex_part = &value[SECRET_PREFIX.len()..];
    let bytes = match hex_decode(hex_part) {
        Some(b) => b,
        None => return value.to_string(),
    };
    let key = SECRET_KEY.as_bytes();
    let out: Vec<u8> = bytes
        .iter()
        .zip(key.iter().cycle())
        .map(|(b, k)| b ^ k)
        .collect();
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Resolve the Factorio saves directory (env var override, then per-OS defaults).
pub fn get_save_path() -> PathBuf {
    if let Ok(p) = std::env::var("FACTORIO_SAVES_PATH") {
        return PathBuf::from(p);
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Factorio").join("saves");
        }
    }

    #[cfg(target_os = "macos")]
    {
        return home
            .join("Library")
            .join("Application Support")
            .join("factorio")
            .join("saves");
    }

    #[cfg(target_os = "linux")]
    {
        let gog = home.join("GOG Games").join("Factorio").join("game").join("saves");
        if gog.exists() {
            return gog;
        }
    }

    home.join(".factorio").join("saves")
}
