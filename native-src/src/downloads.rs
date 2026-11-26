//! Download manager (yt-dlp when available, native fallback) and external player launching.
use crate::api::streams::{self, hide_window};
use crate::api::types::VideoItem;
use crate::store;
use crate::ui::toast;
use gpui_kit::*;
use parking_lot::Mutex;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    Queued,
    Running,
    Done(String),
    Failed(String),
    Cancelled,
}

#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub durable_id: String,
    pub video: VideoItem,
    pub format: String,
    pub directory: String,
    pub status: JobStatus,
    pub progress: f32,
    pub detail: String,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Downloads {
    pub jobs: Arc<Mutex<Vec<Job>>>,
    next: u64,
    ticking: bool,
    history_loaded: bool,
    history_blocked: bool,
}

impl Global for Downloads {}

pub const FORMATS: &[(&str, &str)] = &[
    ("1080", "1080p (recommended)"),
    ("best", "Best available (up to 4K, large files)"),
    ("720", "Up to 720p"),
    ("480", "Up to 480p"),
    ("audio", "Audio only"),
];

/// Starts several downloads, asking for the folder at most once.
pub fn start_many(videos: Vec<VideoItem>, cx: &mut App) {
    if videos.is_empty() {
        return;
    }
    if !store::settings_snapshot().download_dir_chosen {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Save downloads here".into()) });
        cx.spawn(async move |cx| {
            let picked = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|p| p.into_iter().next());
            let _ = cx.update(|cx| match picked {
                Some(dir) => {
                    store::update_settings(|s| {
                        s.download_dir = dir.to_string_lossy().into_owned();
                        s.download_dir_chosen = true;
                    });
                    start_many(videos, cx);
                }
                None => toast(cx, "Download cancelled"),
            });
        })
        .detach();
        return;
    }
    let n = videos.len();
    for v in videos {
        start(v, None, cx);
    }
    if n > 1 {
        toast(cx, format!("Downloading {n} videos"));
    }
}

