use crate::api::innertube;
use crate::api::json::compact_number;
use crate::api::services::{self, Votes};
use crate::api::types::*;
use crate::player::controller::{PlayerEvent, player};
use crate::store;
use crate::ui::app::{AutoplayNext, MenuItem, Route, UiState, navigate, open_menu};
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use std::collections::HashMap;

/// Live chat of a running stream, polled while the page is open.
struct LiveChat {
    messages: Vec<ChatMessage>,
    error: Option<String>,
    /// "Top chat" (YouTube's filtered view) or every message
    top: bool,
    hidden: bool,
    /// bumped to stop the running poll loop when switching views
    generation: u64,
    sending: bool,
    input: Entity<InputState>,
    scroll: ScrollHandle,
    _input_sub: Subscription,
}

const CHAT_KEEP: usize = 250;

pub struct WatchPage {
    id: String,
    playlist_id: Option<String>,
    info: Load<WatchInfo>,
    votes: Option<Votes>,
    /// the like state when the counts were fetched, already part of them
    counted_rating: i8,
    desc_expanded: bool,
    comments: Option<Load<CommentPage>>,
    comments_more_loading: bool,
    comment_sort_new: bool,
    /// "Timed" view: comments that mention a timestamp, shown once playback reaches it
    comment_timed: bool,
    /// While the timed view is open: refreshes it as playback moves and loads more
    /// comments to find timestamped ones. Dropped (stopped) when the view closes.
    timed_ticker: Option<Task<()>>,
    /// How many timed comments were last shown (repaint only when it changes)
    timed_shown: usize,
    replies: HashMap<String, Load<(Vec<Comment>, Option<String>)>>,
    related_more_loading: bool,
    scroll: ScrollHandle,
    local_playlist: Option<(i64, String, Vec<VideoItem>)>,
    chat: Option<LiveChat>,
    /// repaints the chapters panel when playback moves into another chapter
    chapters_ticker: Option<Task<()>>,
    chapters_shown: Option<usize>,
    _sub: Subscription,
}

/// The chapters panel beside the player is open (toggled from the player's
/// chapter title or the description; closed again for each new video).
static CHAPTERS_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn toggle_chapters(cx: &mut App) {
    CHAPTERS_OPEN.fetch_xor(true, std::sync::atomic::Ordering::SeqCst);
    cx.refresh_windows();
}

