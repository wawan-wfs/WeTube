use crate::api::innertube::{self, SearchFilter};
use crate::api::types::*;
use crate::store;
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;

pub struct SearchPage {
    query: String,
    filter: SearchFilter,
    show_filters: bool,
    items: Load<Vec<Item>>,
    continuation: Option<String>,
    loading_more: bool,
    scroll: ScrollHandle,
    serial: u64,
}

impl SearchPage {
    pub fn new(query: String, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut s = Self {
            query,
            filter: SearchFilter::default(),
            show_filters: false,
            items: Load::Loading,
            continuation: None,
            loading_more: false,
            scroll: ScrollHandle::new(),
            serial: 0,
        };
        s.load(cx);
        s
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.items = Load::Loading;
        self.continuation = None;
        self.loading_more = false;
        self.serial += 1;
        let serial = self.serial;
        let (q, f) = (self.query.clone(), self.filter);
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::search(&q, &f)).await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                match res {
                    Ok(p) => {
                        this.items = Load::Ready(p.items);
                        this.continuation = p.continuation;
                    }
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
            let res = blocking(move || innertube::search_continue(&tok)).await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial { return; }
                this.loading_more = false;
                match res {
                    Ok(p) => {
                        if let Some(items) = this.items.ready_mut() {
                            let mut seen: std::collections::HashSet<String> = items.iter().filter_map(|i| i.as_video().map(|v| v.id.clone())).collect();
                            items.extend(p.items.into_iter().filter(|i| i.as_video().map(|v| seen.insert(v.id.clone())).unwrap_or(true)));
                        }
                        this.continuation = p.continuation;
                    }
                    Err(_) => this.continuation = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn filter_group(&self, title: &str, options: &[(&str, u8)], current: u8, set: fn(&mut SearchFilter, u8), cx: &mut Context<Self>) -> Div {
        let p = pal(cx);
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(150.))
            .child(div().pb_3().mb_2().border_b_1().border_color(p.border).text_xs().font_weight(FontWeight::BOLD).text_color(p.text).child(title.to_uppercase()));
        for (label, v) in options {
            let v = *v;
            let active = current == v && v != 0;
            col = col.child(
                div()
                    .id(SharedString::from(format!("f-{title}-{v}")))
                    .py_2()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if active { p.text } else { p.text2 })
                    .when(active, |d| d.font_weight(FontWeight::BOLD))
                    .hover(move |s| s.text_color(p.text))
                    .child(label.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let cur = current;
                        set(&mut this.filter, if cur == v { 0 } else { v });
                        this.load(cx);
                    })),
            );
        }
        col
    }

    fn feature(&self, label: &str, on: bool, toggle: fn(&mut SearchFilter), cx: &mut Context<Self>) -> Stateful<Div> {
        let p = pal(cx);
        div()
            .id(SharedString::from(format!("feat-{label}")))
            .py_2()
            .text_sm()
            .cursor_pointer()
            .text_color(if on { p.text } else { p.text2 })
            .when(on, |d| d.font_weight(FontWeight::BOLD))
            .hover(move |s| s.text_color(p.text))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(&mut this.filter);
                this.load(cx);
            }))
    }
}

