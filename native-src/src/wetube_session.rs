//! WeTube Native: opt-in, bounded video-tab session state. No credentials or URLs.
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
static LAST: Mutex<Option<String>> = Mutex::new(None);
const KEY: &str = "wetube_video_tabs_v1";
pub const LIMIT: usize = 11;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct VideoTabs { ids: Vec<String> }

pub fn valid_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn clean(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    // Preserve order and allow intentionally duplicated tabs.
    ids.into_iter().filter(|id| valid_id(id)).take(LIMIT).collect()
}
fn decode(raw: &str) -> Vec<String> {
    if raw.len() > 4096 { return Vec::new(); }
    serde_json::from_str::<VideoTabs>(raw).map(|v| clean(v.ids)).unwrap_or_default()
}
pub fn load() -> Vec<String> {
    if !crate::store::settings_snapshot().wetube_restore_tabs { return Vec::new(); }
    crate::store::kv_get(KEY).map(|s| decode(&s)).unwrap_or_default()
}
pub fn save(ids: impl IntoIterator<Item = String>) {
    if !crate::store::settings_snapshot().wetube_restore_tabs { return; }
    if let Ok(s) = serde_json::to_string(&VideoTabs { ids: clean(ids) }) {
        if let Ok(mut last) = LAST.lock() {
            if last.as_deref() != Some(s.as_str()) {
                if crate::store::wetube_kv_set(KEY, &s) { *last = Some(s); }
            }
        }
    }
}
pub fn clear() { if let Ok(mut last) = LAST.lock() { let _ = crate::store::wetube_kv_set(KEY, ""); *last = None; } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ids_are_strict() { assert!(valid_id("dQw4w9WgXcQ")); assert!(!valid_id("https://x.y")); assert!(!valid_id("abcdefghij/")); }
    #[test] fn malformed_and_oversize_are_empty() { assert!(decode("{").is_empty()); assert!(decode(&"x".repeat(4097)).is_empty()); }
    #[test] fn limits_and_duplicates() { assert_eq!(clean(vec!["dQw4w9WgXcQ".into(); 20]).len(), LIMIT); assert_eq!(decode(r#"{"ids":["bad","dQw4w9WgXcQ"]}"#), vec!["dQw4w9WgXcQ"]); }
}
