use crate::api::types::*;
use crate::store;
use crate::ui::app::{MenuItem, Route, confirm, navigate, open_menu, prompt_text};
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;
use gpui_kit::component::input::{Input, InputState};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    All,
    Videos,
    Shorts,
    Live,
}

pub struct FeedPage {
    videos: Vec<VideoItem>,
    profile: Option<i64>,
    kind: Kind,
    refreshing: bool,
    progress: Arc<(AtomicUsize, AtomicUsize)>,
    scroll: ScrollHandle,
    last_updated: Option<i64>,
    shown: usize,
}

impl FeedPage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut s = Self {
            videos: Vec::new(),
            profile: None,
            kind: Kind::All,
            refreshing: false,
            progress: Arc::new((AtomicUsize::new(0), AtomicUsize::new(0))),
            scroll: ScrollHandle::new(),
            last_updated: None,
            shown: 60,
        };
        s.load_cached();
        let stale = s.last_updated.map(|t| crate::api::json::now() - t > 30 * 60).unwrap_or(true);
        if crate::api::auth::signed_in() || (stale && !store::subscriptions().is_empty()) {
            s.refresh(cx);
        }
        s
    }

    fn filter(&self) -> Option<Vec<String>> {
        self.profile.and_then(|id| store::profiles().into_iter().find(|p| p.id == id).map(|p| p.channels))
    }

    fn load_cached(&mut self) {
        let f = self.filter();
        let (v, at) = crate::feed::cached_feed(f.as_deref());
        self.videos = v;
        self.last_updated = at;
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.progress.0.store(0, Ordering::SeqCst);
        let filter = self.filter();
        let account = crate::api::auth::signed_in() && filter.is_none();
        self.progress.1.store(if account { 0 } else { store::subscriptions().len() }, Ordering::SeqCst);
        let prog = self.progress.clone();
        // UI ticker for progress
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let go = this
                    .update(cx, |t, cx| {
                        cx.notify();
                        t.refreshing
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let res = blocking(move || {
                // signed in: the account's own subscriptions feed, one request instead of one per channel
                if account {
                    if let Ok(v) = crate::api::auth::subscriptions_videos(4) {
                        if !v.is_empty() {
                            return (v, 0);
                        }
                    }
                }
                crate::feed::refresh(filter, move |d, t| {
                    prog.0.store(d, Ordering::SeqCst);
                    prog.1.store(t, Ordering::SeqCst);
                })
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                this.videos = res.0;
                this.last_updated = Some(crate::api::json::now());
                if res.1 > 0 {
                    toast(cx, format!("{} channels could not be refreshed", res.1));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for FeedPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        if near_bottom(&self.scroll, 800.0) && self.shown < self.videos.len() {
            self.shown += 60;
        }
        let width = content_width(cx);
        let (_, card_w) = grid_columns(width, 300.0);
        if store::subscriptions().is_empty() && !crate::api::auth::signed_in() {
            return page_scroll("feed-scroll", &self.scroll)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(empty_state("tv", "No subscriptions yet", "", cx))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(pill("go-signin", Some("circle-user"), "Sign in", cx).on_click(|_, window, cx| crate::ui::app::open_sign_in(window, cx)))
                                .child(pill("go-import", Some("upload"), "Import subscriptions", cx).on_click(|_, window, cx| navigate(Route::Settings, window, cx))),
                        ),
                )
                .into_any_element();
        }
        let profiles = store::profiles();
        let mut bar = div().flex().items_center().gap_2().flex_wrap().child(chip("pf-all", "All channels", self.profile.is_none(), cx).on_click(cx.listener(|t, _, _, cx| {
            t.profile = None;
            t.load_cached();
            cx.notify();
        })));
        for pr in &profiles {
            let id = pr.id;
            bar = bar.child(chip(SharedString::from(format!("pf-{id}")), pr.name.clone(), self.profile == Some(id), cx).on_click(cx.listener(move |t, _, _, cx| {
                t.profile = Some(id);
                t.load_cached();
                cx.notify();
            })));
        }
        bar = bar.child(div().w(px(1.)).h_6().bg(p.border).mx_2());
        for (k, l) in [(Kind::All, "All"), (Kind::Videos, "Videos"), (Kind::Shorts, "Shorts"), (Kind::Live, "Live")] {
            if k == Kind::Shorts && s.hide_shorts {
                continue;
            }
            bar = bar.child(chip(SharedString::from(format!("k-{l}")), l, self.kind == k, cx).on_click(cx.listener(move |t, _, _, cx| {
                t.kind = k;
                cx.notify();
            })));
        }
        let (done, total) = (self.progress.0.load(Ordering::SeqCst), self.progress.1.load(Ordering::SeqCst));
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_4()
            .pb_4()
            .child(bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .when_some(self.last_updated.filter(|_| !self.refreshing), |d, t| d.child(div().text_xs().text_color(p.text2).child(format!("Updated {}", crate::api::json::relative_from_ts(t)))))
                    .when(self.refreshing, |d| d.child(div().text_xs().text_color(p.text2).child(if total == 0 { "Refreshing…".to_string() } else { format!("Refreshing {done}/{total}…") })))
                    .child(
                        pill("hide-watched", Some(if s.hide_watched_in_feed { "eye-off" } else { "eye" }), if s.hide_watched_in_feed { "Watched hidden" } else { "Hide watched" }, cx).on_click(|_, _, cx| {
                            store::update_settings(|s| s.hide_watched_in_feed ^= true);
                            cx.refresh_windows();
                        }),
                    )
                    .child(pill("refresh", Some("refresh-cw"), "Refresh", cx).on_click(cx.listener(|t, _, _, cx| t.refresh(cx))))
                    .child(pill("manage", Some("users"), "Manage", cx).on_click(|_, window, cx| navigate(Route::Channels, window, cx))),
            );
        let kind = self.kind;
        let vids: Vec<&VideoItem> = self
            .videos
            .iter()
            .filter(|v| !store::is_filtered(v, &s))
            .filter(|v| !(s.hide_watched_in_feed && is_watched(&v.id, cx)))
            .filter(|v| match kind {
                Kind::All => true,
                Kind::Videos => !v.is_short && !v.is_live,
                Kind::Shorts => v.is_short,
                Kind::Live => v.is_live || v.is_upcoming,
            })
            .collect();
        let total_vids = vids.len();
        let body: AnyElement = if vids.is_empty() && self.refreshing {
            spinner(cx).into_any_element()
        } else if vids.is_empty() {
            empty_state("inbox", "No videos", "", cx).into_any_element()
        } else if kind == Kind::Shorts {
            let (_, w) = grid_columns(width, 200.0);
            grid(vids.iter().take(self.shown).map(|v| shorts_card(v, w, cx)).collect(), width).into_any_element()
        } else {
            // group by day buckets like YouTube
            let now = crate::api::json::now();
            let bucket = |ts: Option<i64>| -> &'static str {
                let d = ts.map(|t| now - t).unwrap_or(i64::MAX);
                if d < 86400 {
                    "Today"
                } else if d < 2 * 86400 {
                    "Yesterday"
                } else if d < 7 * 86400 {
                    "This week"
                } else if d < 30 * 86400 {
                    "This month"
                } else {
                    "Older"
                }
            };
            let mut col = div().flex().flex_col().gap_8();
            let mut cur: Option<&str> = None;
            let mut group: Vec<AnyElement> = Vec::new();
            for v in vids.iter().take(self.shown) {
                let b = bucket(v.published_ts);
                if cur != Some(b) {
                    if let Some(c) = cur {
                        col = col.child(div().flex().flex_col().gap_4().child(section_title(c, cx)).child(grid(std::mem::take(&mut group), width)));
                    }
                    cur = Some(b);
                }
                let v2 = (*v).clone();
                group.push(video_card(&v2, card_w, true, cx));
            }
            if let Some(c) = cur {
                col = col.child(div().flex().flex_col().gap_4().child(section_title(c, cx)).child(grid(group, width)));
            }
            if self.shown < total_vids {
                col = col.child(spinner(cx));
            }
            col.into_any_element()
        };
        page_scroll("feed-scroll", &self.scroll).child(div().px_6().pt_4().pb_24().flex().flex_col().gap_4().child(workspace_header("Subscriptions", "Follow your channels and browse recent uploads", "tv", cx)).child(header).child(body)).into_any_element()
    }
}

// ───────────────────────── manage channels & profiles ─────────────────────────

pub struct ChannelsPage {
    filter: Entity<InputState>,
    scroll: ScrollHandle,
    profile: Option<i64>,
}

impl ChannelsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter channels"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self { filter, scroll: ScrollHandle::new(), profile: None }
    }
}

