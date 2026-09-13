//! i18n: loads translation catalogs from `i18n/en.json` and `i18n/es.json`
//! (embedded at compile time) and exposes one lookup function used by both the
//! UI (all static texts are pushed to the `I18n` Slint global) and the log.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

const EN_JSON: &str = include_str!("../i18n/en.json");
const ES_JSON: &str = include_str!("../i18n/es.json");

/// "en" or "es" (thread-safe because log messages are formatted on worker
/// threads too). Defaults to English.
static LANG: Mutex<&'static str> = Mutex::new("en");

fn catalog(lang: &str) -> Value {
    let raw = if lang == "es" { ES_JSON } else { EN_JSON };
    serde_json::from_str(raw).expect("translation catalog is valid JSON")
}

/// Load a catalog section ("ui" | "logs") into a flat key -> String map.
pub fn load_section(lang: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let cat = catalog(lang);
    if let Some(entries) = cat.get(section).and_then(|s| s.as_object()) {
        for (k, v) in entries {
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.insert(k.clone(), val);
        }
    }
    out
}

/// Set the active language ("en" | "es"). Unknown values map to English.
pub fn set_language(lang: &str) {
    *LANG.lock().unwrap() = if lang == "es" { "es" } else { "en" };
}

/// Current language.
pub fn language() -> &'static str {
    *LANG.lock().unwrap()
}

/// Look up `logs.<key>` for the current language. Falls back to English when
/// the key is missing, and to the key itself when it is missing in both.
pub fn t(key: &str) -> String {
    let lang = language();
    let cat = catalog(lang);
    if let Some(v) = cat
        .get("logs")
        .and_then(|l| l.get(key))
        .and_then(|v| v.as_str())
    {
        return v.to_string();
    }
    // Fallback: English catalog.
    let cat_en = catalog("en");
    cat_en
        .get("logs")
        .and_then(|l| l.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(key)
        .to_string()
}