pub fn start(video: VideoItem, format: Option<String>, cx: &mut App) {
    if !crate::wetube_session::valid_id(&video.id) { toast(cx, "Invalid video ID"); return; }
    // First download: ask where to save (once), like a browser does.
    if !store::settings_snapshot().download_dir_chosen {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save downloads here".into()),
        });
        cx.spawn(async move |cx| {
            let picked = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|p| p.into_iter().next());
            let _ = cx.update(|cx| match picked {
                Some(dir) => {
                    store::update_settings(|s| {
                        s.download_dir = dir.to_string_lossy().into_owned();
                        s.download_dir_chosen = true;
                    });
                    start(video, format, cx);
                }
                None => toast(cx, "Download cancelled"),
            });
        })
        .detach();
        return;
    }
    if cx.default_global::<Downloads>().history_blocked { toast(cx, "Download history could not be read. New downloads are paused to protect local recovery data."); return; }
    let fmt = format.unwrap_or_else(|| store::settings_snapshot().download_format.clone());
    if !FORMATS.iter().any(|(key, _)| *key == fmt.as_str()) && fmt != "single" {
        toast(cx, "Unsupported download format"); return;
    }
    let (duplicate, active_count) = {
        let jobs = cx.default_global::<Downloads>().jobs.lock();
        let active = |j: &&Job| matches!(j.status, JobStatus::Queued | JobStatus::Running);
        (jobs.iter().filter(active).any(|j| j.video.id == video.id && j.format == fmt),
         jobs.iter().filter(active).count())
    };
    if duplicate { toast(cx, "This video and format is already queued"); return; }
    if active_count >= 500 { toast(cx, "Download queue is full (500 active jobs)"); return; }
    // without yt-dlp only 360p (one combined stream) or audio can be saved
    if fmt != "audio" && fmt != "single" && streams::ytdlp_path().is_none() {
        if YTDLP_FETCHING.load(Ordering::SeqCst) {
            PENDING.lock().push((video, Some(fmt)));
            return;
        }
        let v2 = video.clone();
        crate::ui::toast_with_actions(
            cx,
            "Higher quality downloads use yt-dlp",
            vec![
                (
                    format!("Get yt-dlp ({YTDLP_SIZE})").into(),
                    Arc::new(move |_, cx: &mut App| {
                        PENDING.lock().push((video.clone(), Some(fmt.clone())));
                        fetch_ytdlp(cx);
                    }),
                ),
                ("Download 360p".into(), Arc::new(move |_, cx: &mut App| start(v2.clone(), Some("single".into()), cx))),
            ],
        );
        return;
    }
    // separate video + audio streams need FFmpeg to be joined into one file
    if fmt != "audio" && fmt != "single" && streams::ytdlp_path().is_some() && ffmpeg_path().is_none() {
        if FFMPEG_FETCHING.load(Ordering::SeqCst) {
            // queued behind the FFmpeg download
            PENDING.lock().push((video, Some(fmt)));
            return;
        }
        let v2 = video.clone();
        crate::ui::toast_with_actions(
            cx,
            "Videos above 360p need FFmpeg to join video and audio",
            vec![
                (
                    format!("Get FFmpeg ({FFMPEG_SIZE})").into(),
                    Arc::new(move |_, cx: &mut App| {
                        PENDING.lock().push((video.clone(), Some(fmt.clone())));
                        fetch_ffmpeg(cx);
                    }),
                ),
                ("Download 360p".into(), Arc::new(move |_, cx: &mut App| start(v2.clone(), Some("single".into()), cx))),
            ],
        );
        return;
    }
    let d = cx.default_global::<Downloads>();
    d.next += 1;
    let job = Job {
        id: d.next,
        durable_id: new_durable_id(),
        video: video.clone(),
        format: fmt,
        directory: store::settings_snapshot().download_dir.clone(),
        status: JobStatus::Queued,
        progress: 0.0,
        detail: String::new(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let jobs = d.jobs.clone();
    {
        let mut list = jobs.lock();
        list.push(job.clone());
        // Persist admission before starting any worker; never silently lose queued intent.
        if !persist_history(&list) {
            list.pop();
            drop(list);
            toast(cx, "Could not save download queue; download not started");
            return;
        }
    }
    toast(cx, format!("Downloading “{}”", video.title));
    spawn_job(job, jobs);
    ensure_ticking(cx);
}

fn spawn_job(job: Job, jobs: Arc<Mutex<Vec<Job>>>) {
    let jid = job.id;
    let cancel = job.cancel.clone();
    let jobs2 = jobs.clone();
    std::thread::spawn(move || {
        let update = |f: &dyn Fn(&mut Job)| {
            if let Some(j) = jobs2.lock().iter_mut().find(|j| j.id == jid) {
                f(j);
            }
        };
        // at most 3 downloads at a time; the rest wait their turn
        loop {
            if cancel.load(Ordering::SeqCst) {
                update(&|j| j.status = JobStatus::Cancelled);
                let _ = persist_history(&jobs2.lock());
                return;
            }
            if claim_download_slot(&mut jobs2.lock(), jid) { break; }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        let res = run_job(&job, &cancel, &|p, detail| {
            update(&|j| {
                if p.is_finite() { j.progress = p.clamp(0.0, 1.0); }
                j.detail = detail.chars().filter(|c| !c.is_control()).take(240).collect();
            })
        });
        match res {
            Ok(path) => update(&|j| {
                j.status = JobStatus::Done(path.clone());
                j.progress = 1.0;
            }),
            Err(e) if cancel.load(Ordering::SeqCst) => {
                let _ = e;
                update(&|j| j.status = JobStatus::Cancelled)
            }
            Err(e) => update(&|j| j.status = JobStatus::Failed(e.to_string())),
        }
        if persist_history(&jobs2.lock()) {
            let done=jobs2.lock().iter().any(|j|j.id==jid&&matches!(j.status,JobStatus::Done(_)));
            if done {
                crate::wetube_publish::forget(&job.durable_id);
                let _=store::wetube_kv_set(&format!("wetube_resume_{}",job.durable_id),"");
            }
        } else {eprintln!("WeTube download journal: terminal state write failed");}
    });
}

/// Atomic FIFO admission while holding the job-list lock.
fn claim_download_slot(jobs: &mut [Job], id: u64) -> bool {
    if jobs.iter().filter(|j| j.status == JobStatus::Running).count() >= 3 { return false; }
    let Some(next) = jobs.iter_mut().filter(|j| j.status == JobStatus::Queued && !j.cancel.load(Ordering::SeqCst)).min_by_key(|j| j.id) else { return false };
    if next.id != id { return false; }
    next.status = JobStatus::Running;
    true
}
pub fn retry(id: u64, cx: &mut App) {
    if history_blocked(cx) { toast(cx, "Download journal is unreadable"); return; }
    let jobs = cx.default_global::<Downloads>().jobs.clone();
    let prepared = (|| -> anyhow::Result<Job> {
        let mut list = jobs.lock();
        let i = list.iter().position(|j| j.id == id && matches!(j.status, JobStatus::Failed(_) | JobStatus::Cancelled))
            .ok_or_else(|| anyhow::anyhow!("Job is not retryable"))?;
        let old = list[i].clone();
        anyhow::ensure!(list.iter().filter(|j| matches!(j.status,JobStatus::Queued|JobStatus::Running)).count()<500,"Download queue is full");
        anyhow::ensure!(!list.iter().any(|j| j.id!=id && j.video.id==old.video.id && j.format==old.format && matches!(j.status,JobStatus::Queued|JobStatus::Running)),"This video and format is already active");
        anyhow::ensure!(FORMATS.iter().any(|(k,_)| *k==old.format.as_str()) || old.format=="single","Unsupported download format");
        let mut job=old.clone();
        if job.directory.is_empty() { job.directory=store::settings_snapshot().download_dir.clone(); }
        anyhow::ensure!(!job.directory.is_empty(),"Choose a download folder in Settings");
        job.status=JobStatus::Queued;
        job.progress=0.0; job.detail.clear(); job.cancel=Arc::new(AtomicBool::new(false));
        list[i]=job.clone();
        if !persist_history(&list) { list[i]=old; anyhow::bail!("Could not persist retry"); }
        Ok(job)
    })();
    match prepared {
        Ok(job)=>{ spawn_job(job,jobs); ensure_ticking(cx); cx.refresh_windows(); }
        Err(e)=>toast(cx,e.to_string()),
    }
}
/// Explicit clean retry: use a new durable identity, leaving old partial data
/// untouched for user recovery or manual cleanup. Never delete unknown files.
pub fn restart(id:u64,cx:&mut App) {
    if history_blocked(cx) {toast(cx,"Download journal is unreadable");return;}
    let jobs=cx.default_global::<Downloads>().jobs.clone();
    let ok={
        let mut list=jobs.lock();
        if let Some(i)=list.iter().position(|j|j.id==id && matches!(j.status,JobStatus::Failed(_)|JobStatus::Cancelled)) {
            let old=list[i].durable_id.clone();
            list[i].durable_id=new_durable_id();
            if persist_history(&list) {true} else {list[i].durable_id=old;false}
        }else{false}
    };
    if ok { retry(id,cx); }else{toast(cx,"Could not persist clean retry");}
}
/// Remove only the native partial owned by a terminal job, with explicit consent.
/// yt-dlp staging may contain merge outputs; never recursively delete them.
pub fn discard_partial(id:u64,cx:&mut App) {
    if history_blocked(cx){toast(cx,"Download journal is unreadable");return;}
    let job=cx.default_global::<Downloads>().jobs.lock().iter()
        .find(|j|j.id==id&&matches!(j.status,JobStatus::Failed(_)|JobStatus::Cancelled)).cloned();
    let Some(job)=job else{toast(cx,"Job is not eligible for cleanup");return;};
    let result=(||->anyhow::Result<()> {
        anyhow::ensure!(!job.directory.is_empty()&&valid_durable_id(&job.durable_id),"Missing job identity or original folder");
        let root=std::fs::canonicalize(&job.directory)?;
        let p=root.join(format!(".wetube-{}.part",job.durable_id));
        match std::fs::symlink_metadata(&p) {
            Ok(meta)=>{
                anyhow::ensure!(meta.is_file()&&!meta.file_type().is_symlink(),"Unsafe partial file");
                anyhow::ensure!(std::fs::canonicalize(&p)?.parent()==Some(root.as_path()),"Partial outside original folder");
                anyhow::ensure!(store::wetube_kv_set(&format!("wetube_resume_{}",job.durable_id),""),"Could not clear resume checkpoint");
                std::fs::remove_file(&p)?;
            }
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
            Err(e)=>return Err(e.into()),
        }
        Ok(())
    })();
    match result {Ok(())=>toast(cx,"Native partial cleared. yt-dlp staging files are retained."),Err(e)=>toast(cx,e.to_string())}
}
fn new_durable_id() -> String { format!("{:032x}",rand::random::<u128>()) }
pub(crate) fn valid_durable_id(id:&str)->bool { id.len()==32 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }

fn ensure_ticking(cx: &mut App) {
    if cx.default_global::<Downloads>().ticking {
        return;
    }
    cx.default_global::<Downloads>().ticking = true;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            let active = cx
                .update(|cx| {
                    cx.refresh_windows();
                    let d = cx.default_global::<Downloads>();
                    let active = d.jobs.lock().iter().any(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running));
                    if !active {
                        d.ticking = false;
                    }
                    if !active {
                        save_history(cx);
                    }
                    active
                });
            if !active {
                break;
            }
        }
    })
    .detach();
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(150)
        .collect()
}