impl Render for ChannelsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let q = self.filter.read(cx).value().to_lowercase();
        let subs: Vec<store::Subscription> = store::subscriptions().into_iter().filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q)).collect();
        let profiles = store::profiles();
        let cur_profile = self.profile.and_then(|id| profiles.iter().find(|p| p.id == id).cloned());
        let mut pbar = div().flex().items_center().gap_2().flex_wrap().child(chip("mp-none", "All", self.profile.is_none(), cx).on_click(cx.listener(|t, _, _, cx| {
            t.profile = None;
            cx.notify();
        })));
        for pr in &profiles {
            let id = pr.id;
            pbar = pbar.child(
                chip(SharedString::from(format!("mp-{id}")), format!("{} ({})", pr.name, pr.channels.len()), self.profile == Some(id), cx).on_click(cx.listener(move |t, _, _, cx| {
                    t.profile = Some(id);
                    cx.notify();
                })),
            );
        }
        pbar = pbar
            .child(pill("mp-new", Some("plus"), "New profile", cx).on_click(|_, window, cx| {
                prompt_text("New profile", "Profile name (e.g. Music, Tech)", window, cx, |name, _, cx| {
                    let colors = [0x3ea6ff, 0xff4e45, 0x2ba640, 0xffa000, 0xab47bc, 0x26c6da];
                    let c = colors[store::profiles().len() % colors.len()];
                    store::create_profile(&name, c);
                    cx.refresh_windows();
                });
            }))
            .when_some(self.profile, |d, id| {
                d.child(pill("mp-del", Some("trash-2"), "Delete profile", cx).on_click(cx.listener(move |t, _, window, cx| {
                    let ent = cx.entity().downgrade();
                    confirm("Delete profile?", "Channels stay subscribed.", "Delete", window, cx, move |_, cx| {
                        store::delete_profile(id);
                        if let Some(e) = ent.upgrade() {
                            e.update(cx, |t, cx| {
                                t.profile = None;
                                cx.notify();
                            });
                        }
                    });
                    let _ = t;
                })))
            });
        let mut list = div().flex().flex_col().gap_1();
        for s in &subs {
            let cid = s.channel_id.clone();
            let cid2 = s.channel_id.clone();
            let name = s.name.clone();
            let in_profile = cur_profile.as_ref().map(|pr| pr.channels.contains(&s.channel_id));
            let pid = self.profile;
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .hover(move |st| st.bg(p.surface))
                    .child(avatar(s.avatar.as_deref(), px(48.), &s.name, cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("mc-{}", s.channel_id)))
                            .flex_1()
                            .text_base()
                            .text_color(p.text)
                            .cursor_pointer()
                            .child(s.name.clone())
                            .on_click(move |_, window, cx| navigate(Route::Channel(cid.clone()), window, cx)),
                    )
                    .when_some(in_profile.zip(pid), |d, (inp, pid)| {
                        let cid3 = s.channel_id.clone();
                        d.child(
                            pill(SharedString::from(format!("mpt-{}", s.channel_id)), Some(if inp { "square-check" } else { "square" }), "In profile", cx).on_click(move |_, _, cx| {
                                store::set_profile_channel(pid, &cid3, !inp);
                                cx.refresh_windows();
                            }),
                        )
                    })
                    .child(
                        icon_button(SharedString::from(format!("mpm-{}", s.channel_id)), "ellipsis-vertical", cx).on_click(move |ev, window, cx| {
                            let mut items = Vec::new();
                            for pr in store::profiles() {
                                let member = pr.channels.contains(&cid2);
                                let c = cid2.clone();
                                items.push(MenuItem::new(if member { "square-check" } else { "square" }, format!("Profile: {}", pr.name), move |_, cx| {
                                    store::set_profile_channel(pr.id, &c, !member);
                                    cx.refresh_windows();
                                }));
                            }
                            let c = cid2.clone();
                            let n = name.clone();
                            items.push(MenuItem::new("user-minus", "Unsubscribe", move |_, cx| {
                                store::unsubscribe(&c);
                                crate::ui::cards::sync_subscription(c.clone(), false, cx);
                                toast(cx, format!("Unsubscribed from {n}"));
                                cx.refresh_windows();
                            }));
                            open_menu(ev.position(), items, window, cx);
                        }),
                    ),
            );
        }
        page_scroll("chans-scroll", &self.scroll).child(
            div().flex().justify_center().px_6().pt_6().pb_24().child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .w(px(900.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_title(format!("Subscriptions ({})", store::subscriptions().len()), cx))
                            .child(div().w(px(260.)).px_3().py_1().rounded_full().border_1().border_color(p.border).child(Input::new(&self.filter).appearance(false).cleanable(true))),
                    )
                    .child(pbar)
                    .child(list),
            ),
        )
    }
}
