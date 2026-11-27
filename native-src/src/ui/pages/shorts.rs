use crate::api::innertube::{self, SearchFilter};
use crate::api::json::compact_number;
use crate::api::services;
use crate::api::types::*;
use crate::player::controller::player;
use crate::store;
use crate::ui::app::{Route, UiState, navigate};
use crate::ui::*;
use crate::ui::pages::watch::{reply_parents, thread_branch};
use crate::ui::widgets::{Load, comment_body};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct ShortsPage {
    list: Vec<VideoItem>,
    idx: usize,
    loading: bool,
    votes: HashMap<String, services::Votes>,
    last_step: Instant,
    /// wheel / touchpad gesture tracking: one Short per gesture
    wheel_acc: f32,
    last_wheel: Instant,
    wheel_used: bool,
    continuation: Option<String>,
    /// `true` while the reel sequence feeds the list (its continuation is a reel token)
    reel_feed: bool,
    /// title / channel / comments token per Short (the feed only carries ids)
    meta: HashMap<String, WatchInfo>,
    show_comments: bool,
    comments: Option<Load<CommentPage>>,
    comments_for: String,
    comments_more: bool,
    /// opened reply threads, by comment id
    replies: HashMap<String, Load<(Vec<Comment>, Option<String>)>>,
    /// where the comments panel was drawn: the wheel scrolls comments there, not Shorts
    comments_bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    _sub: Subscription,
}

