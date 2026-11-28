use crate::api::innertube;
use crate::api::types::*;
use crate::store;
use crate::ui::app::{MenuItem, open_menu};
use crate::ui::cards::*;
use crate::ui::widgets::*;
use crate::ui::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};

pub struct ChannelPage {
    id: String,
    data: Load<ChannelPage0>,
    tab: usize,
    tab_items: Load<Vec<Item>>,
    continuation: Option<String>,
    loading_more: bool,
    scroll: ScrollHandle,
    search: Entity<InputState>,
    searching: Option<String>,
    serial: u64,
    /// the channel description is shown in full
    about_open: bool,
    catalog: crate::wetube_catalog::Catalog,
    _sub: Subscription,
}

#[derive(Clone)]
struct ChannelPage0 {
    info: ChannelItem,
    banner: Option<String>,
    tabs: Vec<ChannelTab>,
}

const WANTED_TABS: &[&str] = &["Home", "Videos", "Shorts", "Live", "Podcasts", "Releases", "Playlists", "Courses", "Shows"];

impl ChannelPage {
    pub fn new(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search channel"));
        let sub = cx.subscribe_in(&search, window, |this: &mut Self, st, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let q = st.read(cx).value().to_string();
                if !q.trim().is_empty() {
                    this.search_channel(q, cx);
                }
            }
        });
        let mut s = Self {
            id,
            data: Load::Loading,
            tab: 0,
            tab_items: Load::Loading,
            continuation: None,
            loading_more: false,
            scroll: ScrollHandle::new(),
            search,
            searching: None,
            serial: 0,
            about_open: false,
            catalog: Default::default(),
            _sub: sub,
        };
        s.load(cx);
        s
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            // Videos tab first (most useful), header comes with it
            let res = blocking(move || innertube::channel(&id, Some(crate::feed::VIDEOS_TAB))).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(cp) => {
                        let tabs: Vec<ChannelTab> = cp
                            .tabs
                            .into_iter()
                            .filter(|t| WANTED_TABS.contains(&t.title.as_str()))
                            .filter(|t| !(store::settings_snapshot().hide_shorts && t.title == "Shorts"))
                            .collect();
                        this.tab = tabs.iter().position(|t| t.title == "Videos").unwrap_or(0);
                        this.tab_items = Load::Ready(cp.page.items);
                        this.continuation = cp.page.continuation;
                        if store::is_subscribed(&cp.info.id) {
                            store::subscribe(&cp.info.id, &cp.info.name, cp.info.avatar.as_deref());
                        }
                        this.data = Load::Ready(ChannelPage0 { info: cp.info, banner: cp.banner, tabs });
                    }
                    Err(e) => this.data = Load::Error(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.catalog.reset();
        let Some(d) = self.data.ready() else { return };
        let Some(t) = d.tabs.get(idx).cloned() else { return };
        self.tab = idx;
        self.searching = None;
        self.tab_items = Load::Loading;
        self.continuation = None;
        self.loading_more = false;
        self.serial += 1;
        let serial = self.serial;
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::channel(&id, t.params.as_deref())).await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                match res {
                    Ok(cp) => {
                        this.tab_items = Load::Ready(cp.page.items);
                        this.continuation = cp.page.continuation;
                    }
                    Err(e) => this.tab_items = Load::Error(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn search_channel(&mut self, q: String, cx: &mut Context<Self>) {
        self.catalog.reset();
        self.searching = Some(q.clone());
        self.tab_items = Load::Loading;
        self.continuation = None;
        self.loading_more = false;
        self.serial += 1;
        let serial = self.serial;
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::channel_search(&id, &q)).await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                match res {
                    Ok(p) => {
                        this.tab_items = Load::Ready(p.items);
                        this.continuation = p.continuation;
                    }
                    Err(e) => this.tab_items = Load::Error(e.to_string()),
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
        let info = self.data.ready().map(|d| d.info.clone());
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let res = blocking(move || innertube::browse_continue(&tok)).await;
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial { return; }
                this.loading_more = false;
                match res {
                    Ok(mut p) => {
                        if let Some(info) = &info {
                            for it in p.items.iter_mut() {
                                if let Item::Video(v) = it {
                                    if v.channel_name.is_empty() {
                                        v.channel_name = info.name.clone();
                                    }
                                    v.channel_id.get_or_insert(info.id.clone());
                                }
                            }
                        }
                        if let Some(items) = this.tab_items.ready_mut() {
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
}

impl Render for ChannelPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let has_more = self.continuation.is_some();
        maybe_load_more(&self.scroll, &mut self.loading_more, has_more, cx, |this, cx| this.load_more(cx));
        let layout=workspace_surface(crate::wetube_workspace::Workspace::Channel,cx);
        let width=px(layout.width);
        let d = match &self.data {
            Load::Loading => return page_scroll("ch-scroll", &self.scroll).child(spinner(cx)).into_any_element(),
            Load::Error(e) => return page_scroll("ch-scroll", &self.scroll).child(error_state(e, cx)).into_any_element(),
            Load::Ready(d) => d.clone(),
        };
        let s = store::settings_snapshot();
        let subscribed = store::is_subscribed(&d.info.id);
        let blocked = s.blocked_channels.iter().any(|(i, _)| *i == d.info.id);
        let speed = store::channel_speed(&d.info.id);
        let info2 = d.info.clone();
        let header = div()
            .flex()
            .flex_col()
            .gap_4()
            .when_some(d.banner.clone(), |el, b| {
                el.child(div().w_full().h(width / 6.2).rounded_2xl().overflow_hidden().bg(p.surface).child(img(SharedString::from(b)).size_full().object_fit(ObjectFit::Cover)))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .p_4().rounded_xl().border_1().border_color(p.border).bg(p.surface)
                    .when(layout.compact,|d|d.flex_col().items_start())
                    .child(avatar(d.info.avatar.as_deref(), if width<px(620.){px(88.)}else{px(160.)}, &d.info.name, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .flex_1()
                            .child(div().text_3xl().font_weight(FontWeight::BOLD).text_color(p.text).child(d.info.name.clone()))
                            .child(
                                div().text_sm().text_color(p.text2).child(
                                    [d.info.handle.clone(), d.info.subscribers_text.clone(), d.info.video_count_text.clone()]
                                        .into_iter()
                                        .flatten()
                                        .collect::<Vec<_>>()
                                        .join(" • "),
                                ),
                            )
                            .when_some(d.info.description.clone().filter(|x| !x.trim().is_empty()), |el, desc| {
                                // two lines unless opened: some channels write pages here
                                let short = clamp_text(desc.trim(), 2, 180);
                                let long = short.is_some();
                                let open = self.about_open;
                                el.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .max_w(px(700.))
                                        .child(div().text_sm().text_color(p.text2).child(if open || !long {
                                            rich_text("ch-about", desc.trim().to_string(), cx)
                                        } else {
                                            div().child(short.unwrap_or_default()).into_any_element()
                                        }))
                                        .when(long, |d| {
                                            d.child(
                                                div()
                                                    .id("ch-about-more")
                                                    .text_sm()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(p.text)
                                                    .cursor_pointer()
                                                    .child(if open { "Show less" } else { "Show more" })
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.about_open ^= true;
                                                        cx.notify();
                                                    })),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .gap_2().flex_wrap()
                                    .pt_2()
                                    .child(subscribe_button("ch-sub".into(), &d.info, subscribed, cx))
                                    .when_some(speed, |el, sp| el.child(pill("ch-speed", Some("gauge"), format!("{sp}x speed"), cx)))
                                    .child(pill("ch-more", Some("ellipsis"), "", cx).w_9().px_0().justify_center().on_click(move |ev, window, cx| {
                                        let info = info2.clone();
                                        let info3 = info2.clone();
                                        let id = info2.id.clone();
                                        let mut items = vec![
                                            MenuItem::new("link", "Copy channel link", move |_, cx| {
                                                copy_to_clipboard(format!("https://www.youtube.com/channel/{}", info.id), cx)
                                            }),
                                            MenuItem::new("globe", "Open on YouTube", move |_, _| {
                                                let _ = open::that(format!("https://www.youtube.com/channel/{id}"));
                                            }),
                                        ];
                                        if blocked {
                                            items.push(MenuItem::new("circle-check", "Unblock channel", move |_, cx| {
                                                let id = info3.id.clone();
                                                store::update_settings(|s| s.blocked_channels.retain(|(i, _)| *i != id));
                                                cx.refresh_windows();
                                            }));
                                        } else {
                                            items.push(MenuItem::new("ban", "Block channel (hide everywhere)", move |_, cx| {
                                                let (id, name) = (info3.id.clone(), info3.name.clone());
                                                store::update_settings(|s| s.blocked_channels.push((id, name)));
                                                cx.refresh_windows();
                                            }));
                                        }
                                        if store::speed_reset_available(&info2.id) {
                                            let id = info2.id.clone();
                                            items.push(MenuItem::new("gauge", "Reset remembered speed", move |_, cx| {
                                                store::clear_channel_speed(&id);
                                                cx.refresh_windows();
                                            }));
                                        }
                                        open_menu(ev.position(), items, window, cx);
                                    })),
                            ),
                    ),
            );
        let mut tabs = div().flex().flex_wrap().items_center().gap_3().border_b_1().border_color(p.border);
        for (i, t) in d.tabs.iter().enumerate() {
            let active = i == self.tab && self.searching.is_none();
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("ct-{i}")))
                    .py_3()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .text_color(if active { p.text } else { p.text2 })
                    .when(active, |el| el.border_b_2().border_color(p.text))
                    .hover(move |s| s.text_color(p.text))
                    .child(t.title.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.select_tab(i, cx))),
            );
        }
        tabs = tabs.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .w(px(220.))
                .child(icon("search").size_4().text_color(p.text2))
                .child(Input::new(&self.search).appearance(false).cleanable(true)),
        );
        tabs=tabs.child(chip("channel-display",if self.catalog.list{"List view"}else{"Grid view"},self.catalog.list,cx).on_click(cx.listener(|this,_,_,cx|{this.catalog.list^=true;cx.notify();})));
        let tab_title = d.tabs.get(self.tab).map(|t| t.title.clone()).unwrap_or_default();
        let body: AnyElement = match &self.tab_items {
            Load::Loading => spinner(cx).into_any_element(),
            Load::Error(e) => error_state(e, cx).into_any_element(),
            Load::Ready(items) => {
                let items: Vec<&Item> = items
                    .iter()
                    .filter(|i| match i {
                        Item::Video(v) => !store::is_filtered(v, &s) || store::settings_snapshot().blocked_channels.iter().any(|(c, _)| Some(c) == v.channel_id.as_ref()),
                        _ => true,
                    })
                    .collect();
                let page=self.catalog.window(items.len(),48);
                let items=&items[page.range()];
                let mut col=div().flex().flex_col().gap_4();
                let content=if items.is_empty() {
                    empty_state("video-off", "This channel has no content here", "", cx).into_any_element()
                } else if tab_title == "Shorts" && self.searching.is_none() {
                    let (_, w) = grid_columns(width, 200.0);
                    grid(items.iter().filter_map(|i| i.as_video()).map(|v| shorts_card(v, w, cx)).collect(), width).into_any_element()
                } else {
                    let (_, w) = grid_columns(width, 280.0);
                    let list=self.catalog.list;
                    let cards:Vec<_>=items.iter().map(|i|if list{catalog_row(i,width,cx)}else{grid_item(i,w,tab_title=="Home"||self.searching.is_some(),cx)}).collect();
                    let mut col=div().flex().flex_col().gap_6().child(if list{div().flex().flex_col().gap_4().children(cards)}else{grid(cards,width)});
                    if self.loading_more {
                        col = col.child(spinner(cx));
                    }
                    col.into_any_element()
                };
                col=col.child(content).child(catalog_nav("channel-catalog",page,cx,|this,n|{this.catalog.page=n;this.scroll=ScrollHandle::new();}));
                col.into_any_element()
            }
        };
        page_scroll("ch-scroll", &self.scroll)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .px_6()
                    .pt_4()
                    .pb_24()
                    .child(div().w(width).flex().flex_col().gap_4().child(workspace_header("Channel", "Explore uploads, playlists and community", "tv", cx)).child(header).child(tabs).child(body)),
            )
            .into_any_element()
    }
}
