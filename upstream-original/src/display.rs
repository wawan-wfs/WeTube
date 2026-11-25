//! Picks the Linux display server before the window opens.
//!
//! Some Wayland compositors (ChromeOS's Linux container among them) accept the
//! window but never show it. X11, through XWayland, works there. The choice is:
//! `STREAMTUBE_BACKEND=x11|wayland` if set, else the "Display server" setting,
//! else X11 inside ChromeOS and Wayland elsewhere. If a Wayland window still
//! hasn't drawn anything after a while, the app restarts itself on X11 and keeps
//! that setting.
use std::sync::atomic::{AtomicBool, Ordering};

/// Set once the main window has drawn its first frame.
static DRAWN: AtomicBool = AtomicBool::new(false);
/// Set in the environment of a copy started by the fallback.
const RELAUNCHED: &str = "STREAMTUBE_RELAUNCHED";
/// How long a Wayland window may take to show up before falling back.
#[cfg(target_os = "linux")]
const GRACE: std::time::Duration = std::time::Duration::from_secs(12);

pub fn mark_drawn() {
    DRAWN.store(true, Ordering::Relaxed);
}

/// Whether this process was started by the fallback (it must not hand over to
/// the copy that started it).
pub fn relaunched() -> bool {
    std::env::var_os(RELAUNCHED).is_some()
}

/// Call at startup, before any threads that read the environment.
#[cfg(target_os = "linux")]
pub fn choose() {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let x11 = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    if !wayland || !x11 {
        // only one of them is available: nothing to choose
        return;
    }
    let forced = std::env::var("STREAMTUBE_BACKEND").unwrap_or_default().to_lowercase();
    let setting = crate::store::settings_snapshot().display_server.clone();
    let use_x11 = match forced.as_str() {
        "x11" => true,
        "wayland" => false,
        _ => setting == "x11" || (setting == "auto" && in_chromeos()),
    };
    if use_x11 {
        // SAFETY: called from main before other threads start
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        return;
    }
    if forced == "wayland" || setting == "wayland" || relaunched() {
        return;
    }
    // safety net: no frame on screen after a while means the window never appeared
    std::thread::spawn(|| {
        std::thread::sleep(GRACE);
        if DRAWN.load(Ordering::Relaxed) {
            return;
        }
        eprintln!("Stream Tube: the Wayland window didn't appear; restarting on X11 (Settings → Backend & network → Display server)");
        crate::store::update_settings(|s| s.display_server = "x11".into());
        let Ok(exe) = std::env::current_exe() else { return };
        let started = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env_remove("WAYLAND_DISPLAY")
            .env(RELAUNCHED, "1")
            .spawn();
        if started.is_ok() {
            std::process::exit(0);
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn choose() {}

/// ChromeOS runs Linux apps in a container that has this folder.
#[cfg(target_os = "linux")]
fn in_chromeos() -> bool {
    std::path::Path::new("/opt/google/cros-containers").is_dir() || std::env::var_os("SOMMELIER_VERSION").is_some()
}