impl ShortsPage {
    pub fn new(start: Option<String>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&player(cx), |this: &mut Self, _, ev: &crate::player::controller::PlayerEvent, cx| {
            if matches!(ev, crate::player::controller::PlayerEvent::Ended) && store::settings_snapshot().shorts_autoadvance {
                this.advance(cx);
            }
        });
        cx.on_release(|_, cx| {
            let l = store::settings_snapshot().loop_video;
            player(cx).update(cx, |p, _| p.set_loop(l));
        })
        .detach();
        let mut s = Self {
            list: Vec::new(),
            idx: 0,
            loading: true,
            votes: HashMap::new(),
            last_step: Instant::now(),
            wheel_acc: 0.0,
            last_wheel: Instant::now(),
            wheel_used: false,
            continuation: None,
            reel_feed: false,
            meta: HashMap::new(),
            show_comments: false,
            comments: None,
            comments_for: String::new(),
            comments_more: false,
            replies: HashMap::new(),
            comments_bounds: Default::default(),
            _sub: sub,
        };
        if let Some(id) = &start {
            s.list.push(VideoItem { id: id.clone(), is_short: true, thumbnail: innertube::default_thumb(id), ..Default::default() });
            s.play_current(cx);
        }
        let start2 = start.clone();
        cx.spawn(async move |this, cx| {
            let res = blocking(move || {
                // YouTube's own Shorts sequence: the account's recommendations with the
                // browser sign-in, otherwise one that follows on from the last Short watched
                let seed = crate::store::history_list("", 60).into_iter().find(|h| h.video.is_short).map(|h| h.video.id);
                let seeded = seed.as_deref().and_then(|s| innertube::shorts_feed(None, Some(s)).ok());
                let feed = seeded.filter(|(i, _)| !i.is_empty()).or_else(|| innertube::shorts_feed(None, None).ok());
                if let Some((items, cont)) = feed {
                    if !items.is_empty() {
                        return (items, cont, true);
                    }
                }
                // fallback: subscription shorts, then a shorts search
                let (feed, _) = crate::feed::cached_feed(None);
                let mut out: Vec<VideoItem> = feed.into_iter().filter(|v| v.is_short).take(30).collect();
                let f = SearchFilter { duration: 1, kind: 1, ..Default::default() };
                let mut cont = None;
                if let Ok(p) = innertube::search("#shorts", &f) {
                    cont = p.continuation;
                    out.extend(p.items.into_iter().filter_map(|i| match i {
                        Item::Video(mut v) => {
                            v.is_short = true;
                            Some(v)
                        }
                        _ => None,
                    }));
                }
                (out, cont, false)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                let s = store::settings_snapshot();
                let mut seen: std::collections::HashSet<String> = this.list.iter().map(|v| v.id.clone()).collect();
                for v in res.0 {
                    if !store::is_filtered(&VideoItem { is_short: false, ..v.clone() }, &s) && seen.insert(v.id.clone()) {
                        this.list.push(v);
                    }
                }
                this.continuation = res.1;
                this.reel_feed = res.2;
                if start2.is_none() && !this.list.is_empty() {
                    this.play_current(cx);
                }
                cx.notify();
            });
        })
        .detach();
        s
    }

    fn play_current(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.list.get(self.idx).cloned() else { return };
        let pl = player(cx);
        pl.update(cx, |p, cx| {
            p.queue.clear();
            p.queue_pos = None;
            p.set_loop(!store::settings_snapshot().shorts_autoadvance);
            p.play(v.clone(), Some(0.0), cx);
        });
        self.fetch_meta(v.id.clone(), cx);
        if let Some(n) = self.list.get(self.idx + 1) {
            self.fetch_meta(n.id.clone(), cx);
        }
        if self.show_comments {
            self.open_comments(cx);
        }
        if store::settings_snapshot().ryd && !self.votes.contains_key(&v.id) {
            let id = v.id.clone();
            cx.spawn(async move |this, cx| {
                let id2 = id.clone();
                if let Ok(vt) = blocking(move || services::ryd_votes(&id2)).await {
                    let _ = this.update(cx, |t, cx| {
                        t.votes.insert(id, vt);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        // prefetch more when near the end
        if self.idx + 3 >= self.list.len() {
            if let Some(tok) = self.continuation.take() {
                let reel = self.reel_feed;
                cx.spawn(async move |this, cx| {
                    let res = blocking(move || {
                        if reel {
                            innertube::shorts_feed(Some(&tok), None)
                        } else {
                            innertube::search_continue(&tok).map(|p| {
                                let items = p
                                    .items
                                    .into_iter()
                                    .filter_map(|i| match i {
                                        Item::Video(mut v) => {
                                            v.is_short = true;
                                            Some(v)
                                        }
                                        _ => None,
                                    })
                                    .collect();
                                (items, p.continuation)
                            })
                        }
                    })
                    .await;
                    if let Ok((items, cont)) = res {
                        let _ = this.update(cx, |t, cx| {
                            t.continuation = cont;
                            for v in items {
                                if !t.list.iter().any(|x| x.id == v.id) {
                                    t.list.push(v);
                                }
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn fetch_meta(&mut self, id: String, cx: &mut Context<Self>) {
        if self.meta.contains_key(&id) {
            return;
        }
        self.meta.insert(id.clone(), WatchInfo { id: id.clone(), ..Default::default() });
        cx.spawn(async move |this, cx| {
            let id2 = id.clone();
            let res = blocking(move || innertube::watch_next(&id2, None)).await;
            let _ = this.update(cx, |t, cx| {
                match res {
                    Ok(w) => {
                        if let Some(v) = t.list.iter_mut().find(|v| v.id == id) {
                            if v.title.is_empty() {
                                v.title = w.title.clone();
                            }
                            if v.channel_name.is_empty() {
                                v.channel_name = w.channel.name.clone();
                            }
                            if v.channel_id.is_none() && !w.channel.id.is_empty() {
                                v.channel_id = Some(w.channel.id.clone());
                            }
                            if v.channel_avatar.is_none() {
                                v.channel_avatar = w.channel.avatar.clone();
                            }
                        }
                        t.meta.insert(id.clone(), w);
                        let current = t.list.get(t.idx).map(|v| v.id == id).unwrap_or(false);
                        if t.show_comments && current && t.comments.is_none() {
                            t.comments_for.clear();
                            t.open_comments(cx);
                        }
                    }
                    Err(_) => {
                        t.meta.remove(&id);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Loads comments for the current Short into the side panel.
    fn open_comments(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.list.get(self.idx) else { return };
        let id = v.id.clone();
        self.show_comments = true;
        if self.comments_for == id && self.comments.is_some() {
            cx.notify();
            return;
        }
        self.comments_for = id.clone();
        self.replies.clear();
        let token = self.meta.get(&id).and_then(|w| w.comments_token.clone());
        let Some(token) = token else {
            // still waiting for the watch info, or comments are off
            let pending = self.meta.get(&id).map(|w| w.title.is_empty()).unwrap_or(true);
            self.comments = if pending { None } else { Some(Load::Error("Comments are turned off.".into())) };
            cx.notify();
            return;
        };
        self.comments = Some(Load::Loading);
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::comments(&token)).await;
            let _ = this.update(cx, |t, cx| {
                if t.comments_for == id {
                    let sort = t.meta.get(&id).map(|w| w.comments_sort.clone()).unwrap_or_default();
                    t.comments = Some(match res {
                        Ok(mut p) => {
                            if p.sort_top_token.is_none() && p.sort_new_token.is_none() {
                                (p.sort_top_token, p.sort_new_token) = sort;
                            }
                            Load::Ready(p)
                        }
                        Err(e) => Load::Error(e.to_string()),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn load_comment_page(&mut self, token: String, append: bool, cx: &mut Context<Self>) {
        if self.comments_more {
            return;
        }
        self.comments_more = append;
        if !append {
            self.comments = Some(Load::Loading);
        }
        let id = self.comments_for.clone();
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::comments(&token)).await;
            let _ = this.update(cx, |t, cx| {
                t.comments_more = false;
                if t.comments_for != id {
                    return;
                }
                match (res, append) {
                    (Ok(mut p), false) => {
                        if p.sort_top_token.is_none() && p.sort_new_token.is_none() {
                            (p.sort_top_token, p.sort_new_token) = t.meta.get(&id).map(|w| w.comments_sort.clone()).unwrap_or_default();
                        }
                        t.comments = Some(Load::Ready(p))
                    }
                    (Err(e), false) => t.comments = Some(Load::Error(e.to_string())),
                    (res, true) => {
                        if let Some(Load::Ready(c)) = t.comments.as_mut() {
                            match res {
                                Ok(p) => {
                                    c.comments.extend(p.comments);
                                    c.continuation = p.continuation;
                                }
                                Err(_) => c.continuation = None,
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_replies(&mut self, comment_id: String, token: String, cx: &mut Context<Self>) {
        let append = matches!(self.replies.get(&comment_id), Some(Load::Ready(_)));
        if !append {
            self.replies.insert(comment_id.clone(), Load::Loading);
        }
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::comments(&token)).await;
            let _ = this.update(cx, |t, cx| {
                // closed while loading: leave it closed
                if !t.replies.contains_key(&comment_id) {
                    return;
                }
                match res {
                    Ok(p) => {
                        if let Some(Load::Ready((list, cont))) = t.replies.get_mut(&comment_id) {
                            list.extend(p.comments);
                            *cont = p.continuation;
                        } else {
                            t.replies.insert(comment_id, Load::Ready((p.comments, p.continuation)));
                        }
                    }
                    Err(e) => {
                        t.replies.insert(comment_id, Load::Error(e.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A reply and, hanging under it, the replies that answer it.
    fn reply_subtree(&mut self, list: &[Comment], parents: &[Option<usize>], i: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let kids: Vec<usize> = (i + 1..list.len()).filter(|&j| parents[j] == Some(i)).collect();
        let me = self.comment_row(&list[i], true, !kids.is_empty(), cx);
        if kids.is_empty() {
            return me;
        }
        let mut col = div().flex().flex_col().child(me);
        for (k, &j) in kids.iter().enumerate() {
            col = col.child(thread_branch(self.reply_subtree(list, parents, j, cx), k + 1 == kids.len(), 24., p.border));
        }
        col.into_any_element()
    }

    /// One comment; an opened one has its replies hanging under it on a thread line.
    fn comment_row(&mut self, c: &Comment, is_reply: bool, thread: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let cid = c.id.clone();
        let open = self.replies.contains_key(&c.id);
        let size = if is_reply { 24. } else { 32. };
        let a = avatar(c.author_avatar.as_deref(), px(size), &c.author, cx).into_any_element();
        let a = if thread || (!is_reply && open) {
            div().flex().flex_col().items_center().flex_none().child(a).child(div().flex_1().w(px(2.)).mt_1().bg(p.border)).into_any_element()
        } else {
            a
        };
        let toggle = c.replies_token.clone().filter(|_| !is_reply && !c.reply_count.is_empty() && c.reply_count != "0").map(|tok| {
            let cid2 = cid.clone();
            div()
                .id(SharedString::from(format!("scr-btn-{}", c.id)))
                .flex()
                .items_center()
                .gap_1()
                .text_color(p.link)
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(icon(if open { "chevron-up" } else { "chevron-down" }).size_4().text_color(p.link))
                .child(format!("{} {}", c.reply_count, if c.reply_count == "1" { "reply" } else { "replies" }))
                .on_click(cx.listener(move |t, _, _, cx| {
                    if t.replies.remove(&cid2).is_some() {
                        cx.notify();
                    } else {
                        t.load_replies(cid2.clone(), tok.clone(), cx);
                    }
                }))
        });
        let el = div()
            .flex()
            .gap_3()
            .child(a)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(c.author.clone()))
                            .child(div().text_color(p.text2).child(c.published.clone())),
                    )
                    .child(div().text_sm().text_color(p.text).child(comment_body(format!("sct-{}", c.id), c, cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_xs()
                            .text_color(p.text2)
                            .child(div().flex().items_center().gap_1().child(icon("thumbs-up").size_4().text_color(p.text2)).child(c.likes.clone()))
                            .when(c.hearted, |d| d.child(icon("heart").size_4().text_color(rgb(0xff0033))))
                            .children(toggle),
                    ),
            )
            .into_any_element();
        let Some(r) = self.replies.get(&c.id) else { return el };
        let r: Option<Result<(Vec<Comment>, Option<String>), String>> = match r {
            Load::Loading => None,
            Load::Error(e) => Some(Err(e.clone())),
            Load::Ready((l, c)) => Some(Ok((l.clone(), c.clone()))),
        };
        let mut col = div().flex().flex_col();
        match r {
            None => col = col.child(thread_branch(spinner(cx).into_any_element(), true, size, p.border)),
            Some(Err(e)) => col = col.child(thread_branch(div().text_xs().text_color(p.text2).child(e).into_any_element(), true, size, p.border)),
            Some(Ok((list, cont))) => {
                // replies to replies nest under the reply they answer
                let parents = reply_parents(&list);
                let roots: Vec<usize> = (0..list.len()).filter(|&i| parents[i].is_none()).collect();
                for (k, &i) in roots.iter().enumerate() {
                    let last = k + 1 == roots.len() && cont.is_none();
                    col = col.child(thread_branch(self.reply_subtree(&list, &parents, i, cx), last, size, p.border));
                }
                if let Some(tok) = cont {
                    col = col.child(thread_branch(
                        div()
                            .id(SharedString::from(format!("scr-more-{cid}")))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.link)
                            .cursor_pointer()
                            .child("Show more replies")
                            .on_click(cx.listener(move |t, _, _, cx| t.load_replies(cid.clone(), tok.clone(), cx)))
                            .into_any_element(),
                        true,
                        size,
                        p.border,
                    ));
                }
            }
        }
        div().flex().flex_col().child(el).child(col).into_any_element()
    }

    fn advance(&mut self, cx: &mut Context<Self>) {
        if self.idx + 1 < self.list.len() {
            self.idx += 1;
            self.last_step = Instant::now();
            self.play_current(cx);
        }
    }

    fn render_comments(&mut self, h: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let p = pal(cx);
        let ready = self.comments.as_ref().and_then(|c| c.ready());
        let count = ready.and_then(|c| c.count_text.clone());
        let top_tok = ready.and_then(|c| c.sort_top_token.clone());
        let new_tok = ready.and_then(|c| c.sort_new_token.clone());
        let sort_chip = |id: &'static str, label: &'static str, tok: Option<String>, cx: &mut Context<Self>| {
            let p = pal(cx);
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_full()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .bg(p.chip)
                .text_color(p.text)
                .cursor_pointer()
                .hover(move |s| s.bg(p.hover))
                .child(label)
                .when_some(tok, |d, tok| d.on_click(cx.listener(move |t, _, _, cx| t.load_comment_page(tok.clone(), false, cx))))
        };
        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().text_base().font_weight(FontWeight::BOLD).text_color(p.text).child("Comments"))
                            .when_some(count, |d, c| d.child(div().text_sm().text_color(p.text2).child(c))),
                    )
                    .child(icon_button("sh-cm-close", "x", cx).on_click(cx.listener(|t, _, _, cx| {
                        t.show_comments = false;
                        cx.notify();
                    }))),
            )
            .when(top_tok.is_some() || new_tok.is_some(), |d| {
                d.child(div().flex().gap_2().child(sort_chip("sh-cm-top", "Top", top_tok, cx)).child(sort_chip("sh-cm-new", "Newest", new_tok, cx)))
            });
        let mut body = div().id("sh-cm-list").flex_1().min_h_0().overflow_y_scroll().px_4().py_3().flex().flex_col().gap_4();
        match &self.comments {
            None | Some(Load::Loading) => body = body.child(div().py_8().flex().justify_center().child(spinner(cx))),
            Some(Load::Error(e)) => body = body.child(div().text_sm().text_color(p.text2).child(e.clone())),
            Some(Load::Ready(page)) => {
                let (list, more) = (page.comments.clone(), page.continuation.clone());
                if list.is_empty() {
                    body = body.child(div().text_sm().text_color(p.text2).child("No comments yet."));
                }
                for c in &list {
                    body = body.child(self.comment_row(c, false, false, cx));
                }
                if let Some(tok) = more {
                    body = body.child(
                        div()
                            .id("sh-cm-more")
                            .py_2()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.link)
                            .cursor_pointer()
                            .child(if self.comments_more { "Loading…" } else { "Show more comments" })
                            .on_click(cx.listener(move |t, _, _, cx| t.load_comment_page(tok.clone(), true, cx))),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .w(px(380.))
            .h(h)
            .rounded_xl()
            .overflow_hidden()
            .bg(p.surface)
            .border_1()
            .border_color(p.border)
            .child(header)
            .child(body)
            .into_any_element()
    }

    pub fn step(&mut self, d: i32, _window: &mut Window, cx: &mut Context<Self>) {
        if self.last_step.elapsed() < Duration::from_millis(350) {
            return;
        }
        let n = self.idx as i32 + d;
        if n < 0 || n as usize >= self.list.len() {
            return;
        }
        self.last_step = Instant::now();
        self.idx = n as usize;
        self.play_current(cx);
    }
}

impl Render for ShortsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let ui = *cx.global::<UiState>();
        if self.list.is_empty() {
            return div().size_full().child(if self.loading { spinner(cx).into_any_element() } else { empty_state("zap", "No Shorts found", "", cx).into_any_element() }).into_any_element();
        }
        let v = self.list[self.idx].clone();
        // Comments occupy horizontal space; keep the video within the viewport.
        let reserved = if self.show_comments { px(460.) } else { px(128.) };
        let available_w = (ui.content_width - reserved).max(px(140.));
        let header=if ui.viewport.width<px(1050.){104.}else{56.};
        let h = (ui.viewport.height - px(header + 32.)).max(px(160.)).min(available_w * (16. / 9.));
        let w = h * (9. / 16.);
        let votes = self.votes.get(&v.id).cloned();
        let cid = v.channel_id.clone();
        let watch_id = v.id.clone();
        let share_id = v.id.clone();
        let action = |id: &'static str, ic: &'static str, label: String, cx: &App| {
            let p = pal(cx);
            div()
                .id(id)
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .child(div().flex().items_center().justify_center().size_12().rounded_full().bg(p.chip).hover(move |s| s.bg(p.hover)).child(icon(ic).size_6().text_color(p.text)))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(p.text).child(label))
        };
        let comments_panel = if self.show_comments { self.render_comments(h, cx) } else { div().into_any_element() };
        let can_up = self.idx > 0;
        let can_down = self.idx + 1 < self.list.len();
        div()
            .id("shorts")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .gap_4()
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, window, cx| {
                // A touchpad swipe is a stream of small deltas and a wheel notch one big one.
                // Sum them, move one Short past a threshold, and ignore the rest of that
                // gesture (it ends after a short pause).
                if this.show_comments && this.comments_bounds.get().contains(&window.mouse_position()) {
                    return;
                }
                let dy: f32 = ev.delta.pixel_delta(window.line_height()).y.into();
                if this.last_wheel.elapsed() > Duration::from_millis(220) {
                    this.wheel_acc = 0.0;
                    this.wheel_used = false;
                }
                this.last_wheel = Instant::now();
                if this.wheel_used {
                    return;
                }
                this.wheel_acc += dy;
                if this.wheel_acc.abs() >= 48.0 {
                    let d = if this.wheel_acc < 0.0 { 1 } else { -1 };
                    this.wheel_acc = 0.0;
                    this.wheel_used = true;
                    this.last_step = Instant::now() - Duration::from_secs(1);
                    this.step(d, window, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .w(w)
                    .h(h)
                    .rounded_xl()
                    .overflow_hidden()
                    .bg(black())
                    .child(crate::ui::slot::player_slot(cx))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(70.))
                            .px_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .id("sh-chan")
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .cursor_pointer()
                                    .on_click(move |_, window, cx| {
                                        if let Some(c) = cid.clone() {
                                            navigate(Route::Channel(c), window, cx);
                                        }
                                    })
                                    .child(avatar(v.channel_avatar.as_deref(), px(32.), &v.channel_name, cx))
                                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(white()).child(v.channel_name.clone())),
                            )
                            .child(div().text_sm().text_color(white()).line_clamp(2).child(v.title.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .h(h)
                    .gap_4()
                    .pb_4()
                    .child(action("sh-like", "thumbs-up", votes.as_ref().map(|v| compact_number(v.likes)).unwrap_or_else(|| "Like".into()), cx))
                    .child(action("sh-dislike", "thumbs-down", votes.as_ref().map(|v| compact_number(v.dislikes)).unwrap_or_else(|| "Dislike".into()), cx))
                    .child({
                        let n = self.meta.get(&v.id).and_then(|w| w.comment_count_text.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Comments".into());
                        action("sh-comments", "message-square", n, cx).on_click(cx.listener(|t, _, _, cx| {
                            if t.show_comments {
                                t.show_comments = false;
                                cx.notify();
                            } else {
                                t.open_comments(cx);
                            }
                        }))
                    })
                    .child(action("sh-share", "share-2", "Share".into(), cx).on_click(move |_, _, cx| copy_to_clipboard(format!("https://youtube.com/shorts/{share_id}"), cx)))
                    .child(action("sh-watch", "monitor-play", "Full".into(), cx).on_click(move |_, window, cx| {
                        navigate(Route::Watch { id: watch_id.clone(), playlist: None, start: None }, window, cx)
                    })),
            )
            .when(self.show_comments, |d| {
                let b = self.comments_bounds.clone();
                d.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(canvas(move |bounds, _, _| b.set(bounds), |_, _, _, _| {}).absolute().size_full())
                        .child(comments_panel),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(icon_button("sh-up", "arrow-up", cx).size_12().bg(p.chip).when(!can_up, |d| d.opacity(0.3)).on_click(cx.listener(|t, _, window, cx| t.step(-1, window, cx))))
                    .child(icon_button("sh-down", "arrow-down", cx).size_12().bg(p.chip).when(!can_down, |d| d.opacity(0.3)).on_click(cx.listener(|t, _, window, cx| t.step(1, window, cx)))),
            )
            .into_any_element()
    }
}
