#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod api;
mod assets;
mod display;
mod perf;
mod recommend;
mod downloads;
mod feed;
mod import;
mod links;
mod locale;
mod player;
mod store;
mod ui;
mod updater;
mod wetube_session;
mod wetube_migration;
mod wetube_links;
mod wetube_queue;
mod wetube_runtime;
mod wetube_recovery;
mod wetube_process;
mod wetube_resume;
mod wetube_publish;
mod wetube_profile;
mod wetube_journal;
mod wetube_transfer;
mod wetube_workspace;
mod wetube_occurrence;
mod wetube_visual;
mod wetube_catalog;

pub(crate) const PRODUCT_NAME: &str = "WeTube Desktop";
pub(crate) const PRODUCT_CREDIT: &str = "by WFS Project";
pub(crate) const PRODUCT_FULL_NAME: &str = "WeTube Desktop by WFS Project";

use gpui_kit::*;

/// Window placement from the last session (size, position, maximized), or a
/// centered default sized to the screen.
fn saved_window_bounds(cx: &App) -> WindowBounds {
    if let Some(v) = store::kv_get("window").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let (w, h) = (f("w"), f("h"));
        if w >= 640.0 && h >= 400.0 {
            let b = Bounds::new(point(px(f("x")), px(f("y"))), size(px(w), px(h)));
            // ignore positions that are no longer on any display
            let visible = cx.displays().iter().any(|d| d.bounds().intersects(&b));
            if visible {
                return if v.get("maximized").and_then(|m| m.as_bool()).unwrap_or(false) {
                    WindowBounds::Maximized(b)
                } else {
                    WindowBounds::Windowed(b)
                };
            }
        }
    }
    let display = cx.primary_display().map(|d| d.bounds().size).unwrap_or(size(px(1600.), px(1000.)));
    let sz = size((display.width * 0.82).min(px(1600.)), (display.height * 0.85).min(px(1000.)));
    WindowBounds::Windowed(Bounds::centered(None, sz, cx))
}

pub fn save_window_bounds(window: &Window) {
    let (b, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (b, false),
        WindowBounds::Maximized(b) => (b, true),
        WindowBounds::Fullscreen(b) => (b, false),
    };
    let v = serde_json::json!({
        "x": f32::from(b.origin.x), "y": f32::from(b.origin.y),
        "w": f32::from(b.size.width), "h": f32::from(b.size.height),
        "maximized": maximized,
    });
    store::kv_set("window", &v.to_string());
}

fn main() {
    if matches!(std::env::args().nth(1).as_deref(), Some("--runtime-self-test" | "--runtime-video-test")) {
        std::process::exit(wetube_runtime::run_cli());
    }
    let probe=match wetube_visual::Probe::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(probe)=>probe,Err(e)=>{eprintln!("Visual probe: {e}");std::process::exit(2);}
    };
    if probe.is_some(){
        let valid=std::env::var_os("WETUBE_NATIVE_DATA_DIR").map(std::path::PathBuf::from)
            .ok_or_else(||anyhow::anyhow!("WETUBE_NATIVE_DATA_DIR must be set for visual probe"))
            .and_then(|p|wetube_visual::Probe::validate_profile(&p));
        if let Err(e)=valid{eprintln!("Visual probe: {e}");std::process::exit(2);}
    }
    // Hold the profile lock until GUI shutdown; runtime CLI does not use the profile.
    let _profile = match wetube_profile::ProfileLock::acquire(&store::data_dir()) {
        Ok(lock)=>lock,
        Err(e)=>{eprintln!("WeTube profile is busy or inaccessible: {e}");std::process::exit(2);}
    };
    perf::mark("main");
    // the local stream proxy must never be reached through the user's proxy
    // SAFETY: set before any other thread starts
    unsafe {
        std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    }
    perf::init();
    store::init();
    if let Some(p)=&probe {store::update_settings(|s|s.ui_scale=p.scale);}
    perf::mark("store");
    display::choose();
    perf::mark("display");
    // one running app: a second launch (e.g. a clicked streamtube: link) hands over and exits
    let arg = std::env::args().nth(1).unwrap_or_default();
    if false {
        return;
    }
    let incoming: Option<futures::channel::mpsc::UnboundedReceiver<String>> = None;
    perf::mark("links");
    gpui_kit::application()
        .with_assets(assets::Assets)
        .with_http_client(ui::UreqHttp::new())
        .run(move |cx: &mut App| {
            perf::mark("gpui running");
            gpui_kit::init(cx);
            perf::mark("gpui_kit init");
            player::preview::init(cx);
            perf::mark("preview init");
            cx.set_global(ui::Pal::new(true, store::settings_snapshot().accent));
            ui::apply_theme(None, cx);
            player::controller::init(cx);
            perf::mark("player init");
            let bounds=if let Some(p)=&probe {
                let sz=size(px(p.width as f32),px(p.height as f32));
                WindowBounds::Windowed(Bounds::centered(None,sz,cx))
            } else {saved_window_bounds(cx)};
            if let Some(p)=probe.clone(){cx.set_global(p);}
            let opts = WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions { title: Some(PRODUCT_FULL_NAME.into()), appears_transparent: true, ..Default::default() }),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            };
            gpui_kit::open_window(opts, cx, |window, cx| {
                ui::apply_theme(Some(window), cx);
                cx.new(|cx| ui::app::AppView::new(window, cx))
            })
            .expect("failed to open window");
            perf::mark("window open");
            ui::app_icon::apply(cx);
            // Custom URL registration disabled pending secure single-instance implementation.
            perf::mark("icon");
            if let Some(mut rx) = incoming {
                cx.spawn(async move |cx| {
                    use futures::StreamExt;
                    while let Some(arg) = rx.next().await {
                        let _ = cx.update(|cx| {
                            let pip = ui::pip::handle(cx);
                            let Some(main) = cx.windows().into_iter().find(|w| Some(*w) != pip) else { return };
                            let _ = main.update(cx, |_, window, cx| {
                                ui::app::restore_window(window);
                                window.activate_window();
                                if let Some(route) = ui::app::route_from_link(&arg) {
                                    ui::app::navigate(route, window, cx);
                                }
                            });
                        });
                    }
                })
                .detach();
            }
            cx.on_window_closed(|cx, _| {
                // the picture-in-picture window alone doesn't keep the app running
                let pip = ui::pip::handle(cx);
                if cx.windows().iter().all(|w| Some(*w) == pip) {
                    player::controller::player(cx).update(cx, |p, _| p.save_progress(true));
                    cx.quit();
                }
            })
            .detach();
        });
}
