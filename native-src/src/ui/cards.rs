//! Video / channel / playlist cards in YouTube desktop style.
use super::*;
use crate::api::types::*;
use crate::store;
use crate::ui::app::{MenuItem, Route, navigate, open_menu};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

// ───────────────────────── watched progress cache ─────────────────────────

#[derive(Default)]
pub struct Watched(pub HashMap<String, (f64, f64)>);
impl Global for Watched {}

pub fn watched_progress(id: &str, cx: &App) -> Option<f32> {
    let (p, d) = *cx.try_global::<Watched>()?.0.get(id)?;
    if d > 0.0 {
        Some((p / d).clamp(0.0, 1.0) as f32)
    } else {
        Some(0.0)
    }
}

pub fn is_watched(id: &str, cx: &App) -> bool {
    cx.try_global::<Watched>().map(|w| w.0.contains_key(id)).unwrap_or(false)
}

// ───────────────────────── DeArrow lazy lookups ─────────────────────────

fn pending() -> &'static Mutex<HashSet<String>> {
    static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Returns (title, thumbnail) possibly replaced by DeArrow.
pub fn branded(v: &VideoItem, cx: &mut App) -> (String, String) {
    let s = store::settings_snapshot();
    if !s.dearrow_titles && !s.dearrow_thumbnails {
        return (v.title.clone(), v.thumbnail.clone());
    }
    match crate::api::services::branding_cached(&v.id) {
        Some(Some(b)) => {
            let title = if s.dearrow_titles { b.title.clone().unwrap_or(v.title.clone()) } else { v.title.clone() };
            let thumb = match (s.dearrow_thumbnails, b.thumbnail_time) {
                (true, Some(t)) => crate::api::services::dearrow_thumbnail_url(&v.id, t),
                _ => v.thumbnail.clone(),
            };
            (title, thumb)
        }
        Some(None) => (v.title.clone(), v.thumbnail.clone()),
        None => {
            let id = v.id.clone();
            if pending().lock().insert(id.clone()) {
                cx.spawn(async move |cx| {
                    let id2 = id.clone();
                    blocking(move || crate::api::services::dearrow_branding(&id2)).await;
                    // cached now; dropping the mark lets it be looked up again if the cache is trimmed
                    pending().lock().remove(&id);
                    let _ = cx.update(|cx| cx.refresh_windows());
                })
                .detach();
            }
            (v.title.clone(), v.thumbnail.clone())
        }
    }
}

// ───────────────────────── shared pieces ─────────────────────────

pub fn meta_line(v: &VideoItem) -> String {
    let s = store::settings_snapshot();
    let mut parts = Vec::new();
    if !s.hide_view_counts {
        if let Some(vw) = &v.views_text {
            parts.push(vw.clone());
        }
    }
    if v.is_live {
        if parts.is_empty() {
            parts.push("LIVE".into());
        }
    } else if let Some(p) = &v.published_text {
        parts.push(p.clone());
    } else if let Some(ts) = v.published_ts {
        parts.push(crate::api::json::relative_from_ts(ts));
    }
    parts.join(" • ")
}

fn badge(v: &VideoItem, cx: &App) -> Option<AnyElement> {
    let _ = cx;
    let (text, bg): (String, Hsla) = if v.is_live {
        ("LIVE".into(), rgb(0xcc0000).into())
    } else if v.is_upcoming {
        ("UPCOMING".into(), hsla(0., 0., 0., 0.8))
    } else if v.is_short && v.duration_text.is_none() {
        ("SHORTS".into(), hsla(0., 0., 0., 0.8))
    } else {
        (v.duration_text.clone()?, hsla(0., 0., 0., 0.8))
    };
    Some(
        div()
            .absolute()
            .bottom_1()
            .right_1()
            .px_1()
            .rounded_sm()
            .bg(bg)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(white())
            .child(text)
            .into_any_element(),
    )
}

pub fn thumbnail(v: &VideoItem, url: String, w: Pixels, h: Pixels, radius: Pixels, cx: &App) -> Div {
    let p = pal(cx);
    let progress = watched_progress(&v.id, cx);
    div()
        .relative()
        .w(w)
        .h(h)
        .flex_none()
        .rounded(radius * (store::settings_snapshot().thumb_radius / 12.0))
        .overflow_hidden()
        .bg(p.surface)
        .child(img(SharedString::from(url)).size_full().object_fit(ObjectFit::Cover))
        .child(crate::player::preview::slot_marker(&v.id, cx))
        .children(badge(v, cx))
        .when_some(progress.filter(|p| *p > 0.01), |d, pr| {
            d.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(4.))
                    .bg(hsla(0., 0., 0.6, 0.6))
                    .child(div().h_full().w(relative(pr)).bg(p.accent)),
            )
        })
}

