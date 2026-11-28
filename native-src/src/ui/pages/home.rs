use crate::ui::app::{navigate, Route};
use crate::api::innertube::{self, EXPLORE};
use crate::api::types::*;
use crate::store;
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;

/// Marks continuations of the signed-in home feed (they need the TV client).
const ACCOUNT: &str = "acct:";
/// Marks continuations of the home feed loaded with the browser sign-in.
const WEB: &str = "web:";

/// Videos at the top of the last home load. A refresh moves them down so it
/// actually shows something new (YouTube returns much the same first page).
static LAST_TOP: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

fn freshen(items: Vec<Item>) -> Vec<Item> {
    use rand::seq::SliceRandom;
    let last: std::collections::HashSet<String> = LAST_TOP.lock().iter().cloned().collect();
    let id = |i: &Item| if let Item::Video(v) = i { Some(v.id.clone()) } else { None };
    let (mut fresh, mut seen): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|i| id(i).map(|x| !last.contains(&x)).unwrap_or(true));
    let mut rng = rand::rng();
    // keep the feed's rough order but mix it within small windows
    for chunk in fresh.chunks_mut(8) {
        chunk.shuffle(&mut rng);
    }
    seen.shuffle(&mut rng);
    fresh.append(&mut seen);
    *LAST_TOP.lock() = fresh.iter().take(24).filter_map(id).collect();
    fresh
}

/// Where the home feed comes from, so a cached feed is only shown for the same source.
fn feed_source() -> &'static str {
    if crate::api::web::connected() {
        "web"
    } else if crate::api::auth::signed_in() {
        "account"
    } else {
        "local"
    }
}

/// The home feed from the last session, shown at once when the app starts while
/// the new one loads. Kept for three days.
fn cached_home() -> Option<Vec<Item>> {
    let raw=store::wetube_kv_read_bounded("home_cache",1024*1024).ok()??;
    let v:serde_json::Value=serde_json::from_str(&raw).ok()?;
    if v.get("source")?.as_str()? != "local" {
        // A source class ("account"/"web") is not an account identity. Do
        // not reuse or retain these older private recommendation snapshots.
        let _=store::wetube_kv_set("home_cache", "");return None;
    }
    if feed_source()!="local" {return None;}
    let age=crate::api::json::now().checked_sub(v.get("at")?.as_i64()?)?;
    if !(0..=3*24*3600).contains(&age){return None;}
    let items:Vec<Item>=serde_json::from_value(v.get("items")?.clone()).ok()?;
    (!items.is_empty()).then_some(items)
}

fn save_home(items:&[Item],source:&str) {
    if source!="local"||feed_source()!="local" {return;}
    let v=serde_json::json!({"source":"local","at":crate::api::json::now(),"items":items.iter().take(60).collect::<Vec<_>>()});
    if let Ok(raw)=serde_json::to_string(&v){if raw.len()<=1024*1024 {let _=store::wetube_kv_set("home_cache",&raw);}}
}

/// The "Recently uploaded" chip (after All and the Explore categories).
const RECENT: usize = usize::MAX;
/// How far back "Recently uploaded" reaches.
const RECENT_DAYS: i64 = 14;

