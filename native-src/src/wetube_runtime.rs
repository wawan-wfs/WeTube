//! Offline smoke test using the application's actual native libmpv software renderer.
use crate::player::mpv::{Mpv, MpvEvent};
use std::{path::Path, time::{Duration, Instant}};
fn check(fixture: &Path, motion: bool) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(fixture.is_file(), "Local video fixture missing");
    let (tx, rx) = std::sync::mpsc::channel();
    let mpv = Mpv::new(&[("config","no"), ("load-scripts","no"), ("vo","libmpv"), ("ao","null"),
        ("idle","yes"), ("keep-open","yes"), ("terminal","no"), ("ytdl","no"), ("hwdec","no")],
        move |ev| { let _ = tx.send(ev); })?;
    mpv.set_render_size(160, 90);
    let fixture = fixture.canonicalize()?;
    let path = fixture.to_str().ok_or_else(|| anyhow::anyhow!("Non-Unicode fixture path"))?;
    #[cfg(windows)]
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    mpv.command_sync(&["loadfile", path])?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut loaded = false;
    let mut frame = None;
    let mut first_color = None;
    let mut changed = false;
    let mut seek_requested = false;
    let mut seek_confirmed = false;
    let mut max_position = 0.0f64;
    while Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
            match ev {
                MpvEvent::FileLoaded => loaded = true,
                MpvEvent::EndFile { error: Some(e), .. } => anyhow::bail!("Local playback: {e}"),
                _ => {}
            }
        }
        if loaded {
            if let Some(f) = mpv.take_frame() {
                if f.width > 0 && f.height > 0 && f.data.chunks_exact(4).any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0) {
                    frame = Some((f.width, f.height));
                    if !motion { break; }
                    let mid = ((f.height as usize / 2) * f.width as usize + f.width as usize / 2) * 4;
                    if let Some(px) = f.data.get(mid..mid+3) {
                        let color = [px[0], px[1], px[2]];
                        if let Some(first) = first_color {
                            if first != color { changed = true; }
                        } else { first_color = Some(color); }
                    }
                }
            }
        }
        if motion && loaded {
            if let Some(pos) = mpv.get_f64("time-pos").filter(|v| v.is_finite()) {
                max_position = max_position.max(pos);
                if changed && pos >= 0.4 && !seek_requested {
                    mpv.command_sync(&["seek", "2.5", "absolute+exact"])?;
                    seek_requested = true;
                } else if seek_requested && (2.3..=3.9).contains(&pos) {
                    seek_confirmed = true;
                    break;
                }
            }
        }
    }
    if motion {
        anyhow::ensure!(changed && seek_confirmed, "Moving video or seek acceptance did not complete");
    }
    let (w,h) = frame.ok_or_else(|| anyhow::anyhow!("No non-black decoded frame from local fixture"))?;
    Ok(serde_json::json!({"status":"PASS","file_loaded":loaded,"frame_width":w,"frame_height":h,"motion_required":motion,"distinct_frames":changed,"seek_confirmed":seek_confirmed,"max_position_seconds":max_position,"scope":"application Rust libmpv renderer; local fixture only; not GPUI or YouTube"}))
}
pub fn run_cli() -> i32 {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 { return 2; }
    let motion = args[1] == "--runtime-video-test";
    let report = match check(Path::new(&args[2]), motion) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"status":"FAIL","error":e.to_string()}),
    };
    if std::fs::write(&args[3], report.to_string()).is_err() { return 3; }
    if report["status"] == "PASS" { 0 } else { 1 }
}