fn run_job(job: &Job, cancel: &AtomicBool, progress: &dyn Fn(f32, &str)) -> anyhow::Result<String> {
    let s = store::settings_snapshot();
    anyhow::ensure!(!job.directory.is_empty(), "Select a download folder");
    let dir = std::path::PathBuf::from(&job.directory);
    std::fs::create_dir_all(&dir)?;
    anyhow::ensure!(crate::wetube_session::valid_id(&job.video.id) && valid_durable_id(&job.durable_id),"Invalid job identity");
    if let Some(path)=crate::wetube_publish::recover(job,&dir,cancel)? {return Ok(path);}
    let url = format!("https://www.youtube.com/watch?v={}", job.video.id);
    if let Some(bin) = streams::ytdlp_path() {
        let fmt = match job.format.as_str() {
            "audio" => "bestaudio[ext=m4a]/bestaudio".to_string(),
            // one file with video and audio (no FFmpeg needed)
            "single" => "b".to_string(),
            "best" => "bv*+ba/b".to_string(),
            h => format!("bv*[height<={h}]+ba/b[height<={h}]/b"),
        };
        // Stable per-job staging isolates yt-dlp .part files and merge outputs.
        let stage = job_stage(&dir, &job.durable_id)?;
        let output = ytdlp_run(&bin, &stage, &url, &fmt, &s, &[], None, cancel, progress)?;
        anyhow::ensure!(!cancel.load(Ordering::SeqCst),"cancelled");
        let source = std::path::PathBuf::from(verify_output(&stage,&output)?);
        let name=source.file_name().ok_or_else(||anyhow::anyhow!("Missing output filename"))?;
        let target=dir.join(name);
        return crate::wetube_publish::publish(job,&dir,&source,&target,cancel);
    }
    anyhow::ensure!(job.format=="audio" || job.format=="single","yt-dlp is required for this format");
    native_download(job, &dir, cancel, progress)
}

/// yt-dlp extractor arguments for the iOS client, including the formats yt-dlp
/// would otherwise hold back for lack of a PO token.
pub const YTDLP_IOS: &str = "youtube:player_client=ios;formats=missing_pot";

/// Whether a failed yt-dlp run is worth retrying through the iOS client.
pub fn retry_with_ios(err: &str) -> bool {
    let e = err.to_lowercase();
    if ["cancelled","verification","confirm you","bot","private","sign in","authorization","members-only","age-restricted","copyright","403","429","refused","rate-limit"].iter().any(|term| e.contains(term)) { return false; }
    e.contains("requested format") || e.contains("no video formats")
}

/// `name`: the file name without extension, when the URL isn't a YouTube page
/// (yt-dlp would otherwise name the file after the manifest).
#[allow(clippy::too_many_arguments)]
fn ytdlp_run(bin: &str, dir: &std::path::Path, url: &str, fmt: &str, s: &store::Settings, extra: &[&str], name: Option<&str>, cancel: &AtomicBool, progress: &dyn Fn(f32, &str)) -> anyhow::Result<String> {
    {
        let mut cmd = Command::new(bin);
        cmd.arg("--ignore-config");
        cmd.args(extra);
        if let Some(p) = crate::store::proxy_setting() {
            cmd.arg("--proxy").arg(p);
        }
        if let Some(ff) = ffmpeg_path().filter(|f| f != "ffmpeg") {
            cmd.arg("--ffmpeg-location").arg(ff);
        }
        if s.ipv4_only {
            // YouTube's bot check is triggered far more often on IPv6
            cmd.arg("--force-ipv4");
        }
        // --print makes yt-dlp quiet, so progress is asked for explicitly in a fixed format.
        // Fragments and chunks download in parallel, which is much faster than one stream.
        cmd.args(["--newline", "--no-warnings", "--no-playlist", "--no-overwrites", "--continue", "--progress", "--progress-delta", "0.4", "--socket-timeout", "20", "--retries", "2"])
            .args(["--progress-template", "download:[dl] %(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s"])
            .args(["-N", "8", "--http-chunk-size", "10M", "-f", fmt, "-o"])
            .arg(match name {
                Some(n) => dir.join(format!("{n}.%(ext)s")),
                None => dir.join("%(title).150B [%(id)s].%(ext)s"),
            })
            .args(["--print", "after_move:filepath"])
            .arg(url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if s.sponsorblock && ffmpeg_path().is_some() {
            cmd.args(["--sponsorblock-mark", "all"]);
        }
        hide_window(&mut cmd);
        let mut child = crate::wetube_process::ScopedChild::spawn(&mut cmd)?;
        progress(0.0, "Getting video info…");
        let stdout = child.stdout.take().unwrap();
        // Drain both pipes concurrently; cancellation must not depend on a progress line.
        let stderr = child.stderr.take().unwrap();
        let (err_tx, err_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            use std::io::Read;
            let mut r = BufReader::new(stderr);
            let mut tail = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 { break; }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > 65536 { tail.drain(..tail.len()-65536); }
            }
            let _ = err_tx.send(String::from_utf8_lossy(&tail).into_owned());
        });
        let (line_tx, line_rx) = std::sync::mpsc::sync_channel(32);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line_tx.send(line).is_err() { break; }
            }
        });
        let started=std::time::Instant::now();
        let mut last_output = started;
        let mut final_path = String::new();
        // video and audio arrive one after the other: weight them 90 / 10
        let (mut phase, mut last_pct) = (0u32, 0.0f32);
        loop {
            if cancel.load(Ordering::SeqCst) || last_output.elapsed() > std::time::Duration::from_secs(600) || started.elapsed()>std::time::Duration::from_secs(6*3600) {
                child.terminate();
                if cancel.load(Ordering::SeqCst) { anyhow::bail!("cancelled"); }
                anyhow::bail!("Download tool idle or total timeout");
            }
            let line = match line_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(line) => { last_output = std::time::Instant::now(); line }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if let Some(rest) = line.strip_prefix("[dl]") {
                let mut parts = rest.trim().split('|');
                let pct = parts.next().and_then(|p| p.trim().trim_end_matches('%').parse::<f32>().ok()).unwrap_or(0.0);
                let speed = parts.next().unwrap_or("").trim();
                let eta = parts.next().unwrap_or("").trim();
                if pct + 20.0 < last_pct {
                    phase += 1;
                }
                last_pct = pct;
                let overall = if phase == 0 { pct / 100.0 * 0.9 } else { 0.9 + pct / 100.0 * 0.1 };
                let what = if phase == 0 { "" } else { "Audio · " };
                let detail = if pct >= 100.0 { "Finishing…".to_string() } else { format!("{what}{pct:.0}% · {speed} · {eta} left") };
                progress(overall.min(0.99), &detail);
            } else if !line.starts_with('[') && !line.trim().is_empty() {
                final_path = line.trim().to_string();
            }
        }
        let status = loop {
            if cancel.load(Ordering::SeqCst) || last_output.elapsed() > std::time::Duration::from_secs(600) || started.elapsed()>std::time::Duration::from_secs(6*3600) {
                child.terminate();
                if cancel.load(Ordering::SeqCst) { anyhow::bail!("cancelled"); }
                anyhow::bail!("Download tool exit timeout");
            }
            if let Some(status) = child.try_wait()? { break status; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let err = err_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default();
        if !status.success() {
            anyhow::bail!("{}", ytdlp_failure(status.code(), &err));
        }
        anyhow::ensure!(!final_path.is_empty(), "Download tool did not report a completed file");
        verify_output(dir, &final_path)
    }
}

