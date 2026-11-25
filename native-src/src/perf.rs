//! Lightweight performance counters (enabled with FERROTUBE_PERF=1), printed every 2 s.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

pub static MPV_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static MPV_RENDER_US: AtomicU64 = AtomicU64::new(0);
pub static UI_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static UI_HANDLE_US: AtomicU64 = AtomicU64::new(0);
pub static VIEW_RENDERS: AtomicU64 = AtomicU64::new(0);
pub static APP_RENDERS: AtomicU64 = AtomicU64::new(0);
pub static PAGE_RENDERS: AtomicU64 = AtomicU64::new(0);
static ON: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn add(c: &AtomicU64, v: u64) {
    if on() {
        c.fetch_add(v, Ordering::Relaxed);
    }
}

pub fn init() {
    if std::env::var("STREAMTUBE_PERF").is_ok() {
        ON.store(true, Ordering::Relaxed);
        START.get_or_init(Instant::now);
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let t = |c: &AtomicU64| c.swap(0, Ordering::Relaxed);
            let (mf, mr, uf, uh, vr, ar, pr) =
                (t(&MPV_FRAMES), t(&MPV_RENDER_US), t(&UI_FRAMES), t(&UI_HANDLE_US), t(&VIEW_RENDERS), t(&APP_RENDERS), t(&PAGE_RENDERS));
            eprintln!(
                "[perf] mpv {:.1} fps ({:.2} ms/render) | ui frames {:.1}/s ({:.2} ms/handle) | player renders {:.1}/s | app renders {:.1}/s | page renders {:.1}/s",
                mf as f64 / 2.0,
                if mf > 0 { mr as f64 / mf as f64 / 1000.0 } else { 0.0 },
                uf as f64 / 2.0,
                if uf > 0 { uh as f64 / uf as f64 / 1000.0 } else { 0.0 },
                vr as f64 / 2.0,
                ar as f64 / 2.0,
                pr as f64 / 2.0,
            );
        });
    }
}

/// Startup timeline (with STREAMTUBE_PERF set): milliseconds since launch at each step.
pub fn mark(what: &str) {
    static T0: OnceLock<Instant> = OnceLock::new();
    let t0 = *T0.get_or_init(Instant::now);
    if std::env::var_os("STREAMTUBE_PERF").is_some() {
        eprintln!("[startup] {:>6.1} ms  {what}", t0.elapsed().as_secs_f64() * 1000.0);
    }
}