impl Render for SearchPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let s = store::settings_snapshot();
        let has_more = self.continuation.is_some();
        maybe_load_more(&self.scroll, &mut self.loading_more, has_more, cx, |this, cx| this.load_more(cx));
        let width = content_width(cx).min(px(1280.));
        let thumb_w = if width < px(900.) { px(240.) } else { px(360.) };
        let f = self.filter;
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .pb_2()
            .child(
                div().flex().flex_wrap().gap_2().children(
                    [("All", 0u8), ("Videos", 1), ("Channels", 2), ("Playlists", 3), ("Movies", 4)].into_iter().map(|(l, k)| {
                        chip(SharedString::from(format!("t-{l}")), l, f.kind == k, cx).on_click(cx.listener(move |this, _, _, cx| {
                            this.filter.kind = k;
                            this.load(cx);
                        }))
                    }),
                ),
            )
            .child(
                pill("filters", Some("sliders-horizontal"), if f.is_default() { "Filters".to_string() } else { "Filters •".to_string() }, cx).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.show_filters ^= true;
                        cx.notify();
                    }),
                ),
            );
        let filters = self.show_filters.then(|| {
            div()
                .flex()
                .gap_6()
                .p_4()
                .mb_4()
                .rounded_xl()
                .bg(p.surface)
                .child(self.filter_group("Upload date", &[("Last hour", 1), ("Today", 2), ("This week", 3), ("This month", 4), ("This year", 5)], f.date, |f, v| f.date = v, cx))
                .child(self.filter_group("Type", &[("Video", 1), ("Channel", 2), ("Playlist", 3), ("Movie", 4)], f.kind, |f, v| f.kind = v, cx))
                .child(self.filter_group("Duration", &[("Under 4 minutes", 1), ("4 - 20 minutes", 3), ("Over 20 minutes", 2)], f.duration, |f, v| f.duration = v, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(150.))
                        .child(div().pb_3().mb_2().border_b_1().border_color(p.border).text_xs().font_weight(FontWeight::BOLD).text_color(p.text).child("FEATURES"))
                        .child(self.feature("Live", f.live, |f| f.live ^= true, cx))
                        .child(self.feature("4K", f.four_k, |f| f.four_k ^= true, cx))
                        .child(self.feature("HD", f.hd, |f| f.hd ^= true, cx))
                        .child(self.feature("Subtitles/CC", f.subtitles, |f| f.subtitles ^= true, cx))
                        .child(self.feature("Creative Commons", f.creative_commons, |f| f.creative_commons ^= true, cx))
                        .child(self.feature("HDR", f.hdr, |f| f.hdr ^= true, cx)),
                )
                .child(self.filter_group("Sort by", &[("Relevance", 0), ("Upload date", 2), ("View count", 3), ("Rating", 1)], f.sort, |f, v| f.sort = v, cx))
        });
        let body: AnyElement = match &self.items {
            Load::Loading => spinner(cx).into_any_element(),
            Load::Error(e) => error_state(e, cx).into_any_element(),
            Load::Ready(items) => {
                let mut col = div().flex().flex_col().gap_4();
                let mut shorts = Vec::new();
                for it in items {
                    match it {
                        Item::Video(v) if store::is_filtered(v, &s) => {}
                        Item::Video(v) if v.is_short => shorts.push(v.clone()),
                        Item::Video(v) => col = col.child(video_row(v, thumb_w, cx)),
                        Item::Channel(c) => col = col.child(channel_row(c, cx)),
                        Item::Playlist(pl) => col = col.child(playlist_row(pl, cx)),
                    }
                }
                if !shorts.is_empty() && !s.hide_shorts {
                    col = col.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .py_4()
                            .border_t_1()
                            .border_b_1()
                            .border_color(p.border)
                            .child(div().flex().items_center().gap_2().child(icon("zap").text_color(p.accent)).child(section_title("Shorts", cx)))
                            .child(div().flex().gap_4().overflow_hidden().children(shorts.iter().take(6).map(|v| shorts_card(v, px(190.), cx)))),
                    );
                }
                if items.is_empty() {
                    col = col.child(empty_state("search-x", "No results found", "Try different keywords or remove filters", cx));
                }
                if self.loading_more {
                    col = col.child(spinner(cx));
                }
                col.into_any_element()
            }
        };
        page_scroll("search-scroll", &self.scroll).child(
            div()
                .flex()
                .justify_center()
                .px_6()
                .pt_4()
                .pb_24()
                .child(div().w(width).flex().flex_col().gap_3()
                    .child(workspace_header("Search results", self.query.clone(), "search", cx))
                    .child(header).children(filters).child(body)),
        )
    }
}
