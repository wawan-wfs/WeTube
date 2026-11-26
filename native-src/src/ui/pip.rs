//! Picture-in-picture: the player in a small always-on-top window that stays
//! visible over other apps.
use crate::player::controller::player;
use crate::player::view::PlayerView;
use crate::ui::*;

#[derive(Default)]
struct PipWindow(Option<AnyWindowHandle>);
impl Global for PipWindow {}

pub struct PipView {
    view: Entity<PlayerView>,
}

pub fn handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<PipWindow>().and_then(|p| p.0)
}

pub fn open(cx: &mut App) {
    if let Some(h) = cx.try_global::<PipWindow>().and_then(|p| p.0) {
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let size = cx.primary_display().map(|d| {
        let b = d.bounds();
        let w = px(480.).min((b.size.width - px(48.)).max(px(256.))).min((b.size.height - px(96.)).max(px(144.)) * (16.0 / 9.0));
        size(w, w * (9.0 / 16.0))
    }).unwrap_or(size(px(480.), px(270.)));
    let origin = cx
        .primary_display()
        .map(|d| {
            let b = d.bounds();
            point((b.origin.x + b.size.width - size.width - px(24.)).max(b.origin.x), (b.origin.y + b.size.height - size.height - px(72.)).max(b.origin.y))
        })
        .unwrap_or(point(px(80.), px(80.)));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size))),
        titlebar: None,
        kind: WindowKind::PopUp,
        is_resizable: true,
        is_minimizable: false,
        window_min_size: Some(gpui_kit::size(px(256.), px(144.))),
        ..Default::default()
    };
    let res = gpui_kit::open_window(opts, cx, |_, cx| {
        let pl = player(cx);
        let view = cx.new(|cx| {
            let mut v = PlayerView::new(pl, false, cx);
            v.pip = true;
            v
        });
        cx.new(|_| PipView { view })
    });
    match res {
        Ok((h, _)) => cx.set_global(PipWindow(Some(h))),
        Err(e) => toast(cx, crate::ui::friendly_error(&e.to_string())),
    }
    cx.refresh_windows();
}

pub fn close(window: &mut Window, cx: &mut App) {
    cx.set_global(PipWindow(None));
    window.remove_window();
    cx.refresh_windows();
}

impl Render for PipView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("pip")
            .group("pip")
            .relative()
            .size_full()
            .bg(black())
            .child(self.view.clone())
            // top strip: drag to move, close button
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .visible()
                    .bg(linear_gradient(180., linear_color_stop(hsla(0., 0., 0., 0.6), 0.), linear_color_stop(hsla(0., 0., 0., 0.), 1.)))
                    .child(div().id("pip-drag").flex_1().h_full().window_control_area(WindowControlArea::Drag))
                    .child(
                        div()
                            .id("pip-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hsla(0., 0., 1., 0.15)))
                            .child(icon("x").size_4().text_color(white()))
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                close(window, cx);
                            }),
                    ),
            )
    }
}
