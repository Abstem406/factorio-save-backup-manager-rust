//! Link resolvers: converts landing-page links into direct download URLs
//! (mirrors services/resolver.js from the JS version).

use std::time::Duration;

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())
}

/// Resolve a sharing link into a direct download URL.
pub fn resolve_direct_link(url: &str) -> String {
    if url.contains("buzzheavier.com") {
        return resolve_buzzheavier(url);
    }
    if url.contains("rootz.so") {
        return resolve_rootz(url);
    }
    url.to_string()
}

fn resolve_buzzheavier(url: &str) -> String {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(_) => return url.to_string(),
    };

    let html = match client.get(url).send() {
        Ok(r) => r.text().unwrap_or_default(),
        Err(_) => return url.to_string(),
    };

    // Find the HTMX download trigger: hx-get="/download..."
    let hx = html
        .split("hx-get=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .filter(|p| p.contains("/download"))
        .map(|p| p.to_string());

    let Some(download_path) = hx else {
        return url.to_string();
    };

    let download_url = if download_path.starts_with("http") {
        download_path.clone()
    } else {
        format!("https://buzzheavier.com{download_path}")
    };

    // The endpoint answers with an HX-Redirect header pointing at the CDN
    match http() {
        Ok(nc) => match nc.get(&download_url).header("HX-Request", "true").send() {
            Ok(r) => {
                if let Some(direct) = r
                    .headers()
                    .get("HX-Redirect")
                    .and_then(|v| v.to_str().ok())
                {
                    return direct.to_string();
                }
                url.to_string()
            }
            Err(_) => url.to_string(),
        },
        Err(_) => url.to_string(),
    }
}

fn resolve_rootz(url: &str) -> String {
    // https://www.rootz.so/d/{shortId} -> internal API
    let short_id = url
        .split('/')
        .filter(|p| !p.is_empty())
        .next_back()
        .unwrap_or("");

    if short_id.is_empty() {
        return url.to_string();
    }

    let api_url = format!("https://www.rootz.so/api/files/download-by-short/{short_id}");

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(_) => return url.to_string(),
    };

    let resp = match client.get(&api_url).send() {
        Ok(r) => r,
        Err(_) => return url.to_string(),
    };

    if !resp.status().is_success() {
        return url.to_string();
    }

    let v: serde_json::Value = match resp.json() {
        Ok(v) => v,
        Err(_) => return url.to_string(),
    };

    v.get("data")
        .and_then(|d| d.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or(url)
        .to_string()
}