pub fn video_menu_items(v: &VideoItem) -> Vec<MenuItem> {
    let mut items = Vec::new();
    {
        let route = if v.is_short { Route::Shorts(Some(v.id.clone())) } else { Route::Watch { id: v.id.clone(), playlist: None, start: None } };
        items.push(MenuItem::new("app-window", "Open in new tab", move |window, cx| {
            crate::ui::app::open_in_new_tab(route.clone(), false, window, cx)
        }));
    }
    let v1 = v.clone();
    items.push(MenuItem::new("list-plus", "Add to queue", move |_, cx| {
        crate::player::controller::enqueue(v1.clone(), cx);
        toast(cx, "Added to queue");
    }));
    let v2 = v.clone();
    items.push(MenuItem::new("clock", "Save to Watch later", move |_, cx| {
        if let Some(wl) = store::playlists().into_iter().find(|p| p.protected && p.name == "Watch later") {
            store::playlist_add(wl.id, &v2);
            toast(cx, "Saved to Watch later");
        }
    }));
    let v3 = v.clone();
    items.push(MenuItem::new("bookmark", "Save to playlist…", move |window, cx| {
        crate::ui::app::open_save_dialog(v3.clone(), window, cx);
    }));
    let v4 = v.clone();
    items.push(MenuItem::new("download", "Download", move |_, cx| {
        crate::downloads::start(v4.clone(), None, cx);
    }));
    let v5 = v.clone();
    items.push(MenuItem::new("external-link", "Open in external player", move |_, cx| {
        crate::downloads::open_external(&v5.id, cx);
    }));
    let id = v.id.clone();
    items.push(MenuItem::new("link", "Copy link", move |_, cx| {
        copy_to_clipboard(format!("https://www.youtube.com/watch?v={id}"), cx);
    }));
    let id2 = v.id.clone();
    items.push(MenuItem::new("globe", "Open on YouTube", move |_, _| {
        let _ = open::that(format!("https://www.youtube.com/watch?v={id2}"));
    }));
    if let Some(cid) = v.channel_id.clone() {
        let name = v.channel_name.clone();
        items.push(MenuItem::new("ban", "Don't recommend channel", move |_, cx| {
            let (cid, name) = (cid.clone(), name.clone());
            store::update_settings(|s| {
                if !s.blocked_channels.iter().any(|(i, _)| *i == cid) {
                    s.blocked_channels.push((cid, name));
                }
            });
            toast(cx, "Channel blocked. Manage in Settings → Content filters");
            cx.refresh_windows();
        }));
    }
    if is_watched_in_history(&v.id) {
        let id3 = v.id.clone();
        items.push(MenuItem::new("eye-off", "Remove from history", move |_, cx| {
            store::history_remove(&id3);
            cx.default_global::<Watched>().0.remove(&id3);
            cx.refresh_windows();
        }));
    }
    items
}

fn is_watched_in_history(id: &str) -> bool {
    store::history_position(id).is_some()
}

fn more_button(id: SharedString, v: &VideoItem, cx: &App) -> Stateful<Div> {
    let p = pal(cx);
    let v = v.clone();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size_8()
        .rounded_full()
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover))
        .child(icon("ellipsis-vertical").size_4().text_color(p.text))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |ev, window, cx| {
            cx.stop_propagation();
            open_menu(ev.position(), video_menu_items(&v), window, cx);
        })
}

fn watch_route(v: &VideoItem) -> Route {
    Route::Watch { id: v.id.clone(), playlist: None, start: None }
}

// ───────────────────────── grid card (home / subscriptions / channel) ─────────────────────────

/// Starts resolving a video's streams when the pointer rests on its thumbnail,
/// so playback begins almost immediately on click.
fn prefetch_hover(id: &str) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let id = id.to_string();
    move |hovered, _, cx| {
        if *hovered {
            crate::api::streams::prefetch_on_hover(&id);
        }
        crate::player::preview::hover(&id, *hovered, cx);
    }
}