/// Stable, privacy-safe error categories; never persist raw stderr (it can contain URLs or tokens).
pub(crate) fn ytdlp_failure(code: Option<i32>, stderr: &str) -> String {
    let e = stderr.to_ascii_lowercase();
    let cause = if e.contains("confirm you") && e.contains("bot") {
        "Provider requested verification. Open the official YouTube website."
    } else if e.contains("private video") || e.contains("sign in") || e.contains("age-restricted") || e.contains("copyright") || e.contains("members-only") {
        "Media unavailable or requires authorization. Check access on the official site."
    } else if e.contains("ffmpeg") || e.contains("ffprobe") {
        "FFmpeg/ffprobe missing or failed. Check media tools in Settings."
    } else if e.contains("requested format") || e.contains("no video formats") {
        "Requested format unavailable. Choose an available format."
    } else if e.contains("403") || e.contains("429") {
        "Provider refused or rate-limited the request. Do not retry repeatedly."
    } else if e.contains("timed out") || e.contains("connection") || e.contains("certificate") || e.contains("network") {
        "Network or TLS failure. Check connection and proxy settings."
    } else if e.contains("permission denied") || e.contains("access is denied") || e.contains("no space") {
        "Cannot write output. Check folder permission and free space."
    } else if stderr.trim().is_empty() {
        "Tool returned no error details. Check the yt-dlp executable and version in Settings."
    } else {
        "Tool reported an unclassified error. Check the configured yt-dlp version and media availability."
    };
    format!("yt-dlp exit {}: {cause}", code.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()))
}
#[cfg(test)]
mod wetube_diagnostics_tests {
    #[::core::prelude::v1::test]
    fn failure_redacts_sensitive_text() {
        let m = super::ytdlp_failure(Some(1), "ERROR: https://example.org/?token=secret 403");
        assert!(m.contains("exit 1")); assert!(m.contains("refused")); assert!(!m.contains("secret"));
        assert!(super::ytdlp_failure(Some(2), "").contains("no error details"));
        assert!(super::ytdlp_failure(None, "ffmpeg not found").contains("FFmpeg"));
        assert!(super::ytdlp_failure(Some(1), "Sign in to confirm you're not a bot").contains("official YouTube"));
    }
}

/// Without yt-dlp: progressive (muxed) stream or audio-only stream.
fn native_download(job: &Job, dir: &std::path::Path, cancel: &AtomicBool, progress: &dyn Fn(f32, &str)) -> anyhow::Result<String> {
    let info = streams::get_streams(&job.video.id)?;
    let (fmt, ext) = if job.format == "audio" {
        let a = info.audio.iter().find(|a| a.mime.contains("mp4")).or(info.audio.first()).ok_or_else(|| anyhow::anyhow!("no audio stream"))?;
        (a.clone(), if a.mime.contains("mp4") { "m4a" } else { "webm" })
    } else {
        let m = info.muxed.iter().max_by_key(|m| m.height).ok_or_else(|| anyhow::anyhow!("no combined stream (install yt-dlp for high quality downloads)"))?;
        (m.clone(), "mp4")
    };
    let path = dir.join(format!("{} [{}].{ext}", sanitize(&info.title), job.video.id));
    anyhow::ensure!(!path.exists(), "Destination already exists");
    anyhow::ensure!(valid_durable_id(&job.durable_id),"Invalid durable download identity");
    let partial=dir.join(format!(".wetube-{}.part",job.durable_id));
    let key=format!("wetube_resume_{}",job.durable_id);
    crate::api::http::download_resumable(&fmt.url,&partial,&key,fmt.itag,|done,total| {
        let p=total.filter(|t|*t>0).map(|t|done as f32/t as f32).unwrap_or(0.0);
        progress(p,&format!("{:.1} MB",done as f64/1e6));
        !cancel.load(Ordering::SeqCst)
    })?;
    anyhow::ensure!(!cancel.load(Ordering::SeqCst),"cancelled");
    crate::wetube_publish::publish(job,dir,&partial,&path,cancel)
}

pub fn cancel(id: u64, cx: &mut App) {
    if let Some(j) = cx.default_global::<Downloads>().jobs.lock().iter().find(|j| j.id == id) {
        j.cancel.store(true, Ordering::SeqCst);
    }
}

pub fn open_external(id: &str, cx: &mut App) {
    open_external_at(id, 0.0, cx)
}

pub fn open_external_at(id: &str, time: f64, cx: &mut App) {
    let custom = store::settings_snapshot().external_player.trim().to_string();
    let id = id.to_string();
    toast(cx, "Opening in your video player…");
    cx.spawn(async move |cx| {
        let res = crate::ui::blocking(move || launch_external(&id, time, &custom)).await;
        let _ = cx.update(|cx| match res {
            Ok(name) => toast(cx, format!("Playing in {name}. Keep WeTube Desktop by WFS Project open while it plays")),
            Err(e) => toast(cx, format!("Couldn't open a video player: {e}")),
        });
    })
    .detach();
}

/// The external player "Automatic" would use: VLC or mpv when installed.
pub fn detect_player() -> Option<(&'static str, String)> {
    let on_path = |exe: &str| -> Option<String> {
        let name = if cfg!(windows) { format!("{exe}.exe") } else { exe.to_string() };
        std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&name)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
    };
    let mut found: Vec<(&'static str, String)> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(root) = std::env::var_os(var) {
                let p = std::path::Path::new(&root).join("VideoLAN").join("VLC").join("vlc.exe");
                if p.is_file() {
                    found.push(("VLC", p.to_string_lossy().into_owned()));
                }
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let p = std::path::Path::new(&local).join("Programs").join("mpv").join("mpv.exe");
            if p.is_file() {
                found.push(("mpv", p.to_string_lossy().into_owned()));
            }
        }
    }
    found.extend(on_path("vlc").map(|p| ("VLC", p)));
    found.extend(on_path("mpv").map(|p| ("mpv", p)));
    found.extend(on_path("celluloid").map(|p| ("Celluloid", p)));
    found.into_iter().next()
}

