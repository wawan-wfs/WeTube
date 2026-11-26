//! Opt-in playback recovery, independent of manually saved queues and history.
//! Local-only, bounded, no signed URLs, tokens, credentials or automatic playback.
use crate::api::types::VideoItem;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
const KEY: &str = "wetube_playback_recovery_v1";
const LIMIT: usize = 200;
const MAX_BYTES: usize = 256 * 1024;
const MAX_POSITION: f64 = 604800.0;
static LAST: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry { id: String, title: String, channel: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot { version: u8, items: Vec<Entry>, selected: usize, position: f64 }
pub struct Recovery { pub queue: Vec<VideoItem>, pub selected: usize, pub position: f64 }

fn short(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(160).collect()
}
fn entry(v: &VideoItem) -> Entry {
    Entry { id: v.id.clone(), title: short(&v.title), channel: short(&v.channel_name) }
}
fn position(t: f64) -> f64 {
    if t.is_finite() { t.clamp(0.0, MAX_POSITION).floor() } else { 0.0 }
}
fn snapshot(video: &VideoItem, queue: &[VideoItem], selected: Option<usize>, t: f64) -> anyhow::Result<Snapshot> {
    anyhow::ensure!(crate::wetube_session::valid_id(&video.id), "Invalid recovery video");
    let (items, selected) = match selected.filter(|&i| queue.get(i).is_some_and(|v| v.id == video.id)) {
        Some(i) => {
            // Retain the active occurrence, even in queues larger than the storage limit.
            let start = i.saturating_sub(LIMIT / 2).min(queue.len().saturating_sub(LIMIT));
            let end = start.saturating_add(LIMIT).min(queue.len());
            let mut entries = Vec::with_capacity(end - start);
            let mut active = None;
            for (offset, v) in queue[start..end].iter().enumerate() {
                if crate::wetube_session::valid_id(&v.id) {
                    if start + offset == i { active = Some(entries.len()); entries.push(entry(video)); }
                    else { entries.push(entry(v)); }
                }
            }
            (entries, active.ok_or_else(|| anyhow::anyhow!("Active recovery entry missing"))?)
        }
        None => (vec![entry(video)], 0),
    };
    Ok(Snapshot { version: 1, items, selected, position: position(t) })
}
fn encode(s: &Snapshot) -> anyhow::Result<String> {
    let raw = serde_json::to_string(s)?;
    anyhow::ensure!(raw.len() <= MAX_BYTES, "Recovery snapshot too large");
    Ok(raw)
}
fn decode(raw: &str) -> anyhow::Result<Snapshot> {
    anyhow::ensure!(raw.len() <= MAX_BYTES, "Recovery snapshot too large");
    let s: Snapshot = serde_json::from_str(raw)?;
    anyhow::ensure!(s.version == 1 && !s.items.is_empty() && s.items.len() <= LIMIT
        && s.selected < s.items.len() && s.position.is_finite() && (0.0..=MAX_POSITION).contains(&s.position),
        "Invalid recovery snapshot");
    anyhow::ensure!(s.items.iter().all(|v| crate::wetube_session::valid_id(&v.id)
        && v.title == short(&v.title) && v.channel == short(&v.channel)), "Invalid recovery metadata");
    Ok(s)
}
pub fn save(video: &VideoItem, queue: &[VideoItem], selected: Option<usize>, t: f64) -> anyhow::Result<()> {
    if !crate::store::settings_snapshot().wetube_recover_playback { return Ok(()); }
    let raw = encode(&snapshot(video, queue, selected, t)?)?;
    let mut last = LAST.lock().map_err(|_| anyhow::anyhow!("Recovery lock unavailable"))?;
    if last.as_deref() == Some(raw.as_str()) { return Ok(()); }
    anyhow::ensure!(crate::store::wetube_kv_set(KEY, &raw), "Could not save playback recovery");
    *last = Some(raw);
    Ok(())
}
pub fn load() -> anyhow::Result<Option<Recovery>> {
    if !crate::store::settings_snapshot().wetube_recover_playback { return Ok(None); }
    let Some(raw) = crate::store::wetube_kv_read(KEY)?.filter(|v| !v.is_empty()) else { return Ok(None) };
    let s = decode(&raw)?;
    let queue = s.items.into_iter().map(|v| VideoItem {
        thumbnail: crate::api::innertube::default_thumb(&v.id),
        id: v.id, title: v.title, channel_name: v.channel, ..Default::default()
    }).collect();
    Ok(Some(Recovery { queue, selected: s.selected, position: s.position }))
}
pub fn clear() -> bool {
    let Ok(mut last) = LAST.lock() else { return false };
    if !crate::store::wetube_kv_set(KEY, "") { return false; }
    *last = None;
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    fn v(id: &str) -> VideoItem { VideoItem { id: id.into(), title: "Title\n".into(), ..Default::default() } }
    #[test] fn duplicate_occurrences_and_time() {
        let a = v("abcdefghijk"); let b = v("12345678901");
        let s = snapshot(&a, &[a.clone(), b, a.clone()], Some(2), 45.7).unwrap();
        assert_eq!(s.selected, 2); assert_eq!(s.position, 45.0); assert_eq!(s.items[2].title, "Title");
        assert_eq!(decode(&encode(&s).unwrap()).unwrap(), s);
    }
    #[test] fn large_queue_retains_current_occurrence() {
        let a = v("abcdefghijk"); let q = vec![a.clone(); 1000];
        let s = snapshot(&a, &q, Some(999), 3.0).unwrap();
        assert_eq!(s.items.len(), LIMIT); assert_eq!(s.selected, LIMIT-1);
    }
    #[test] fn unrelated_queue_and_invalid_id() {
        let a = v("abcdefghijk"); let b = v("12345678901");
        let s = snapshot(&a, &[b], Some(0), f64::NAN).unwrap();
        assert_eq!(s.items.len(), 1); assert_eq!(s.position, 0.0);
        assert!(snapshot(&v("bad"), &[], None, 0.0).is_err());
    }
    #[test] fn rejects_corruption_unknown_fields_and_limits() {
        assert!(decode("{}").is_err()); assert!(decode(&"x".repeat(MAX_BYTES + 1)).is_err());
        assert!(decode(r#"{"version":1,"items":[],"selected":0,"position":1,"token":"secret"}"#).is_err());
        let a = v("abcdefghijk"); let mut s = snapshot(&a, &[], None, 0.0).unwrap();
        s.selected = 1; assert!(decode(&encode(&s).unwrap()).is_err());
    }
    #[test] fn invalid_neighbors_do_not_shift_active_incorrectly() {
        let a = v("abcdefghijk"); let s = snapshot(&a, &[v("bad"), a.clone(), v("bad")], Some(1), 1.0).unwrap();
        assert_eq!(s.selected, 0); assert_eq!(s.items.len(), 1);
    }
}
