//! Subscription feed: parallel per-channel fetching with caching (like FreeTube's local feed).
use crate::api::types::*;
use crate::api::{innertube, services};
use crate::store::{self, Subscription};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const VIDEOS_TAB: &str = "EgZ2aWRlb3PyBgQKAjoA";
pub const SHORTS_TAB: &str = "EgZzaG9ydHPyBgUKA5oBAA==";
pub const LIVE_TAB: &str = "EgdzdHJlYW1z8gYECgJ6AA==";

pub fn cached_feed(channel_filter: Option<&[String]>) -> (Vec<VideoItem>, Option<i64>) {
    let subs = store::subscriptions();
    let mut all = Vec::new();
    let mut oldest: Option<i64> = None;
    for s in subs {
        if let Some(f) = channel_filter {
            if !f.contains(&s.channel_id) {
                continue;
            }
        }
        if let Some((vids, at)) = store::feed_cache_get(&s.channel_id) {
            oldest = Some(oldest.map_or(at, |o: i64| o.min(at)));
            all.extend(vids);
        }
    }
    sort_feed(&mut all);
    (all, oldest)
}

pub fn sort_feed(v: &mut Vec<VideoItem>) {
    v.sort_by(|a, b| b.published_ts.unwrap_or(0).cmp(&a.published_ts.unwrap_or(0)));
    let mut seen = std::collections::HashSet::new();
    v.retain(|x| seen.insert(x.id.clone()));
}

fn fetch_channel(sub: &Subscription, full: bool, shorts: bool, live: bool) -> Option<Vec<VideoItem>> {
    let mut out: Vec<VideoItem> = Vec::new();
    let mut ok = false;
    if full {
        if let Ok(p) = innertube::channel(&sub.channel_id, Some(VIDEOS_TAB)) {
            ok = true;
            // refresh name/avatar in subscription list
            if !p.info.name.is_empty() {
                store::subscribe(&sub.channel_id, &p.info.name, p.info.avatar.as_deref());
            }
            let rss_dates = None::<()>;
            let _ = rss_dates;
            out.extend(p.page.items.into_iter().filter_map(|i| match i {
                Item::Video(v) => Some(v),
                _ => None,
            }));
            for (tab, flag) in [(SHORTS_TAB, shorts), (LIVE_TAB, live)] {
                if flag {
                    if let Ok(t) = innertube::channel(&sub.channel_id, Some(tab)) {
                        out.extend(t.page.items.into_iter().filter_map(|i| match i {
                            Item::Video(mut v) => {
                                if v.channel_name.is_empty() {
                                    v.channel_name = sub.name.clone();
                                }
                                v.channel_id.get_or_insert(sub.channel_id.clone());
                                // shorts lack dates; place them by fetch time ordering
                                if v.published_ts.is_none() {
                                    v.published_ts = Some(crate::api::json::now() - 7 * 86400);
                                }
                                Some(v)
                            }
                            _ => None,
                        }));
                    }
                }
            }
        }
    }
    if !ok {
        if let Ok((_, vids)) = services::channel_rss(&sub.channel_id) {
            ok = true;
            out = vids;
            if !shorts {
                out.retain(|v| !v.is_short);
            }
        }
    }
    if !ok {
        return None;
    }
    for v in out.iter_mut() {
        if v.channel_name.is_empty() {
            v.channel_name = sub.name.clone();
        }
        v.channel_id.get_or_insert(sub.channel_id.clone());
        if v.channel_avatar.is_none() {
            v.channel_avatar = sub.avatar.clone();
        }
    }
    Some(out)
}

/// Refreshes the feed for all (or filtered) subscriptions. `progress(done, total)` is called from worker threads.
pub fn refresh(channel_filter: Option<Vec<String>>, progress: impl Fn(usize, usize) + Send + Sync + 'static) -> (Vec<VideoItem>, usize) {
    let s = store::settings_snapshot();
    let full = s.feed_source != "rss";
    let (shorts, live) = (s.feed_include_shorts && !s.hide_shorts, s.feed_include_live);
    let subs: Vec<Subscription> = store::subscriptions()
        .into_iter()
        .filter(|x| channel_filter.as_ref().map(|f| f.contains(&x.channel_id)).unwrap_or(true))
        .collect();
    let total = subs.len();
    let subs = Arc::new(subs);
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(progress);
    let workers = 8.min(total.max(1));
    let mut handles = Vec::new();
    for _ in 0..workers {
        let (subs, next, done, failed, progress) = (subs.clone(), next.clone(), done.clone(), failed.clone(), progress.clone());
        handles.push(std::thread::spawn(move || {
            loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(sub) = subs.get(i) else { break };
                match fetch_channel(sub, full, shorts, live) {
                    Some(v) => store::feed_cache_put(&sub.channel_id, &v),
                    None => {
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                progress(d, total);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let filter = channel_filter.as_deref();
    let (feed, _) = cached_feed(filter);
    (feed, failed.load(Ordering::SeqCst))
}