/// Resolves the streams and starts a player on them. The streams are served by the
/// app's local proxy (YouTube refuses the open-ended requests media players make),
/// so playback keeps working only while Stream Tube is open.
fn launch_external(id: &str, time: f64, custom: &str) -> anyhow::Result<String> {
    let info = streams::get_streams(id)?;
    let s = store::settings_snapshot();
    // H.264 + AAC plays in every player
    let (v, a) = streams::pick_formats(&info, s.default_quality, "h264", None);
    let local = |f: &crate::api::types::StreamFormat| crate::player::proxy::local_url(&f.url, f.content_length, &f.mime);
    let live = info.is_live || (info.video.is_empty() && info.muxed.is_empty());
    let hls = info.hls_url.as_deref().map(|u| crate::player::proxy::local_hls(u, s.default_quality, None));
    let (video, audio) = if live || info.hls_only {
        (hls.clone().ok_or_else(|| anyhow::anyhow!("no stream for this video"))?, None)
    } else {
        match (v, a) {
            (Some(v), a) => (local(v), a.map(local)),
            (None, Some(a)) => (local(a), None),
            _ => (info.muxed.last().map(local).ok_or_else(|| anyhow::anyhow!("no stream for this video"))?, None),
        }
    };
    // at the very end there's nothing left to play: start from the top
    let start = (time > 1.0 && (info.length_secs == 0 || time + 5.0 < info.length_secs as f64)).then_some(time as u64);
    let title = info.title.clone();

    let (name, exe) = if custom.is_empty() {
        match detect_player() {
            Some((n, p)) => (n.to_string(), p),
            None => return open_with_default(&info, start),
        }
    } else {
        let n = std::path::Path::new(custom).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| custom.to_string());
        (n, custom.to_string())
    };
    let lower = exe.to_lowercase();
    let mut cmd = Command::new(&exe);
    if lower.contains("vlc") {
        if let Some(a) = &audio {
            cmd.arg(format!("--input-slave={a}"));
        }
        if let Some(t) = start {
            cmd.arg(format!("--start-time={t}"));
        }
        cmd.arg(format!("--meta-title={title}")).arg(&video);
    } else if lower.contains("celluloid") {
        if let Some(a) = &audio {
            cmd.arg(format!("--mpv-audio-file={a}"));
        }
        if let Some(t) = start {
            cmd.arg(format!("--mpv-start={t}"));
        }
        cmd.arg(&video);
    } else if lower.contains("mpv") {
        if let Some(a) = &audio {
            cmd.arg(format!("--audio-file={a}"));
        }
        if let Some(t) = start {
            cmd.arg(format!("--start={t}"));
        }
        cmd.arg(format!("--force-media-title={title}")).arg(&video);
    } else {
        // any other player: one stream with sound
        let one = match (&audio, info.muxed.last()) {
            (Some(_), Some(m)) => local(m),
            _ => video.clone(),
        };
        cmd.arg(one);
    }
    cmd.spawn().map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
    Ok(name)
}

/// No known player installed: a playlist with one stream (video and sound
/// together), opened by whatever the system uses for playlists.
fn open_with_default(info: &crate::api::types::StreamInfo, start: Option<u64>) -> anyhow::Result<String> {
    let s = store::settings_snapshot();
    let url = if info.is_live || info.hls_only {
        info.hls_url.as_deref().map(|u| crate::player::proxy::local_hls(u, s.default_quality, None))
    } else {
        info.muxed.last().map(|m| crate::player::proxy::local_url(&m.url, m.content_length, &m.mime))
    }
    .ok_or_else(|| anyhow::anyhow!("this video has no stream a default player can open. Install VLC or mpv"))?;
    let mut m3u = format!("#EXTM3U\n#EXTINF:{},{}\n", info.length_secs, info.title.replace(['\n', '\r'], " "));
    if let Some(t) = start {
        m3u.push_str(&format!("#EXTVLCOPT:start-time={t}\n"));
    }
    m3u.push_str(&url);
    m3u.push('\n');
    let path = std::env::temp_dir().join("streamtube-play.m3u");
    std::fs::write(&path, m3u)?;
    open::that(&path)?;
    Ok("your default player".into())
}

/// Removes a finished/failed job from the list (the file stays).
pub fn remove(id: u64, cx: &mut App) {
    cx.default_global::<Downloads>().jobs.lock().retain(|j| j.id != id || matches!(j.status, JobStatus::Running | JobStatus::Queued));
    save_history(cx);
    cx.refresh_windows();
}

/// Clears every finished/failed/cancelled entry.
pub fn clear_history(cx: &mut App) {
    cx.default_global::<Downloads>().jobs.lock().retain(|j| matches!(j.status, JobStatus::Running | JobStatus::Queued));
    save_history(cx);
    cx.refresh_windows();
}

/// Deletes the downloaded file and removes the entry.
pub fn delete_file(id: u64, cx: &mut App) {
    let entry = cx.default_global::<Downloads>().jobs.lock().iter().find(|j| j.id == id).and_then(|j| match &j.status {
        JobStatus::Done(p) => Some((j.directory.clone(),p.clone())),
        _ => None,
    });
    if let Some((dir,p)) = entry {
        if dir.is_empty() { toast(cx,"Legacy download has no pinned folder; remove the list entry or delete the file yourself."); return; }
        if std::path::Path::new(&p).exists() && verify_output(std::path::Path::new(&dir),&p).is_err() {
            toast(cx,"File no longer belongs to the original download folder"); return;
        }
        match std::fs::remove_file(&p) {
            Ok(()) => toast(cx, "Deleted from disk"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                toast(cx, format!("Could not delete: {e}"));
                return;
            }
        }
    }
    remove(id, cx);
}

/// The real file for a finished download. yt-dlp reports the merged name even
/// when the merge didn't happen, and older entries may only hold the folder, so
/// fall back to the largest media file in the folder tagged with the video id.
pub fn resolve_output(path: &str, video_id: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    if p.is_file() {
        return Some(path.to_string());
    }
    let dir = if p.is_dir() { p.to_path_buf() } else { p.parent()?.to_path_buf() };
    let tag = format!("[{video_id}]");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.contains(&tag) && !n.ends_with(".part") && !n.ends_with(".ytdl") && !n.ends_with(".tmp")
        })
        .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p.to_string_lossy().into_owned())
}

/// Windows MoveFileExW with flags=0 refuses replacement, including on FAT/exFAT.
/// Non-Windows fallback uses a same-directory hard link.
#[cfg(windows)]
pub(crate) fn commit_download(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" { fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32; }
    let src = std::fs::canonicalize(from)?;
    let parent = std::fs::canonicalize(to.parent().ok_or_else(|| std::io::Error::other("Missing destination parent"))?)?;
    let dst = parent.join(to.file_name().ok_or_else(|| std::io::Error::other("Missing destination filename"))?);
    let src: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let dst: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both buffers are NUL-terminated and live for the duration of the call.
    if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), 0) } == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}
#[cfg(not(windows))]
pub(crate) fn commit_download(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::hard_link(from,to)
}

/// Staging must remain under the pinned destination; reject symbolic links and junction escapes.
fn job_stage(dir:&std::path::Path,id:&str)->anyhow::Result<std::path::PathBuf> {
    anyhow::ensure!(valid_durable_id(id),"Invalid job identity");
    let path=dir.join(format!(".wetube-job-{id}"));
    match std::fs::create_dir(&path) {
        Ok(())=>{},
        Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{},
        Err(e)=>return Err(e.into()),
    }
    let meta=std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(meta.is_dir() && !meta.file_type().is_symlink(),"Unsafe staging directory");
    let root=std::fs::canonicalize(dir)?;
    let resolved=std::fs::canonicalize(&path)?;
    anyhow::ensure!(resolved.parent()==Some(root.as_path()),"Staging directory outside pinned destination");
    Ok(path)
}

