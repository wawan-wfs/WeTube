//! Recommendations without an account: videos related to what was watched and
//! searched in Stream Tube. Everything it starts from is the local history; the
//! only requests are YouTube's ordinary "related videos" and search calls.
use crate::api::innertube;
use crate::api::types::{Item, VideoItem};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Recent videos the feed is built from, and how many come from further back.
const RECENT_SEEDS: usize = 6;
const OLDER_SEEDS: usize = 2;
const SEARCHES: usize = 3;
/// A finished feed is reused for a while; building one takes ~8 requests.
const KEEP: Duration = Duration::from_secs(20 * 60);

static CACHE: Mutex<Option<(Vec<Item>, Instant, u64)>> = Mutex::new(None);

/// Whether there's anything to base recommendations on.
pub fn available() -> bool {
    crate::store::settings_snapshot().history_enabled && !crate::store::history_list("", 1).is_empty()
}

/// A fingerprint of the newest history entries, so a fresh watch rebuilds the feed.
fn history_mark() -> u64 {
    crate::store::history_list("", 3).iter().map(|h| h.watched_at as u64).sum()
}

/// Videos related to the local watch and search history, newest interests first,
/// without anything already watched. Empty when there's no history.
pub fn for_you() -> Vec<Item> {
    let mark = history_mark();
    if let Some((items, at, m)) = CACHE.lock().as_ref() {
        if at.elapsed() < KEEP && *m == mark {
            return items.clone();
        }
    }
    let history = crate::store::history_list("", 400);
    if history.is_empty() {
        return Vec::new();
    }
    let watched: HashSet<String> = history.iter().map(|h| h.video.id.clone()).collect();
    // seeds: the latest long videos (one per channel, for variety) plus a couple older ones
    let mut channels = HashSet::new();
    let longs: Vec<&VideoItem> = history
        .iter()
        .map(|h| &h.video)
        .filter(|v| !v.is_short)
        .filter(|v| channels.insert(v.channel_id.clone().unwrap_or_else(|| v.channel_name.clone())))
        .collect();
    let mut seeds: Vec<String> = longs.iter().take(RECENT_SEEDS).map(|v| v.id.clone()).collect();
    {
        use rand::seq::IndexedRandom;
        let older: Vec<&&VideoItem> = longs.iter().skip(RECENT_SEEDS).collect();
        seeds.extend(older.choose_multiple(&mut rand::rng(), OLDER_SEEDS).map(|v| v.id.clone()));
    }
    let queries = crate::store::search_history("", SEARCHES);
    let lists: Vec<Vec<VideoItem>> = std::thread::scope(|s| {
        let mut hs = Vec::new();
        for id in &seeds {
            hs.push(s.spawn(move || innertube::watch_next(id, None).map(|w| videos(w.related)).unwrap_or_default()));
        }
        for q in &queries {
            hs.push(s.spawn(move || {
                innertube::search(q, &Default::default()).map(|p| videos(p.items).into_iter().take(8).collect()).unwrap_or_default()
            }));
        }
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    // round robin across the lists, so every interest shows up near the top
    let mut out = Vec::new();
    let mut seen = watched;
    let mut iters: Vec<_> = lists.into_iter().map(|l| l.into_iter()).collect();
    loop {
        let mut any = false;
        for it in iters.iter_mut() {
            // up to two from each list per round
            for _ in 0..2 {
                if let Some(v) = it.next() {
                    any = true;
                    if seen.insert(v.id.clone()) {
                        out.push(Item::Video(v));
                    }
                }
            }
        }
        if !any {
            break;
        }
    }
    if !out.is_empty() {
        *CACHE.lock() = Some((out.clone(), Instant::now(), mark));
    }
    out
}

fn videos(items: Vec<Item>) -> Vec<VideoItem> {
    items
        .into_iter()
        .filter_map(|i| match i {
            Item::Video(v) if !v.is_short && !v.is_upcoming => Some(v),
            _ => None,
        })
        .collect()
}