pub fn video_card(v: &VideoItem, width: Pixels, show_channel: bool, cx: &mut App) -> AnyElement {
    if v.is_short {
        return shorts_card(v, width.min(px(240.)), cx);
    }
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let h = width * (9. / 16.);
    let route = watch_route(v);
    let channel_route = v.channel_id.clone().map(Route::Channel);
    let meta = meta_line(v);
    let show_avatar = show_channel && !store::settings_snapshot().hide_channel_avatars;
    div()
        .id(SharedString::from(format!("vc-{}", v.id)))
        .on_hover(prefetch_hover(&v.id))
        .flex()
        .flex_col()
        .w(width)
        .gap_3()
        .cursor_pointer()
        .group("card")
        .on_click(move |_, window, cx| navigate(route.clone(), window, cx))
        .child(thumbnail(v, thumb, width, h, px(12.), cx))
        .child(
            div()
                .flex()
                .gap_3()
                .w_full()
                .when(show_avatar, |d| {
                    let cr = channel_route.clone();
                    d.child(
                        div()
                            .id(SharedString::from(format!("va-{}", v.id)))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                if let Some(r) = cr.clone() {
                                    navigate(r, window, cx);
                                }
                            })
                            .child(avatar(v.channel_avatar.as_deref(), px(36.), &v.channel_name, cx)),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.text)
                                .line_clamp(2)
                                .child(title),
                        )
                        .when(show_channel && !v.channel_name.is_empty(), |d| {
                            let cr = channel_route.clone();
                            d.child(
                                div()
                                    .id(SharedString::from(format!("vn-{}", v.id)))
                                    .text_sm()
                                    .text_color(p.text2)
                                    .hover(move |s| s.text_color(p.text))
                                    .truncate()
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(r) = cr.clone() {
                                            navigate(r, window, cx);
                                        }
                                    })
                                    .child(v.channel_name.clone()),
                            )
                        })
                        .child(div().text_sm().text_color(p.text2).truncate().child(meta)),
                )
                .child(more_button(SharedString::from(format!("vm-{}", v.id)), v, cx)),
        )
        .into_any_element()
}

pub fn shorts_card(v: &VideoItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let id = v.id.clone();
    let h = width * (16. / 9.);
    div()
        .id(SharedString::from(format!("sc-{}", v.id)))
        .flex()
        .flex_col()
        .w(width)
        .gap_2()
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let route = if store::settings_snapshot().shorts_in_player {
                Route::Watch { id: id.clone(), playlist: None, start: None }
            } else {
                Route::Shorts(Some(id.clone()))
            };
            navigate(route, window, cx)
        })
        .child(thumbnail(v, thumb, width, h, px(12.), cx))
        .child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(title))
                        .when_some(v.views_text.clone(), |d, vt| d.child(div().text_xs().text_color(p.text2).child(vt))),
                )
                .child(more_button(SharedString::from(format!("sm-{}", v.id)), v, cx)),
        )
        .into_any_element()
}

// ───────────────────────── list row (search results) ─────────────────────────

pub fn video_row(v: &VideoItem, thumb_w: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let route = watch_route(v);
    let channel_route = v.channel_id.clone().map(Route::Channel);
    let meta = meta_line(v);
    div()
        .id(SharedString::from(format!("vr-{}", v.id)))
        .on_hover(prefetch_hover(&v.id))
        .flex()
        .gap_4()
        .w_full()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(route.clone(), window, cx))
        .child(thumbnail(v, thumb, thumb_w, thumb_w * (9. / 16.), px(12.), cx))
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
                        .gap_2()
                        .child(div().flex_1().text_lg().text_color(p.text).line_clamp(2).child(title))
                        .child(more_button(SharedString::from(format!("vrm-{}", v.id)), v, cx)),
                )
                .child(div().text_xs().text_color(p.text2).child(meta))
                .when(!v.channel_name.is_empty(), |d| {
                    let cr = channel_route.clone();
                    d.child(
                        div()
                            .id(SharedString::from(format!("vrc-{}", v.id)))
                            .flex()
                            .items_center()
                            .gap_2()
                            .py_2()
                            .text_xs()
                            .text_color(p.text2)
                            .hover(move |s| s.text_color(p.text))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                if let Some(r) = cr.clone() {
                                    navigate(r, window, cx);
                                }
                            })
                            .child(avatar(v.channel_avatar.as_deref(), px(24.), &v.channel_name, cx))
                            .child(v.channel_name.clone()),
                    )
                })
                .when_some(v.description_snippet.clone(), |d, s| {
                    d.child(div().text_xs().text_color(p.text2).line_clamp(2).child(s))
                }),
        )
        .into_any_element()
}

