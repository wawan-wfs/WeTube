use crate::store;
use crate::ui::app::{Route, confirm, navigate, prompt_text};
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;
use gpui_kit::component::input::{Input, InputState};

#[derive(Clone, Copy, PartialEq)]
enum HistTab {
    Account,
    Device,
    Searches,
}

pub struct HistoryPage {
    filter: Entity<InputState>,
    scroll: ScrollHandle,
    limit: usize,
    tab: HistTab,
    /// the signed-in account's YouTube watch history
    yt: Option<Load<Vec<crate::api::types::VideoItem>>>,
    yt_cont: Option<String>,
    yt_loading_more: bool,
}

fn only_videos(items: Vec<crate::api::types::Item>) -> impl Iterator<Item = crate::api::types::VideoItem> {
    items.into_iter().filter_map(|i| if let crate::api::types::Item::Video(v) = i { Some(v) } else { None })
}

impl HistoryPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search history"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        let signed_in = crate::api::auth::signed_in();
        let mut s = Self {
            filter,
            scroll: ScrollHandle::new(),
            limit: 100,
            tab: if signed_in { HistTab::Account } else { HistTab::Device },
            yt: None,
            yt_cont: None,
            yt_loading_more: false,
        };
        if signed_in {
            crate::ui::app::sync_account_history(cx);
            s.yt = Some(Load::Loading);
            cx.spawn(async move |this, cx| {
                let res = blocking(crate::api::auth::history).await;
                let _ = this.update(cx, |t, cx| {
                    t.yt = Some(match res {
                        Ok(page) => {
                            t.yt_cont = page.continuation;
                            Load::Ready(only_videos(page.items).collect())
                        }
                        Err(e) => Load::Error(e.to_string()),
                    });
                    cx.notify();
                });
            })
            .detach();
        }
        s
    }

    fn load_more_yt(&mut self, cx: &mut Context<Self>) {
        let Some(tok) = self.yt_cont.take() else {
            self.yt_loading_more = false;
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = blocking(move || crate::api::auth::browse_continue(&tok)).await;
            let _ = this.update(cx, |t, cx| {
                t.yt_loading_more = false;
                if let Ok(page) = res {
                    t.yt_cont = page.continuation;
                    if let Some(Load::Ready(list)) = &mut t.yt {
                        let seen: std::collections::HashSet<String> = list.iter().map(|v| v.id.clone()).collect();
                        list.extend(only_videos(page.items).filter(|v| !seen.contains(&v.id)));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn account_list(&mut self, q: &str, cx: &mut Context<Self>) -> Div {
        let mut list = div().flex().flex_col().gap_4().flex_1().min_w_0();
        let has_more = self.yt_cont.is_some();
        let shown: Vec<crate::api::types::VideoItem> = match &self.yt {
            None => return list.child(empty_state("history", "Sign in to see your YouTube watch history", "", cx)),
            Some(Load::Loading) => return list.child(loading_row(cx)),
            Some(Load::Error(e)) => {
                let e = e.clone();
                return list.child(empty_state("cloud-off", "Could not load your YouTube history", &e, cx));
            }
            Some(Load::Ready(videos)) => {
                let ql = q.to_lowercase();
                videos.iter().filter(|v| ql.is_empty() || v.title.to_lowercase().contains(&ql) || v.channel_name.to_lowercase().contains(&ql)).cloned().collect()
            }
        };
        if shown.is_empty() {
            list = list.child(empty_state("history", if q.is_empty() { "Your YouTube watch history is empty" } else { "No matches" }, "", cx));
        }
        for v in &shown {
            let id = v.id.clone();
            list = list.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(video_row(v, px(246.), cx)))
                    .child(icon_button(SharedString::from(format!("yhx-{}", v.id)), "x", cx).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_account_entry(id.clone(), cx);
                    }))),
            );
        }
        if has_more {
            list = list.child(loading_row(cx));
        }
        list
    }

    /// Removes an entry from the YouTube history (and this device's), restoring it if that fails.
    fn remove_account_entry(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(Load::Ready(list)) = &mut self.yt else { return };
        let Some(pos) = list.iter().position(|v| v.id == id) else { return };
        let removed = list.remove(pos);
        store::history_remove(&id);
        cx.default_global::<Watched>().0.remove(&id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let id2 = id.clone();
            let res = blocking(move || crate::api::auth::remove_from_history(&id2)).await;
            let _ = this.update(cx, |t, cx| match res {
                Ok(()) => toast(cx, "Removed from watch history"),
                Err(e) => {
                    if let Some(Load::Ready(list)) = &mut t.yt {
                        list.insert(pos.min(list.len()), removed);
                    }
                    toast(cx, format!("Couldn't remove it from YouTube: {}", crate::ui::friendly_error(&e.to_string())));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn search_list(&mut self, q: &str, cx: &mut Context<Self>) -> Div {
        let p = pal(cx);
        let mut list = div().flex().flex_col().gap_1().flex_1().min_w_0();
        let ql = q.to_lowercase();
        let items: Vec<String> = store::search_history("", 500).into_iter().filter(|s| ql.is_empty() || s.to_lowercase().contains(&ql)).collect();
        if items.is_empty() {
            return list.child(empty_state("search", if q.is_empty() { "No searches yet" } else { "No matches" }, "Searches made in this app show here", cx));
        }
        for (i, item) in items.into_iter().enumerate() {
            let (a, b) = (item.clone(), item.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("sh-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .hover(|d| d.bg(p.hover))
                    .on_click(move |_, window, cx| navigate(Route::Search(a.clone()), window, cx))
                    .child(icon("history").size_4().text_color(p.text2))
                    .child(div().flex_1().min_w_0().truncate().text_color(p.text).child(item))
                    .child(icon_button(SharedString::from(format!("shx-{i}")), "x", cx).on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        store::search_history_remove(&b);
                        cx.notify();
                    }))),
            );
        }
        list
    }
}

fn loading_row(cx: &App) -> Div {
    let p = pal(cx);
    div().py_6().flex().justify_center().text_color(p.text2).child("Loading…")
}

impl Render for HistoryPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        let q = self.filter.read(cx).value().to_string();
        if near_bottom(&self.scroll, 800.0) {
            match self.tab {
                HistTab::Device => self.limit += 100,
                HistTab::Account if !self.yt_loading_more && self.yt_cont.is_some() => {
                    self.yt_loading_more = true;
                    cx.defer_in_entity(|t, cx| t.load_more_yt(cx));
                }
                _ => {}
            }
        }
        let width = content_width(cx).min(px(1100.));
        let signed_in = crate::api::auth::signed_in();
        let mut tabs = div().flex().gap_2();
        let tab_defs: Vec<(HistTab, &str)> = if signed_in {
            vec![(HistTab::Account, "YouTube history"), (HistTab::Device, "On this device"), (HistTab::Searches, "Searches")]
        } else {
            vec![(HistTab::Device, "Watched"), (HistTab::Searches, "Searches")]
        };
        for (t, label) in tab_defs {
            tabs = tabs.child(chip(SharedString::from(format!("ht-{label}")), label, self.tab == t, cx).on_click(cx.listener(move |this, _, _, cx| {
                this.tab = t;
                cx.notify();
            })));
        }
        let entries = if self.tab == HistTab::Device { store::history_list(&q, self.limit) } else { Vec::new() };
        let mut list = div().flex().flex_col().gap_4().flex_1().min_w_0();
        let mut cur_day = String::new();
        for h in &entries {
            let day = chrono::DateTime::from_timestamp(h.watched_at, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%A, %B %-d, %Y").to_string())
                .unwrap_or_default();
            if day != cur_day {
                list = list.child(div().pt_4().text_lg().font_weight(FontWeight::BOLD).text_color(p.text).child(day.clone()));
                cur_day = day;
            }
            let id = h.video.id.clone();
            list = list.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(video_row(&h.video, px(246.), cx)))
                    .child(icon_button(SharedString::from(format!("hx-{}", h.video.id)), "x", cx).on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        store::history_remove(&id);
                        cx.default_global::<Watched>().0.remove(&id);
                        crate::ui::app::refresh_all_pages(cx);
                    }))),
            );
        }
        if self.tab == HistTab::Account {
            list = self.account_list(&q, cx);
        } else if self.tab == HistTab::Searches {
            list = self.search_list(&q, cx);
        } else if entries.is_empty() {
            list = list.child(empty_state("history", if q.is_empty() { "Your watch history is empty" } else { "No matches" }, "", cx));
        }
        let side = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(320.))
            .flex_none()
            .child(div().flex().items_center().gap_2().border_b_1().border_color(p.border).pb_1().child(icon("search").size_4().text_color(p.text2)).child(Input::new(&self.filter).appearance(false).cleanable(true)))
            .child(
                pill("h-clear", Some("trash-2"), "Clear all watch history", cx).on_click(|_, window, cx| {
                    confirm("Clear watch history?", "All watch history on this device and resume positions will be deleted.", "Clear", window, cx, |_, cx| {
                        store::history_clear();
                        cx.default_global::<Watched>().0.clear();
                        crate::ui::app::refresh_all_pages(cx);
                    });
                }),
            )
            .child(
                pill("h-pause", Some(if s.history_enabled { "pause" } else { "play" }), if s.history_enabled { "Pause watch history" } else { "Turn on watch history" }, cx).on_click(cx.listener(|_, _, _, cx| {
                    store::update_settings(|s| s.history_enabled ^= true);
                    cx.notify();
                })),
            )
            .child(
                pill("h-search", Some("search-x"), "Clear search history", cx).on_click(cx.listener(|_, _, _, cx| {
                    store::search_history_clear();
                    toast(cx, "Search history cleared");
                    cx.notify();
                })),
            );
        page_scroll("hist-scroll", &self.scroll).child(
            div().flex().justify_center().px_6().pt_6().pb_24().child(
                div()
                    .w(width)
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(workspace_header("History", "Review and manage your watch and search history", "history", cx))
                    .child(tabs)
                    .child(div().flex().gap_8().child(list).child(side)),
            ),
        )
    }
}

