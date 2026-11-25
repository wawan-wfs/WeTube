//! Player controller: owns the mpv instance, stream selection, SponsorBlock, queue and progress.
use super::mpv::{Mpv, MpvEvent, PropValue};
use crate::api::services::{self, Segment};
use crate::api::streams;
use crate::api::types::*;
use crate::store;
use crate::ui::{blocking, toast};
use gpui_kit::*;
use std::collections::HashSet;
use std::sync::Arc;

pub enum PlayerEvent {
    Ended,
    Loaded,
    /// average colour of the picture changed (ambient mode)
    Ambient,
}

pub struct Player {
    /// speed to go back to while a press-and-hold plays at 2x
    pub boost: Option<f64>,
    mpv: Option<Arc<Mpv>>,
    pub init_error: Option<String>,
    pub video: Option<VideoItem>,
    pub streams: Option<Arc<StreamInfo>>,
    pub loading: bool,
    pub error: Option<String>,
    pub quality: u32,
    pub audio_only: bool,
    pub time: f64,
    pub duration: f64,
    pub paused: bool,
    pub buffering: bool,
    pub seeking: bool,
    pub cache_time: f64,
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub eof: bool,
    pub frame: Option<Arc<RenderImage>>,
    old_frames: Vec<Arc<RenderImage>>,
    pub video_size: (f64, f64),
    pub segments: Vec<Segment>,
    skipped: HashSet<String>,
    pub sb_prompt: Option<Segment>,
    muted_by_sb: bool,
    pub caption: Option<usize>,
    pub queue: Vec<VideoItem>,
    pub queue_pos: Option<usize>,
    pub chapters: Vec<Chapter>,
    pub heatmap: Vec<(f64, f64, f32)>,
    /// archived annotations of the current video
    pub annotations: Arc<Vec<crate::api::annotations::Annotation>>,
    /// average colour of recent frames, for the ambient glow
    pub ambient: Option<Rgba>,
    ambient_tick: u32,
    pub loop_video: bool,
    load_serial: u64,
    last_progress_save: f64,
    pub storyboard: Option<Storyboard>,
    last_prop_notify: std::time::Instant,
    fallback_tried: bool,
    pub channel_id: Option<String>,
    pub last_activity: std::time::Instant,
    pub audio_track: Option<String>,
    /// "Stats for nerds" overlay
    pub show_stats: bool,
}

impl EventEmitter<PlayerEvent> for Player {}

pub struct PlayerHandle(pub Entity<Player>);
impl Global for PlayerHandle {}

pub fn player(cx: &App) -> Entity<Player> {
    cx.global::<PlayerHandle>().0.clone()
}

pub fn init(cx: &mut App) {
    let p = cx.new(Player::new);
    cx.set_global(PlayerHandle(p));
}

pub fn enqueue(v: VideoItem, cx: &mut App) {
    player(cx).update(cx, |p, cx| {
        if p.video.is_none() {
            p.queue.push(v.clone());
            p.queue_pos = Some(p.queue.len() - 1);
            p.play(v, None, cx);
        } else {
            if p.queue.is_empty() {
                if let Some(cur) = p.video.clone() {
                    p.queue.push(cur);
                    p.queue_pos = Some(0);
                }
            }
            p.queue.push(v);
        }
        cx.notify();
    });
}

impl Player {
    fn new(cx: &mut Context<Self>) -> Self {
        let s = store::settings_snapshot();
        Self {
            mpv: None,
            init_error: None,
            video: None,
            streams: None,
            loading: false,
            error: None,
            quality: s.default_quality,
            audio_only: s.audio_only,
            time: 0.0,
            duration: 0.0,
            paused: true,
            buffering: false,
            seeking: false,
            cache_time: 0.0,
            volume: s.volume,
            muted: false,
            speed: s.default_speed,
            boost: None,
            eof: false,
            frame: None,
            old_frames: Vec::new(),
            video_size: (16.0, 9.0),
            segments: Vec::new(),
            skipped: HashSet::new(),
            sb_prompt: None,
            muted_by_sb: false,
            caption: None,
            queue: Vec::new(),
            queue_pos: None,
            chapters: Vec::new(),
            heatmap: Vec::new(),
            annotations: Arc::new(Vec::new()),
            ambient: None,
            ambient_tick: 0,
            loop_video: s.loop_video,
            load_serial: 0,
            last_progress_save: 0.0,
            storyboard: None,
            last_prop_notify: std::time::Instant::now(),
            fallback_tried: false,
            channel_id: None,
            last_activity: std::time::Instant::now(),
            audio_track: None,
            show_stats: false,
        }
        .with_cx(cx)
    }

    fn with_cx(self, _cx: &mut Context<Self>) -> Self {
        self
    }