/// Compact row used for related videos / queues.
pub fn compact_row(v: &VideoItem, active: bool, on_click: Option<Arc<dyn Fn(&mut Window, &mut App)>>, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let route = watch_route(v);
    div()
        .id(SharedString::from(format!("cr-{}", v.id)))
        .on_hover(prefetch_hover(&v.id))
        .flex()
        .gap_2()
        .w_full()
        .p_1()
        .rounded_lg()
        .cursor_pointer()
        .when(active, |d| d.bg(p.hover))
        .hover(move |s| s.bg(p.surface))
        .on_click(move |_, window, cx| match &on_click {
            Some(f) => f(window, cx),
            None => navigate(route.clone(), window, cx),
        })
        .child(thumbnail(v, thumb, px(168.), px(94.), px(8.), cx))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(title))
                .child(div().text_xs().text_color(p.text2).truncate().child(v.channel_name.clone()))
                .child(div().text_xs().text_color(p.text2).truncate().child(meta_line(v))),
        )
        .child(more_button(SharedString::from(format!("crm-{}", v.id)), v, cx))
        .into_any_element()
}

// ───────────────────────── channel & playlist ─────────────────────────

pub fn channel_row(c: &ChannelItem, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = c.id.clone();
    let subscribed = store::is_subscribed(&c.id);
    let c2 = c.clone();
    div()
        .id(SharedString::from(format!("ch-{}", c.id)))
        .flex()
        .items_center()
        .gap_4()
        .w_full()
        .py_2()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Channel(id.clone()), window, cx))
        .child(div().w(px(360.)).flex().justify_center().child(avatar(c.avatar.as_deref(), px(136.), &c.name, cx)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_1()
                .child(div().text_lg().text_color(p.text).child(c.name.clone()))
                .child(
                    div().text_xs().text_color(p.text2).child(
                        [c.handle.clone(), c.subscribers_text.clone(), c.video_count_text.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" • "),
                    ),
                )
                .when_some(c.description.clone(), |d, s| d.child(div().text_xs().text_color(p.text2).line_clamp(2).child(s))),
        )
        .child(subscribe_button(SharedString::from(format!("chs-{}", c.id)), &c2, subscribed, cx))
        .into_any_element()
}

/// Mirrors a subscribe/unsubscribe to the signed-in YouTube account.
pub fn sync_subscription(channel_id: String, on: bool, cx: &mut App) {
    if !crate::api::auth::signed_in() || channel_id.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        let res = blocking(move || crate::api::auth::subscribe(&channel_id, on)).await;
        if res.is_err() {
            let _ = cx.update(|cx| toast(cx, "Couldn't update the subscription on your YouTube account"));
        }
    })
    .detach();
}

pub fn subscribe_button(id: SharedString, c: &ChannelItem, subscribed: bool, cx: &App) -> AnyElement {
    let p = pal(cx);
    let c = c.clone();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h_9()
        .px_4()
        .rounded_full()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(subscribed, |d| d.bg(p.chip).text_color(p.text).hover(move |s| s.bg(p.hover)).child(icon("bell").size_4()).child("Subscribed"))
        .when(!subscribed, |d| d.bg(p.chip_active_bg).text_color(p.chip_active_fg).child("Subscribe"))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            let on = !store::is_subscribed(&c.id);
            if on {
                store::subscribe(&c.id, &c.name, c.avatar.as_deref());
                toast(cx, format!("Subscribed to {}", c.name));
            } else {
                store::unsubscribe(&c.id);
                toast(cx, format!("Unsubscribed from {}", c.name));
            }
            sync_subscription(c.id.clone(), on, cx);
            cx.refresh_windows();
        })
        .into_any_element()
}