fn chapters_open() -> bool {
    CHAPTERS_OPEN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Seeks to a chapter or timestamp and brings the player back into view.
fn jump_to(t: f64, window: &mut Window, cx: &mut App) {
    player(cx).update(cx, |p, cx| {
        p.seek_to(t);
        p.set_paused(false);
        cx.notify();
    });
    scroll_page_key("home", false, window);
}

impl WatchPage {
    pub fn new(id: String, playlist_id: Option<String>, start: Option<f64>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pl = player(cx);
        let sub = cx.subscribe(&pl, |this: &mut Self, _, ev: &PlayerEvent, cx| {
            match ev {
                PlayerEvent::Ended => this.on_ended(cx),
                PlayerEvent::Ambient => cx.notify(),
                _ => {}
            }
        });
        // local playlist "local:<id>"
        let mut local_playlist = None;
        if let Some(lp) = playlist_id.as_deref().and_then(|p| p.strip_prefix("local:")).and_then(|n| n.parse::<i64>().ok()) {
            let name = store::playlists().into_iter().find(|p| p.id == lp).map(|p| p.name).unwrap_or_default();
            local_playlist = Some((lp, name, store::playlist_items(lp)));
        }
        let playing = pl.read(cx).video.as_ref().map(|v| v.id.clone());
        if playing.as_deref() != Some(id.as_str()) {
            let stub = VideoItem { id: id.clone(), thumbnail: innertube::default_thumb(&id), ..Default::default() };
            pl.update(cx, |p, cx| {
                // keep queue if this video is in it
                if let Some(pos) = p.queue.iter().position(|v| v.id == id) {
                    p.queue_pos = Some(pos);
                } else if let Some((_, _, items)) = &local_playlist {
                    p.queue = items.clone();
                    p.queue_pos = items.iter().position(|v| v.id == id);
                } else {
                    p.queue.clear();
                    p.queue_pos = None;
                }
                p.play(stub, start, cx);
            });
        } else if let Some(t) = start {
            pl.update(cx, |p, _| p.seek_to(t));
        }
        cx.set_global(AutoplayNext(None));
        let mut s = Self {
            id,
            playlist_id,
            info: Load::Loading,
            votes: None,
            counted_rating: 0,
            desc_expanded: false,
            comments: None,
            comments_more_loading: false,
            comment_sort_new: false,
            comment_timed: false,
            timed_ticker: None,
            timed_shown: 0,
            replies: HashMap::new(),
            related_more_loading: false,
            scroll: ScrollHandle::new(),
            local_playlist,
            chat: None,
            chapters_ticker: None,
            chapters_shown: None,
            _sub: sub,
        };
        CHAPTERS_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
        s.make_chat(window, cx);
        s.load(cx);
        s
    }

    fn make_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in = crate::api::auth::signed_in();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(if signed_in { "Chat…" } else { "Sign in to chat" }));
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, st, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let text = st.read(cx).value().to_string();
                this.send_chat(text, window, cx);
            }
        });
        self.chat = Some(LiveChat {
            messages: Vec::new(),
            error: None,
            top: true,
            hidden: false,
            generation: 0,
            sending: false,
            input,
            scroll: ScrollHandle::new(),
            _input_sub: sub,
        });
    }

    /// Starts (or restarts, after a view switch) polling the live chat.
    fn start_chat(&mut self, cx: &mut Context<Self>) {
        let Some((top_tok, all_tok)) = self.info.ready().and_then(|w| w.live_chat.clone()) else { return };
        let Some(chat) = &mut self.chat else { return };
        chat.generation += 1;
        chat.messages.clear();
        chat.error = None;
        let generation = chat.generation;
        let mut token = if chat.top { top_tok } else { all_tok };
        cx.spawn(async move |this, cx| {
            loop {
                let t = token.clone();
                let res = blocking(move || innertube::live_chat(&t)).await;
                let mut wait = 5000;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let Some(chat) = &mut this.chat else { return false };
                        if chat.generation != generation {
                            return false;
                        }
                        let at_bottom = {
                            let off: f32 = (-chat.scroll.offset().y).into();
                            let max: f32 = chat.scroll.max_offset().y.into();
                            max <= 0.0 || off >= max - 40.0
                        };
                        let go = match res {
                            Ok(b) => {
                                chat.error = None;
                                let seen: std::collections::HashSet<String> = chat.messages.iter().rev().take(300).map(|m| m.id.clone()).collect();
                                chat.messages.extend(b.messages.into_iter().filter(|m| m.id.is_empty() || !seen.contains(&m.id)));
                                if chat.messages.len() > CHAT_KEEP {
                                    let cut = chat.messages.len() - CHAT_KEEP;
                                    chat.messages.drain(..cut);
                                }
                                wait = b.timeout_ms.clamp(1000, 10000);
                                match b.next {
                                    Some(n) => {
                                        token = n;
                                        true
                                    }
                                    None => {
                                        chat.error = Some("Chat has ended".into());
                                        false
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = e;
                                chat.error = Some("Could not load chat, retrying…".into());
                                wait = 10000;
                                true
                            }
                        };
                        if at_bottom {
                            chat.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                        go
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
            }
        })
        .detach();
    }

    fn send_chat(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if !crate::api::auth::signed_in() {
            toast(cx, "Sign in to chat");
            return;
        }
        let Some(w) = self.info.ready() else { return };
        let (channel, video) = (w.channel.id.clone(), w.id.clone());
        let Some(chat) = &mut self.chat else { return };
        if chat.sending {
            return;
        }
        chat.sending = true;
        chat.input.update(cx, |st, cx| st.set_value("", window, cx));
        cx.spawn(async move |this, cx| {
            let t = text.clone();
            let res = blocking(move || crate::api::auth::send_live_chat(&channel, &video, &t)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(chat) = &mut this.chat {
                    chat.sending = false;
                }
                if let Err(e) = res {
                    toast(cx, format!("Message not sent: {e}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_chat(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.info.ready().map(|w| w.live_chat.is_some()).unwrap_or(false);
        let p = pal(cx);
        let chat = self.chat.as_ref()?;
        if !live {
            return None;
        }
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(p.border)
            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Live chat"))
            .child(chip("chat-top", "Top", chat.top, cx).on_click(cx.listener(|this, _, _, cx| {
                if let Some(c) = &mut this.chat {
                    if !c.top {
                        c.top = true;
                        this.start_chat(cx);
                    }
                }
            })))
            .child(chip("chat-all", "All", !chat.top, cx).on_click(cx.listener(|this, _, _, cx| {
                if let Some(c) = &mut this.chat {
                    if c.top {
                        c.top = false;
                        this.start_chat(cx);
                    }
                }
            })))
            .child(
                icon_button("chat-hide", if chat.hidden { "chevron-down" } else { "chevron-up" }, cx)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(c) = &mut this.chat {
                            c.hidden ^= true;
                        }
                        cx.notify();
                    })),
            );
        let mut panel = div().flex().flex_col().mb_4().rounded_xl().border_1().border_color(p.border).overflow_hidden().child(header);
        if chat.hidden {
            return Some(panel.into_any_element());
        }
        let mut list = div().id("chat-list").h(px(440.)).overflow_y_scroll().track_scroll(&chat.scroll).flex().flex_col().gap_1().px_3().py_2();
        if chat.messages.is_empty() {
            list = list.child(div().py_6().flex().justify_center().text_sm().text_color(p.text2).child(chat.error.clone().unwrap_or_else(|| "Loading chat…".into())));
        }
        for m in &chat.messages {
            let name_color = match m.role {
                ChatRole::Owner => rgb(0xffd600).into(),
                ChatRole::Moderator => rgb(0x5e84f1).into(),
                ChatRole::Member => rgb(0x2ba640).into(),
                ChatRole::Viewer => p.text2,
            };
            let mut row = div()
                .flex()
                .items_start()
                .gap_2()
                .py_1()
                .child(avatar(m.author_photo.as_deref(), px(24.), &m.author, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .items_center()
                                .child(div().text_color(name_color).font_weight(FontWeight::MEDIUM).child(m.author.clone()))
                                .when(m.role == ChatRole::Moderator, |d| d.child(icon("wrench").size_3().text_color(name_color)))
                                .when(m.verified, |d| d.child(icon("circle-check").size_3().text_color(p.text2)))
                                .when_some(m.paid.clone(), |d, a| d.child(div().px_1p5().rounded_md().bg(rgb(0xe62117)).text_color(white()).text_xs().font_weight(FontWeight::BOLD).child(a))),
                        )
                        .child(div().text_color(p.text).child(m.text.clone())),
                );
            if m.paid.is_some() {
                row = row.px_2().rounded_lg().bg(p.hover);
            }
            list = list.child(row);
        }
        if let (Some(e), false) = (&chat.error, chat.messages.is_empty()) {
            list = list.child(div().py_2().text_xs().text_color(p.text3).child(e.clone()));
        }
        let signed_in = crate::api::auth::signed_in();
        let input_row = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(p.border)
            .child(div().flex_1().min_w_0().child(Input::new(&chat.input).disabled(!signed_in || chat.sending)))
            .when(signed_in, |d| {
                d.child(icon_button("chat-send", "send", cx).on_click(cx.listener(|this, _, window, cx| {
                    if let Some(c) = &this.chat {
                        let t = c.input.read(cx).value().to_string();
                        this.send_chat(t, window, cx);
                    }
                })))
            });
        panel = panel.child(nested_scroll("chat-wrap", list)).child(input_row);
        Some(panel.into_any_element())
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let pid = self.playlist_id.clone().filter(|p| !p.starts_with("local:"));
        let ryd = store::settings_snapshot().ryd;
        cx.spawn(async move |this, cx| {
            let id2 = id.clone();
            let info_f = blocking(move || innertube::watch_next(&id2, pid.as_deref()));
            let id3 = id.clone();
            let votes_f = blocking(move || if ryd { services::ryd_votes(&id3).ok() } else { None });
            let (info, votes) = futures::join!(info_f, votes_f);
            let _ = this.update(cx, |this, cx| {
                this.votes = votes;
                this.counted_rating = store::rating(&this.id);
                match info {
                    Ok(w) => {
                        let s = store::settings_snapshot();
                        let pl = player(cx);
                        let chapters = w.chapters.clone();
                        let heatmap = w.heatmap.clone();
                        let full = VideoItem {
                            id: w.id.clone(),
                            title: w.title.clone(),
                            channel_name: w.channel.name.clone(),
                            channel_id: Some(w.channel.id.clone()).filter(|c| !c.is_empty()),
                            channel_avatar: w.channel.avatar.clone(),
                            thumbnail: innertube::default_thumb(&w.id),
                            views_text: Some(w.views_text.clone()),
                            published_text: Some(w.date_text.clone()),
                            is_live: w.is_live,
                            ..Default::default()
                        };
                        pl.update(cx, |p, cx| {
                            if p.video.as_ref().map(|v| v.id == full.id).unwrap_or(false) {
                                p.chapters = chapters;
                                p.heatmap = heatmap;
                                let dur = p.video.as_ref().and_then(|v| v.duration_secs);
                                let mut v = full.clone();
                                v.duration_secs = dur.or(Some(p.duration as u64));
                                v.duration_text = v.duration_secs.filter(|d| *d > 0).map(crate::api::json::format_duration);
                                if p.channel_id.is_none() {
                                    p.channel_id = v.channel_id.clone();
                                }
                                p.video = Some(v.clone());
                                store::history_add(&v);
                                // playlist queue
                                if let Some(wp) = &w.playlist {
                                    if !wp.items.is_empty() {
                                        p.queue = wp.items.clone();
                                        p.queue_pos = wp.items.iter().position(|x| x.id == full.id);
                                    }
                                }
                                if !p.loading { p.checkpoint_recovery(); }
                                cx.notify();
                            }
                        });
                        let next = w
                            .related
                            .iter()
                            .filter_map(|i| i.as_video())
                            .find(|v| !v.is_short && !v.is_live && !store::is_filtered(v, &s))
                            .cloned();
                        if let Some(n) = &next {
                            // warm up the autoplay candidate so the next video starts instantly
                            let id = n.id.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_secs(8));
                                crate::api::streams::prefetch(&id);
                            });
                        }
                        cx.set_global(AutoplayNext(next));
                        let comments_token = w.comments_token.clone();
                        let has_chat = w.live_chat.is_some();
                        this.info = Load::Ready(w);
                        if has_chat {
                            this.start_chat(cx);
                        }
                        if !s.hide_comments {
                            if let Some(t) = comments_token {
                                this.load_comments(t, false, cx);
                            }
                        }
                    }
                    Err(e) => this.info = Load::Error(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_ended(&mut self, cx: &mut Context<Self>) {
        let s = store::settings_snapshot();
        let next = cx.global::<AutoplayNext>().0.clone().filter(|_| s.autoplay_next);
        let looping = player(cx).read(cx).loop_video;
        if next.is_none() && !looping && s.exit_fullscreen_at_end && cx.global::<UiState>().fullscreen {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, cx| crate::ui::app::toggle_fullscreen(window, cx));
            }
        }
        if !s.autoplay_next {
            return;
        }
        if let Some(v) = next {
            toast(cx, format!("Up next: {}", v.title));
            let id = v.id.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                let _ = cx.update(|cx| {
                    let still_ended = player(cx).read(cx).eof;
                    if still_ended {
                        if let Some(w) = cx.active_window() {
                            let _ = w.update(cx, |_, window, cx| navigate(Route::Watch { id, playlist: None, start: None }, window, cx));
                        }
                    }
                });
            })
            .detach();
        }
    }

    /// Comments already loaded that mention a timestamp, earliest first.
    fn stamped(&self) -> Vec<(f64, Comment)> {
        let Some(Load::Ready(data)) = &self.comments else { return Vec::new() };
        let mut v: Vec<(f64, Comment)> = data.comments.iter().filter_map(|c| first_timestamp(&c.text).map(|t| (t, c.clone()))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    fn set_timed(&mut self, on: bool, cx: &mut Context<Self>) {
        self.comment_timed = on;
        self.timed_shown = 0;
        self.timed_ticker = on.then(|| {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let alive = this.update(cx, |this, cx| {
                    if !this.comment_timed {
                        return false;
                    }
                    let now = player(cx).read(cx).time;
                    let shown = this.stamped().iter().filter(|(t, _)| *t <= now + 0.5).count();
                    if shown != this.timed_shown {
                        this.timed_shown = shown;
                        cx.notify();
                    }
                    // keep loading pages (up to a few hundred comments) to find more timestamps
                    if let Some(Load::Ready(c)) = &this.comments {
                        if let (Some(tok), false, true) = (c.continuation.clone(), this.comments_more_loading, c.comments.len() < 400) {
                            this.comments_more_loading = true;
                            this.load_comments(tok, true, cx);
                        }
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            })
        });
        cx.notify();
    }

    fn load_comments(&mut self, token: String, append: bool, cx: &mut Context<Self>) {
        if !append {
            self.comments = Some(Load::Loading);
        }
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::comments(&token)).await;
            let _ = this.update(cx, |this, cx| {
                this.comments_more_loading = false;
                match (res, append) {
                    (Ok(page), true) => {
                        if let Some(Load::Ready(c)) = this.comments.as_mut() {
                            c.comments.extend(page.comments);
                            c.continuation = page.continuation;
                        }
                    }
                    (Ok(mut page), false) => {
                        if page.sort_top_token.is_none() && page.sort_new_token.is_none() {
                            if let Some(w) = this.info.ready() {
                                (page.sort_top_token, page.sort_new_token) = w.comments_sort.clone();
                            }
                        }
                        this.comments = Some(Load::Ready(page))
                    }
                    (Err(e), false) => this.comments = Some(Load::Error(e.to_string())),
                    (Err(_), true) => {
                        if let Some(Load::Ready(c)) = this.comments.as_mut() {
                            c.continuation = None;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_replies(&mut self, comment_id: String, token: String, cx: &mut Context<Self>) {
        let append = matches!(self.replies.get(&comment_id), Some(Load::Ready(_)));
        if !append {
            self.replies.insert(comment_id.clone(), Load::Loading);
        }
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::comments(&token)).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(p) => {
                        if let Some(Load::Ready((list, cont))) = this.replies.get_mut(&comment_id) {
                            list.extend(p.comments);
                            *cont = p.continuation;
                        } else {
                            this.replies.insert(comment_id, Load::Ready((p.comments, p.continuation)));
                        }
                    }
                    Err(e) => {
                        this.replies.insert(comment_id, Load::Error(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more_related(&mut self, cx: &mut Context<Self>) {
        let Some(tok) = self.info.ready().and_then(|w| w.related_continuation.clone()) else {
            self.related_more_loading = false;
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::related_continue(&tok)).await;
            let _ = this.update(cx, |this, cx| {
                this.related_more_loading = false;
                if let (Ok(p), Some(w)) = (res, this.info.ready_mut()) {
                    w.related.extend(p.items);
                    w.related_continuation = p.continuation;
                } else if let Some(w) = this.info.ready_mut() {
                    w.related_continuation = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ───── sections ─────

    fn render_info(&mut self, w: &WatchInfo, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        let subscribed = store::is_subscribed(&w.channel.id);
        let rating = store::rating(&w.id);
        // the fetched counts already hold the vote made before they were fetched;
        // move them by whatever changed since, shown exactly so a vote is visible
        let shift = |n: u64, was: bool, now: bool| (n + now as u64).saturating_sub(was as u64);
        let was = self.counted_rating;
        let base_likes = self.votes.as_ref().map(|v| v.likes).or_else(|| w.likes_text.as_deref().and_then(crate::api::json::parse_count));
        let like_n = base_likes.map(|n| shift(n, was == 1, rating == 1));
        let dislike_n = self.votes.as_ref().map(|v| shift(v.dislikes, was == -1, rating == -1));
        let likes = like_n
            .map(vote_count)
            .or_else(|| w.likes_text.clone().filter(|t| !t.chars().any(|c| c.is_ascii_digit())))
            .unwrap_or_else(|| "Like".into());
        let dislikes = dislike_n.map(vote_count);
        let ratio = like_n.zip(dislike_n).filter(|(l, d)| l + d > 0).map(|(l, d)| l as f32 / (l + d) as f32);
        let video_item = player(cx).read(cx).video.clone().unwrap_or_else(|| VideoItem { id: w.id.clone(), title: w.title.clone(), ..Default::default() });
        let cid = w.channel.id.clone();
        let title = if s.dearrow_titles {
            services::branding_cached(&w.id).flatten().and_then(|b| b.title).unwrap_or(w.title.clone())
        } else {
            w.title.clone()
        };
        let v_dl = video_item.clone();
        let v_save = video_item.clone();
        let v_more = video_item.clone();
        let share_id = w.id.clone();
        let channel = w.channel.clone();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .pt_3()
            .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(p.text).child(title))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .id("w-chan")
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .cursor_pointer()
                                    .on_click(move |_, window, cx| navigate(Route::Channel(cid.clone()), window, cx))
                                    .child(avatar(w.channel.avatar.as_deref(), px(40.), &w.channel.name, cx))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(w.channel.name.clone()))
                                            .when_some(w.channel.subscribers_text.clone(), |d, t| d.child(div().text_xs().text_color(p.text2).child(t))),
                                    ),
                            )
                            .child(div().ml_3().child(subscribe_button("w-sub".into(), &channel, subscribed, cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .h_9()
                                            .rounded_full()
                                            .bg(p.chip)
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.text)
                                            .child(
                                                div()
                                                    .id("w-like")
                                                    .flex()
                                                    .items_center()
                                                    .h_full()
                                                    .gap_2()
                                                    .pl_4()
                                                    .pr_3()
                                                    .rounded_l_full()
                                                    .cursor_pointer()
                                                    .when(rating == 1, |d| d.bg(p.hover))
                                                    .hover(move |st| st.bg(p.hover))
                                                    .child(icon("thumbs-up").size_5().text_color(if rating == 1 { p.accent } else { p.text }))
                                                    .child(likes)
                                                    .on_click({
                                                        let id = w.id.clone();
                                                        move |_, _, cx| set_rating(&id, if rating == 1 { 0 } else { 1 }, cx)
                                                    }),
                                            )
                                            .child(div().w(px(1.)).h_6().bg(p.border))
                                            .child(
                                                div()
                                                    .id("w-dislike")
                                                    .flex()
                                                    .items_center()
                                                    .h_full()
                                                    .gap_2()
                                                    .pl_3()
                                                    .pr_4()
                                                    .rounded_r_full()
                                                    .cursor_pointer()
                                                    .when(rating == -1, |d| d.bg(p.hover))
                                                    .hover(move |st| st.bg(p.hover))
                                                    .child(icon("thumbs-down").size_5().text_color(if rating == -1 { p.accent } else { p.text }))
                                                    .when_some(dislikes, |d, t| d.child(t))
                                                    .on_click({
                                                        let id = w.id.clone();
                                                        move |_, _, cx| set_rating(&id, if rating == -1 { 0 } else { -1 }, cx)
                                                    }),
                                            ),
                                    )
                                    .when_some(ratio, |d, r| {
                                        d.child(div().mx_4().mt_1().h(px(2.)).rounded_full().bg(p.text3).child(div().h_full().w(relative(r)).bg(p.text)))
                                    }),
                            )
                            .child(pill("w-share", Some("share-2"), "Share", cx).on_click(move |_, _, cx| {
                                copy_to_clipboard(format!("https://youtu.be/{share_id}"), cx)
                            }))
                            .child(pill("w-dl", Some("download"), "Download", cx).on_click(move |ev, window, cx| {
                                let items: Vec<MenuItem> = crate::downloads::FORMATS
                                    .iter()
                                    .map(|(f, label)| {
                                        let v = v_dl.clone();
                                        let f = f.to_string();
                                        MenuItem::new(if f == "audio" { "music" } else { "film" }, *label, move |_, cx| {
                                            crate::downloads::start(v.clone(), Some(f.clone()), cx)
                                        })
                                    })
                                    .collect();
                                open_menu(ev.position(), items, window, cx);
                            }))
                            .child(pill("w-save", Some("bookmark"), "Save", cx).on_click(move |_, window, cx| {
                                crate::ui::app::open_save_dialog(v_save.clone(), window, cx)
                            }))
                            .child(
                                pill("w-more", Some("ellipsis"), "", cx).w_9().px_0().justify_center().on_click(move |ev, window, cx| {
                                    open_menu(ev.position(), video_menu_items(&v_more), window, cx);
                                }),
                            ),
                    ),
            )
            .when(!s.hide_description, |d| d.child(self.render_description(w, cx)))
            .into_any_element()
    }

    fn render_description(&mut self, w: &WatchInfo, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let expanded = self.desc_expanded;
        let s = store::settings_snapshot();
        let views = if s.hide_view_counts { String::new() } else { w.views_text.clone() };
        let desc = if expanded {
            w.description.clone()
        } else {
            w.description.lines().take(3).collect::<Vec<_>>().join("\n")
        };
        let chapters = w.chapters.clone();
        div()
            .id("desc")
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_xl()
            .bg(p.chip)
            .when(!expanded, |d| d.cursor_pointer().hover(move |s| s.bg(p.hover)))
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.desc_expanded {
                    this.desc_expanded = true;
                    cx.notify();
                }
            }))
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(format!("{views}  {}", w.date_text)))
            .child(div().text_sm().text_color(p.text).child(rich_text("desc-text", desc, cx)))
            .when(expanded && !chapters.is_empty() && !s.hide_chapters, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pt_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Chapters"))
                                .child(
                                    div()
                                        .id("chap-all")
                                        .px_3()
                                        .py_1()
                                        .rounded_full()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.link)
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(p.hover))
                                        .child("View all")
                                        .on_click(|_, window, cx| {
                                            cx.stop_propagation();
                                            CHAPTERS_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
                                            scroll_page_key("home", false, window);
                                            cx.refresh_windows();
                                        }),
                                ),
                        )
                        .child(div().flex().gap_2().overflow_hidden().children(chapters.into_iter().enumerate().map(|(i, c)| {
                            let t = c.start;
                            div()
                                .id(SharedString::from(format!("chap-{i}")))
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w(px(160.))
                                .gap_1()
                                .p_1()
                                .rounded_lg()
                                .cursor_pointer()
                                .hover(move |s| s.bg(p.hover))
                                .when_some(c.thumbnail.clone(), |d, th| d.child(img(SharedString::from(th)).w(px(152.)).h(px(86.)).rounded_md()))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(c.title.clone()))
                                .child(div().text_xs().text_color(p.link).child(crate::api::json::format_duration(t as u64)))
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    jump_to(t, window, cx)
                                })
                        }))),
                )
            })
            .child(
                div()
                    .id("desc-toggle")
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .cursor_pointer()
                    .child(if expanded { "Show less" } else { "...more" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.desc_expanded ^= true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_comment(&mut self, c: &Comment, is_reply: bool, cx: &mut Context<Self>) -> AnyElement {
        self.render_comment_t(c, is_reply, false, cx)
    }

    /// A reply and, hanging under it, the replies that answer it.
    fn reply_subtree(&mut self, list: &[Comment], parents: &[Option<usize>], i: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let kids: Vec<usize> = (i + 1..list.len()).filter(|&j| parents[j] == Some(i)).collect();
        let me = self.render_comment_t(&list[i], true, !kids.is_empty(), cx);
        if kids.is_empty() {
            return me;
        }
        let mut col = div().flex().flex_col().child(me);
        for (k, &j) in kids.iter().enumerate() {
            col = col.child(thread_branch(self.reply_subtree(list, parents, j, cx), k + 1 == kids.len(), 24., p.border));
        }
        col.into_any_element()
    }

    /// `thread`: draw the thread line down from the avatar (replies hang below).
    fn render_comment_t(&mut self, c: &Comment, is_reply: bool, thread: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let cid = c.id.clone();
        let token = c.replies_token.clone();
        let replies = self.replies.get(&c.id);
        let author_id = c.author_id.clone();
        let mut el = div()
            .flex()
            .gap_4()
            .child({
                let a = avatar(c.author_avatar.as_deref(), px(if is_reply { 24. } else { 40. }), &c.author, cx).into_any_element();
                if thread || (!is_reply && replies.is_some()) {
                    // the thread line starts under the avatar of an opened comment
                    div().flex().flex_col().items_center().flex_none().child(a).child(div().flex_1().w(px(2.)).mt_1().bg(p.border)).into_any_element()
                } else {
                    a
                }
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .when(c.is_pinned, |d| d.child(div().flex().items_center().gap_1().text_xs().text_color(p.text2).child(icon("pin").size_3()).child("Pinned")))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("ca-{}", c.id)))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .when(c.is_creator, |d| d.px_2().rounded_full().bg(p.hover))
                                    .text_color(p.text)
                                    .child(c.author.clone())
                                    .on_click(move |_, window, cx| {
                                        if let Some(a) = author_id.clone() {
                                            navigate(Route::Channel(a), window, cx)
                                        }
                                    }),
                            )
                            .when(c.is_verified, |d| d.child(icon("badge-check").size_3().text_color(p.text2)))
                            .child(div().text_xs().text_color(p.text2).child(c.published.clone())),
                    )
                    .child(div().text_sm().text_color(p.text).child(comment_body(format!("ct-{}", c.id), c, cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .text_xs()
                            .text_color(p.text2)
                            .child(div().flex().items_center().gap_1().child(icon("thumbs-up").size_4().text_color(p.text2)).child(c.likes.clone()))
                            .child(icon("thumbs-down").size_4().text_color(p.text2))
                            .when(c.hearted, |d| d.child(icon("heart").size_4().text_color(rgb(0xff0033)))),
                    )
                    .when_some(token.filter(|_| !is_reply), |d, tok| {
                        let open = replies.is_some();
                        let cid2 = cid.clone();
                        d.child(
                            div()
                                .id(SharedString::from(format!("cr-btn-{}", c.id)))
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1p5()
                                .rounded_full()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.link)
                                .cursor_pointer()
                                .hover(move |s| s.bg(p.hover))
                                .child(icon(if open { "chevron-up" } else { "chevron-down" }).size_4().text_color(p.link))
                                .child(format!("{} {}", c.reply_count, if c.reply_count == "1" { "reply" } else { "replies" }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.replies.contains_key(&cid2) {
                                        this.replies.remove(&cid2);
                                        cx.notify();
                                    } else {
                                        this.load_replies(cid2.clone(), tok.clone(), cx);
                                    }
                                })),
                        )
                    }),
            )
            .into_any_element();
        if let Some(r) = replies {
            let r: Option<Result<(Vec<Comment>, Option<String>), String>> = match r {
                Load::Loading => None,
                Load::Error(e) => Some(Err(e.clone())),
                Load::Ready((l, c)) => Some(Ok((l.clone(), c.clone()))),
            };
            let mut col = div().flex().flex_col();
            match r {
                None => col = col.child(thread_branch(spinner(cx).into_any_element(), true, 40., p.border)),
                Some(Err(e)) => col = col.child(thread_branch(div().text_xs().text_color(p.text2).child(e).into_any_element(), true, 40., p.border)),
                Some(Ok((list, cont))) => {
                    // replies to replies nest under the reply they answer
                    let parents = reply_parents(&list);
                    let roots: Vec<usize> = (0..list.len()).filter(|&i| parents[i].is_none()).collect();
                    for (k, &i) in roots.iter().enumerate() {
                        let last = k + 1 == roots.len() && cont.is_none();
                        col = col.child(thread_branch(self.reply_subtree(&list, &parents, i, cx), last, 40., p.border));
                    }
                    if let Some(tok) = cont {
                        let cid3 = cid.clone();
                        col = col.child(thread_branch(
                            div()
                                .id(SharedString::from(format!("cr-more-{cid}")))
                                .text_sm()
                                .text_color(p.link)
                                .cursor_pointer()
                                .child("Show more replies")
                                .on_click(cx.listener(move |this, _, _, cx| this.load_replies(cid3.clone(), tok.clone(), cx)))
                                .into_any_element(),
                            true,
                            40.,
                            p.border,
                        ));
                    }
                }
            }
            el = div().flex().flex_col().child(el).child(col).into_any_element();
        }
        el
    }

    fn render_comments(&mut self, w: &WatchInfo, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        if store::settings_snapshot().hide_comments {
            return div().into_any_element();
        }
        let Some(state) = &self.comments else {
            return div().py_4().text_sm().text_color(p.text2).child(if w.comments_token.is_none() { "Comments are turned off." } else { "" }).into_any_element();
        };
        let data = match state {
            Load::Loading => return spinner(cx).into_any_element(),
            Load::Error(e) => return div().py_4().text_sm().text_color(p.text2).child(format!("Could not load comments: {e}")).into_any_element(),
            Load::Ready(c) => c.clone(),
        };
        let sort_new = self.comment_sort_new;
        let timed = self.comment_timed;
        let (top, new) = (data.sort_top_token.clone(), data.sort_new_token.clone());
        let mut col = div().flex().flex_col().gap_6().pt_6().child(
            div()
                .flex()
                .items_center()
                .gap_6()
                .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(p.text).child({
                    let n = data.count_text.clone().or(w.comment_count_text.clone()).unwrap_or_default();
                    if n.to_lowercase().contains("comment") { n } else { format!("{n} Comments").trim().to_string() }
                }))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .when_some(top, |d, t| {
                            d.child(chip("c-top", "Top", !sort_new && !timed, cx).on_click(cx.listener(move |this, _, _, cx| {
                                this.comment_sort_new = false;
                                this.set_timed(false, cx);
                                this.load_comments(t.clone(), false, cx);
                            })))
                        })
                        .child(chip("c-timed", "Timed", timed, cx).on_click(cx.listener(|this, _, _, cx| {
                            let on = !this.comment_timed;
                            this.set_timed(on, cx);
                        })))
                        .when_some(new, |d, t| {
                            d.child(chip("c-new", "Newest", sort_new && !timed, cx).on_click(cx.listener(move |this, _, _, cx| {
                                this.comment_sort_new = true;
                                this.set_timed(false, cx);
                                this.load_comments(t.clone(), false, cx);
                            })))
                        }),
                ),
        );
        if timed {
            // like youtube.com: a comment shows up once the video reaches the moment
            // it's about, the latest one on top
            let now = player(cx).read(cx).time;
            let stamped = self.stamped();
            let total = stamped.len();
            let mut reached: Vec<(f64, Comment)> = stamped.into_iter().filter(|(t, _)| *t <= now + 0.5).collect();
            reached.reverse();
            let later = total - reached.len();
            col = col.child(div().text_sm().text_color(p.text2).child(if total == 0 {
                if data.continuation.is_some() && data.comments.len() < 400 {
                    format!("Looking for comments with timestamps… ({} checked)", data.comments.len())
                } else {
                    format!("None of the {} comments checked mention a timestamp.", data.comments.len())
                }
            } else if reached.is_empty() {
                format!("Comments appear here as the video reaches the moment they mention ({total} so far).")
            } else if later > 0 {
                format!("{later} more later in the video")
            } else {
                String::new()
            }));
            for (i, (t, c)) in reached.iter().enumerate() {
                let t2 = *t;
                col = col.child(
                    div()
                        .flex()
                        .gap_3()
                        .p_2()
                        .rounded_lg()
                        .when(i == 0, |d| d.bg(p.hover))
                        .child(
                            div()
                                .id(SharedString::from(format!("ctime-{}", c.id)))
                                .flex_none()
                                .h_6()
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded_md()
                                .bg(p.chip)
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.link)
                                .cursor_pointer()
                                .child(crate::api::json::format_duration(*t as u64))
                                .on_click(move |_, _, cx| {
                                    player(cx).update(cx, |p, cx| {
                                        p.seek_to(t2);
                                        p.set_paused(false);
                                        cx.notify();
                                    })
                                }),
                        )
                        .child(div().flex_1().min_w_0().child(self.render_comment(c, false, cx))),
                );
            }
        } else {
            for c in &data.comments {
                col = col.child(self.render_comment(c, false, cx));
            }
        }
        if data.continuation.is_some() && self.comments_more_loading {
            col = col.child(spinner(cx));
        }
        col.into_any_element()
    }

    /// YouTube's "In this video" panel: every chapter, the one playing marked,
    /// each a click away.
    fn render_chapters_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (chapters, now) = {
            let r = player(cx).read(cx);
            (r.chapters.clone(), r.time)
        };
        if !chapters_open() || chapters.len() < 2 || store::settings_snapshot().hide_chapters {
            self.chapters_ticker = None;
            return None;
        }
        let current = chapters.iter().rposition(|c| c.start <= now + 0.01);
        self.chapters_shown = current;
        if self.chapters_ticker.is_none() {
            self.chapters_ticker = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let alive = this.update(cx, |this, cx| {
                    let r = player(cx).read(cx);
                    let cur = r.chapters.iter().rposition(|c| c.start <= r.time + 0.01);
                    if cur != this.chapters_shown {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }));
        }
        let p = pal(cx);
        let mut list = div().id("chapters-list").flex().flex_col().max_h(px(480.)).overflow_y_scroll().py_1();
        for (i, c) in chapters.iter().enumerate() {
            let t = c.start;
            let active = current == Some(i);
            list = list.child(
                div()
                    .id(SharedString::from(format!("chp-{i}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .when(active, |d| d.bg(p.hover))
                    .hover(move |s| s.bg(p.hover))
                    .child(match c.thumbnail.clone() {
                        Some(th) => img(SharedString::from(th)).w(px(112.)).h(px(63.)).flex_none().rounded_md().into_any_element(),
                        None => div()
                            .w(px(28.))
                            .flex_none()
                            .text_sm()
                            .text_center()
                            .text_color(if active { p.accent } else { p.text2 })
                            .child(if active { "▶".to_string() } else { (i + 1).to_string() })
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(c.title.clone()))
                            .child(
                                div()
                                    .w_auto()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.link)
                                    .child(crate::api::json::format_duration(t as u64)),
                            ),
                    )
                    .on_click(move |_, window, cx| jump_to(t, window, cx)),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .rounded_xl()
                .border_1()
                .border_color(p.border)
                .overflow_hidden()
                .mb_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .p_3()
                        .bg(p.surface)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_base().font_weight(FontWeight::BOLD).text_color(p.text).child("Chapters"))
                                .child(div().text_xs().text_color(p.text2).child(format!("{} / {}", current.map(|c| c + 1).unwrap_or(0), chapters.len()))),
                        )
                        .child(icon_button("chp-close", "x", cx).on_click(|_, _, cx| {
                            CHAPTERS_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
                            cx.refresh_windows();
                        })),
                )
                .child(nested_scroll("chapters-wrap", list))
                .into_any_element(),
        )
    }

    fn render_queue_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = pal(cx);
        let pl = player(cx);
        let (queue, pos) = {
            let r = pl.read(cx);
            (r.queue.clone(), r.queue_pos)
        };
        if queue.is_empty() {
            return None;
        }
        let title = self
            .info
            .ready()
            .and_then(|w| w.playlist.as_ref().map(|p| p.title.clone()))
            .or(self.local_playlist.as_ref().map(|l| l.1.clone()))
            .unwrap_or_else(|| "Queue".into());
        let mut list = div().id("queue-list").flex().flex_col().max_h(px(400.)).overflow_y_scroll();
        for (i, v) in queue.iter().enumerate() {
            let plc = pl.clone();
            let up = pl.clone();
            let down = pl.clone();
            let remove = pl.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w_5().text_xs().text_color(p.text2).text_center().child(if pos == Some(i) { "▶".to_string() } else { (i + 1).to_string() }))
                    .child(div().flex_1().min_w_0().child(compact_row(
                        v,
                        pos == Some(i),
                        Some(Arc::new(move |_, cx: &mut App| plc.update(cx, |p, cx| p.play_queue_index(i, cx)))),
                        cx,
                    )))
                    .when(i > 0, |d| d.child(pill(SharedString::from(format!("queue-up-{i}")), None, "↑", cx).on_click(move |_, _, cx| up.update(cx, |p, cx| { if crate::wetube_queue::move_item(&mut p.queue, &mut p.queue_pos, i, true) { p.checkpoint_recovery(); } cx.notify(); }))))
                    .when(i + 1 < queue.len(), |d| d.child(pill(SharedString::from(format!("queue-down-{i}")), None, "↓", cx).on_click(move |_, _, cx| down.update(cx, |p, cx| { if crate::wetube_queue::move_item(&mut p.queue, &mut p.queue_pos, i, false) { p.checkpoint_recovery(); } cx.notify(); }))))
                    .when(pos != Some(i), |d| d.child(icon_button(SharedString::from(format!("queue-remove-{i}")), "x", cx).on_click(move |_, _, cx| remove.update(cx, |p, cx| { if crate::wetube_queue::remove_item(&mut p.queue, &mut p.queue_pos, i) { p.checkpoint_recovery(); } cx.notify(); })))),
            );
        }
        let plc2 = pl.clone();
        let plc3 = pl.clone();
        let save = pl.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .rounded_xl()
                .border_1()
                .border_color(p.border)
                .overflow_hidden()
                .mb_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .p_3()
                        .bg(p.surface)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_base().font_weight(FontWeight::BOLD).text_color(p.text).child(title))
                                .child(div().text_xs().text_color(p.text2).child(format!("{} / {}", pos.map(|p| p + 1).unwrap_or(0), queue.len()))),
                        )
                        .child(
                            div()
                                .flex()
                                .child(icon_button("q-shuffle", "shuffle", cx).on_click(move |_, _, cx| {
                                    plc2.update(cx, |p, cx| {
                                        use rand::seq::SliceRandom;
                                        let mut indexed: Vec<_> = std::mem::take(&mut p.queue).into_iter().enumerate().collect();
                                        indexed.shuffle(&mut rand::rng());
                                        p.queue_pos = p.queue_pos.and_then(|old| indexed.iter().position(|(i, _)| *i == old));
                                        p.queue = indexed.into_iter().map(|(_, v)| v).collect();
                                        p.checkpoint_recovery();
                                        cx.notify();
                                    })
                                }))
                                .child(icon_button("q-clear", "x", cx).on_click(move |_, _, cx| {
                                    plc3.update(cx, |p, cx| {
                                        p.queue.clear();
                                        p.queue_pos = None;
                                        p.checkpoint_recovery();
                                        cx.notify();
                                    })
                                })),
                        ),
                )
                .child(div().flex().items_center().justify_between().px_3().py_2().border_b_1().border_color(p.border)
                    .child(div().text_xs().text_color(p.text2).child("↑ ↓ reorder · × remove"))
                    .child(pill("wetube-save-queue", Some("list-video"), "Save queue", cx).on_click(move |_, _, cx| {
                        let result = { let p = save.read(cx); crate::wetube_queue::save(&p.queue, p.queue_pos) };
                        match result { Ok(()) => toast(cx, "Queue saved locally"), Err(e) => toast(cx, e.to_string()) }
                    })))
                .child(nested_scroll("queue-wrap", list))
                .into_any_element(),
        )
    }

    fn render_related(&mut self, w: &WatchInfo, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        if s.hide_related {
            return div().into_any_element();
        }
        let autoplay = s.autoplay_next;
        let mut col = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .pb_1()
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Up next"))
                .child(
                    div()
                        .id("autoplay")
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(p.text2)
                        .cursor_pointer()
                        .child("Autoplay")
                        .child(
                            div()
                                .w(px(36.))
                                .h(px(14.))
                                .rounded_full()
                                .bg(if autoplay { p.text2 } else { p.hover })
                                .relative()
                                .child(div().absolute().top(px(-3.)).left(if autoplay { px(16.) } else { px(-2.) }).size(px(20.)).rounded_full().bg(if autoplay { p.text } else { p.text3 })),
                        )
                        .on_click(|_, _, cx| {
                            store::update_settings(|s| s.autoplay_next ^= true);
                            cx.refresh_windows();
                        }),
                ),
        );
        let mut shorts = Vec::new();
        for it in &w.related {
            match it {
                Item::Video(v) if store::is_filtered(v, &s) => {}
                Item::Video(v) if v.is_short => shorts.push(v.clone()),
                Item::Video(v) => col = col.child(compact_row(v, false, None, cx)),
                Item::Playlist(pl) => {
                    let id = pl.id.clone();
                    col = col.child(
                        div()
                            .id(SharedString::from(format!("rp-{}", pl.id)))
                            .flex()
                            .gap_2()
                            .p_1()
                            .cursor_pointer()
                            .on_click(move |_, window, cx| navigate(Route::Playlist(id.clone()), window, cx))
                            .child(playlist_thumb(pl.thumbnail.clone(), pl.video_count_text.clone(), px(168.), px(94.), cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(pl.title.clone()))
                                    .when_some(pl.channel_name.clone(), |d, c| d.child(div().text_xs().text_color(p.text2).child(c))),
                            ),
                    );
                }
                Item::Channel(_) => {}
            }
        }
        if !shorts.is_empty() && !s.hide_shorts {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_2()
                    .child(div().flex().items_center().gap_2().child(icon("zap").text_color(p.accent)).child(div().font_weight(FontWeight::SEMIBOLD).child("Shorts")))
                    .child(div().flex().gap_2().overflow_hidden().children(shorts.iter().take(3).map(|v| shorts_card(v, px(126.), cx)))),
            );
        }
        if self.related_more_loading {
            col = col.child(spinner(cx));
        }
        col.into_any_element()
    }
}