/// Require a nonempty regular file contained by the selected directory.
fn verify_output(dir: &std::path::Path, path: &str) -> anyhow::Result<String> {
    let root = std::fs::canonicalize(dir)?;
    let link=std::fs::symlink_metadata(path)?;
    anyhow::ensure!(link.is_file() && !link.file_type().is_symlink(),"Output is not a regular file");
    let full = std::fs::canonicalize(path)?;
    anyhow::ensure!(full.starts_with(&root) && full != root, "Output is outside the download directory");
    let meta = std::fs::metadata(&full)?;
    anyhow::ensure!(meta.is_file() && meta.len() > 0, "Output is not a nonempty file");
    Ok(path.to_string())
}
#[cfg(test)]
mod wetube_download_regression {
    use super::*;
    fn job(id: u64) -> Job {
        Job { id, durable_id: new_durable_id(), video: VideoItem::default(), format: "audio".into(), directory: String::new(), status: JobStatus::Queued, progress: 0.0, detail: String::new(), cancel: Arc::new(AtomicBool::new(false)) }
    }
    #[::core::prelude::v1::test] fn fifo_and_capacity() {
        let mut jobs=vec![job(1),job(2),job(3),job(4)];
        assert!(!claim_download_slot(&mut jobs,2));
        for i in 1..=3 { assert!(claim_download_slot(&mut jobs,i)); }
        assert!(!claim_download_slot(&mut jobs,4));
        jobs[0].status=JobStatus::Done("file".into());
        assert!(claim_download_slot(&mut jobs,4));
    }
    #[::core::prelude::v1::test] fn cancelled_waiter_does_not_block() {
        let mut jobs=vec![job(1),job(2)];
        jobs[0].cancel.store(true,Ordering::SeqCst);
        assert!(claim_download_slot(&mut jobs,2));
    }
}

/// Opens a downloaded file with the system's default player.
pub fn open_file(path: &str, cx: &mut App) {
    let Some(path) = std::path::Path::new(path).is_file().then(|| path.to_string()).or_else(|| {
        // find the id from the "[id]" in the file name
        let name = std::path::Path::new(path).file_name()?.to_string_lossy().into_owned();
        let id = name.rsplit_once('[')?.1.split_once(']')?.0.to_string();
        resolve_output(path, &id)
    }) else {
        toast(cx, "File not found (moved or deleted)");
        return;
    };
    // the default app is launched off the UI thread; Explorer is the most
    // reliable way to hand a file to its default app on Windows
    std::thread::spawn(move || {
        #[cfg(windows)]
        {
            let mut c = Command::new("explorer");
            c.arg(&path);
            if c.spawn().is_ok() {
                return;
            }
        }
        let _ = open::that(&path);
    });
}

/// (active jobs, overall progress 0..1 of active jobs)
pub fn summary(cx: &App) -> (usize, f32) {
    let Some(d) = cx.try_global::<Downloads>() else { return (0, 0.0) };
    let jobs = d.jobs.lock();
    let active: Vec<&Job> = jobs.iter().filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running)).collect();
    if active.is_empty() {
        return (0, 0.0);
    }
    let p = active.iter().map(|j| j.progress).sum::<f32>() / active.len() as f32;
    (active.len(), p)
}

// ── history: finished downloads survive restarts ──

#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    #[serde(default)]
    durable_id: String,
    video: VideoItem,
    format: String,
    path: Option<String>,
    error: Option<String>,
    #[serde(default)]
    directory: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    cancelled: bool,
}

const HISTORY_KEY: &str = "download_history";
const HISTORY_LIMIT: usize = 16 * 1024 * 1024;
const INTERRUPTED: &str = "Interrupted by previous shutdown. Retry explicitly; existing files will not be overwritten.";

fn bounded_metadata(v: &VideoItem) -> VideoItem {
    let short = |s: &str| s.chars().filter(|c| !c.is_control()).take(180).collect::<String>();
    VideoItem { id: v.id.clone(), title: short(&v.title), channel_name: short(&v.channel_name),
        thumbnail: crate::api::innertube::default_thumb(&v.id), duration_secs: v.duration_secs, ..Default::default() }
}
fn saved_job(j: &Job) -> Saved {
    let (path,error,active,cancelled) = match &j.status {
        JobStatus::Done(p) => (Some(p.clone()),None,false,false),
        JobStatus::Failed(e) => (None,Some(if e.starts_with("yt-dlp exit ") || e == INTERRUPTED { e.clone() } else { "Download failed. Retry or check media tools and output folder.".into() }),false,false),
        JobStatus::Queued | JobStatus::Running => (None,None,true,false),
        JobStatus::Cancelled => (None,None,false,true),
    };
    Saved { durable_id: j.durable_id.clone(), video: bounded_metadata(&j.video), format: j.format.clone(), path, error, directory: j.directory.clone(), active, cancelled }
}
fn restore_status(s: &Saved) -> JobStatus {
    if s.active { JobStatus::Failed(INTERRUPTED.into()) }
    else if s.cancelled { JobStatus::Cancelled }
    else if let Some(p) = &s.path { JobStatus::Done(p.clone()) }
    else { JobStatus::Failed(s.error.clone().unwrap_or_else(|| "Download failed".into())) }
}
fn persist_history(jobs: &[Job]) -> bool {
    let rows: anyhow::Result<Vec<_>>=jobs.iter().map(|j|Ok((j.durable_id.clone(),serde_json::to_string(&saved_job(j))?))).collect();
    rows.and_then(|r|store::write_download_rows(&r)).is_ok()
}
fn save_history(cx: &mut App) {
    let ok = {
        let d = cx.default_global::<Downloads>();
        if d.history_blocked { return; }
        let result = persist_history(&d.jobs.lock());
        result
    };
    if !ok { toast(cx, "Could not save download history"); }
}

/// Restore interrupted work as explicit retry candidates, never auto-start it.
pub fn load_history(cx: &mut App) {
    let d = cx.default_global::<Downloads>();
    if d.history_loaded { return; }
    d.history_loaded = true;
    let loaded=(||->anyhow::Result<Vec<Saved>>{
        if let Some(rows)=store::download_rows()? {
            return rows.into_iter().map(|(id,raw)| {
                let s:Saved=serde_json::from_str(&raw)?;
                anyhow::ensure!(s.durable_id==id,"Download journal identity mismatch");
                Ok(s)
            }).collect();
        }
        let Some(raw)=store::wetube_kv_read_bounded(HISTORY_KEY,HISTORY_LIMIT)? else{return Ok(Vec::new());};
        Ok(serde_json::from_str(&raw)?)
    })();
    let Ok(saved)=loaded else{d.history_blocked=true;return;};
    let mut identities=std::collections::HashSet::new();
    if saved.iter().any(|s| !crate::wetube_session::valid_id(&s.video.id) || (!s.durable_id.is_empty() && (!valid_durable_id(&s.durable_id) || !identities.insert(s.durable_id.clone())))) {
        d.history_blocked=true; return;
    }
    let mut jobs = d.jobs.lock();
    for s in saved {
        let status = restore_status(&s);
        d.next += 1;
        let progress = if matches!(status, JobStatus::Done(_)) { 1.0 } else { 0.0 };
        jobs.push(Job { id: d.next, durable_id: if s.durable_id.is_empty() { new_durable_id() } else { s.durable_id }, video: s.video, format: s.format, directory: s.directory,
            status, progress, detail: String::new(), cancel: Arc::new(AtomicBool::new(false)) });
    }
    if !persist_history(&jobs) { d.history_blocked=true; }
}

