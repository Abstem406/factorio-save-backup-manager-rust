//! Real Discord integration: webhook notifications and bot-based lookup of the
//! latest backup message (mirrors services/discord.js from the JS version).

use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

#[derive(Serialize)]
struct WebhookEmbed {
    title: String,
    description: String,
    #[serde(rename = "type")]
    embed_type: String,
    color: i64,
    fields: Vec<EmbedField>,
    timestamp: String,
    footer: EmbedFooter,
}

#[derive(Serialize)]
struct EmbedField {
    name: String,
    value: String,
    inline: bool,
}

#[derive(Serialize)]
struct EmbedFooter {
    text: String,
}

#[derive(Serialize)]
struct WebhookBody {
    embeds: Vec<WebhookEmbed>,
}

/// Send the "backup uploaded" notification to a Discord webhook.
pub fn send_notification(
    webhook_url: &str,
    file_name: &str,
    download_url: &str,
    service_name: &str,
) -> Result<(), String> {
    if webhook_url.is_empty() {
        return Ok(());
    }

    let body = WebhookBody {
        embeds: vec![WebhookEmbed {
            title: "🚀 Factorio Backup Successful".into(),
            description: format!("A new backup has been uploaded to **{service_name}**."),
            embed_type: "rich".into(),
            color: 0xe67e22,
            fields: vec![
                EmbedField {
                    name: "📁 Filename".into(),
                    value: format!("`{file_name}`"),
                    inline: true,
                },
                EmbedField {
                    name: "🌐 Service".into(),
                    value: service_name.to_string(),
                    inline: true,
                },
                EmbedField {
                    name: "🔗 Download Link".into(),
                    value: download_url.to_string(),
                    inline: false,
                },
            ],
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            footer: EmbedFooter {
                text: "Factorio Backup Manager".into(),
            },
        }],
    };

    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = http
        .post(webhook_url)
        .json(&body)
        .send()
        .map_err(|e| format!("Webhook request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!(
            "Discord notification failed ({status}): {}",
            text.chars().take(200).collect::<String>()
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct LatestBackup {
    pub url: String,
    pub file_name: String,
    pub timestamp: String,
}

#[derive(serde::Deserialize)]
struct DiscordMessage {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    embeds: Vec<DiscordEmbed>,
}

#[derive(serde::Deserialize)]
struct DiscordEmbed {
    #[serde(default)]
    fields: Vec<DiscordEmbedField>,
}

#[derive(serde::Deserialize)]
struct DiscordEmbedField {
    name: String,
    value: String,
}

/// Fetch the latest backup message in a channel using a bot token.
pub fn get_latest_backup_url(bot_token: &str, channel_id: &str) -> Result<LatestBackup, String> {
    if bot_token.is_empty() || channel_id.is_empty() {
        return Err("Discord Bot Token and Channel ID are required for downloading.".to_string());
    }

    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = http
        .get(format!(
            "https://discord.com/api/v10/channels/{channel_id}/messages?limit=50"
        ))
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .map_err(|e| format!("Failed to fetch messages: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!(
            "Failed to fetch messages ({status}): {}",
            text.chars().take(200).collect::<String>()
        ));
    }

    let messages: Vec<DiscordMessage> = resp.json().map_err(|e| e.to_string())?;

    for message in messages {
        for embed in &message.embeds {
            for field in &embed.fields {
                if field.name.contains("Download Link") {
                    let url =
                        extract_url(&field.value).unwrap_or_else(|| field.value.trim().to_string());
                    let file_name = embed
                        .fields
                        .iter()
                        .find(|f| f.name.contains("Filename"))
                        .map(|f| f.value.replace('`', ""))
                        .unwrap_or_else(|| "latest_backup.zip".to_string());
                    return Ok(LatestBackup {
                        url,
                        file_name,
                        timestamp: message.timestamp.clone().unwrap_or_default(),
                    });
                }
            }
        }

        if let Some(content) = &message.content {
            if let Some(url) = extract_url(content) {
                return Ok(LatestBackup {
                    url,
                    file_name: "latest_backup.zip".into(),
                    timestamp: message.timestamp.clone().unwrap_or_default(),
                });
            }
        }
    }

    Err("No backup link found in the last 50 messages.".to_string())
}

fn extract_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.starts_with("http://") || w.starts_with("https://"))
        .map(|s| s.trim_end_matches('>').to_string())
}

/// Download a URL to a file, checking that we actually got a file and not an
/// HTML error page (rate limit / block detection, like the JS version).
pub fn download_to_file(url: &str, destination: &Path, _file_name: &str) -> Result<u64, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    let mut resp = http.get(url).send().map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!(
            "Download failed ({status}): {}",
            text.chars().take(200).collect::<String>()
        ));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let mut first_chunk: Vec<u8> = Vec::new();
    let mut out = std::fs::File::create(destination)
        .map_err(|e| format!("Cannot create {}: {e}", destination.display()))?;

    let mut total: u64 = 0;
    let mut buf = [0u8; 65536];
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if first_chunk.len() < 4096 {
            let take = n.min(4096 - first_chunk.len());
            first_chunk.extend_from_slice(&buf[..take]);
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        total += n as u64;
    }

    // HTML page detection (Rootz overloaded / blocked)
    if content_type.contains("text/html") {
        let head = String::from_utf8_lossy(&first_chunk).to_string();
        let _ = std::fs::remove_file(destination);
        if head.contains("Currently receiving high amount of requests") {
            return Err("Rootz is currently overloaded. Wait a few seconds and try again.".into());
        }
        return Err("The link points to an HTML page instead of a file. The service may be blocking downloads.".into());
    }

    if total < 1024 {
        let _ = std::fs::remove_file(destination);
        return Err("Downloaded file is suspiciously small (<1KB) - not a valid save.".into());
    }

    let _ = _file_name;
    Ok(total)
}