pub fn channel_card(c: &ChannelItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = c.id.clone();
    let subscribed = store::is_subscribed(&c.id);
    div()
        .id(SharedString::from(format!("cc-{}", c.id)))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .w(width)
        .py_4()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Channel(id.clone()), window, cx))
        .child(avatar(c.avatar.as_deref(), px(104.), &c.name, cx))
        .child(div().text_base().text_color(p.text).text_center().line_clamp(1).child(c.name.clone()))
        .when_some(c.subscribers_text.clone(), |d, s| d.child(div().text_xs().text_color(p.text2).child(s)))
        .child(subscribe_button(SharedString::from(format!("ccs-{}", c.id)), c, subscribed, cx))
        .into_any_element()
}

pub fn playlist_card(pl: &PlaylistItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = pl.id.clone();
    let h = width * (9. / 16.);
    div()
        .id(SharedString::from(format!("pc-{}", pl.id)))
        .flex()
        .flex_col()
        .w(width)
        .gap_3()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Playlist(id.clone()), window, cx))
        .child(playlist_thumb(pl.thumbnail.clone(), pl.video_count_text.clone(), width, h, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(pl.title.clone()))
                .when_some(pl.channel_name.clone(), |d, c| d.child(div().text_sm().text_color(p.text2).truncate().child(c)))
                .child(div().text_sm().text_color(p.text2).child("View full playlist")),
        )
        .into_any_element()
}

pub fn playlist_thumb(thumb: Option<String>, count: Option<String>, w: Pixels, h: Pixels, cx: &App) -> Div {
    let p = pal(cx);
    div()
        .relative()
        .w(w)
        .h(h)
        .pt_2()
        .child(
            div()
                .absolute()
                .top_0()
                .left(px(12.))
                .right(px(12.))
                .h_4()
                .rounded_t_lg()
                .bg(p.hover),
        )
        .child(
            div()
                .relative()
                .size_full()
                .rounded_xl()
                .overflow_hidden()
                .bg(p.surface)
                .when_some(thumb, |d, t| d.child(img(SharedString::from(t)).size_full().object_fit(ObjectFit::Cover)))
                .child(
                    div()
                        .absolute()
                        .bottom_1()
                        .right_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1()
                        .rounded_sm()
                        .bg(hsla(0., 0., 0., 0.8))
                        .text_xs()
                        .text_color(white())
                        .child(icon("list-video").size_3().text_color(white()))
                        .child(count.unwrap_or_default()),
                ),
        )
}

pub fn playlist_row(pl: &PlaylistItem, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = pl.id.clone();
    div()
        .id(SharedString::from(format!("pr-{}", pl.id)))
        .flex()
        .gap_4()
        .w_full()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Playlist(id.clone()), window, cx))
        .child(playlist_thumb(pl.thumbnail.clone(), pl.video_count_text.clone(), px(360.), px(202.), cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .child(div().text_lg().text_color(p.text).line_clamp(2).child(pl.title.clone()))
                .when_some(pl.channel_name.clone(), |d, c| d.child(div().text_xs().text_color(p.text2).child(c)))
                .child(div().text_xs().text_color(p.text2).font_weight(FontWeight::MEDIUM).child("View full playlist")),
        )
        .into_any_element()
}

/// A reading-oriented catalog layout using the same production navigation/actions.
pub fn catalog_row(item:&Item,width:Pixels,cx:&mut App)->AnyElement {
    match item {
        Item::Video(v)=>video_row(v,(width*0.35).max(px(96.)).min(px(240.)),cx),
        Item::Channel(c)=>channel_row(c,cx),
        Item::Playlist(p)=>playlist_row(p,cx),
    }
}

/// Renders any item in a grid.
pub fn grid_item(item: &Item, width: Pixels, show_channel: bool, cx: &mut App) -> AnyElement {
    match item {
        Item::Video(v) => video_card(v, width, show_channel, cx),
        Item::Channel(c) => channel_card(c, width, cx),
        Item::Playlist(p) => playlist_card(p, width, cx),
    }
}

/// Grid layout: computes columns from available width.
pub fn grid(items: Vec<AnyElement>, _avail: Pixels) -> Div {
    let gy = match store::settings_snapshot().density.as_str() {
        "compact" => 20.,
        "comfortable" => 48.,
        _ => 36.,
    };
    div().flex().flex_wrap().gap_x_4().gap_y(px(gy)).w_full().children(items)
}

pub fn grid_columns(avail: Pixels, min_card: f32) -> (usize, Pixels) {
    let s=store::settings_snapshot();
    let (cols,card)=crate::wetube_workspace::grid(avail.into(),min_card,s.grid_columns as usize,s.ui_scale);
    (cols,px(card))
}