pub struct LibraryPage {
    scroll: ScrollHandle,
    filter: Entity<InputState>,
    /// the signed-in account's YouTube playlists
    yt: Option<Load<Vec<crate::api::types::PlaylistItem>>>,
}

impl LibraryPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search library and history"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        let mut s = Self { scroll: ScrollHandle::new(), filter, yt: None };
        if crate::api::auth::signed_in() {
            s.yt = Some(Load::Loading);
            cx.spawn(async move |this, cx| {
                let res = blocking(crate::api::auth::my_playlists).await;
                let _ = this.update(cx, |t, cx| {
                    t.yt = Some(match res {
                        Ok(l) => Load::Ready(l),
                        Err(e) => Load::Error(e.to_string()),
                    });
                    cx.notify();
                });
            })
            .detach();
        }
        s
    }
}

impl Render for LibraryPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let width = content_width(cx);
        let (_, w) = grid_columns(width, 260.0);
        let pls = store::playlists();
        let q = self.filter.read(cx).value().trim().to_lowercase();
        let count = pls.iter().filter(|pl| pl.name.to_lowercase().contains(&q)).count();
        let total_videos: usize = pls.iter().map(|pl| pl.count as usize).sum();
        let mut cards: Vec<AnyElement> = pls
            .iter().filter(|pl| pl.name.to_lowercase().contains(&q))
            .map(|pl| {
                let id = pl.id;
                div()
                    .id(SharedString::from(format!("lp-{id}")))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(w)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| navigate(Route::LocalPlaylist(id), window, cx))
                    .child(playlist_thumb(pl.thumbnail.clone(), Some(format!("{} videos", pl.count)), w, w * (9. / 16.), cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(pl.protected, |d| d.child(icon(if pl.name == "Watch later" { "clock" } else { "heart" }).size_4().text_color(p.text2)))
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(pl.name.clone())),
                    )
                    .child(div().text_xs().text_color(p.text2).child("Local • Private"))
                    .into_any_element()
            })
            .collect();
        cards.push(
            div()
                .id("lp-new")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .w(w)
                .h(w * (9. / 16.))
                .rounded_xl()
                .border_2()
                .border_dashed()
                .border_color(p.border)
                .cursor_pointer()
                .hover(move |s| s.bg(p.surface))
                .child(icon("plus").size_8().text_color(p.text2))
                .child(div().text_sm().text_color(p.text2).child("New playlist"))
                .on_click(|_, window, cx| {
                    prompt_text("New playlist", "Playlist name", window, cx, |name, _, cx| {
                        store::playlist_create(&name);
                        cx.refresh_windows();
                    });
                })
                .into_any_element(),
        );
        let saved: Vec<_> = store::saved_playlists().into_iter().filter(|pl| pl.title.to_lowercase().contains(&q)).collect();
        let history: Vec<_> = store::history_list(&q, 12).into_iter().map(|h| h.video).collect();
        let recovery_enabled = store::settings_snapshot().wetube_recover_playback;
        let (_, hw) = grid_columns(width, 220.0);
        page_scroll("lib-scroll", &self.scroll).child(
            div()
                .flex()
                .flex_col()
                .gap_10()
                .px_6()
                .pt_6()
                .pb_24()
                .child(div().flex().flex_col().gap_3().p_5().rounded_xl().bg(p.surface).border_1().border_color(p.border)
                    .child(workspace_header("Your library", "Playlists, saved videos and playback recovery", "library", cx))
                    .child(div().text_sm().text_color(p.text2).child(format!("{} local playlists · {} saved entries · on this device", pls.len(), total_videos)))
                    .child(div().flex().flex_wrap().gap_2()
                        .child(pill("lib-go-history", Some("history"), "Watch history", cx).on_click(|_, window, cx| navigate(Route::History, window, cx)))
                        .child(pill("lib-new-playlist", Some("plus"), "New playlist", cx).on_click(|_, window, cx| {
                            prompt_text("New playlist", "Playlist name", window, cx, |name, _, cx| { store::playlist_create(&name); cx.refresh_windows(); });
                        })))
                    .child(div().flex().items_center().gap_2().child(icon("search").size_4().text_color(p.text2)).child(Input::new(&self.filter).appearance(false).cleanable(true)))
                    .child(div().text_xs().text_color(p.text2).child(format!("Showing {count} of {} playlists", pls.len())))
                    .child(div().flex().items_center().gap_2().child(pill("wetube-restore-queue", Some("list-video"), "Play saved queue", cx).on_click(|_, window, cx| {
                        match crate::wetube_queue::load() {
                            Ok(Some((videos, selected))) => {
                                let id = videos[selected].id.clone();
                                crate::player::controller::player(cx).update(cx, |p, cx| { p.queue = videos; p.play_queue_index(selected, cx); });
                                navigate(Route::Watch { id, playlist: None, start: None }, window, cx);
                            }
                            Ok(None) => toast(cx, "No saved queue. Save one from the watch page."),
                            Err(e) => toast(cx, e.to_string()),
                        }
                    })).child(div().text_xs().text_color(p.text2).child("Restore explicitly; never starts on launch")))
                    .when(recovery_enabled, |d| d.child(div().flex().flex_wrap().items_center().gap_2()
                        .child(pill("wetube-resume-recovery", Some("history"), "Resume last playback", cx).on_click(|_, window, cx| {
                            match crate::wetube_recovery::load() {
                                Ok(Some(r)) => {
                                    let video = r.queue[r.selected].clone();
                                    let id = video.id.clone();
                                    crate::player::controller::player(cx).update(cx, |p, cx| {
                                        p.queue = r.queue; p.queue_pos = Some(r.selected); p.play(video, Some(r.position), cx);
                                    });
                                    navigate(Route::Watch { id, playlist: None, start: None }, window, cx);
                                }
                                Ok(None) => toast(cx, "No recovery snapshot yet"),
                                Err(e) => toast(cx, e.to_string()),
                            }
                        })).child(div().text_xs().text_color(p.text2).child("Private checkpoint · up to 200 items")))))
                .when(!history.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(section_title("History", cx))
                                    .child(pill("lib-hist", None, "View all", cx).on_click(|_, window, cx| navigate(Route::History, window, cx))),
                            )
                            .child(div().flex().gap_4().overflow_hidden().children(history.iter().take(6).map(|v| video_card(v, hw, true, cx)))),
                    )
                })
                .when_some(self.yt.as_ref().map(|l| match l {
                    Load::Loading => spinner(cx).into_any_element(),
                    Load::Error(e) => div().text_sm().text_color(p.text2).child(format!("Could not load your YouTube playlists: {e}")).into_any_element(),
                    Load::Ready(l) if l.is_empty() => div().text_sm().text_color(p.text2).child("No playlists on your account yet").into_any_element(),
                    Load::Ready(l) => grid(l.iter().filter(|pl| pl.title.to_lowercase().contains(&q)).map(|pl| playlist_card(pl, w, cx)).collect(), width).into_any_element(),
                }), |d, body| {
                    d.child(div().flex().flex_col().gap_4().child(section_title("Your YouTube playlists", cx)).child(body))
                })
                .child(div().flex().flex_col().gap_4().child(section_title("On this computer", cx)).child(grid(cards, width)))
                .when(!saved.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(section_title("Saved playlists", cx))
                            .child(grid(saved.iter().map(|pl| playlist_card(pl, w, cx)).collect(), width)),
                    )
                }),
        )
    }
}
