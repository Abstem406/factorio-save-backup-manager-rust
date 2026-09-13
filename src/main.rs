//! Factorio Save Backup Manager - Rust + Slint edition.
//! Real implementations: Google Drive OAuth2 REST uploads/downloads, Discord
//! webhook notifications + bot downloads, link resolvers, config persistence.

// GUI-only on Windows: no console window is allocated on launch.
// (The in-app log pane replaces eprintln! output there.)
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

slint::include_modules!();

mod config;
mod i18n;
mod services;

use config::AppConfig;
use copypasta::ClipboardProvider;
use services::discord;
use services::gdrive::{self, has_stored_refresh_token, CloudFile, GDriveClient};
use services::resolver;
use slint::Model;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------- logging ---

const MAX_LOG_LINES: usize = 500;

#[derive(Clone)]
struct SharedLog {
    lines: Arc<Mutex<VecDeque<String>>>,
    dirty: Arc<Mutex<bool>>,
}

impl SharedLog {
    fn new() -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES))),
            dirty: Arc::new(Mutex::new(false)),
        }
    }

    fn add(&self, msg: &str) {
        let time = chrono::Local::now().format("%H:%M:%S");
        let mut lines = self.lines.lock().unwrap();
        lines.push_back(format!("[{time}] {msg}"));
        while lines.len() > MAX_LOG_LINES {
            lines.pop_front();
        }
        *self.dirty.lock().unwrap() = true;
        eprintln!("{msg}");
    }

    /// Returns true once if new lines arrived since the last check.
    fn take_dirty(&self) -> bool {
        let mut d = self.dirty.lock().unwrap();
        let v = *d;
        *d = false;
        v
    }

    fn get_text(&self) -> String {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// --------------------------------------------------------------- commands ---

enum Command {
    ForceCheck,
    SyncFromCloud,
    DownloadDiscord,
    GenerateTestSave,
    SaveSettings(Box<AppConfig>),
    AuthorizeGDrive(Box<AppConfig>),
    SetLanguage(String),
}

// ------------------------------------------------------------ backup core ---

struct BackupCore {
    log: SharedLog,
    config: Mutex<AppConfig>,
    save_path: PathBuf,
    last_hash: Mutex<Option<String>>,
    last_link: Mutex<String>,
    last_link_date: Mutex<String>,
    next_check: Mutex<SystemTime>,
}

impl BackupCore {
    fn new(log: SharedLog, config: AppConfig) -> Self {
        let save_path = config::get_save_path();
        Self {
            log,
            config: Mutex::new(config),
            save_path,
            last_hash: Mutex::new(None),
            last_link: Mutex::new(String::new()),
            last_link_date: Mutex::new("N/A".to_string()),
            // First auto-check happens after one full interval, not at startup
            next_check: Mutex::new(SystemTime::now() + Duration::from_secs(5 * 60)),
        }
    }

    fn set_next_check_in(&self, secs: u64) {
        *self.next_check.lock().unwrap() = SystemTime::now() + Duration::from_secs(secs);
    }

    fn seconds_until_next_check(&self) -> i64 {
        let next = *self.next_check.lock().unwrap();
        next.duration_since(SystemTime::now())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn interval_secs(&self) -> u64 {
        self.config.lock().unwrap().check_interval.max(1) * 60
    }

    fn format_backup_name(&self, original: &str) -> String {
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let mut base = original.trim_end_matches(".zip").to_string();

        // Strip an existing _YYYYMMDD_HHMMSS suffix
        if base.len() > 15 {
            let tail = &base[base.len() - 15..];
            if tail.starts_with('_') && tail[1..8].bytes().all(|b| b.is_ascii_digit()) {
                base = base[..base.len() - 15].to_string();
            }
        }

        let prefix = self
            .config
            .lock()
            .unwrap()
            .backup_prefix
            .clone()
            .unwrap_or_default();
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!(
                "{}_",
                prefix
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
                    .collect::<String>()
            )
        };

        format!("{prefix}{base}_{timestamp}.zip")
    }

    fn latest_save(&self) -> Option<(PathBuf, String)> {
        let entries = fs::read_dir(&self.save_path).ok()?;
        let mut saves: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("zip"))
            })
            .collect();
        saves.sort_by(|a, b| {
            let ta = a.metadata().and_then(|m| m.modified()).ok();
            let tb = b.metadata().and_then(|m| m.modified()).ok();
            tb.cmp(&ta)
        });
        let first = saves.first()?;
        Some((
            first.path(),
            first.file_name().to_string_lossy().to_string(),
        ))
    }

    fn list_saves(&self, max: usize) -> Vec<(PathBuf, String, f64)> {
        let entries = match fs::read_dir(&self.save_path) {
            Ok(e) => e,
            Err(_) => return vec![],
        };
        let mut saves: Vec<(PathBuf, String, f64)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map_or(false, |ext| ext.eq_ignore_ascii_case("zip"))
            })
            .map(|e| {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1024.0 / 1024.0;
                let name = e.file_name().to_string_lossy().to_string();
                (e.path(), name, size)
            })
            .collect();
        saves.sort_by(|a, b| {
            let ta = fs::metadata(&a.0).and_then(|m| m.modified()).ok();
            let tb = fs::metadata(&b.0).and_then(|m| m.modified()).ok();
            tb.cmp(&ta)
        });
        saves.truncate(max);
        saves
    }

    fn hash_of(&self, path: &Path) -> Option<String> {
        let mut file = fs::File::open(path).ok()?;
        let mut hasher = DefaultHasher::new();
        let mut buf = [0u8; 65536];
        loop {
            match std::io::Read::read(&mut file, &mut buf) {
                Ok(0) => break,
                Ok(n) => buf[..n].hash(&mut hasher),
                Err(_) => return None,
            }
        }
        Some(format!("{:x}", hasher.finish()))
    }

    fn gdrive_client(&self) -> Result<GDriveClient, String> {
        let creds_path = self
            .config
            .lock()
            .unwrap()
            .google_drive
            .as_ref()
            .and_then(|g| g.credentials_path.clone())
            .unwrap_or_else(|| "./credentials.json".to_string());
        GDriveClient::new(&creds_path)
    }

    fn gdrive_folder(&self) -> Option<String> {
        self.config
            .lock()
            .unwrap()
            .google_drive
            .as_ref()
            .and_then(|g| g.folder_id.clone())
            .filter(|s| !s.is_empty())
    }

    fn notify_discord(&self, file_name: &str, link: &str) {
        let (webhook, service) = {
            let cfg = self.config.lock().unwrap();
            match &cfg.discord_webhook {
                Some(w) if !w.is_empty() => (w.clone(), cfg.cloud_service.clone()),
                _ => return,
            }
        };
        match discord::send_notification(&webhook, file_name, link, &service) {
            Ok(()) => self.log.add(&i18n::t("discord-notification-sent")),
            Err(e) => self.log.add(&format!("⚠️ {} {e}", i18n::t("error-notifying-discord"))),
        }
    }

    fn upload_to_cloud(&self, file_path: &Path, remote_name: &str) -> Result<String, String> {
        let service = self.config.lock().unwrap().cloud_service.clone();
        match service.as_str() {
            "google-drive" => {
                let client = self.gdrive_client()?;
                let folder = self.gdrive_folder();
                client.upload_file(file_path, remote_name, folder.as_deref())
            }
            other => Err(format!("{}: {other}", i18n::t("unsupported-service"))),
        }
    }

    fn after_upload(&self, remote_name: &str, link: &str) {
        *self.last_link.lock().unwrap() = link.to_string();
        *self.last_link_date.lock().unwrap() =
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.log
            .add(&format!("✅ {}: {remote_name}", i18n::t("upload-successful")));
        self.log.add(&format!("🔗 Link: {link}"));
        self.notify_discord(remote_name, link);
    }

    fn local_backup_of(&self, path: &Path) {
        let dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("backups");
        if let Err(e) = fs::create_dir_all(&dir) {
            self.log
                .add(&format!("⚠️ {}: {e}", i18n::t("backups-dir-error")));
            return;
        }
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "save".into());
        let dest = dir.join(format!("{name}_localbackup_{stamp}.zip"));
        match fs::copy(path, &dest) {
            Ok(_) => {
                self.log.add(&format!(
                    "💾 {}: {}",
                    i18n::t("local-backup-created"),
                    dest.display()
                ));
                self.prune_local_backups(&dir);
            }
            Err(e) => self
                .log
                .add(&format!("⚠️ {}: {e}", i18n::t("local-backup-error"))),
        }
    }

    fn prune_local_backups(&self, dir: &Path) {
        let mut files: Vec<std::fs::DirEntry> = fs::read_dir(dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_string_lossy().contains("_localbackup_"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by(|a, b| {
            let ta = a.metadata().and_then(|m| m.modified()).ok();
            let tb = b.metadata().and_then(|m| m.modified()).ok();
            tb.cmp(&ta)
        });
        for f in files.iter().skip(5) {
            let _ = fs::remove_file(f.path());
        }
    }

    // ------------------------------------------------- real operations ------

    fn check_for_changes(&self) {
        let Some((path, name)) = self.latest_save() else {
            self.log.add(&i18n::t("no-saves-found"));
            return;
        };
        let Some(hash) = self.hash_of(&path) else {
            self.log.add(&i18n::t("could-not-read-save"));
            return;
        };
        if *self.last_hash.lock().unwrap() == Some(hash.clone()) {
            self.log.add(&i18n::t("no-changes"));
            return;
        }
        self.log
            .add(&format!("🔎 {}: {name}", i18n::t("change-detected")));
        let remote_name = self.format_backup_name(&name);
        match self.upload_to_cloud(&path, &remote_name) {
            Ok(link) => {
                self.after_upload(&remote_name, &link);
                *self.last_hash.lock().unwrap() = Some(hash);
            }
            Err(e) => self
                .log
                .add(&format!("❌ {}: {e}", i18n::t("upload-error"))),
        }
    }

    fn upload_specific(&self, path: &Path, name: &str) {
        let size_mb = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64 / 1024.0 / 1024.0;
        self.log.add(&format!(
            "📦 {}: {name} ({size_mb:.2} MB)...",
            i18n::t("uploading")
        ));
        let remote_name = self.format_backup_name(name);
        match self.upload_to_cloud(path, &remote_name) {
            Ok(link) => self.after_upload(&remote_name, &link),
            Err(e) => self
                .log
                .add(&format!("❌ {}: {e}", i18n::t("manual-upload-error"))),
        }
    }

    fn sync_from_cloud(&self) {
        self.log.add(&i18n::t("sync-looking"));
        let client = match self.gdrive_client() {
            Ok(c) => c,
            Err(e) => {
                self.log.add(&format!("❌ {e}"));
                return;
            }
        };
        let folder = self.gdrive_folder();
        let cloud_files = match client.list_backups(folder.as_deref(), 25) {
            Ok(f) => f,
            Err(e) => {
                self.log
                    .add(&format!("❌ {}: {e}", i18n::t("list-cloud-error")));
                return;
            }
        };
        if cloud_files.is_empty() {
            self.log.add(&i18n::t("no-saves-cloud"));
            return;
        }
        let latest = &cloud_files[0];
        let cloud_time = parse_drive_time(&latest.modified_time).unwrap_or(UNIX_EPOCH);
        let local_time = self
            .latest_save()
            .and_then(|(p, _)| fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .unwrap_or(UNIX_EPOCH);

        if cloud_time > local_time {
            self.log.add(&format!(
                "⚠️ {}: {} ({})",
                i18n::t("newer-save-cloud"),
                latest.name,
                chrono::DateTime::<chrono::Local>::from(cloud_time)
                    .format("%Y-%m-%d %H:%M:%S")
            ));
            if let Some((p, _)) = self.latest_save() {
                self.local_backup_of(&p);
            }
            let target = self.save_path.join(&latest.name);
            match client.download_file(&latest.id, &target) {
                Ok(()) => {
                    self.log.add(&format!(
                        "📥 {}: {}",
                        i18n::t("downloaded"),
                        target.display()
                    ));
                    if let Some(h) = self.hash_of(&target) {
                        *self.last_hash.lock().unwrap() = Some(h);
                    }
                    self.set_next_check_in(self.interval_secs());
                }
                Err(e) => self
                    .log
                    .add(&format!("❌ {}: {e}", i18n::t("download-error"))),
            }
        } else {
            self.log.add(&i18n::t("local-up-to-date"));
        }
    }

    fn list_cloud_files(&self) -> Result<Vec<CloudFile>, String> {
        let client = self.gdrive_client()?;
        let folder = self.gdrive_folder();
        client.list_backups(folder.as_deref(), 25)
    }

    fn download_cloud_file(&self, file: &CloudFile) {
        self.log.add(&format!(
            "📥 {}: {}...",
            i18n::t("downloading-cloud"),
            file.name
        ));
        if let Some((p, _)) = self.latest_save() {
            self.local_backup_of(&p);
        }
        let client = match self.gdrive_client() {
            Ok(c) => c,
            Err(e) => {
                self.log.add(&format!("❌ {e}"));
                return;
            }
        };
        let target = self.save_path.join(&file.name);
        match client.download_file(&file.id, &target) {
            Ok(()) => {
                let size =
                    fs::metadata(&target).map(|m| m.len()).unwrap_or(0) as f64 / 1024.0 / 1024.0;
                self.log.add(&format!(
                    "✅ {}: {} ({size:.2} MB)",
                    i18n::t("download-complete"),
                    target.display()
                ));
            }
            Err(e) => self
                .log
                .add(&format!("❌ {}: {e}", i18n::t("download-error"))),
        }
    }

    fn download_from_discord(&self) {
        self.log.add(&i18n::t("discord-looking"));
        let (token, channel) = {
            let cfg = self.config.lock().unwrap();
            (
                cfg.discord_bot_token.clone().unwrap_or_default(),
                cfg.discord_channel_id.clone().unwrap_or_default(),
            )
        };
        if token.is_empty() || channel.is_empty() {
            self.log.add(&i18n::t("discord-not-configured"));
            return;
        }
        let latest = match discord::get_latest_backup_url(&token, &channel) {
            Ok(l) => l,
            Err(e) => {
                self.log.add(&format!("❌ {e}"));
                return;
            }
        };
        self.log.add(&format!(
            "🔎 {}: {} ({})",
            i18n::t("latest-backup"),
            latest.file_name,
            latest.timestamp
        ));

        let target = self.save_path.join(&latest.file_name);

        // Google Drive links: the webhook embed carries the webViewLink
        // (.../file/d/<ID>/view), which only serves an HTML page. Extract the
        // file ID and use the authenticated Drive download API instead.
        if let Some(file_id) = extract_drive_file_id(&latest.url) {
            self.log.add(&format!(
                "🌐 {}: Google Drive (file {file_id})",
                i18n::t("final-url")
            ));
            let client = match self.gdrive_client() {
                Ok(c) => c,
                Err(e) => {
                    self.log.add(&format!("❌ {e}"));
                    return;
                }
            };
            match client.download_file(&file_id, &target) {
                Ok(()) => {
                    let size = fs::metadata(&target).map(|m| m.len()).unwrap_or(0) as f64
                        / 1024.0
                        / 1024.0;
                    self.log.add(&format!(
                        "✅ {}: {} ({size:.2} MB)",
                        i18n::t("download-complete"),
                        target.display()
                    ));
                }
                Err(e) => self
                    .log
                    .add(&format!("❌ {}: {e}", i18n::t("download-error"))),
            }
            return;
        }

        // Other services: resolve landing pages into direct URLs first.
        self.log.add(&format!(
            "🔗 {}: {}",
            i18n::t("resolving-link"),
            latest.url
        ));
        let direct = resolver::resolve_direct_link(&latest.url);
        self.log
            .add(&format!("🌐 {}: {direct}", i18n::t("final-url")));

        match discord::download_to_file(&direct, &target, &latest.file_name) {
            Ok(bytes) => {
                let size = bytes as f64 / 1024.0 / 1024.0;
                self.log.add(&format!(
                    "✅ {}: {} ({size:.2} MB)",
                    i18n::t("download-complete"),
                    target.display()
                ));
            }
            Err(e) => self
                .log
                .add(&format!("❌ {}: {e}", i18n::t("download-error"))),
        }
    }

    fn generate_test_save(&self) {
        if let Err(e) = fs::create_dir_all(&self.save_path) {
            self.log
                .add(&format!("❌ {}: {e}", i18n::t("folder-create-error")));
            return;
        }
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let test = self.save_path.join(format!("_autosave-test_{stamp}.zip"));
        match fs::write(&test, format!("Factorio test save - {}", chrono::Local::now())) {
            Ok(()) => {
                self.log.add(&format!(
                    "🧪 {}: {}",
                    i18n::t("test-save-created"),
                    test.display()
                ));
                self.log.add(&i18n::t("test-save-note"));
            }
            Err(e) => self
                .log
                .add(&format!("❌ {}: {e}", i18n::t("test-save-error"))),
        }
    }
}

/// Extract the file ID from a Google Drive link (webViewLink
/// `.../file/d/<ID>/view` or `...?id=<ID>` style URLs). Returns None for
/// non-Google URLs.
fn extract_drive_file_id(url: &str) -> Option<String> {
    let is_drive = url.contains("drive.google.com") || url.contains("docs.google.com");
    if !is_drive {
        return None;
    }
    if let Some(rest) = url.split("/file/d/").nth(1) {
        let id = rest.split('/').next()?;
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    if let Some(rest) = url.split("id=").nth(1) {
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

fn parse_drive_time(s: &str) -> Option<SystemTime> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(dt.timestamp_millis() as u64))
}

// ------------------------------------------------------------- language ----

/// Weak handle to the main window, used by the language combo callback to
/// retranslate the UI instantly (a callback cannot also capture `window`).
static WINDOW_FOR_LANG: Mutex<Option<slint::Weak<MainWindow>>> = Mutex::new(None);

// ------------------------------------------------------------------- main ---

/// Push every translation of the given language into the I18n Slint global.
/// Texts come from the `ui` section of `i18n/<lang>.json`.
fn apply_i18n_to_ui(window: &MainWindow, lang_idx: i32) {
    let lang = if lang_idx == 1 { "es" } else { "en" };
    i18n::set_language(lang);
    let m = i18n::load_section(lang, "ui");
    let s = |k: &str| -> slint::SharedString {
        m.get(k).cloned().unwrap_or_default().into()
    };
    let g = window.global::<I18n>();
    g.set_lang_index(lang_idx);

    g.set_app_title(s("app-title"));
    g.set_status_panel(s("status-panel"));
    g.set_tools_panel(s("tools-panel"));
    g.set_console_panel(s("console-panel"));
    g.set_working(s("working"));
    g.set_service(s("service"));
    g.set_interval_label(s("interval-label"));
    g.set_mins(s("mins"));
    g.set_save_path(s("save-path"));
    g.set_last_link(s("last-link"));
    g.set_link_date(s("link-date"));
    g.set_no_backups(s("no-backups"));
    g.set_copy(s("copy"));
    g.set_next_check(s("next-check"));
    g.set_force_check(s("force-check"));
    g.set_manual_upload(s("manual-upload"));
    g.set_sync_cloud(s("sync-cloud"));
    g.set_download_drive(s("download-drive"));
    g.set_download_discord(s("download-discord"));
    g.set_test_save(s("test-save"));
    g.set_settings(s("settings"));
    g.set_footer(s("footer"));
    g.set_picker_cloud_title(s("picker-cloud-title"));
    g.set_picker_local_title(s("picker-local-title"));
    g.set_cancel(s("cancel"));
    g.set_download_b(s("download-b"));
    g.set_upload_b(s("upload-b"));
    g.set_settings_title(s("settings-title"));
    g.set_general_sec(s("general-sec"));
    g.set_discord_sec(s("discord-sec"));
    g.set_gdrive_sec(s("gdrive-sec"));
    g.set_lang_label(s("lang-label"));
    g.set_check_interval(s("check-interval"));
    g.set_backup_prefix(s("backup-prefix"));
    g.set_prefix_ph(s("prefix-ph"));
    g.set_obfuscate_cb(s("obfuscate-cb"));
    g.set_webhook(s("webhook"));
    g.set_bot_token(s("bot-token"));
    g.set_token_ph(s("token-ph"));
    g.set_channel_id(s("channel-id"));
    g.set_channel_ph(s("channel-ph"));
    g.set_creds_path(s("creds-path"));
    g.set_folder_id(s("folder-id"));
    g.set_authorize(s("authorize"));
    g.set_authorize_wait(s("authorize-wait"));
    g.set_listing_cloud(s("listing-cloud"));
    g.set_gd_no_creds_title(s("gd-no-creds-title"));
    g.set_gd_no_creds_body(s("gd-no-creds-body"));
    g.set_gd_creds_title(s("gd-creds-title"));
    g.set_gd_creds_body(s("gd-creds-body"));
    g.set_gd_ready_title(s("gd-ready-title"));
    g.set_gd_ready_body(s("gd-ready-body"));
    g.set_save_b(s("save-b"));

    g.set_tip_copy(s("tip-copy"));
    g.set_tip_force_check(s("tip-force-check"));
    g.set_tip_manual_upload(s("tip-manual-upload"));
    g.set_tip_sync_cloud(s("tip-sync-cloud"));
    g.set_tip_download_drive(s("tip-download-drive"));
    g.set_tip_download_discord(s("tip-download-discord"));
    g.set_tip_test_save(s("tip-test-save"));
    g.set_tip_settings(s("tip-settings"));
    g.set_tip_next_check(s("tip-next-check"));
    g.set_tip_interval(s("tip-interval"));
    g.set_tip_prefix(s("tip-prefix"));
    g.set_tip_language(s("tip-language"));
    g.set_tip_obfuscate(s("tip-obfuscate"));
    g.set_tip_webhook(s("tip-webhook"));
    g.set_tip_channel(s("tip-channel"));
    g.set_tip_bot(s("tip-bot"));
    g.set_tip_creds(s("tip-creds"));
    g.set_tip_folder(s("tip-folder"));
    g.set_tip_authorize(s("tip-authorize"));
    g.set_tip_save(s("tip-save"));
    g.set_tip_cancel(s("tip-cancel"));

    // Language names are NOT translated; the model is set as a default in
    // ui/i18n.slint so it is never swapped at runtime.
}

fn main() {
    let window = MainWindow::new().unwrap();
    *WINDOW_FOR_LANG.lock().unwrap() = Some(window.as_weak());

    // El fondo de la consola va incrustado en el binario (ver console-bg en
    // main_window.slint), no se carga desde el disco en runtime.

    let log = SharedLog::new();

    let cfg = AppConfig::load().unwrap_or_default();
    let lang = cfg.ui_language().to_string();
    i18n::set_language(&lang);
    log.add(&format!(
        "{}: {} {}, {} {} min",
        i18n::t("config-loaded"),
        i18n::t("service-word"),
        cfg.cloud_service,
        i18n::t("interval-word"),
        cfg.check_interval
    ));

    let core = Arc::new(BackupCore::new(log.clone(), cfg.clone()));

    // ------------------------------------------------- UI initial values ----
    let lang_idx = if lang == "es" { 1 } else { 0 };
    apply_i18n_to_ui(&window, lang_idx);
    window.set_last_link(i18n::t("no-backups").into());
    window.set_cloud_service(cfg.cloud_service.clone().into());
    window.set_check_interval(cfg.check_interval as i32);
    window.set_save_path(core.save_path.to_string_lossy().to_string().into());
    window.set_interval_index(interval_to_index(cfg.check_interval));
    window.set_settings_lang_index(lang_idx);
    sync_settings_fields(&window, &cfg);
    core.set_next_check_in(cfg.check_interval.max(1) * 60);

    let (tx, rx) = mpsc::channel::<Command>();
    let rx = Arc::new(Mutex::new(rx));

    // ------------------------------------------------- worker thread --------
    {
        let core = Arc::clone(&core);
        let rx = Arc::clone(&rx);
        let w = window.as_weak();
        std::thread::spawn(move || loop {
            // Drain all pending commands (each may take a while: uploads run
            // here, never on the UI thread).
            loop {
                let next = rx.lock().unwrap().try_recv();
                match next {
                    Ok(cmd) => run_command(&core, cmd, &w),
                    Err(_) => break,
                }
            }

            // Periodic auto backup
            if core.seconds_until_next_check() <= 0 {
                core.set_next_check_in(core.interval_secs());
                core.check_for_changes();
            }

            std::thread::sleep(Duration::from_millis(500));
        });
    }

    // ------------------------------------------- UI pump (log + countdown) --
    let timer = slint::Timer::default();
    {
        let log_ui = log.clone();
        let core_ui = Arc::clone(&core);
        let w = window.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(300),
            move || {
                let Some(ui) = w.upgrade() else { return };
                if log_ui.take_dirty() {
                    ui.set_log_text(log_ui.get_text().into());
                }
                ui.set_countdown_seconds(core_ui.seconds_until_next_check().max(0) as i32);
                let secs = core_ui.seconds_until_next_check().max(0) as i64;
                let text = if secs <= 0 {
                    format!("{}: --:--", i18n::t("next-check"))
                } else {
                    format!(
                        "{}: {:02}:{:02}",
                        i18n::t("next-check"),
                        secs / 60,
                        secs % 60
                    )
                };
                ui.set_countdown_text(text.into());
                ui.set_busy(false);
                let link = core_ui.last_link.lock().unwrap().clone();
                let has = link.starts_with("http");
                ui.set_has_link(has);
                if has {
                    ui.set_last_link(link.into());
                }
                let date = core_ui.last_link_date.lock().unwrap().clone();
                ui.set_link_date(date.into());
            },
        );
    }

    // ------------------------------------------------- button callbacks -----
    macro_rules! simple_command {
        ($cb:ident, $cmd:expr) => {{
            let tx = tx.clone();
            window.$cb(move || {
                let _ = tx.send($cmd);
            });
        }};
    }
    simple_command!(on_force_check_clicked, Command::ForceCheck);
    simple_command!(on_sync_from_cloud_clicked, Command::SyncFromCloud);
    simple_command!(on_download_discord_clicked, Command::DownloadDiscord);
    simple_command!(on_generate_test_save_clicked, Command::GenerateTestSave);

    {
        // Download from Drive: fetch the list, open the picker, download on confirm
        let core = Arc::clone(&core);
        let w = window.as_weak();
        window.on_download_gdrive_clicked(move || {
            let Some(ui) = w.upgrade() else { return };
            ui.set_busy(true);
            ui.set_busy_text(i18n::t("listing-cloud").into());
            let core = Arc::clone(&core);
            let w = w.clone();
            std::thread::spawn(move || match core.list_cloud_files() {
                Ok(files) if files.is_empty() => {
                    core.log.add(&i18n::t("no-cloud-backups"));
                }
                Ok(files) => {
                    core.log.add(&format!(
                        "☁️ {}: {}.",
                        i18n::t("saves-found-cloud"),
                        files.len()
                    ));
                    let names: Vec<slint::SharedString> = files
                        .iter()
                        .map(|f| {
                            format!(
                                "{} ({:.2} MB, {})",
                                f.name,
                                f.size_mb(),
                                f.modified_time
                            )
                            .into()
                        })
                        .collect();
                    w.upgrade_in_event_loop(move |ui| {
                        ui.set_picker_model(slint::ModelRc::new(slint::VecModel::from(
                            names,
                        )));
                        ui.set_picker_index(0);
                        ui.set_picker_kind(0);
                        ui.set_picker_title(ui.global::<I18n>().get_picker_cloud_title().into());
                        ui.set_picker_visible(true);
                    })
                    .ok();
                }
                Err(e) => core.log.add(&format!("❌ {e}")),
            });
        });
    }

    {
        // Manual upload: list local saves, pick one, upload it
        let core = Arc::clone(&core);
        let w = window.as_weak();
        window.on_manual_upload_clicked(move || {
            let Some(ui) = w.upgrade() else { return };
            let saves = core.list_saves(20);
            if saves.is_empty() {
                core.log.add(&i18n::t("no-local-saves"));
                return;
            }
            let names: Vec<slint::SharedString> = saves
                .iter()
                .map(|(_, n, mb)| format!("{n} ({mb:.2} MB)").into())
                .collect();
            ui.set_picker_model(slint::ModelRc::new(slint::VecModel::from(names)));
            ui.set_picker_index(0);
            ui.set_picker_kind(1);
            ui.set_picker_title(ui.global::<I18n>().get_picker_local_title().into());
            ui.set_picker_visible(true);
        });
    }

    {
        // Picker confirm: kind 0 = download cloud file, kind 1 = upload local save
        let core = Arc::clone(&core);
        let w = window.as_weak();
        window.on_picker_download_clicked(move || {
            let Some(ui) = w.upgrade() else { return };
            let idx = ui.get_picker_index().max(0) as usize;
            let kind = ui.get_picker_kind();
            let model: Vec<String> = ui
                .get_picker_model()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let name = model
                .get(idx)
                .map(|s| s.split(" (").next().unwrap_or(s).to_string())
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let core = Arc::clone(&core);
            if kind == 0 {
                std::thread::spawn(move || match core.list_cloud_files() {
                    Ok(files) => {
                        if let Some(f) = files.iter().find(|f| f.name == name) {
                            core.download_cloud_file(f);
                        } else {
                            core.log.add(&format!(
                                "❌ {}: {name}",
                                i18n::t("not-found-cloud")
                            ));
                        }
                    }
                    Err(e) => core.log.add(&format!("❌ {e}")),
                });
            } else {
                let path = core.save_path.join(&name);
                std::thread::spawn(move || core.upload_specific(&path, &name));
            }
        });
    }

    {
        // copy last link to clipboard
        let core = Arc::clone(&core);
        window.on_copy_last_link_clicked(move || {
            let link = core.last_link.lock().unwrap().clone();
            if link.starts_with("http") {
                if let Ok(mut cb) = copypasta::ClipboardContext::new() {
                    let _ = cb.set_contents(link);
                }
                core.log.add(&i18n::t("link-copied"));
            }
        });
    }

    {
        let core = Arc::clone(&core);
        let w = window.as_weak();
        window.on_open_settings_clicked(move || {
            let Some(ui) = w.upgrade() else { return };
            // Re-sync fields so the Google Drive status box and language
            // selection always reflect the current (possibly unsaved) state.
            let cfg = collect_settings(&ui);
            ui.set_gdrive_state(gdrive_state_for(&cfg));
            let lang_idx = if core_config_lang(&core) == "es" { 1 } else { 0 };
            // Revert the live UI language to the saved one (Cancel = revert).
            apply_i18n_to_ui(&ui, lang_idx);
            ui.set_settings_lang_index(lang_idx);
            ui.set_settings_visible(true);
            ui.set_settings_status("".into());
        });
    }

    {
        // Settings dialog Save button.
        let core = Arc::clone(&core);
        let tx = tx.clone();
        let w = window.as_weak();
        window.on_settings_saved(move || {
            let Some(ui) = w.upgrade() else { return };
            let cfg = collect_settings(&ui);
            core.log.add(&i18n::t("settings-saved"));
            ui.set_settings_visible(false);
            let _ = tx.send(Command::SaveSettings(Box::new(cfg)));
        });
    }

    {
        // Settings dialog Cancel button: revert the live language preview.
        let core = Arc::clone(&core);
        let w = window.as_weak();
        window.on_settings_canceled(move || {
            let Some(ui) = w.upgrade() else { return };
            let lang_idx = if core_config_lang(&core) == "es" { 1 } else { 0 };
            apply_i18n_to_ui(&ui, lang_idx);
            ui.set_settings_lang_index(lang_idx);
        });
    }

    {
        // Language combo box: apply the translation immediately (same UI tick),
        // then tell the worker so logs use the new language too. The change only
        // becomes permanent when the user presses Save.
        let tx = tx.clone();
        window.on_language_changed(move |idx| {
            let lang = if idx == 1 { "es" } else { "en" };
            // Skip no-op changes (e.g. programmatic reverts at startup/cancel).
            if lang == i18n::language() {
                return;
            }
            // Instant visual feedback: retranslate every string right now.
            if let Some(ui) = WINDOW_FOR_LANG.lock().unwrap().as_ref().and_then(|w| w.upgrade()) {
                apply_i18n_to_ui(&ui, idx);
            }
            let _ = tx.send(Command::SetLanguage(lang.to_string()));
        });
    }

    {
        let tx = tx.clone();
        let w = window.as_weak();
        window.on_authorize_gdrive_clicked(move || {
            let Some(ui) = w.upgrade() else { return };
            ui.set_settings_status(i18n::t("authorize-wait").into());
            let cfg = collect_settings(&ui);
            let _ = tx.send(Command::AuthorizeGDrive(Box::new(cfg)));
        });
    }

    window.run().unwrap();
}

// ------------------------------------------------------- helper functions ---

/// Current language stored in the live config ("en" | "es").
fn core_config_lang(core: &Arc<BackupCore>) -> String {
    core.config.lock().unwrap().ui_language().to_string()
}

/// Compute the Google Drive status shown in Settings:
/// 0 = no credentials found, 1 = path set but file missing,
/// 2 = credentials file found, 3 = found + OAuth already authorized.
/// An empty field auto-detects `credentials.json` (exe dir, AppImage bundle
/// dir, or cwd) so users don't have to type "./credentials.json".
fn gdrive_state_for(cfg: &AppConfig) -> i32 {
    let field = cfg
        .google_drive
        .as_ref()
        .and_then(|g| g.credentials_path.clone())
        .unwrap_or_default();
    let field_is_empty = field.trim().is_empty();
    let candidate = if field_is_empty {
        "credentials.json"
    } else {
        field.as_str()
    };
    if !gdrive::resolve_credentials_path(candidate).exists() {
        return if field_is_empty { 0 } else { 1 };
    }
    if has_stored_refresh_token() { 3 } else { 2 }
}

fn interval_to_index(mins: u64) -> i32 {
    match mins {
        0..=1 => 0,
        2 => 1,
        3..=5 => 2,
        6..=10 => 3,
        11..=15 => 4,
        16..=30 => 5,
        _ => 6,
    }
}

fn index_to_interval(i: i32) -> u64 {
    [1u64, 2, 5, 10, 15, 30, 60]
        .get(i.max(0) as usize)
        .copied()
        .unwrap_or(5)
}

fn sync_settings_fields(window: &MainWindow, cfg: &AppConfig) {
    window.set_settings_prefix(cfg.backup_prefix.clone().unwrap_or_default().into());
    window.set_settings_obfuscate(cfg.obfuscate_secrets);
    window.set_settings_webhook(cfg.discord_webhook.clone().unwrap_or_default().into());
    window.set_settings_bot_token(cfg.discord_bot_token.clone().unwrap_or_default().into());
    window.set_settings_channel_id(cfg.discord_channel_id.clone().unwrap_or_default().into());
    let gdrive = cfg.google_drive.clone().unwrap_or_default();
    window.set_settings_gdrive_creds(gdrive.credentials_path.unwrap_or_default().into());
    window.set_settings_gdrive_folder(gdrive.folder_id.unwrap_or_default().into());
}

fn collect_settings(ui: &MainWindow) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.cloud_service = "google-drive".to_string();
    cfg.check_interval = index_to_interval(ui.get_interval_index());
    let prefix = ui.get_settings_prefix().to_string();
    cfg.backup_prefix = if prefix.is_empty() { None } else { Some(prefix) };
    cfg.obfuscate_secrets = ui.get_settings_obfuscate();

    let webhook = ui.get_settings_webhook().to_string();
    cfg.discord_webhook = if webhook.is_empty() { None } else { Some(webhook) };

    let bot = ui.get_settings_bot_token().to_string();
    cfg.discord_bot_token = if bot.is_empty() { None } else { Some(bot) };

    let chan = ui.get_settings_channel_id().to_string();
    cfg.discord_channel_id = if chan.is_empty() { None } else { Some(chan) };

    let mut g = config::GoogleDriveConfig::default();
    let creds = ui.get_settings_gdrive_creds().to_string();
    g.credentials_path = if creds.is_empty() { None } else { Some(creds) };
    let folder = ui.get_settings_gdrive_folder().to_string();
    g.folder_id = if folder.is_empty() { None } else { Some(folder) };
    cfg.google_drive = Some(g);
    cfg.language = if ui.get_settings_lang_index() == 1 {
        "es".to_string()
    } else {
        "en".to_string()
    };
    cfg
}

fn run_command(core: &Arc<BackupCore>, cmd: Command, w: &slint::Weak<MainWindow>) {
    match cmd {
        Command::ForceCheck => {
            core.log.add(&i18n::t("force-check-started"));
            core.check_for_changes();
            core.set_next_check_in(core.interval_secs());
        }
        Command::SyncFromCloud => core.sync_from_cloud(),
        Command::DownloadDiscord => core.download_from_discord(),
        Command::GenerateTestSave => core.generate_test_save(),
        Command::SaveSettings(cfg) => {
            cfg.save();
            let interval = cfg.check_interval;
            let lang = cfg.ui_language().to_string();
            *core.config.lock().unwrap() = *cfg;
            i18n::set_language(&lang);
            core.set_next_check_in(interval.max(1) * 60);
            core.log.add(&i18n::t("settings-applied"));
        }
        Command::AuthorizeGDrive(cfg) => {
            let creds_path = cfg
                .google_drive
                .as_ref()
                .and_then(|g| g.credentials_path.clone())
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| "credentials.json".into());
            let (state, status) = match GDriveClient::new(&creds_path) {
                Ok(client) => match client.authorize() {
                    Ok(()) => {
                        core.log.add(&i18n::t("gdrive-authorized"));
                        let c = *cfg;
                        c.save();
                        *core.config.lock().unwrap() = c.clone();
                        (gdrive_state_for(&c), i18n::t("gdrive-authorized"))
                    }
                    Err(e) => {
                        let msg = format!("❌ {}: {e}", i18n::t("authorize-failed"));
                        core.log.add(&msg.clone());
                        (gdrive_state_for(&cfg), msg)
                    }
                },
                Err(e) => {
                    let msg = format!("❌ {e}");
                    core.log.add(&msg.clone());
                    (gdrive_state_for(&cfg), msg)
                }
            };
            // Update the settings dialog from the UI thread: refresh the
            // Google Drive status box and replace the "authorize-wait" label
            // with the real result (success or the actual error).
            let w = w.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = w.upgrade() {
                    ui.set_gdrive_state(state);
                    ui.set_settings_status(status.into());
                }
            });
        }
        Command::SetLanguage(lang) => {
            i18n::set_language(&lang);
            core.log.add(&i18n::t(if lang == "es" {
                "language-changed-es"
            } else {
                "language-changed-en"
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_drive_file_id_from_webview_link() {
        assert_eq!(
            extract_drive_file_id(
                "https://drive.google.com/file/d/1AbC_dEf-123/view?usp=sharing"
            ),
            Some("1AbC_dEf-123".to_string())
        );
    }

    #[test]
    fn extracts_drive_file_id_from_id_param() {
        assert_eq!(
            extract_drive_file_id("https://drive.google.com/uc?export=download&id=XYZ_456"),
            Some("XYZ_456".to_string())
        );
    }

    #[test]
    fn ignores_non_drive_links() {
        assert_eq!(extract_drive_file_id("https://buzzheavier.com/abc.zip"), None);
        assert_eq!(extract_drive_file_id("https://example.com/file/d/123/view"), None);
    }
}
