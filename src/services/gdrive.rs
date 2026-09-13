//! Real Google Drive integration using OAuth2 installed-app flow with a local
//! HTTP redirect server (same approach as the JS version: credentials.json +
//! gdrive-token.json stored next to the executable).

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Chain, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// --------------------------------------------------------------------------
// Embedded assets (served by the local OAuth callback server).
//
// These are embedded at compile time so the whole authorization flow stays
// offline-capable: the callback page and both Factorio images travel inside
// the binary and do not require a separate assets directory at runtime.
// --------------------------------------------------------------------------

/// The callback HTML page shown after a successful Google Drive authorization.
/// Static assets (images) are embedded as base64 data URIs so the page can
/// render fully offline.
const CALLBACK_HTML: &str = include_str!("../../callback.html");

/// Embedded copy of the Factorio logo used in the callback page.
const FACTORIO_CHAD_PNG: &[u8] = include_bytes!("../../factorio_chad.png");

/// Embedded copy of the background image used in the callback page.
const FACTORIO_CHAD_BG_PNG: &[u8] = include_bytes!("../../factorio_chad_bg.png");

/// Encode a byte slice as a standard base64 string, suitable for use in a
/// `data:` URI (e.g. `data:image/png;base64,<encoded>`).
fn base64_embed(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Write a simple HTTP response to the given stream.
/// The body may be text or binary (used for the images).
fn respond(stream: &mut std::net::TcpStream, status: u16, content_type: &str, body: &[u8]) -> bool {
    let status_text = match status {
        200 => "200 OK",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        status = status,
        status_text = status_text,
        content_type = content_type,
        len = body.len(),
    );
    let written = stream.write_all(response.as_bytes()).is_ok()
        && stream.write_all(body).is_ok()
        && stream.flush().is_ok();
    written
}


const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const SCOPES: &str = "https://www.googleapis.com/auth/drive.file";

pub struct GDriveClient {
    http: reqwest::blocking::Client,
    client_id: String,
    client_secret: String,
    token_path: PathBuf,
    access_token: Mutex<Option<String>>,
    refresh_token: Mutex<Option<String>>,
    token_expiry: Mutex<Option<Instant>>,
}

#[derive(Deserialize)]
struct ClientKeys {
    installed: Option<ClientCreds>,
    web: Option<ClientCreds>,
}

#[derive(Deserialize)]
struct ClientCreds {
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize, Clone)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Resolve a (possibly relative) credentials path: next to the executable
/// first, then cwd-relative (like the JS version).
pub fn resolve_credentials_path(credentials_path: &str) -> PathBuf {
    if Path::new(credentials_path).is_absolute() {
        return PathBuf::from(credentials_path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(credentials_path);
            if p.exists() {
                return p;
            }
        }
    }
    // Fall back to cwd-relative (like the JS version)
    std::env::current_dir()
        .map(|d| d.join(credentials_path))
        .unwrap_or_else(|_| PathBuf::from(credentials_path))
}

/// Where the OAuth token file is stored (next to the executable).
pub fn token_file_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("gdrive-token.json");
        }
    }
    PathBuf::from("gdrive-token.json")
}

/// True if a stored OAuth refresh token exists (i.e. the app was already
/// authorized at least once). Used to show accurate status in Settings.
pub fn has_stored_refresh_token() -> bool {
    fs::read_to_string(token_file_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("refresh_token")
                .and_then(|t| t.as_str())
                .map(|t| !t.is_empty())
        })
        .unwrap_or(false)
}