pub fn history_blocked(cx: &App) -> bool {
    cx.try_global::<Downloads>().is_some_and(|d| d.history_blocked)
}
pub fn interrupted_count(cx: &App) -> usize {
    cx.try_global::<Downloads>().map(|d| d.jobs.lock().iter().filter(|j| matches!(&j.status, JobStatus::Failed(e) if e == INTERRUPTED)).count()).unwrap_or(0)
}
#[cfg(test)]
mod journal_tests {
    use super::*;
    fn example() -> Job {
        Job { id: 1, durable_id: new_durable_id(), video: VideoItem { id:"abcdefghijk".into(), title:"video".into(), ..Default::default() }, format:"audio".into(), directory:"downloads".into(), status:JobStatus::Running, progress:0.5, detail:"".into(),cancel:Arc::new(AtomicBool::new(false)) }
    }
    #[::core::prelude::v1::test] fn active_recovers_without_auto_start() {
        let s=saved_job(&example());
        assert!(s.active);
        assert_eq!(restore_status(&s), JobStatus::Failed(INTERRUPTED.into()));
        assert_eq!(s.directory,"downloads");
        assert!(valid_durable_id(&s.durable_id));
        assert!(!valid_durable_id("../unsafe"));
    }
    #[::core::prelude::v1::test] fn terminal_state_roundtrip() {
        let mut j=example(); j.status=JobStatus::Cancelled;
        let s:Saved=serde_json::from_str(&serde_json::to_string(&saved_job(&j)).unwrap()).unwrap();
        assert_eq!(restore_status(&s),JobStatus::Cancelled);
        j.status=JobStatus::Done("file.mp4".into());
        assert_eq!(restore_status(&saved_job(&j)),JobStatus::Done("file.mp4".into()));
    }
    #[::core::prelude::v1::test] fn legacy_history_and_sensitive_error() {
        let j=example();
        let old=serde_json::json!({"video":j.video,"format":"audio","path":"old.mp4","error":null});
        let s:Saved=serde_json::from_value(old).unwrap();
        assert_eq!(restore_status(&s),JobStatus::Done("old.mp4".into()));
        let mut j=example(); j.status=JobStatus::Failed("https://example.org/?secret=abc".into());
        assert!(!serde_json::to_string(&saved_job(&j)).unwrap().contains("secret=abc"));
    }
}

// ── FFmpeg (joins separate video and audio streams) ──

static FFMPEG_FETCHING: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Vec<(VideoItem, Option<String>)>> = Mutex::new(Vec::new());

/// yt-dlp's FFmpeg builds: (repository, release, file). The Linux one is static,
/// so it runs on any distribution.
const FFMPEG_ASSET: (&str, &str, &str) = (
    "yt-dlp/FFmpeg-Builds",
    "tags/latest",
    if cfg!(windows) { "ffmpeg-master-latest-win64-gpl-shared.zip" } else { "ffmpeg-master-latest-linux64-gpl.tar.xz" },
);

/// Download sizes shown on the buttons that fetch the tools.
pub const FFMPEG_SIZE: &str = if cfg!(windows) { "91 MB" } else { "158 MB" };
pub const YTDLP_SIZE: &str = if cfg!(windows) { "24 MB" } else { "41 MB" };

/// Downloads a file from a GitHub release into `dest`, and keeps it only if it
/// matches the SHA-256 GitHub lists for it. `release` is "latest" or "tags/<tag>".
/// How far the FFmpeg / yt-dlp download has got: idle, a percentage, or unpacking.
static FFMPEG_PCT: AtomicU32 = AtomicU32::new(IDLE);
static YTDLP_PCT: AtomicU32 = AtomicU32::new(IDLE);
const IDLE: u32 = u32::MAX;
const UNPACKING: u32 = 1000;
/// bumped each time a tool download finishes, so pages showing whether it's
/// installed look again without being reopened
static TOOLS_GEN: AtomicU64 = AtomicU64::new(0);

pub enum ToolFetch {
    Downloading(u32),
    Unpacking,
}

fn fetch_state(a: &AtomicU32) -> Option<ToolFetch> {
    match a.load(Ordering::SeqCst) {
        IDLE => None,
        UNPACKING => Some(ToolFetch::Unpacking),
        n => Some(ToolFetch::Downloading(n.min(100))),
    }
}

pub fn ffmpeg_fetch() -> Option<ToolFetch> {
    fetch_state(&FFMPEG_PCT)
}

pub fn ytdlp_fetch() -> Option<ToolFetch> {
    fetch_state(&YTDLP_PCT)
}

pub fn tools_generation() -> u64 {
    TOOLS_GEN.load(Ordering::SeqCst)
}

/// Repaints while a tool downloads, so its progress moves on screen.
fn repaint_while(flag: &'static AtomicBool, cx: &mut App) {
    cx.spawn(async move |cx| {
        while flag.load(Ordering::SeqCst) {
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            let _ = cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

fn download_github_asset((repo, release, name): (&str, &str, &str), dest: &std::path::Path) -> anyhow::Result<()> {
    download_github_asset_progress((repo, release, name), dest, None)
}

fn download_github_asset_progress(
    (repo, release, name): (&str, &str, &str),
    dest: &std::path::Path,
    pct: Option<&AtomicU32>,
) -> anyhow::Result<()> {
    let rel = crate::api::http::get_json(&format!("https://api.github.com/repos/{repo}/releases/{release}"))?;
    let asset = rel
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|x| x.get("name").and_then(|n| n.as_str()) == Some(name)))
        .ok_or_else(|| anyhow::anyhow!("{name} is not in the latest release"))?;
    let url = asset.get("browser_download_url").and_then(|u| u.as_str()).filter(|u| u.starts_with(&format!("https://github.com/{repo}/releases/download/")));
    let sha = asset.get("digest").and_then(|d| d.as_str()).and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase);
    let (Some(url), Some(sha)) = (url, sha) else { anyhow::bail!("GitHub gave no checksum for {name}") };
    {
        let mut f = std::fs::File::create(dest)?;
        let size = asset.get("size").and_then(|s| s.as_u64());
        crate::api::http::download_to(url, &mut f, |done, total| {
            if let (Some(p), Some(t)) = (pct, total.or(size).filter(|t| *t > 0)) {
                p.store(((done * 100) / t).min(100) as u32, Ordering::SeqCst);
            }
            true
        })?;
    }
    if crate::updater::file_sha256(dest)? != sha {
        let _ = std::fs::remove_file(dest);
        anyhow::bail!("the downloaded {name} doesn't match its checksum");
    }
    Ok(())
}

fn ffmpeg_dir() -> std::path::PathBuf {
    store::data_dir().join("ffmpeg")
}

/// FFmpeg next to the app, in the app's data folder, or on PATH.
pub fn ffmpeg_path() -> Option<String> {
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let mut cands = vec![ffmpeg_dir().join(exe)];
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        cands.push(d.join(exe));
        cands.push(d.join("ffmpeg").join(exe));
    }
    if let Some(p) = cands.into_iter().find(|p| p.is_file()) {
        return Some(p.to_string_lossy().into_owned());
    }
    static ON_PATH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on_path = *ON_PATH.get_or_init(|| {
        let mut c = Command::new("ffmpeg");
        c.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
        hide_window(&mut c);
        c.status().map(|s| s.success()).unwrap_or(false)
    });
    on_path.then(|| "ffmpeg".to_string())
}