impl Render for WatchPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::add(&crate::perf::PAGE_RENDERS, 1);
        let p = pal(cx);
        let ui = *cx.global::<UiState>();
        // infinite scroll: comments, then related
        if near_bottom(&self.scroll, 600.0) {
            if let Some(Load::Ready(c)) = &self.comments {
                if let (Some(tok), false) = (c.continuation.clone(), self.comments_more_loading) {
                    self.comments_more_loading = true;
                    let t = tok.clone();
                    cx.defer_in_entity(move |this, cx| this.load_comments(t, true, cx));
                }
            }
            let has_rel = self.info.ready().map(|w| w.related_continuation.is_some()).unwrap_or(false);
            if has_rel && !self.related_more_loading {
                self.related_more_loading = true;
                cx.defer_in_entity(|this, cx| this.load_more_related(cx));
            }
        }
        if ui.fullscreen {
            return div().size_full().bg(black()).child(crate::ui::slot::player_slot(cx)).into_any_element();
        }
        let vp = ui.viewport;
        let avail = vp.width - px(48.);
        let side_w = px(402.);
        let two_col = avail > px(1000.);
        let primary_w = if two_col { (avail - side_w - px(24.)).min(px(1280.)) } else { avail };
        let player_h = if ui.theater {
            (vp.width * (9. / 16.)).min(vp.height - px(56. + 120.))
        } else {
            primary_w * (9. / 16.)
        };
        let info = match &self.info {
            Load::Ready(w) => Some(w.clone()),
            _ => None,
        };
        let primary_info: AnyElement = match (&self.info, &info) {
            (_, Some(w)) => {
                let i = self.render_info(w, cx);
                let c = self.render_comments(w, cx);
                div().flex().flex_col().child(i).child(c).into_any_element()
            }
            (Load::Error(e), _) => div().py_4().text_color(p.text2).child(format!("Could not load video details: {e}")).into_any_element(),
            _ => spinner(cx).into_any_element(),
        };
        let chapters_panel = self.render_chapters_panel(cx);
        let queue_panel = self.render_queue_panel(cx);
        let chat_panel = self.render_chat(cx);
        let related = info.as_ref().map(|w| self.render_related(w, cx));
        let secondary = div()
            .flex()
            .flex_col()
            .w(if two_col { side_w } else { avail })
            .flex_none()
            .children(chapters_panel)
            .children(chat_panel)
            .children(queue_panel)
            .children(related);
        // ambient mode: a soft glow in the picture's colours around the player
        let glow = (!ui.theater && store::settings_snapshot().ambient_mode)
            .then(|| crate::player::controller::player(cx).read(cx).ambient)
            .flatten()
            .map(|c| {
                let mut h: Hsla = c.into();
                h.a = if pal(cx).dark { 0.55 } else { 0.35 };
                h
            });
        let player_box = div()
            .w(if ui.theater { vp.width } else { primary_w })
            .h(player_h)
            .flex_none()
            .when(!ui.theater, |d| d.rounded_xl().overflow_hidden())
            .when_some(glow, |d, color| {
                d.shadow(vec![BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(90.), spread_radius: px(12.), inset: false }])
            })
            .child(crate::ui::slot::player_slot(cx));

        let body = if ui.theater {
            div()
                .flex()
                .flex_col()
                .child(div().w_full().bg(black()).flex().justify_center().child(player_box))
                .child(
                    div()
                        .flex()
                        .gap_6()
                        .px_6()
                        .pb_24()
                        .when(!two_col, |d| d.flex_col())
                        .child(div().w(primary_w).flex_none().child(primary_info))
                        .child(div().pt_3().child(secondary)),
                )
        } else {
            div()
                .flex()
                .justify_center()
                .gap_6()
                .px_6()
                .pt_6()
                .pb_24()
                .when(!two_col, |d| d.flex_col())
                .child(div().flex().flex_col().w(primary_w).flex_none().child(player_box).child(primary_info))
                .child(secondary)
        };
        page_scroll("watch-scroll", &self.scroll).child(body).into_any_element()
    }
}