impl GDriveClient {
    /// Load credentials.json (OAuth 2.0 Client ID - Desktop app) and any saved tokens.
    pub fn new(credentials_path: &str) -> Result<Self, String> {
        let resolved = resolve_credentials_path(credentials_path);

        if !resolved.exists() {
            return Err(format!(
                "Google Drive credentials not found at {}.\nDownload an \"OAuth 2.0 Client ID\" (Desktop app) from Google Cloud Console.",
                resolved.display()
            ));
        }

        let content = fs::read_to_string(&resolved)
            .map_err(|e| format!("Cannot read {}: {e}", resolved.display()))?;
        let keys: ClientKeys = serde_json::from_str(&content).map_err(|_| {
            "Invalid credentials.json format. It must be an OAuth 2.0 Client ID (Desktop app), NOT a Service Account key."
                .to_string()
        })?;

        let creds = keys
            .installed
            .or(keys.web)
            .ok_or("Invalid credentials.json: neither 'installed' nor 'web' section found.")?;

        let token_path = token_file_path();

        let mut refresh_token = None;
        if let Ok(saved) = fs::read_to_string(&token_path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&saved) {
                refresh_token = map
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
        }

        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .map_err(|e| e.to_string())?,
            client_id: creds.client_id,
            client_secret: creds.client_secret,
            token_path,
            access_token: Mutex::new(None),
            refresh_token: Mutex::new(refresh_token),
            token_expiry: Mutex::new(None),
        })
    }

    #[allow(dead_code)]
    pub fn is_authorized(&self) -> bool {
        self.refresh_token.lock().unwrap().is_some()
    }

    /// Full authorization: open browser, run a tiny local server that also
    /// serves the callback page (with the Factorio images embedded as data
    /// URIs), capture the OAuth redirect code, then exchange it for tokens.
    pub fn authorize(&self) -> Result<(), String> {
        // 1. Pick a free local port for both the redirect and the static server.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Cannot open local port for OAuth: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        // 2. Build the auth URL.
        let auth_url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            urlencode(&self.client_id),
            urlencode(&redirect_uri),
            urlencode(SCOPES),
        );

        if open_in_browser(&auth_url).is_err() {
            println!("\nOpen this URL in your browser to authorize:\n\n  {auth_url}\n");
        }

        println!("Waiting for authorization on {redirect_uri} ...");

        // 3. Accept exactly one request containing ?code=.
        //    The same connection is then used to also serve the callback page,
        //    so the browser shows the confirmation before closing itself.
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| format!("OAuth redirect failed: {e}"))?;
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        // Parse "GET /?code=xxx HTTP/1.1"
        let code = request
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split('?').nth(1))
            .and_then(|query| {
                query.split('&').find_map(|pair| {
                    let mut it = pair.splitn(2, '=');
                    match (it.next(), it.next()) {
                        (Some("code"), Some(v)) => Some(v.to_string()),
                        _ => None,
                    }
                })
            })
            .ok_or_else(|| {
                let err = request
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.split("error=").nth(1))
                    .unwrap_or("no code received");
                format!("OAuth error: {err}")
            })?;

        // 4. Serve the callback HTML page (with images embedded as data URIs).
        let chad_png_uri = format!(
            "data:image/png;base64,{}",
            base64_embed(&FACTORIO_CHAD_PNG)
        );
        let chad_bg_uri = format!(
            "data:image/png;base64,{}",
            base64_embed(&FACTORIO_CHAD_BG_PNG)
        );
        let html = CALLBACK_HTML
            .replace("__CHAD_PNG_DATAURI__", &chad_png_uri)
            .replace("__CHAD_BG_DATAURI__", &chad_bg_uri);
        let _ = respond(&mut stream, 200, "text/html; charset=utf-8", html.as_bytes());

        // 5. Exchange the code for tokens.
        self.exchange_code(&code, &redirect_uri)
    }

    fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<(), String> {
        let params = [
            ("code", code.to_string()),
            ("client_id", self.client_id.clone()),
            ("client_secret", self.client_secret.clone()),
            ("redirect_uri", redirect_uri.to_string()),
            ("grant_type", "authorization_code".to_string()),
        ];

        let resp = self
            .http
            .post(TOKEN_ENDPOINT)
            .form(&params)
            .send()
            .map_err(|e| format!("Token request failed: {e}"))?;

        let status = resp.status();
        let body = resp.text().map_err(|e| e.to_string())?;

        if !status.is_success() {
            return Err(format!(
                "Google OAuth error ({status}): {}",
                body.chars().take(200).collect::<String>()
            ));
        }

        let tokens: TokenResponse =
            serde_json::from_str(&body).map_err(|e| format!("Bad token response: {e}"))?;
        self.save_tokens(&tokens);
        Ok(())
    }

    fn refresh_access_token(&self) -> Result<(), String> {
        let refresh_token = self
            .refresh_token
            .lock()
            .unwrap()
            .clone()
            .ok_or("Google Drive not authorized yet. Use Settings -> Autorizar Google Drive.")?;

        let params = [
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.clone()),
            ("client_secret", self.client_secret.clone()),
            ("grant_type", "refresh_token".to_string()),
        ];

        let resp = self
            .http
            .post(TOKEN_ENDPOINT)
            .form(&params)
            .send()
            .map_err(|e| format!("Token refresh failed: {e}"))?;

        let status = resp.status();
        let body = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "Google OAuth refresh error ({status}): {}",
                body.chars().take(200).collect::<String>()
            ));
        }

        let tokens: TokenResponse =
            serde_json::from_str(&body).map_err(|e| format!("Bad token response: {e}"))?;
        self.save_tokens(&tokens);
        Ok(())
    }

    fn save_tokens(&self, tokens: &TokenResponse) {
        let merged_refresh = tokens
            .refresh_token
            .clone()
            .or_else(|| self.refresh_token.lock().unwrap().clone());

        *self.access_token.lock().unwrap() = Some(tokens.access_token.clone());
        *self.token_expiry.lock().unwrap() = Some(
            Instant::now() + Duration::from_secs(tokens.expires_in.unwrap_or(3600).saturating_sub(60)),
        );
        *self.refresh_token.lock().unwrap() = merged_refresh.clone();

        let map = serde_json::json!({
            "access_token": tokens.access_token,
            "refresh_token": merged_refresh,
            "expires_in": tokens.expires_in.unwrap_or(3600),
        });
        if let Err(e) = fs::write(&self.token_path, serde_json::to_string_pretty(&map).unwrap()) {
            eprintln!("Cannot save token file: {e}");
        }
    }

    fn access_token(&self) -> Result<String, String> {
        {
            let tok = self.access_token.lock().unwrap();
            let exp = self.token_expiry.lock().unwrap();
            if let (Some(t), Some(e)) = (&*tok, &*exp) {
                if Instant::now() < *e {
                    return Ok(t.clone());
                }
            }
        }
        self.refresh_access_token()?;
        self.access_token
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "Token refresh did not yield an access token.".to_string())
    }

    /// Upload a local file to Drive; returns the webViewLink.
    /// Streams the file from disk (metadata part + file + closing boundary).
    pub fn upload_file(
        &self,
        file_path: &Path,
        file_name: &str,
        folder_id: Option<&str>,
    ) -> Result<String, String> {
        let token = self.access_token()?;

        let meta = match folder_id {
            Some(id) if !id.is_empty() => {
                serde_json::json!({ "name": file_name, "parents": [id] })
            }
            _ => serde_json::json!({ "name": file_name }),
        };

        let file_len = fs::metadata(file_path)
            .map_err(|e| format!("Cannot stat {}: {e}", file_path.display()))?
            .len();

        let file = fs::File::open(file_path)
            .map_err(|e| format!("Cannot open {}: {e}", file_path.display()))?;

        let meta_part = format!(
            "--FBMBOUND\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--FBMBOUND\r\nContent-Type: application/zip\r\n\r\n"
        )
        .into_bytes();
        let tail_part = b"\r\n--FBMBOUND--\r\n".to_vec();
        let total_len = meta_part.len() as u64 + file_len + tail_part.len() as u64;

        // Chain readers so the whole multipart body streams from disk
        let body: Chain<Chain<Cursor<Vec<u8>>, fs::File>, Cursor<Vec<u8>>> =
            Cursor::new(meta_part).chain(file).chain(Cursor::new(tail_part));

        let url = "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=id,webViewLink&supportsAllDrives=true";
        let resp = self
            .http
            .post(url)
            .bearer_auth(&token)
            .header("Content-Type", "multipart/related; boundary=FBMBOUND")
            .body(reqwest::blocking::Body::sized(body, total_len))
            .send()
            .map_err(|e| format!("Upload request failed: {e}"))?;

        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "Google Drive upload error ({status}): {}",
                text.chars().take(300).collect::<String>()
            ));
        }

        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let link = v
            .get("webViewLink")
            .and_then(|x| x.as_str())
            .unwrap_or("https://drive.google.com");
        Ok(link.to_string())
    }

    /// List the most recent .zip backups in the configured folder.
    pub fn list_backups(&self, folder_id: Option<&str>, max: usize) -> Result<Vec<CloudFile>, String> {
        let token = self.access_token()?;

        let mut query = "mimeType='application/zip' and trashed=false".to_string();
        if let Some(id) = folder_id {
            if !id.is_empty() {
                query.push_str(&format!(" and '{id}' in parents"));
            }
        }

        let resp = self
            .http
            .get("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(&token)
            .query(&[
                ("q", query),
                ("fields", "files(id, name, modifiedTime, size, webViewLink)".to_string()),
                ("orderBy", "modifiedTime desc".to_string()),
                ("pageSize", format!("{max}")),
                ("supportsAllDrives", "true".to_string()),
            ])
            .send()
            .map_err(|e| format!("List request failed: {e}"))?;

        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "Google Drive list error ({status}): {}",
                text.chars().take(300).collect::<String>()
            ));
        }

        #[derive(Deserialize)]
        struct FilesResp {
            #[serde(default)]
            files: Vec<CloudFile>,
        }

        let parsed: FilesResp = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(parsed.files)
    }

    /// Download a file by id into destination_path (streamed to disk).
    pub fn download_file(&self, file_id: &str, destination_path: &Path) -> Result<(), String> {
        let token = self.access_token()?;

        let mut resp = self
            .http
            .get(&format!(
                "https://www.googleapis.com/drive/v3/files/{}?alt=media&supportsAllDrives=true",
                urlencode(file_id)
            ))
            .bearer_auth(&token)
            .send()
            .map_err(|e| format!("Download request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(format!(
                "Google Drive download error ({status}): {}",
                text.chars().take(300).collect::<String>()
            ));
        }

        let mut out = fs::File::create(destination_path)
            .map_err(|e| format!("Cannot create {}: {e}", destination_path.display()))?;
        let mut buffer = [0u8; 65536];
        loop {
            let n = resp.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct CloudFile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub modified_time: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default, rename = "webViewLink")]
    #[allow(dead_code)]
    pub web_view_link: Option<String>,
}

impl CloudFile {
    pub fn size_mb(&self) -> f64 {
        self.size
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
            / 1024.0
    }
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let cmd = "open";
        #[cfg(all(unix, not(target_os = "macos")))]
        let cmd = "xdg-open";
        std::process::Command::new(cmd)
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