    fn ensure_mpv(&mut self, cx: &mut Context<Self>) -> Option<Arc<Mpv>> {
        if let Some(m) = &self.mpv {
            return Some(m.clone());
        }
        let s = store::settings_snapshot();
        let (tx, rx) = futures::channel::mpsc::unbounded::<MpvEvent>();
        let vol = format!("{}", s.volume.round());
        let ua = crate::api::http::BROWSER_UA;
        let hw = if s.hwdec { "auto-copy" } else { "no" };
        let opts: Vec<(&str, &str)> = vec![
            ("vo", "libmpv"),
            ("hwdec", hw),
            ("keep-open", "yes"),
            ("idle", "yes"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("osc", "no"),
            ("cache", "yes"),
            ("demuxer-max-bytes", "96MiB"),
            ("demuxer-max-back-bytes", "24MiB"),
            ("demuxer-readahead-secs", "40"),
            ("network-timeout", "20"),
            ("user-agent", ua),
            ("volume", &vol),
            ("volume-max", "150"),
            ("hr-seek", "yes"),
            ("force-seekable", "yes"),
            ("audio-client-name", crate::PRODUCT_FULL_NAME),
            ("sub-font-size", "42"),
            ("sub-border-size", "2.5"),
            ("sub-auto", "no"),
            ("osd-level", "0"),
            // software render path: convert colors only, never scale on the CPU
            ("zimg-fast", "yes"),
            ("zimg-scaler", "bilinear"),
            ("zimg-dither", "no"),
            ("zimg-threads", "auto"),
            ("sws-scaler", "fast-bilinear"),
            ("video-sync", "audio"),
            ("framedrop", "vo"),
        ];
        let opts: Vec<(&str, &str)> = opts.into_iter().collect();
        let proxy = s.proxy.clone();
        let mut opts_owned: Vec<(String, String)> = opts.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        // normal videos reach mpv through the app's local stream proxy, which already
        // uses the user's proxy; mpv itself only fetches live streams directly, and can
        // only use HTTP proxies (127.0.0.1 is excluded through no_proxy, see main)
        if crate::store::mpv_http_proxy(&proxy) {
            opts_owned.push(("http-proxy".into(), proxy));
        }
        if let Ok(f) = std::env::var("STREAMTUBE_MPV_LOG") {
            opts_owned.push(("log-file".into(), f));
        }
        let refs: Vec<(&str, &str)> = opts_owned.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        match Mpv::new(&refs, move |ev| {
            let _ = tx.unbounded_send(ev);
        }) {
            Ok(m) => {
                self.mpv = Some(m.clone());
                cx.spawn(async move |this, cx| {
                    use futures::StreamExt;
                    let mut rx = rx;
                    while let Some(ev) = rx.next().await {
                        // coalesce queued events
                        let mut batch = vec![ev];
                        while let Ok(e) = rx.try_recv() {
                            batch.push(e);
                        }
                        let ok = this
                            .update(cx, |p, cx| {
                                for e in batch {
                                    p.handle_event(e, cx);
                                }
                            })
                            .is_ok();
                        if !ok {
                            break;
                        }
                    }
                })
                .detach();
                Some(m)
            }
            Err(e) => {
                self.init_error = Some(format!("Could not start the video engine (libmpv): {e}"));
                None
            }
        }
    }

    pub fn mpv(&self) -> Option<Arc<Mpv>> {
        self.mpv.clone()
    }

    fn handle_event(&mut self, ev: MpvEvent, cx: &mut Context<Self>) {
        match ev {
            MpvEvent::Frame => {
                let t0 = std::time::Instant::now();
                crate::perf::add(&crate::perf::UI_FRAMES, 1);
                let _g = scopeguard(move || crate::perf::add(&crate::perf::UI_HANDLE_US, t0.elapsed().as_micros() as u64));
                if let Some(m) = &self.mpv {
                    if let Some(f) = m.take_frame() {
                        self.ambient_tick += 1;
                        if self.ambient_tick % 12 == 1 && store::settings_snapshot().ambient_mode {
                            if let Some(c) = average_color(&f.data, f.width, f.height) {
                                let changed = self.ambient.map(|o| (o.r - c.r).abs() + (o.g - c.g).abs() + (o.b - c.b).abs() > 0.04).unwrap_or(true);
                                if changed {
                                    self.ambient = Some(c);
                                    cx.emit(PlayerEvent::Ambient);
                                }
                            }
                        }
                        if let Some(buf) = image::RgbaImage::from_raw(f.width, f.height, f.data) {
                            let img = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buf)]));
                            if let Some(old) = self.frame.replace(img) {
                                self.old_frames.push(old);
                            }
                            // drop textures of frames that are no longer displayed
                            while self.old_frames.len() > 1 {
                                let o = self.old_frames.remove(0);
                                cx.drop_image(o, None);
                            }
                            cx.notify();
                        }
                    }
                }
            }
            MpvEvent::FileLoaded => {
                self.loading = false;
                self.eof = false;
                self.apply_caption();
                cx.emit(PlayerEvent::Loaded);
                cx.notify();
            }
            MpvEvent::PlaybackRestart => {
                self.seeking = false;
                cx.notify();
            }
            MpvEvent::EndFile { eof, error } => {
                if let Some(e) = error {
                    if self.video.is_some() {
                        if self.loading && !self.fallback_tried {
                            self.fallback_to_ytdlp(cx);
                        } else {
                            self.loading = false;
                            self.error = Some(format!("Playback error: {e}"));
                        }
                    }
                }
                if eof {
                    self.on_ended(cx);
                }
                cx.notify();
            }
            MpvEvent::Prop(name, val) => {
                // While frames are flowing, the frame event already redraws the player;
                // only redraw for clock/cache updates when no video frames arrive.
                let quiet = matches!(name, "time-pos" | "demuxer-cache-time")
                    && !self.paused
                    && !self.audio_only
                    && self.frame.is_some()
                    && self.last_prop_notify.elapsed() < std::time::Duration::from_millis(500);
                match (name, val) {
                    ("time-pos", PropValue::Double(t)) => {
                        self.time = t;
                        self.on_time(cx);
                    }
                    ("duration", PropValue::Double(d)) => self.duration = d,
                    ("pause", PropValue::Flag(b)) => self.paused = b,
                    ("eof-reached", PropValue::Flag(b)) => {
                        if b && !self.eof {
                            self.eof = true;
                            self.on_ended(cx);
                        }
                        self.eof = b;
                    }
                    ("paused-for-cache", PropValue::Flag(b)) => self.buffering = b,
                    ("seeking", PropValue::Flag(b)) => self.seeking = b,
                    ("demuxer-cache-time", PropValue::Double(c)) => self.cache_time = c,
                    ("speed", PropValue::Double(s)) => self.speed = s,
                    ("volume", PropValue::Double(v)) => self.volume = v,
                    ("mute", PropValue::Flag(m)) => self.muted = m,
                    ("dwidth", PropValue::Double(w)) => self.video_size.0 = w,
                    ("dheight", PropValue::Double(h)) => self.video_size.1 = h,
                    _ => {}
                }
                if !quiet {
                    self.last_prop_notify = std::time::Instant::now();
                    cx.notify();
                }
            }
            MpvEvent::Shutdown => {}
        }
    }

    fn on_ended(&mut self, cx: &mut Context<Self>) {
        self.save_progress(true);
        if self.loop_video {
            self.seek_to(0.0);
            self.set_paused(false);
            return;
        }
        if let Some(pos) = self.queue_pos {
            if pos + 1 < self.queue.len() {
                self.play_queue_index(pos + 1, cx);
                return;
            }
        }
        cx.emit(PlayerEvent::Ended);
    }

    pub fn play_queue_index(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(v) = self.queue.get(idx).cloned() {
            self.queue_pos = Some(idx);
            let id = v.id.clone();
            self.play(v, None, cx);
            crate::ui::app::sync_watch_route(id, cx);
        }
    }

    fn on_time(&mut self, cx: &mut Context<Self>) {
        let t = self.time;
        // progress save
        if (t - self.last_progress_save).abs() >= 5.0 {
            self.save_progress(false);
            let id = self.video.as_ref().map(|v| v.id.clone());
            if let Some(id) = id {
                cx.default_global::<crate::ui::cards::Watched>().0.insert(id, (t, self.duration));
            }
        }
        // SponsorBlock
        let s = store::settings_snapshot();
        if !s.sponsorblock || self.segments.is_empty() {
            return;
        }
        let mut prompt = None;
        let mut in_mute = false;
        for seg in self.segments.clone() {
            let action = s.sb_actions.get(&seg.category).map(String::as_str).unwrap_or("off");
            if action == "off" || seg.action == "poi" || seg.action == "full" {
                continue;
            }
            let inside = t >= seg.start && t < seg.end - 0.3;
            if !inside {
                continue;
            }
            if seg.action == "mute" {
                in_mute = true;
                continue;
            }
            if seg.end - seg.start < s.sb_min_duration {
                continue;
            }
            match action {
                "skip" if !self.skipped.contains(&seg.uuid) => {
                    self.skipped.insert(seg.uuid.clone());
                    let back_to = seg.start;
                    self.seek_to(seg.end);
                    let server = s.sponsorblock_server.clone();
                    let uuid = seg.uuid.clone();
                    std::thread::spawn(move || services::sponsor_viewed(&server, &uuid));
                    if s.sb_notify {
                        let weak = cx.entity().downgrade();
                        let mut actions: Vec<(SharedString, Arc<dyn Fn(&mut Window, &mut App)>)> = vec![(
                            "Unskip".into(),
                            Arc::new(move |_, cx: &mut App| {
                                if let Some(p) = weak.upgrade() {
                                    p.update(cx, |p, _| p.seek_to(back_to));
                                }
                            }),
                        )];
                        if !s.sb_user_id.is_empty() {
                            for (label, up) in [("Upvote", true), ("Downvote", false)] {
                                let (server, uuid, user) = (s.sponsorblock_server.clone(), seg.uuid.clone(), s.sb_user_id.clone());
                                actions.push((
                                    label.into(),
                                    Arc::new(move |_, cx: &mut App| {
                                        let (server, uuid, user) = (server.clone(), uuid.clone(), user.clone());
                                        cx.spawn(async move |cx| {
                                            let res = crate::ui::blocking(move || services::sponsor_vote(&server, &uuid, &user, up)).await;
                                            let _ = cx.update(|cx| match res {
                                                Ok(()) => toast(cx, "Thanks for voting"),
                                                Err(e) => toast(cx, format!("Vote failed: {e}")),
                                            });
                                        })
                                        .detach();
                                    }),
                                ));
                            }
                        }
                        crate::ui::toast_with_actions(cx, format!("Skipped {}", services::sponsor_category_name(&seg.category)), actions);
                    }
                }
                "ask" => prompt = Some(seg.clone()),
                _ => {}
            }
        }
        if in_mute != self.muted_by_sb {
            self.muted_by_sb = in_mute;
            if let Some(m) = &self.mpv {
                m.set_flag("mute", in_mute || self.muted);
            }
        }
        self.sb_prompt = prompt;
    }

    pub fn save_progress(&mut self, force: bool) {
        if let Some(v) = &self.video {
            if force || self.time > 0.0 {
                store::history_progress(&v.id, self.time, self.duration);
                self.last_progress_save = self.time;
                crate::api::web::report_progress(&v.id, self.time, !self.paused);
            }
        }
    }

    /// Starts playing a video. Fetches streams, SponsorBlock segments and resumes progress.
    pub fn play(&mut self, video: VideoItem, start: Option<f64>, cx: &mut Context<Self>) {
        if !crate::wetube_session::valid_id(&video.id) { toast(cx, "Invalid video ID"); return; }
        let start = start.filter(|t| t.is_finite() && (0.0..=604800.0).contains(t));
        self.save_progress(true);
        if let Some(prev) = &self.video {
            crate::api::web::report_stop(&prev.id, self.time);
        }
        let Some(mpv) = self.ensure_mpv(cx) else {
            self.video = Some(video);
            cx.notify();
            return;
        };
        self.load_serial += 1;
        self.fallback_tried = false;
        let serial = self.load_serial;
        self.video = Some(video.clone());
        self.loading = true;
        self.error = None;
        self.time = 0.0;
        self.duration = video.duration_secs.unwrap_or(0) as f64;
        self.eof = false;
        self.segments.clear();
        self.skipped.clear();
        self.sb_prompt = None;
        self.streams = None;
        self.storyboard = None;
        self.chapters.clear();
        self.heatmap.clear();
        self.annotations = Arc::new(Vec::new());
        self.ambient = None;
        self.channel_id = video.channel_id.clone();
        self.audio_track = None;
        if let Some(old) = self.frame.take() {
            cx.drop_image(old, None);
        }
        let _ = mpv.command(&["stop"]);
        store::history_add(&video);
        // with the browser sign-in, the video goes into the account's YouTube history
        // and the position reported as it plays is where other devices resume
        crate::api::web::report_start(&video.id, self.duration);
        let s = store::settings_snapshot();
        let id = video.id.clone();
        let resume = if start.is_some() {
            start
        } else if s.resume_playback {
            store::progress_map().get(&id).and_then(|(p, d)| {
                (*p > 10.0 && (*d <= 0.0 || *p < d - 15.0)).then_some(*p)
            })
        } else {
            None
        };
        cx.notify();
        if s.annotations {
            self.load_annotations(cx);
        }
        cx.spawn(async move |this, cx| {
            let id2 = id.clone();
            let streams_fut = blocking(move || streams::get_streams(&id2));
            let id3 = id.clone();
            let sb = s.clone();
            let seg_fut = blocking(move || {
                if !sb.sponsorblock {
                    return Vec::new();
                }
                let cats: Vec<String> = sb
                    .sb_actions
                    .iter()
                    .filter(|(_, a)| a.as_str() != "off")
                    .map(|(c, _)| c.clone())
                    .collect();
                services::sponsor_segments(&id3, &sb.sponsorblock_server, &cats).unwrap_or_default()
            });
            let (res, segs) = futures::join!(streams_fut, seg_fut);
            let _ = this.update(cx, |p, cx| {
                if p.load_serial != serial {
                    return;
                }
                p.segments = segs;
                match res {
                    Ok(info) => {
                        let info = Arc::new(info);
                        if p.duration <= 0.0 {
                            p.duration = info.length_secs as f64;
                        }
                        if let Some(v) = p.video.as_mut() {
                            if v.channel_id.is_none() && !info.channel_id.is_empty() {
                                v.channel_id = Some(info.channel_id.clone());
                            }
                            if v.title.is_empty() {
                                v.title = info.title.clone();
                            }
                            if v.channel_name.is_empty() {
                                v.channel_name = info.author.clone();
                            }
                        }
                        if p.channel_id.is_none() && !info.channel_id.is_empty() {
                            p.channel_id = Some(info.channel_id.clone());
                        }
                        p.storyboard = info.storyboard.clone();
                        p.streams = Some(info);
                        // default caption
                        let s = store::settings_snapshot();
                        p.caption = if s.captions_enabled {
                            p.streams.as_ref().and_then(|i| {
                                i.captions
                                    .iter()
                                    .position(|c| c.lang.starts_with(&s.captions_lang) && !c.auto)
                                    .or_else(|| i.captions.iter().position(|c| c.lang.starts_with(&s.captions_lang)))
                            })
                        } else {
                            None
                        };
                        // speed per channel
                        let speed = p
                            .channel_id
                            .as_deref()
                            .filter(|_| s.remember_speed_per_channel)
                            .and_then(store::channel_speed)
                            .unwrap_or(s.default_speed);
                        p.speed = speed;
                        p.load_current(resume, false, cx);
                    }
                    Err(e) => {
                        p.loading = false;
                        p.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches the current video's archived annotations (most videos have none).
    pub fn load_annotations(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.video.as_ref().map(|v| v.id.clone()) else { return };
        let serial = self.load_serial;
        cx.spawn(async move |this, cx| {
            let list = blocking(move || crate::api::annotations::fetch(&id).unwrap_or_default()).await;
            let _ = this.update(cx, |p, cx| {
                if p.load_serial == serial && !list.is_empty() {
                    p.annotations = Arc::new(list);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The built-in streams failed to open: fetch them again through yt-dlp.
    fn fallback_to_ytdlp(&mut self, cx: &mut Context<Self>) {
        self.fallback_tried = true;
        let Some(id) = self.video.as_ref().map(|v| v.id.clone()) else { return };
        streams::invalidate(&id);
        let serial = self.load_serial;
        let resume = (self.time > 1.0).then_some(self.time);
        cx.spawn(async move |this, cx| {
            let res = blocking(move || streams::ytdlp_streams(&id)).await;
            let _ = this.update(cx, |p, cx| {
                if p.load_serial != serial {
                    return;
                }
                match res {
                    Ok(info) => {
                        if p.storyboard.is_none() {
                            p.storyboard = info.storyboard.clone();
                        }
                        p.streams = Some(Arc::new(info));
                        p.load_current(resume, false, cx);
                    }
                    Err(e) => {
                        p.loading = false;
                        p.error = Some(format!("Could not play this video: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// (Re)loads current streams into mpv with current quality/audio settings.
    fn load_current(&mut self, start: Option<f64>, keep_pause: bool, cx: &mut Context<Self>) {
        let (Some(mpv), Some(info)) = (self.mpv.clone(), self.streams.clone()) else { return };
        let s = store::settings_snapshot();
        let src = if info.hls_only {
            let audio = streams::hls_audio_choice(&info, self.audio_track.as_deref());
            info.hls_url.as_deref().map(|u| crate::player::proxy::local_hls(u, self.quality, audio.as_deref()))
        } else if info.is_live || (info.video.is_empty() && info.muxed.is_empty()) {
            info.hls_url.clone()
        } else {
            let (v, a) = streams::pick_formats(&info, self.quality, &s.codec_pref, self.audio_track.as_deref());
            let v = if self.audio_only { None } else { v };
            if v.is_none() && a.is_none() {
                info.muxed.last().map(|m| m.url.clone())
            } else {
                streams::mpv_source(v, a, info.length_secs)
            }
        };
        let Some(src) = src else {
            self.error = Some("No playable stream found".into());
            self.loading = false;
            return;
        };
        let was_paused = self.paused;
        let mut opts = vec![format!("speed={}", self.speed)];
        if let Some(t) = start.filter(|t| *t > 0.0) {
            opts.push(format!("start={t}"));
        }
        if keep_pause && was_paused {
            opts.push("pause=yes".into());
        } else {
            opts.push("pause=no".into());
        }
        if let Some(v) = &self.video {
            opts.push(format!("force-media-title={}", v.title.replace(',', " ")));
        }
        mpv.set("vid", if self.audio_only { "no" } else { "auto" });
        self.loading = true;
        // mpv 0.38 added an index argument; older libmpv (Ubuntu 22.04's 0.34) takes options third
        let joined = opts.join(",");
        let res = mpv.command(&["loadfile", &src, "replace", "-1", &joined]).or_else(|_| mpv.command(&["loadfile", &src, "replace", &joined]));
        if let Err(e) = res {
            self.error = Some(e.to_string());
        }
        cx.notify();
    }

    /// Applies caption styling settings to mpv (see [`caption_font`] and [`mpv_color`]).
    fn apply_caption_style(&self) {
        let Some(mpv) = &self.mpv else { return };
        let s = store::settings_snapshot();
        mpv.set("sub-font-size", &s.caption_size.to_string());
        mpv.set("sub-font", caption_font(&s.caption_font));
        mpv.set("sub-bold", "no");
        mpv.set("sub-margin-y", "40");
        mpv.set("sub-color", &mpv_color(s.caption_color, s.caption_opacity));
        if s.caption_background {
            mpv.set("sub-border-style", "background-box");
            mpv.set("sub-back-color", &mpv_color(s.caption_bg_color, s.caption_bg_opacity));
            mpv.set("sub-border-size", "2");
            mpv.set("sub-shadow-offset", "0");
        } else {
            mpv.set("sub-border-style", "outline-and-shadow");
            mpv.set("sub-border-color", "#FF000000");
            mpv.set("sub-shadow-color", "#80000000");
            let (border, shadow) = match s.caption_edge.as_str() {
                "outline" => ("2.5", "0"),
                "shadow" => ("0", "2"),
                "none" => ("0", "0"),
                _ => ("2.5", "1"),
            };
            mpv.set("sub-border-size", border);
            mpv.set("sub-shadow-offset", shadow);
        }
    }

    /// Re-applies settings that affect the current playback.
    pub fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let s = store::settings_snapshot();
        self.loop_video = s.loop_video;
        if let Some(m) = &self.mpv {
            m.set("hwdec", if s.hwdec { "auto-copy" } else { "no" });
        }
        self.apply_caption_style();
        if self.video.is_some() {
            if s.audio_only != self.audio_only {
                self.set_audio_only(s.audio_only, cx);
            }
            let want_caps = s.captions_enabled;
            if want_caps && self.caption.is_none() {
                if let Some(i) = self.streams.as_ref().and_then(|i| {
                    i.captions
                        .iter()
                        .position(|c| c.lang.starts_with(&s.captions_lang) && !c.auto)
                        .or_else(|| i.captions.iter().position(|c| c.lang.starts_with(&s.captions_lang)))
                }) {
                    self.caption = Some(i);
                    self.apply_caption();
                }
            } else if !want_caps && self.caption.is_some() {
                self.caption = None;
                self.apply_caption();
            }
        }
        cx.notify();
    }

    fn apply_caption(&mut self) {
        self.apply_caption_style();
        let (Some(mpv), Some(info)) = (&self.mpv, &self.streams) else { return };
        let _ = mpv.command(&["sub-remove"]);
        if let Some(c) = self.caption.and_then(|i| info.captions.get(i)) {
            let _ = mpv.command(&["sub-add", &c.url, "select", &c.name, &c.lang]);
        } else {
            mpv.set("sid", "no");
        }
    }

    pub fn set_caption(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        self.caption = idx;
        self.apply_caption();
        if let Some(i) = idx {
            if let Some(c) = self.streams.as_ref().and_then(|s| s.captions.get(i)) {
                let lang = c.lang.split('-').next().unwrap_or("en").to_string();
                store::update_settings(|s| {
                    s.captions_enabled = true;
                    s.captions_lang = lang;
                });
            }
        } else {
            store::update_settings(|s| s.captions_enabled = false);
        }
        cx.notify();
    }

    /// Where "Jump ahead" goes: the next part of the video most viewers skip to.
    pub fn jump_ahead_target(&self) -> Option<f64> {
        let from = self.time + 5.0;
        let later: Vec<&(f64, f64, f32)> = self.heatmap.iter().filter(|m| m.0 > from).collect();
        let max = later.iter().map(|m| m.2).fold(0.0f32, f32::max);
        if max <= 0.0 {
            return None;
        }
        // first local peak that is close to the strongest one ahead
        for (i, m) in later.iter().enumerate() {
            let next = later.get(i + 1).map(|n| n.2).unwrap_or(0.0);
            if m.2 >= max * 0.8 && m.2 >= next {
                return Some(m.0);
            }
        }
        None
    }

    pub fn set_quality(&mut self, q: u32, cx: &mut Context<Self>) {
        if q > 0 && store::settings_snapshot().remember_quality {
            store::update_settings(|s| s.default_quality = q);
        }
        self.quality = q;
        let t = self.time;
        self.load_current(Some(t), true, cx);
    }

    pub fn set_audio_track(&mut self, lang: Option<String>, cx: &mut Context<Self>) {
        self.audio_track = lang;
        let t = self.time;
        self.load_current(Some(t), true, cx);
    }

    pub fn set_audio_only(&mut self, on: bool, cx: &mut Context<Self>) {
        self.audio_only = on;
        store::update_settings(|s| s.audio_only = on);
        if let Some(old) = self.frame.take() {
            cx.drop_image(old, None);
        }
        let t = self.time;
        self.load_current(Some(t), true, cx);
    }

    pub fn available_qualities(&self) -> Vec<u32> {
        let mut q: Vec<u32> = self
            .streams
            .as_ref()
            .map(|s| s.video.iter().filter(|f| f.height > 0).map(streams::fmt_class).collect())
            .unwrap_or_default();
        q.sort_unstable_by(|a, b| b.cmp(a));
        q.dedup();
        q
    }

    pub fn audio_tracks(&self) -> Vec<String> {
        let mut t: Vec<String> = self
            .streams
            .as_ref()
            .map(|s| s.audio.iter().filter_map(|a| a.audio_language.clone()).collect())
            .unwrap_or_default();
        t.sort();
        t.dedup();
        t
    }

    pub fn current_height(&self) -> u32 {
        if self.video_size.1 <= 0.0 {
            return 0;
        }
        streams::quality_class(self.video_size.0 as u32, self.video_size.1 as u32)
    }

    /// Playback details for the "Stats for nerds" overlay, read from mpv.
    pub fn stats(&self) -> Vec<(&'static str, String)> {
        let Some(m) = &self.mpv else { return Vec::new() };
        let f = |k: &str| m.get_f64(k);
        let st = |k: &str| m.get_string(k).filter(|s| !s.is_empty());
        let mut out: Vec<(&'static str, String)> = Vec::new();
        if let Some(v) = &self.video {
            let source = self.streams.as_ref().map(|s| s.source).unwrap_or("");
            out.push(("Video ID", if source.is_empty() { v.id.clone() } else { format!("{}  ·  via {source}", v.id) }));
        }
        if !self.audio_only {
            let (w, h) = (f("video-params/w").unwrap_or(0.0), f("video-params/h").unwrap_or(0.0));
            let fps = f("container-fps").or_else(|| f("estimated-vf-fps")).unwrap_or(0.0);
            if w > 0.0 {
                out.push(("Resolution", format!("{w:.0}x{h:.0}@{fps:.0}")));
            }
            let dropped = f("frame-drop-count").unwrap_or(0.0) + f("decoder-frame-drop-count").unwrap_or(0.0);
            let shown = f("estimated-frame-number").unwrap_or(0.0);
            out.push(("Frames", format!("{dropped:.0} dropped of {shown:.0}")));
            if let Some(c) = st("video-codec") {
                let pf = st("video-params/hw-pixelformat").or_else(|| st("video-params/pixelformat")).unwrap_or_default();
                out.push(("Video codec", if pf.is_empty() { c } else { format!("{c}  ·  {pf}") }));
            }
            let hw = st("hwdec-current").filter(|s| s != "no").unwrap_or_else(|| "off (software)".into());
            out.push(("Hardware decoding", hw));
            let color = [st("video-params/colormatrix"), st("video-params/primaries"), st("video-params/gamma")].into_iter().flatten().collect::<Vec<_>>();
            if !color.is_empty() {
                out.push(("Color", color.join(" / ")));
            }
        }
        if let Some(c) = st("audio-codec-name").or_else(|| st("audio-codec")) {
            let rate = f("audio-params/samplerate").map(|r| format!("  ·  {:.1} kHz", r / 1000.0)).unwrap_or_default();
            let ch = f("audio-params/channel-count").map(|c| format!("  ·  {c:.0} ch")).unwrap_or_default();
            out.push(("Audio codec", format!("{c}{rate}{ch}")));
        }
        let kbps = |k: &str| f(k).filter(|b| *b > 0.0).map(|b| format!("{:.0} kbps", b / 1000.0));
        let rates = [kbps("video-bitrate").map(|v| format!("video {v}")), kbps("audio-bitrate").map(|a| format!("audio {a}"))].into_iter().flatten().collect::<Vec<_>>();
        if !rates.is_empty() {
            out.push(("Bitrate", rates.join("  ·  ")));
        }
        if let Some(b) = f("demuxer-cache-duration") {
            out.push(("Buffer health", format!("{b:.2} s")));
        }
        if let Some(sp) = f("cache-speed") {
            out.push(("Connection speed", format!("{:.0} Kbps", sp * 8.0 / 1000.0)));
        }
        if let Some(mb) = m.get_f64("demuxer-cache-state/fw-bytes") {
            out.push(("Buffered data", format!("{:.1} MB", mb / 1048576.0)));
        }
        out.push(("Volume", format!("{:.0}%{}", self.volume, if self.muted { " (muted)" } else { "" })));
        out.push(("Speed", format!("{:.2}x", self.speed)));
        if let Some(a) = f("avsync") {
            out.push(("A/V sync", format!("{:+.1} ms", a * 1000.0)));
        }
        out.push(("Position", format!("{:.1} / {:.1} s", self.time, self.duration)));
        out
    }

    // ───── transport controls ─────

    pub fn toggle_pause(&mut self) {
        if self.eof {
            self.seek_to(0.0);
            self.set_paused(false);
            return;
        }
        let p = !self.paused;
        self.set_paused(p);
    }

    pub fn set_paused(&mut self, p: bool) {
        self.paused = p;
        if let Some(m) = &self.mpv {
            m.set_flag("pause", p);
        }
        if p {
            self.save_progress(true);
        }
    }

    pub fn seek_to(&mut self, t: f64) {
        if !t.is_finite() { return; }
        let t = if self.duration > 0.0 { t.clamp(0.0, self.duration - 0.1) } else { t.max(0.0) };
        self.time = t;
        if let Some(m) = &self.mpv {
            let _ = m.command(&["seek", &format!("{t}"), "absolute+exact"]);
        }
    }

    pub fn seek_by(&mut self, d: f64) {
        let t = self.time + d;
        self.seek_to(t);
    }

    pub fn set_volume(&mut self, v: f64) {
        let v = v.clamp(0.0, 150.0);
        self.volume = v;
        if let Some(m) = &self.mpv {
            m.set_f64("volume", v);
            if self.muted && v > 0.0 {
                m.set_flag("mute", false);
            }
        }
        store::update_settings(|s| s.volume = v.min(100.0));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(m) = &self.mpv {
            m.set_flag("mute", self.muted);
        }
    }

    /// Press-and-hold: plays at 2x until `end_boost`, without touching the saved speed.
    pub fn start_boost(&mut self) {
        if self.boost.is_some() || self.video.is_none() {
            return;
        }
        self.boost = Some(self.speed);
        if self.paused {
            self.set_paused(false);
        }
        if let Some(m) = &self.mpv {
            m.set_f64("speed", (self.speed * 2.0).clamp(2.0, 4.0));
        }
    }

    pub fn end_boost(&mut self) {
        if let Some(prev) = self.boost.take() {
            if let Some(m) = &self.mpv {
                m.set_f64("speed", prev);
            }
        }
    }

    pub fn set_speed(&mut self, s: f64) {
        let s = (s * 100.0).round() / 100.0;
        let s = s.clamp(0.1, 5.0);
        self.speed = s;
        if let Some(m) = &self.mpv {
            m.set_f64("speed", s);
        }
        if store::settings_snapshot().remember_speed_per_channel {
            if let Some(c) = &self.channel_id {
                store::set_channel_speed(c, s);
            }
        }
    }

    pub fn set_loop(&mut self, l: bool) {
        self.loop_video = l;
    }

    pub fn frame_step(&mut self, forward: bool) {
        if let Some(m) = &self.mpv {
            let _ = m.command(&[if forward { "frame-step" } else { "frame-back-step" }]);
        }
    }

    pub fn screenshot(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = &self.mpv {
            let dir = dirs::picture_dir().unwrap_or_else(std::env::temp_dir).join("Stream Tube");
            let _ = std::fs::create_dir_all(&dir);
            let name = format!(
                "{}-{}.png",
                self.video.as_ref().map(|v| v.id.as_str()).unwrap_or("frame"),
                self.time as u64
            );
            let path = dir.join(name);
            let _ = m.command(&["screenshot-to-file", &path.to_string_lossy(), "video"]);
            toast(cx, format!("Screenshot saved to {}", path.display()));
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.save_progress(true);
        if let Some(v) = &self.video {
            crate::api::web::report_stop(&v.id, self.time);
        }
        self.load_serial += 1;
        if let Some(m) = &self.mpv {
            let _ = m.command(&["stop"]);
        }
        self.video = None;
        self.streams = None;
        self.loading = false;
        self.queue.clear();
        self.queue_pos = None;
        if let Some(old) = self.frame.take() {
            cx.drop_image(old, None);
        }
        cx.notify();
    }

    /// Jumps to the start of the next (`dir` > 0) or previous chapter, as Ctrl+Right
    /// and Ctrl+Left do on YouTube. False when there is no chapter that way.
    pub fn seek_chapter(&mut self, dir: i32) -> bool {
        let starts: Vec<f64> = self.chapters.iter().map(|c| c.start).collect();
        match chapter_target(&starts, self.time, dir) {
            Some(t) => {
                self.seek_to(t);
                true
            }
            None => false,
        }
    }

    pub fn current_chapter(&self) -> Option<&Chapter> {
        self.chapters.iter().rev().find(|c| c.start <= self.time + 0.01)
    }

    pub fn highlight(&self) -> Option<&Segment> {
        self.segments.iter().find(|s| s.action == "poi" || s.category == "poi_highlight")
    }
}

/// Applies changed settings to the running player.
pub fn apply_settings(cx: &mut App) {
    player(cx).update(cx, |p, cx| p.apply_settings(cx));
}

struct Guard<F: FnMut()>(F);
impl<F: FnMut()> Drop for Guard<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}
fn scopeguard<F: FnMut()>(f: F) -> Guard<F> {
    Guard(f)
}

/// Average colour of a BGRA frame from a sparse sample grid, slightly brightened
/// so dark scenes still give a visible glow.
fn average_color(data: &[u8], w: u32, h: u32) -> Option<Rgba> {
    if w < 4 || h < 4 || data.len() < (w * h * 4) as usize {
        return None;
    }
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for gy in 0..12 {
        for gx in 0..16 {
            let x = (w - 1) * gx / 15;
            let y = (h - 1) * gy / 11;
            let i = ((y * w + x) * 4) as usize;
            b += data[i] as u64;
            g += data[i + 1] as u64;
            r += data[i + 2] as u64;
            n += 1;
        }
    }
    let f = |v: u64| ((v as f32 / n as f32) / 255.0 * 1.25).min(1.0);
    Some(Rgba { r: f(r), g: f(g), b: f(b), a: 1.0 })
}

/// The font for a caption font choice. libass looks fonts up by name, and finds
/// the nearest match when one isn't installed.
pub fn caption_font(key: &str) -> &'static str {
    if cfg!(windows) {
        match key {
            "serif" => "Georgia",
            "mono" => "Consolas",
            "casual" => "Comic Sans MS",
            _ => "Segoe UI",
        }
    } else {
        match key {
            "serif" => "DejaVu Serif",
            "mono" => "DejaVu Sans Mono",
            "casual" => "Comic Neue",
            _ => "DejaVu Sans",
        }
    }
}

/// 0xRRGGBB plus an opacity in percent, as mpv's "#AARRGGBB".
pub fn mpv_color(rgb: u32, opacity: u32) -> String {
    let a = (opacity.min(100) * 255 + 50) / 100;
    format!("#{a:02X}{:06X}", rgb & 0xFFFFFF)
}

#[cfg(test)]
mod caption_tests {
    #[::core::prelude::v1::test]
    fn colors() {
        assert_eq!(super::mpv_color(0xFFFF00, 100), "#FFFFFF00");
        assert_eq!(super::mpv_color(0x000000, 75), "#BF000000");
        assert_eq!(super::mpv_color(0x123456, 0), "#00123456");
    }
}

/// Where a chapter jump from `now` lands: the next chapter's start (`dir` > 0), or
/// the previous chapter's start (in the first chapter, its own start).
fn chapter_target(starts: &[f64], now: f64, dir: i32) -> Option<f64> {
    let now = now + 0.01;
    if dir > 0 {
        return starts.iter().copied().find(|&s| s > now);
    }
    let cur = starts.iter().rposition(|&s| s <= now)?;
    Some(starts[cur.saturating_sub(1)])
}

#[cfg(test)]
mod chapter_tests {
    use super::chapter_target;

    #[::core::prelude::v1::test]
    fn jumps() {
        let c = [0.0, 60.0, 120.0];
        assert_eq!(chapter_target(&c, 30.0, 1), Some(60.0));
        assert_eq!(chapter_target(&c, 60.0, 1), Some(120.0));
        assert_eq!(chapter_target(&c, 130.0, 1), None);
        assert_eq!(chapter_target(&c, 90.0, -1), Some(0.0));
        assert_eq!(chapter_target(&c, 60.0, -1), Some(0.0));
        assert_eq!(chapter_target(&c, 10.0, -1), Some(0.0));
        assert_eq!(chapter_target(&[], 10.0, 1), None);
    }
}