/// Videos from the Home feed and the subscriptions uploaded in the last two
/// weeks, newest first, like youtube.com's "Recently uploaded" chip.
fn recently_uploaded() -> anyhow::Result<Vec<Item>> {
    let mut videos: Vec<VideoItem> = Vec::new();
    let mut add = |p: anyhow::Result<Page>| {
        if let Ok(p) = p {
            videos.extend(p.items.into_iter().filter_map(|i| if let Item::Video(v) = i { Some(v) } else { None }));
        }
    };
    // the personal Home feed, two pages of it, from whichever sign-in there is
    if crate::api::web::connected() {
        let first = crate::api::web::home();
        let next = first.as_ref().ok().and_then(|p| p.continuation.clone());
        add(first);
        if let Some(t) = next {
            add(crate::api::web::browse_continue(&t));
        }
    } else if crate::api::auth::signed_in() {
        let first = crate::api::auth::home();
        let next = first.as_ref().ok().and_then(|p| p.continuation.clone());
        add(first);
        if let Some(t) = next {
            add(crate::api::auth::browse_continue(&t));
        }
    } else if crate::recommend::available() {
        add(Ok(Page { items: crate::recommend::for_you(), continuation: None }));
    }
    // new uploads from the subscriptions
    if crate::api::auth::signed_in() {
        add(crate::api::auth::subscriptions_feed());
    }
    if !store::subscriptions().is_empty() {
        let (feed, _) = crate::feed::cached_feed(None);
        videos.extend(feed.into_iter().take(150));
    }
    let since = crate::api::json::now() - RECENT_DAYS * 86400;
    let mut seen = std::collections::HashSet::new();
    let mut recent: Vec<(i64, VideoItem)> = videos
        .into_iter()
        .filter(|v| !v.is_short && !v.is_upcoming && !v.is_live)
        .filter_map(|v| {
            let ts = v.published_ts.or_else(|| v.published_text.as_deref().and_then(crate::api::json::parse_relative_time))?;
            (ts >= since && seen.insert(v.id.clone())).then_some((ts, v))
        })
        .collect();
    if recent.is_empty() {
        anyhow::bail!("No videos from the last {RECENT_DAYS} days in your feed yet. Sign in or subscribe to some channels to fill this up.");
    }
    recent.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(recent
        .into_iter()
        .take(120)
        .map(|(ts, mut v)| {
            // a saved feed keeps the "2 days ago" it was fetched with; say it from
            // the upload time instead, so the dates match the order
            v.published_text = Some(crate::api::json::relative_from_ts(ts));
            Item::Video(v)
        })
        .collect())
}

/// Only the first home load of a session starts from the cache.
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct HomePage {
    chip: usize,
    items: Load<Vec<Item>>,
    continuation: Option<String>,
    loading_more: bool,
    scroll: ScrollHandle,
    serial: u64,
}