/// A like or dislike count, exact below a million (with separators) so that
/// voting visibly moves it.
fn vote_count(n: u64) -> String {
    if n >= 1_000_000 {
        return compact_number(n);
    }
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Likes / dislikes a video locally and on the signed-in account.
fn set_rating(id: &str, r: i8, cx: &mut App) {
    store::set_rating(id, r);
    if crate::api::auth::signed_in() {
        let id = id.to_string();
        let kind = match r {
            1 => "like",
            -1 => "dislike",
            _ => "removelike",
        };
        cx.spawn(async move |cx| {
            if blocking(move || crate::api::auth::like(&id, kind)).await.is_err() {
                let _ = cx.update(|cx| toast(cx, "Couldn't update the rating on your YouTube account"));
            }
        })
        .detach();
    }
    cx.refresh_windows();
}

/// One branch of a comment thread: the child hangs off the line that comes down
/// from the parent's avatar (`avatar` px wide) with a rounded elbow; the line runs
/// on past every child but the last.
pub(crate) fn thread_branch(child: AnyElement, last: bool, avatar: f32, line: Hsla) -> AnyElement {
    let x = avatar / 2.0 - 1.0;
    let indent = avatar + 16.0;
    div()
        .relative()
        .pt_3()
        .pl(px(indent))
        .when(!last, |d| d.child(div().absolute().left(px(x)).top_0().bottom_0().w(px(2.)).bg(line)))
        .child(
            div()
                .absolute()
                .left(px(x))
                .top_0()
                .w(px(indent - x - 4.0))
                .h(px(24.))
                .border_l_2()
                .border_b_2()
                .rounded_bl(px(12.))
                .border_color(line),
        )
        .child(child)
        .into_any_element()
}

/// For each reply, the earlier reply it answers: YouTube keeps replies flat and
/// starts an answer with "@name", so a reply opening with the name of someone who
/// replied before it hangs under that person's latest reply. Nesting stops a few
/// levels deep (deeper answers sit beside the reply they answer).
pub(crate) fn reply_parents(list: &[Comment]) -> Vec<Option<usize>> {
    const MAX_DEPTH: usize = 3;
    let norm = |s: &str| s.trim().trim_start_matches('@').to_lowercase();
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(list.len());
    let mut depth: Vec<usize> = Vec::with_capacity(list.len());
    for (i, c) in list.iter().enumerate() {
        let text = c.text.trim_start();
        let parent = text
            .strip_prefix('@')
            .and_then(|t| t.split(|ch: char| ch.is_whitespace()).next())
            .map(|name| name.trim_end_matches(|ch: char| matches!(ch, ',' | ':' | '.' | '!' | '?')).to_lowercase())
            .filter(|name| !name.is_empty())
            .and_then(|name| (0..i).rev().find(|&j| norm(&list[j].author) == name && list[j].author != c.author));
        let parent = parent.map(|mut j| {
            while depth[j] >= MAX_DEPTH {
                j = parents[j].expect("deep replies have parents");
            }
            j
        });
        depth.push(parent.map(|j| depth[j] + 1).unwrap_or(1));
        parents.push(parent);
    }
    parents
}

#[cfg(test)]
mod tests {
    use super::{Comment, reply_parents};
    fn c(author: &str, text: &str) -> Comment {
        Comment { author: author.into(), text: text.into(), ..Default::default() }
    }
    #[test]
    fn replies_nest_under_the_reply_they_answer() {
        let list = vec![
            c("@ann", "first"),
            c("@bob", "@ann agreed"),
            c("@cat", "unrelated"),
            c("@ann", "@bob, thanks"),
            c("@dan", "@nobody hi"),
            c("@bob", "@ann np"),
            c("@ann", "@bob ok"),
        ];
        let p = reply_parents(&list);
        // index 3 is already three levels deep, so answers to it sit beside it
        assert_eq!(p, vec![None, Some(0), None, Some(1), None, Some(1), Some(1)]);
    }
}