/// Downloads FFmpeg into the data folder, then starts the downloads waiting for it.
pub fn fetch_ffmpeg(cx: &mut App) {
    if FFMPEG_FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    FFMPEG_PCT.store(0, Ordering::SeqCst);
    toast(cx, "Downloading FFmpeg…");
    repaint_while(&FFMPEG_FETCHING, cx);
    cx.spawn(async move |cx| {
        let res = crate::ui::blocking(|| -> anyhow::Result<()> {
            let dir = ffmpeg_dir();
            let tmp = store::data_dir().join("ffmpeg-download");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp)?;
            let archive = tmp.join(FFMPEG_ASSET.2);
            download_github_asset_progress(FFMPEG_ASSET, &archive, Some(&FFMPEG_PCT))?;
            FFMPEG_PCT.store(UNPACKING, Ordering::SeqCst);
            // Windows 10+ ships bsdtar, which reads zip files; GNU tar unpacks .tar.xz
            let mut c = Command::new("tar");
            c.arg(if cfg!(windows) { "-xf" } else { "-xJf" }).arg(&archive).arg("-C").arg(&tmp);
            hide_window(&mut c);
            if !c.status()?.success() {
                anyhow::bail!("could not unpack FFmpeg");
            }
            let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
            let bin = std::fs::read_dir(&tmp)?
                .filter_map(Result::ok)
                .map(|e| e.path().join("bin"))
                .find(|p| p.join(exe).is_file())
                .ok_or_else(|| anyhow::anyhow!("{exe} not found in the archive"))?;
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            for e in std::fs::read_dir(&bin)?.filter_map(Result::ok) {
                // ffmpeg and ffprobe (and on Windows their DLLs); ffplay isn't needed
                if e.file_name().to_string_lossy().starts_with("ffplay") {
                    continue;
                }
                let to = dir.join(e.file_name());
                std::fs::copy(e.path(), &to)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            let _ = std::fs::remove_dir_all(&tmp);
            Ok(())
        })
        .await;
        FFMPEG_FETCHING.store(false, Ordering::SeqCst);
        FFMPEG_PCT.store(IDLE, Ordering::SeqCst);
        TOOLS_GEN.fetch_add(1, Ordering::SeqCst);
        let _ = cx.update(|cx| {
            cx.refresh_windows();
            let pending: Vec<_> = std::mem::take(&mut *PENDING.lock());
            match res {
                Ok(()) => {
                    toast(cx, "FFmpeg is ready");
                    for (v, f) in pending {
                        start(v, f, cx);
                    }
                }
                Err(e) => toast(cx, format!("FFmpeg download failed: {e}")),
            }
        });
    })
    .detach();
}

// ── yt-dlp (the standalone build, kept in the data folder) ──

static YTDLP_FETCHING: AtomicBool = AtomicBool::new(false);

const YTDLP_ASSET: (&str, &str, &str) = ("yt-dlp/yt-dlp", "latest", if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp_linux" });

/// Downloads yt-dlp into the data folder, then starts the downloads waiting for it.
pub fn fetch_ytdlp(cx: &mut App) {
    if YTDLP_FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    YTDLP_PCT.store(0, Ordering::SeqCst);
    toast(cx, "Downloading yt-dlp…");
    repaint_while(&YTDLP_FETCHING, cx);
    cx.spawn(async move |cx| {
        let res = crate::ui::blocking(|| -> anyhow::Result<()> {
            let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
            let dest = store::data_dir().join(name);
            let tmp = store::data_dir().join(format!("{name}.part"));
            download_github_asset_progress(YTDLP_ASSET, &tmp, Some(&YTDLP_PCT))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            }
            let _ = std::fs::remove_file(&dest);
            std::fs::rename(&tmp, &dest)?;
            streams::reset_ytdlp_cache();
            if streams::ytdlp_path().is_none() {
                anyhow::bail!("the downloaded yt-dlp does not run");
            }
            Ok(())
        })
        .await;
        YTDLP_FETCHING.store(false, Ordering::SeqCst);
        YTDLP_PCT.store(IDLE, Ordering::SeqCst);
        TOOLS_GEN.fetch_add(1, Ordering::SeqCst);
        let _ = cx.update(|cx| {
            cx.refresh_windows();
            let pending: Vec<_> = std::mem::take(&mut *PENDING.lock());
            match res {
                Ok(()) => {
                    toast(cx, "yt-dlp is ready");
                    for (v, f) in pending {
                        start(v, f, cx);
                    }
                }
                Err(e) => toast(cx, format!("yt-dlp download failed: {e}")),
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod fetch_tests {
    /// network: cargo test --release verified_download -- --ignored
    #[::core::prelude::v1::test]
    #[ignore]
    fn verified_download() {
        let p = std::env::temp_dir().join("st-ytdlp-test");
        super::download_github_asset(super::YTDLP_ASSET, &p).unwrap();
        assert!(std::fs::metadata(&p).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod kids_download_tests {
    use std::sync::atomic::AtomicBool;
    /// A video made for kids downloads from the iOS HLS manifest when yt-dlp gets no formats.
    /// network + yt-dlp: cargo test kids_download -- --ignored
    #[::core::prelude::v1::test]
    #[ignore]
    fn kids_download() {
        let bin = crate::api::streams::ytdlp_path().expect("yt-dlp");
        let dir = std::env::temp_dir().join("st-kids-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = crate::store::settings_snapshot();
        let cancel = AtomicBool::new(false);
        let url = "https://www.youtube.com/watch?v=e_04ZrNroTo";
        let fmt = "bv*[height<=360]+ba/b[height<=360]/b";
        // yt-dlp alone fails, with an error that leads to the retries
        if let Err(e) = super::ytdlp_run(&bin, &dir, url, fmt, &s, &["--extractor-args", super::YTDLP_IOS], None, &cancel, &|_, _| {}) {
            println!("yt-dlp iOS: {e}");
        }
        let manifest = crate::api::streams::ios_hls_manifest("e_04ZrNroTo").unwrap();
        let name = format!("{} [e_04ZrNroTo]", super::sanitize("Kids: test / video?").replace('%', "%%"));
        let path = super::ytdlp_run(&bin, &dir, &manifest, fmt, &s, &[], Some(&name), &cancel, &|p, d| println!("{p:.2} {d}")).unwrap();
        println!("{path}");
        assert!(path.contains("e_04ZrNroTo"));
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