impl HomePage {
    pub fn new(chip: usize, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut s = Self { chip, items: Load::Loading, continuation: None, loading_more: false, scroll: ScrollHandle::new(), serial: 0 };
        s.load(cx);
        s
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.items = Load::Loading;
        self.continuation = None;
        self.loading_more = false;
        self.serial += 1;
        let serial = self.serial;
        let chip = self.chip;
        let source=feed_source();
        // at startup, show the last feed right away instead of a spinner
        let mut from_cache = false;
        if chip == 0 && !STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if let Some(items) = cached_home() {
                self.items = Load::Ready(items);
                from_cache = true;
            }
        }
        cx.spawn(async move |this, cx| {
            let res = blocking(move || -> anyhow::Result<(Vec<Item>, Option<String>)> {
                if chip == 0 && source=="web" && crate::api::web::connected() {
                    // the account's home feed exactly as youtube.com builds it
                    for _ in 0..2 {
                        match crate::api::web::home() {
                            Ok(p) if !p.items.is_empty() => return Ok((freshen(p.items), p.continuation.map(|c| format!("{WEB}{c}")))),
                            _ => std::thread::sleep(std::time::Duration::from_millis(400)),
                        }
                    }
                }
                if chip == 0 && source!="local" && crate::api::auth::signed_in() {
                    // YouTube's own recommendations for the account (a failed request is
                    // retried once before falling back, so the feed stays personal)
                    let home = crate::api::auth::home().ok().filter(|p| !p.items.is_empty()).or_else(|| {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        crate::api::auth::home().ok()
                    });
                    if let Some(p) = home {
                        if !p.items.is_empty() {
                            // shown as soon as the first page is in; the next page loads
                            // when scrolling, instead of making the start wait for it
                            return Ok((freshen(p.items), p.continuation.map(|c| format!("{ACCOUNT}{c}"))));
                        }
                    }
                }
                if chip == 0 && crate::recommend::available() {
                    // no account: videos related to what was watched and searched here,
                    // with new uploads from local subscriptions mixed in
                    let mut picks = crate::recommend::for_you();
                    if !picks.is_empty() {
                        if !store::subscriptions().is_empty() {
                            let (feed, _) = crate::feed::cached_feed(None);
                            let mut subs = feed.into_iter().take(30).map(Item::Video);
                            let mut out = Vec::new();
                            for (i, it) in picks.drain(..).enumerate() {
                                out.push(it);
                                if i % 3 == 2 {
                                    out.extend(subs.next());
                                }
                            }
                            picks = out;
                        }
                        return Ok((freshen(picks), None));
                    }
                }
                if chip == RECENT {
                    return Ok((recently_uploaded()?, None));
                }
                if chip == 0 {
                    let subs = store::subscriptions();
                    if !subs.is_empty() {
                        let (mut feed, oldest) = crate::feed::cached_feed(None);
                        let stale = oldest.map(|o| crate::api::json::now() - o > 3600).unwrap_or(true);
                        if feed.is_empty() || stale {
                            feed = crate::feed::refresh(None, |_, _| {}).0;
                        }
                        // only the subscriptions: popular videos from unrelated channels mixed
                        // in here read as random videos in the feed
                        let items: Vec<Item> = feed.into_iter().take(80).map(Item::Video).collect();
                        return Ok((freshen(items), None));
                    }
                    // no subscriptions: popular from several categories, interleaved
                    let lists: Vec<Vec<Item>> = std::thread::scope(|s| {
                        let hs: Vec<_> = EXPLORE
                            .iter()
                            .take(4)
                            .map(|(_, id)| s.spawn(move || innertube::explore(id).map(|p| p.items).unwrap_or_default()))
                            .collect();
                        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
                    });
                    let mut out = Vec::new();
                    let mut iters: Vec<_> = lists.into_iter().map(|l| l.into_iter().filter(|i| matches!(i, Item::Video(_)))).collect();
                    let mut seen = std::collections::HashSet::new();
                    loop {
                        let mut any = false;
                        for it in iters.iter_mut() {
                            if let Some(x) = it.next() {
                                any = true;
                                if let Item::Video(v) = &x {
                                    if seen.insert(v.id.clone()) {
                                        out.push(x);
                                    }
                                }
                            }
                        }
                        if !any {
                            break;
                        }
                    }
                    Ok((freshen(out), None))
                } else {
                    let p = innertube::explore(EXPLORE[chip - 1].1)?;
                    Ok((p.items, p.continuation))
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                if source!=feed_source(){this.load(cx);return;}
                crate::perf::mark("home feed fetched");
                match res {
                    Ok((items, cont)) => {
                        if chip == 0 && !items.is_empty() {
                            save_home(&items,source);
                        }
                        // the cached feed stays if the user is already scrolling through it
                        let scrolled = this.scroll.offset().y < px(-40.);
                        if !(from_cache && scrolled) {
                            this.items = Load::Ready(items);
                        }
                        this.continuation = cont;
                    }
                    Err(_) if from_cache => {}
                    Err(e) => this.items = Load::Error(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(tok) = self.continuation.clone() else {
            self.loading_more = false;
            return;
        };
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let res = blocking(move || {
                if let Some(t) = tok.strip_prefix(WEB) {
                    return crate::api::web::browse_continue(t).map(|mut p| {
                        p.continuation = p.continuation.map(|c| format!("{WEB}{c}"));
                        p
                    });
                }
                match tok.strip_prefix(ACCOUNT) {
                    Some(t) => crate::api::auth::browse_continue(t).map(|mut p| {
                        p.continuation = p.continuation.map(|c| format!("{ACCOUNT}{c}"));
                        p
                    }),
                    None => innertube::browse_continue(&tok),
                }
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial { return; }
                this.loading_more = false;
                if let Ok(p) = res {
                    if let Some(items) = this.items.ready_mut() {
                        let mut seen: std::collections::HashSet<String> = items.iter().filter_map(|i| i.as_video().map(|v| v.id.clone())).collect();
                        items.extend(p.items.into_iter().filter(|i| i.as_video().map(|v| seen.insert(v.id.clone())).unwrap_or(true)));
                    }
                    this.continuation = p.continuation;
                } else {
                    this.continuation = None;
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for HomePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        let has_more = self.continuation.is_some();
        maybe_load_more(&self.scroll, &mut self.loading_more, has_more, cx, |this, cx| this.load_more(cx));
        let layout=workspace_surface(crate::wetube_workspace::Workspace::Home,cx);
        let width=px(layout.width);
        let (_, card_w) = grid_columns(width, layout.card_min);
        let mut chips = div().flex().flex_wrap().items_center().gap_2().py_3().px_6().bg(p.bg).child(
            chip(
                "chip-all",
                if crate::api::auth::signed_in() || crate::api::web::connected() || !store::subscriptions().is_empty() {
                    "All"
                } else if crate::recommend::available() {
                    "For you"
                } else {
                    "Popular"
                },
                self.chip == 0,
                cx,
            )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.chip = 0;
                    this.load(cx);
                })),
        );
        chips = chips.child(chip("chip-recent", "Recently uploaded", self.chip == RECENT, cx).on_click(cx.listener(|this, _, _, cx| {
            this.chip = RECENT;
            this.load(cx);
        })));
        if !s.hide_explore {
            for (i, (name, _)) in EXPLORE.iter().enumerate() {
                chips = chips.child(chip(SharedString::from(format!("chip-{i}")), *name, self.chip == i + 1, cx).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.chip = i + 1;
                        this.load(cx);
                    },
                )));
            }
        }
        let body: AnyElement = match &self.items {
            Load::Loading => spinner(cx).into_any_element(),
            Load::Error(e) => error_state(e, cx).into_any_element(),
            Load::Ready(items) => {
                let items: Vec<Item> = items
                    .iter()
                    .filter(|i| match i {
                        Item::Video(v) => !store::is_filtered(v, &s) && !(s.hide_watched_in_feed && is_watched(&v.id, cx)),
                        _ => true,
                    })
                    .cloned()
                    .collect();
                if items.is_empty() {
                    empty_state("tv", "Nothing here yet", "", cx).into_any_element()
                } else {
                    let (videos, shorts): (Vec<Item>, Vec<Item>) =
                        items.into_iter().partition(|i| !matches!(i, Item::Video(v) if v.is_short));
                    let mut col = div().flex().flex_col().gap_8();
                    let cards: Vec<AnyElement> = videos.iter().map(|i| grid_item(i, card_w, true, cx)).collect();
                    let split = cards.len().min(layout.columns * 2);
                    let mut cards = cards.into_iter();
                    col = col.child(grid(cards.by_ref().take(split).collect(), width));
                    if !shorts.is_empty() && !s.hide_shorts {
                        col = col.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(div().flex().items_center().gap_2().child(icon("zap").text_color(p.accent)).child(section_title("Shorts", cx)))
                                .child(div().flex().flex_wrap().gap_4().children(shorts.iter().take(8).filter_map(|i| i.as_video()).map(|v| shorts_card(v, card_w.min(px(200.)), cx)))),
                        );
                    }
                    col = col.child(grid(cards.collect(), width));
                    if self.loading_more {
                        col = col.child(spinner(cx));
                    }
                    col.into_any_element()
                }
            }
        };
        page_scroll("home-scroll",&self.scroll).child(div().flex().flex_col()
            .child(div().px_6().pt_4().pb_1().child(workspace_header("Discover", "Browse videos, channels and your local subscriptions", "compass", cx)))
            .child(div().px_6().py_2().flex().flex_wrap().gap_2()
                .child(pill("discover-refresh",Some("refresh-cw"),"Refresh feed",cx).on_click(cx.listener(|this,_,_,cx|this.load(cx))))
                .child(pill("discover-library",Some("library"),"Library",cx).on_click(|_,window,cx|navigate(Route::Library,window,cx)))
                .child(pill("discover-history",Some("history"),"History",cx).on_click(|_,window,cx|navigate(Route::History,window,cx))))
            .child(chips)
            .child(div().px_6().pt_2().pb_24().child(body)))
    }
}
