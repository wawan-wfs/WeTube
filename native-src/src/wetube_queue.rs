//! Explicit local saved queue: no credentials, URLs, autoplay or remote metadata.
use crate::api::types::VideoItem;
use serde::{Deserialize, Serialize};
const KEY: &str = "wetube_saved_queue_v1";
const LIMIT: usize = 200;
const MAX_BYTES: usize = 256 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Entry { id: String, title: String, channel: String }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Snapshot { version: u8, entries: Vec<Entry>, selected: usize }
fn short(s: &str) -> String { s.chars().filter(|c| !c.is_control()).take(160).collect() }
fn encode(queue: &[VideoItem], selected: Option<usize>) -> anyhow::Result<String> {
    anyhow::ensure!(!queue.is_empty() && queue.len() <= LIMIT, "Save 1–200 videos");
    let selected = selected.unwrap_or(0);
    anyhow::ensure!(selected < queue.len(), "Invalid queue position");
    anyhow::ensure!(queue.iter().all(|v| crate::wetube_session::valid_id(&v.id)), "Invalid video ID");
    let entries = queue.iter().map(|v| Entry { id: v.id.clone(), title: short(&v.title), channel: short(&v.channel_name) }).collect();
    let raw = serde_json::to_string(&Snapshot { version: 1, entries, selected })?;
    anyhow::ensure!(raw.len() <= MAX_BYTES, "Queue metadata exceeds storage limit");
    Ok(raw)
}
fn decode(raw: &str) -> anyhow::Result<Snapshot> {
    anyhow::ensure!(raw.len() <= MAX_BYTES, "Saved queue exceeds storage limit");
    let s: Snapshot = serde_json::from_str(raw)?;
    anyhow::ensure!(s.version == 1 && !s.entries.is_empty() && s.entries.len() <= LIMIT && s.selected < s.entries.len(), "Invalid saved queue");
    anyhow::ensure!(s.entries.iter().all(|v| crate::wetube_session::valid_id(&v.id) && v.title == short(&v.title) && v.channel == short(&v.channel)), "Invalid saved queue metadata");
    Ok(s)
}
pub fn save(queue: &[VideoItem], selected: Option<usize>) -> anyhow::Result<()> {
    let raw = encode(queue, selected)?;
    anyhow::ensure!(crate::store::wetube_kv_set(KEY, &raw), "Could not write saved queue");
    Ok(())
}
pub fn load() -> anyhow::Result<Option<(Vec<VideoItem>, usize)>> {
    let Some(raw) = crate::store::wetube_kv_read(KEY)?.filter(|v| !v.is_empty()) else { return Ok(None) };
    let s = decode(&raw)?;
    Ok(Some((s.entries.into_iter().map(|e| VideoItem { thumbnail: crate::api::innertube::default_thumb(&e.id), id: e.id, title: e.title, channel_name: e.channel, ..Default::default() }).collect(), s.selected)))
}
pub fn clear() -> bool { crate::store::wetube_kv_set(KEY, "") }
// Index-based changes preserve duplicate IDs and the exact playing occurrence.
pub fn move_item<T>(queue: &mut [T], pos: &mut Option<usize>, from: usize, up: bool) -> bool {
    let to = if up { from.checked_sub(1) } else { from.checked_add(1) };
    let Some(to) = to.filter(|&n| n < queue.len()) else { return false };
    if from >= queue.len() { return false; }
    queue.swap(from, to);
    *pos = match *pos { Some(i) if i == from => Some(to), Some(i) if i == to => Some(from), other => other };
    true
}
pub fn remove_item<T>(queue: &mut Vec<T>, pos: &mut Option<usize>, index: usize) -> bool {
    if index >= queue.len() || *pos == Some(index) { return false; }
    queue.remove(index);
    if let Some(i) = *pos { if i > index { *pos = Some(i - 1); } }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    fn video(id: &str) -> VideoItem { VideoItem { id: id.into(), title: "A\nB".into(), ..Default::default() } }
    #[test] fn snapshot_preserves_order_duplicates_and_selected() {
        let q = vec![video("abcdefghijk"), video("12345678901"), video("abcdefghijk")];
        let s = decode(&encode(&q, Some(2)).unwrap()).unwrap();
        assert_eq!(s.selected, 2); assert_eq!(s.entries[0], s.entries[2]); assert_eq!(s.entries[0].title, "AB");
    }
    #[test] fn bad_snapshots_rejected() {
        assert!(encode(&[], None).is_err()); assert!(encode(&[video("bad")], None).is_err()); assert!(encode(&[video("abcdefghijk")], Some(1)).is_err());
        assert!(encode(&vec![video("abcdefghijk"); 201], None).is_err()); assert!(decode("{}").is_err()); assert!(decode(&"a".repeat(MAX_BYTES+1)).is_err());
    }
    #[test] fn moves_and_removes_keep_current_occurrence() {
        let mut q = vec!["a", "b", "a", "c"]; let mut p = Some(2);
        assert!(move_item(&mut q, &mut p, 2, true)); assert_eq!(p, Some(1)); assert_eq!(q, ["a","a","b","c"]);
        assert!(!remove_item(&mut q, &mut p, 1)); assert!(remove_item(&mut q, &mut p, 0)); assert_eq!(p, Some(0)); assert!(!move_item(&mut q, &mut p, 0, true)); assert!(!move_item(&mut q, &mut p, usize::MAX, false));
    }
}
