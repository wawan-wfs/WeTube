//! Stream extraction: yt-dlp for playable formats, the ANDROID_VR Innertube client
//! for fast metadata (storyboards, captions), with an in-memory cache and prefetch.
use super::http;
use super::json::*;
use super::types::*;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::process::Command;

const VR_VERSION: &str = "1.65.10";
const VR_UA: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";

/// Resolves playable streams for a video (cached for a few hours).
///
/// YouTube now cuts anonymous Innertube stream URLs off after ~1 MB unless the
/// request carries a PO token, so in "auto" mode yt-dlp provides the formats
/// while the fast Innertube call contributes storyboards and captions.
pub fn get_streams(video_id: &str) -> Result<StreamInfo> {
    if let Some(info) = cache_get(video_id) {
        return Ok(info);
    }
    // join an in-flight prefetch for the same video instead of starting another
    let lock = inflight_lock(video_id);
    let _g = lock.lock();
    if let Some(info) = cache_get(video_id) {
        return Ok(info);
    }
    let info = resolve(video_id)?;
    cache_put(video_id, &info);
    Ok(info)
}

/// Starts resolving streams in the background (e.g. when hovering a thumbnail).
pub fn prefetch(video_id: &str) {
    if cache_get(video_id).is_some() {
        return;
    }
    let id = video_id.to_string();
    std::thread::spawn(move || {
        let _ = get_streams(&id);
    });
}

/// Prefetches after the pointer rests on a thumbnail for a moment.
pub fn prefetch_on_hover(video_id: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let id = video_id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if SEQ.load(Ordering::SeqCst) == seq {
            prefetch(&id);
        }
    });
}

fn resolve(video_id: &str) -> Result<StreamInfo> {
    let mode = crate::store::settings_snapshot().stream_backend.clone();
    match mode.as_str() {
        "ytdlp" => ytdlp_streams(video_id),
        // a chosen client first, the other built-in one if it fails
        "visionos" | "innertube" => visionos(video_id)
            .or_else(|e| ios(video_id).map_err(|e2| anyhow!("{e}\n{e2}")))
            .or_else(|e| android_vr(video_id).map_err(|e2| anyhow!("{e}\n{e2}"))),
        "android_vr" => android_vr(video_id)
            .and_then(|i| if i.video.is_empty() && i.hls_url.is_none() { Err(anyhow!("Android VR returned no playable formats")) } else { Ok(i) })
            .or_else(|e| visionos(video_id).map_err(|e2| anyhow!("{e}\n{e2}")))
            .or_else(|e| ios(video_id).map_err(|e2| anyhow!("{e}\n{e2}"))),
        _ => {
            // The visionOS client hands out an HLS manifest that needs neither a
            // PO token nor JS challenge solving: fast, and works signed out.
            let t = std::time::Instant::now();
            let vision = visionos(video_id);
            if crate::perf::on() {
                match &vision {
                    Ok(i) => eprintln!("[streams] visionos ok in {:?}: {} variants, hls={:?}", t.elapsed(), i.video.len(), i.hls_url.as_deref().map(|u| &u[..u.len().min(80)])),
                    Err(e) => eprintln!("[streams] visionos failed in {:?}: {e}", t.elapsed()),
                }
            }
            let vision_err = match vision {
                Ok(info) => return Ok(info),
                Err(e) => e,
            };
            // videos made for kids (YouTube Kids) are refused to visionOS and
            // Android VR, but the iOS client serves the same kind of HLS manifest
            let vision_err = match ios(video_id) {
                Ok(info) => return Ok(info),
                Err(e) => anyhow!("{vision_err}\n{e}"),
            };
            let id = video_id.to_string();
            let vr = std::thread::spawn(move || android_vr(&id));
            let yd = ytdlp_streams(video_id);
            let vr = vr.join().unwrap_or_else(|_| Err(anyhow!("innertube thread panicked")));
            match (yd, vr) {
                (Ok(mut y), Ok(v)) => {
                    if y.storyboard.is_none() {
                        y.storyboard = v.storyboard;
                    }
                    if !v.captions.is_empty() {
                        y.captions = v.captions;
                    }
                    Ok(y)
                }
                (Ok(y), Err(_)) => Ok(y),
                // without yt-dlp, Innertube formats are the best we have
                (Err(_), Ok(v)) if !v.video.is_empty() || v.hls_url.is_some() => Ok(v),
                (Err(e1), Ok(_)) => Err(anyhow!("{vision_err}\nyt-dlp: {e1}")),
                (Err(e1), Err(e2)) => Err(anyhow!("{vision_err}\nyt-dlp: {e1}\nInnertube: {e2}")),
            }
        }
    }
}

type Cache = std::collections::HashMap<String, (StreamInfo, std::time::Instant)>;

fn cache() -> &'static parking_lot::Mutex<Cache> {
    static C: std::sync::OnceLock<parking_lot::Mutex<Cache>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(4 * 3600);

fn cache_get(id: &str) -> Option<StreamInfo> {
    let c = cache().lock();
    c.get(id).filter(|(_, t)| t.elapsed() < CACHE_TTL && !c.is_empty()).map(|(i, _)| i.clone())
}

fn cache_put(id: &str, info: &StreamInfo) {
    if info.is_live {
        return; // live manifests expire quickly
    }
    let mut c = cache().lock();
    c.retain(|_, (_, t)| t.elapsed() < CACHE_TTL);
    c.insert(id.to_string(), (info.clone(), std::time::Instant::now()));
}

/// Removes a cached entry (e.g. after its URLs failed to play).
/// Forgets every resolved stream (after switching the playback client).
pub fn clear_cache() {
    cache().lock().clear();
}

pub fn invalidate(id: &str) {
    cache().lock().remove(id);
}

fn inflight_lock(id: &str) -> std::sync::Arc<parking_lot::Mutex<()>> {
    static L: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<String, std::sync::Arc<parking_lot::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let mut m = L.get_or_init(Default::default).lock();
    if m.len() > 200 {
        m.retain(|_, v| std::sync::Arc::strong_count(v) > 1);
    }
    m.entry(id.to_string()).or_default().clone()
}

/// Anonymous visitor id. Player requests carrying one are far less likely to be
/// answered with "Sign in to confirm you're not a bot". Cached for the session.
pub fn visitor_data() -> Option<String> {
    use parking_lot::Mutex;
    static VD: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);
    if let Some((v, t)) = VD.lock().clone() {
        if t.elapsed() < std::time::Duration::from_secs(6 * 3600) {
            return Some(v);
        }
    }
    let body = json!({ "context": { "client": { "clientName": "WEB", "clientVersion": crate::api::innertube::WEB_VERSION, "hl": "en", "gl": "US" } } });
    let r = http::post_json("https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false", &body, &[]).ok()?;
    let v = r.pointer("/responseContext/visitorData")?.as_str()?.to_string();
    *VD.lock() = Some((v.clone(), std::time::Instant::now()));
    Some(v)
}

const VISION_VERSION: &str = "1.02";
const VISION_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

const IOS_VERSION: &str = "20.10.4";
const IOS_UA: &str = "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";

/// An Apple Innertube client whose player response carries a usable HLS manifest.
struct HlsClient {
    label: &'static str,
    source: &'static str,
    id: &'static str,
    version: &'static str,
    ua: &'static str,
    client: Value,
    /// iOS leaves the HLS manifest out of responses tied to the app's visitor id
    visitor: bool,
}

/// visionOS client: metadata, captions and storyboard from the player
/// response, playback from its HLS manifest (variants up to 4K with separate
/// audio renditions).
fn visionos(video_id: &str) -> Result<StreamInfo> {
    hls_client(video_id, HlsClient {
        label: "visionOS",
        source: "visionos",
        id: "101",
        version: VISION_VERSION,
        ua: VISION_UA,
        client: json!({
            "clientName": "VISIONOS", "clientVersion": VISION_VERSION,
            "deviceMake": "Apple", "deviceModel": "RealityDevice17,1",
            "osName": "visionOS", "osVersion": "26.5.23O471",
        }),
        visitor: true,
    })
}

/// iOS client: the same HLS playback as visionOS, and it also plays videos
/// made for kids, which visionOS and Android VR refuse.
/// The iOS client's HLS master playlist for a video (used to download videos
/// made for kids, which yt-dlp gets no formats for).
pub fn ios_hls_manifest(video_id: &str) -> Result<String> {
    ios(video_id)?.hls_url.ok_or_else(|| anyhow!("iOS: no HLS manifest"))
}

fn ios(video_id: &str) -> Result<StreamInfo> {
    hls_client(video_id, HlsClient {
        label: "iOS",
        source: "ios",
        id: "5",
        version: IOS_VERSION,
        ua: IOS_UA,
        client: json!({
            "clientName": "IOS", "clientVersion": IOS_VERSION,
            "deviceMake": "Apple", "deviceModel": "iPhone16,2",
            "osName": "iPhone", "osVersion": "18.3.2.22D82",
        }),
        visitor: false,
    })
}

/// The visitor id that last got the iOS client an HLS manifest. YouTube puts
/// some visitors in a SABR-only experiment that leaves the manifest out, so a
/// visitor that works is kept.
static IOS_VISITOR: parking_lot::Mutex<String> = parking_lot::Mutex::new(String::new());

fn hls_client(video_id: &str, c: HlsClient) -> Result<StreamInfo> {
    let s = crate::store::settings_snapshot();
    let label = c.label;
    let call = |vd: &str| -> Result<Value> {
        let mut client = c.client.clone();
        client["userAgent"] = json!(c.ua);
        client["hl"] = json!(s.language);
        client["gl"] = json!(s.region);
        if !vd.is_empty() {
            client["visitorData"] = json!(vd);
        }
        let body = json!({
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
            "context": { "client": client }
        });
        let mut headers = vec![
            ("User-Agent", c.ua),
            ("X-YouTube-Client-Name", c.id),
            ("X-YouTube-Client-Version", c.version),
            ("Origin", "https://www.youtube.com"),
        ];
        if !vd.is_empty() {
            headers.push(("X-Goog-Visitor-Id", vd));
        }
        let r = http::post_json("https://www.youtube.com/youtubei/v1/player?prettyPrint=false", &body, &headers)?;
        let status = r.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("");
        if status != "OK" {
            let reason = r.pointer("/playabilityStatus/reason").and_then(Value::as_str).unwrap_or(status);
            return Err(anyhow!("{label}: {reason}"));
        }
        Ok(r)
    };
    let has_hls = |r: &Value| r.pointer("/streamingData/hlsManifestUrl").is_some();
    let r = if c.visitor {
        call(&visitor_data().unwrap_or_default())?
    } else {
        // the kept visitor first, then fresh ones (each new request is a new visitor)
        let mut last = None;
        for attempt in 0..5 {
            let kept = IOS_VISITOR.lock().clone();
            let vd = if attempt % 2 == 0 { kept.clone() } else { String::new() };
            let r = call(&vd)?;
            if has_hls(&r) {
                if vd.is_empty() {
                    if let Some(v) = r.pointer("/responseContext/visitorData").and_then(Value::as_str) {
                        *IOS_VISITOR.lock() = v.to_string();
                    }
                }
                last = Some(r);
                break;
            }
            if !kept.is_empty() && vd == kept && attempt >= 2 {
                IOS_VISITOR.lock().clear();
            }
            last = Some(r);
        }
        last.ok_or_else(|| anyhow!("{label}: no response"))?
    };
    let mut info = parse_player(&r);
    let hls = info.hls_url.clone().ok_or_else(|| anyhow!("{label}: no HLS manifest"))?;
    // adaptive URLs from this client need a PO token; only the HLS path is usable
    info.audio.clear();
    info.muxed.clear();
    let (video, audio) = hls_variants(&hls).unwrap_or_default();
    info.video = video;
    // audio renditions (dubbed tracks): only names, the proxy picks the playlist
    info.audio = audio;
    info.hls_only = true;
    info.source = c.source;
    Ok(info)
}

/// The variants of an HLS master playlist, as formats for the quality menu,
/// plus its audio renditions (one per language, `audio_language` = display name).
fn hls_variants(master_url: &str) -> Result<(Vec<StreamFormat>, Vec<StreamFormat>)> {
    let text = http::get_text(master_url)?;
    let attr = |l: &str, k: &str| -> Option<String> {
        let rest = l.split(&format!("{k}=")).nth(1)?;
        Some(if let Some(q) = rest.strip_prefix('"') { q.split('"').next()?.to_string() } else { rest.split(',').next()?.to_string() })
    };
    let mut out: Vec<StreamFormat> = Vec::new();
    let mut audio: Vec<StreamFormat> = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.starts_with("#EXT-X-MEDIA") && l.contains("TYPE=AUDIO") {
            if let Some(name) = hls_media_name(l) {
                if !audio.iter().any(|a| a.audio_language.as_deref() == Some(name.as_str())) {
                    audio.push(StreamFormat {
                        mime: "audio/hls".into(),
                        is_audio: true,
                        audio_default: name.to_lowercase().contains("original"),
                        // the rendition's language code
                        quality_label: attr(l, "LANGUAGE").unwrap_or_default(),
                        audio_language: Some(name),
                        ..Default::default()
                    });
                }
            }
            continue;
        }
        if !l.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let Some(url) = lines.next() else { break };
        let (w, h) = attr(l, "RESOLUTION")
            .and_then(|r| r.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
            .unwrap_or((0, 0));
        let fps = attr(l, "FRAME-RATE").and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0) as u32;
        let bitrate = attr(l, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
        let class = quality_class(w, h);
        // one entry per quality: keep the higher-bitrate variant
        if let Some(e) = out.iter_mut().find(|e| quality_class(e.width, e.height) == class && e.fps == fps) {
            if bitrate > e.bitrate {
                e.bitrate = bitrate;
                e.url = url.trim().to_string();
            }
            continue;
        }
        out.push(StreamFormat {
            itag: 0,
            url: url.trim().to_string(),
            mime: "application/vnd.apple.mpegurl".into(),
            codecs: attr(l, "CODECS").unwrap_or_default(),
            bitrate,
            width: w,
            height: h,
            fps,
            quality_label: format!("{class}p{}", if fps > 30 { fps.to_string() } else { String::new() }),
            ..Default::default()
        });
    }
    if audio.len() < 2 {
        audio.clear();
    }
    Ok((out, audio))
}

/// NAME="..." of an #EXT-X-MEDIA line.
pub fn hls_media_name(l: &str) -> Option<String> {
    let rest = l.split(",NAME=\"").nth(1).or_else(|| l.split(":NAME=\"").nth(1))?;
    Some(rest.split('"').next()?.to_string())
}

/// Which HLS audio rendition to play: the user's pick, else the original
/// (when forced) or the interface-language dub, else the original.
pub fn hls_audio_choice(info: &StreamInfo, picked: Option<&str>) -> Option<String> {
    if info.audio.is_empty() {
        return None;
    }
    if let Some(p) = picked {
        return Some(p.to_string());
    }
    let st = crate::store::settings_snapshot();
    let original = info.audio.iter().find(|a| a.audio_default).and_then(|a| a.audio_language.clone());
    if !st.original_audio {
        let lang = st.language.to_lowercase();
        if let Some(a) = info.audio.iter().find(|a| {
            let code = a.quality_label.to_lowercase();
            !lang.is_empty() && (code == lang || code.starts_with(&format!("{lang}-")))
        }) {
            return a.audio_language.clone();
        }
    }
    original
}

fn android_vr(video_id: &str) -> Result<StreamInfo> {
    let s = crate::store::settings_snapshot();
    let vd = visitor_data().unwrap_or_default();
    let body = json!({
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
        "context": { "client": {
            "clientName": "ANDROID_VR", "clientVersion": VR_VERSION,
            "deviceMake": "Oculus", "deviceModel": "Quest 3",
            "androidSdkVersion": 32, "osName": "Android", "osVersion": "12L",
            "hl": s.language, "gl": s.region, "visitorData": vd
        }}
    });
    let r = http::post_json(
        "https://www.youtube.com/youtubei/v1/player?prettyPrint=false",
        &body,
        &[("User-Agent", VR_UA), ("X-YouTube-Client-Name", "28"), ("X-YouTube-Client-Version", VR_VERSION), ("X-Goog-Visitor-Id", &vd)],
    )?;
    let status = r.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("");
    if status != "OK" {
        let reason = r
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .unwrap_or(status);
        return Err(anyhow!("{reason}"));
    }
    let mut info = parse_player(&r);
    info.source = "innertube";
    Ok(info)
}

fn parse_format(f: &Value) -> Option<StreamFormat> {
    let url = f.get("url")?.as_str()?.to_string();
    let mime_full = f.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let (mime, codecs) = match mime_full.split_once(';') {
        Some((m, c)) => (
            m.trim().to_string(),
            c.trim().trim_start_matches("codecs=").trim_matches('"').to_string(),
        ),
        None => (mime_full.to_string(), String::new()),
    };
    let is_audio = mime.starts_with("audio");
    Some(StreamFormat {
        itag: f.get("itag").and_then(Value::as_u64).unwrap_or(0) as u32,
        url,
        is_audio,
        bitrate: f.get("bitrate").and_then(Value::as_u64).unwrap_or(0),
        width: f.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
        height: f.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
        fps: f.get("fps").and_then(Value::as_u64).unwrap_or(0) as u32,
        quality_label: f.get("qualityLabel").and_then(Value::as_str).unwrap_or("").to_string(),
        audio_language: f
            .pointer("/audioTrack/displayName")
            .and_then(Value::as_str)
            .map(str::to_string),
        audio_default: f.pointer("/audioTrack/audioIsDefault").and_then(Value::as_bool).unwrap_or(true),
        content_length: f.get("contentLength").and_then(Value::as_str).and_then(|c| c.parse().ok()),
        is_muxed: false,
        mime,
        codecs,
    })
}

fn parse_player(r: &Value) -> StreamInfo {
    let vd = r.get("videoDetails").cloned().unwrap_or(Value::Null);
    let sd = r.get("streamingData").cloned().unwrap_or(Value::Null);
    let mut info = StreamInfo {
        id: s(&vd, "/videoId").unwrap_or_default(),
        title: s(&vd, "/title").unwrap_or_default(),
        author: s(&vd, "/author").unwrap_or_default(),
        channel_id: s(&vd, "/channelId").unwrap_or_default(),
        length_secs: s(&vd, "/lengthSeconds").and_then(|x| x.parse().ok()).unwrap_or(0),
        view_count: s(&vd, "/viewCount").and_then(|x| x.parse().ok()),
        description: s(&vd, "/shortDescription").unwrap_or_default(),
        is_live: vd.get("isLive").and_then(Value::as_bool).unwrap_or(false),
        hls_url: s(&sd, "/hlsManifestUrl"),
        ..Default::default()
    };
    for f in sd.get("adaptiveFormats").and_then(Value::as_array).into_iter().flatten() {
        if let Some(sf) = parse_format(f) {
            if sf.is_audio {
                info.audio.push(sf);
            } else {
                info.video.push(sf);
            }
        }
    }
    for f in sd.get("formats").and_then(Value::as_array).into_iter().flatten() {
        if let Some(mut sf) = parse_format(f) {
            sf.is_muxed = true;
            info.muxed.push(sf);
        }
    }
    for t in r
        .pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let url = s(t, "/baseUrl").unwrap_or_default();
        if url.is_empty() {
            continue;
        }
        info.captions.push(CaptionTrack {
            lang: s(t, "/languageCode").unwrap_or_default(),
            name: t.get("name").map(text).unwrap_or_default(),
            url: format!("{}&fmt=vtt", url.replace("&fmt=srv3", "")),
            auto: t.get("kind").and_then(Value::as_str) == Some("asr"),
        });
    }
    if let Some(spec) = r
        .pointer("/storyboards/playerStoryboardSpecRenderer/spec")
        .and_then(Value::as_str)
    {
        info.storyboard = parse_storyboard(spec);
    }
    sort_formats(&mut info);
    info
}

/// spec: "base|w#h#count#cols#rows#interval#name#sigh|..."
pub fn parse_storyboard(spec: &str) -> Option<Storyboard> {
    let mut parts = spec.split('|');
    let base = parts.next()?;
    let levels: Vec<&str> = parts.collect();
    // pick the highest-resolution level (last)
    let (li, lvl) = levels.iter().enumerate().last()?;
    let f: Vec<&str> = lvl.split('#').collect();
    if f.len() < 8 {
        return None;
    }
    let url = base
        .replace("$L", &li.to_string())
        .replace("$N", f[6])
        + &format!("&sigh={}", f[7]);
    Some(Storyboard {
        url_template: url,
        width: f[0].parse().ok()?,
        height: f[1].parse().ok()?,
        count: f[2].parse().ok()?,
        cols: f[3].parse().ok()?,
        rows: f[4].parse().ok()?,
        interval_ms: f[5].parse().ok()?,
    })
}

fn sort_formats(info: &mut StreamInfo) {
    info.video.sort_by(|a, b| (b.height, b.fps, b.bitrate).cmp(&(a.height, a.fps, a.bitrate)));
    info.audio.sort_by(|a, b| b.bitrate.cmp(&a.bitrate));
}

static YTDLP_CACHED: parking_lot::Mutex<Option<(String, Option<String>)>> = parking_lot::Mutex::new(None);

/// Forget where yt-dlp was (or wasn't) found, e.g. after fetching it.
pub fn reset_ytdlp_cache() {
    *YTDLP_CACHED.lock() = None;
}

pub fn ytdlp_path() -> Option<String> {
    static CACHED: &parking_lot::Mutex<Option<(String, Option<String>)>> = &YTDLP_CACHED;
    let custom = crate::store::settings_snapshot().ytdlp_path.clone();
    if let Some((key, found)) = CACHED.lock().clone() {
        if key == custom {
            return found;
        }
    }
    let found = find_ytdlp(&custom);
    *CACHED.lock() = Some((custom, found.clone()));
    found
}

fn find_ytdlp(custom: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    if !custom.is_empty() {
        candidates.push(custom.to_string());
    }
    // bundled next to the app (installer) or kept in the data folder
    let exe = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        let p = d.join(exe);
        if p.is_file() {
            candidates.push(p.to_string_lossy().into_owned());
        }
    }
    let p = crate::store::data_dir().join(exe);
    if p.is_file() {
        candidates.push(p.to_string_lossy().into_owned());
    }
    candidates.push("yt-dlp".into());
    for c in candidates {
        let mut cmd = Command::new(&c);
        cmd.arg("--version");
        hide_window(&mut cmd);
        if cmd.output().map(|o| o.status.success()).unwrap_or(false) {
            return Some(c);
        }
    }
    None
}

pub fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd;
}

pub fn ytdlp_streams(video_id: &str) -> Result<StreamInfo> {
    let bin = ytdlp_path().ok_or_else(|| anyhow!("yt-dlp not found (install it or set its path in Settings)"))?;
    let mut cmd = Command::new(bin);
    cmd.args(["-J", "--no-warnings", "--no-playlist"]);
    if let Some(p) = crate::store::proxy_setting() {
        cmd.arg("--proxy").arg(p);
    }
    if crate::store::settings_snapshot().ipv4_only {
        cmd.arg("--force-ipv4");
    }
    cmd.arg(format!("https://www.youtube.com/watch?v={video_id}"));
    hide_window(&mut cmd);
    let mut out = cmd.output()?;
    if !out.status.success() && crate::downloads::retry_with_ios(&String::from_utf8_lossy(&out.stderr)) {
        // videos made for kids: yt-dlp's default clients refuse them, its iOS client doesn't
        let mut retry = Command::new(ytdlp_path().unwrap_or_default());
        retry.args(cmd.get_args()).arg("--extractor-args").arg(crate::downloads::YTDLP_IOS);
        hide_window(&mut retry);
        if let Ok(o) = retry.output() {
            if o.status.success() {
                out = o;
            }
        }
    }
    if !out.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("yt-dlp failed")));
    }
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let mut info = StreamInfo {
        id: video_id.to_string(),
        title: s(&v, "/title").unwrap_or_default(),
        author: s(&v, "/channel").or_else(|| s(&v, "/uploader")).unwrap_or_default(),
        channel_id: s(&v, "/channel_id").unwrap_or_default(),
        length_secs: v.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as u64,
        view_count: v.get("view_count").and_then(Value::as_u64),
        description: s(&v, "/description").unwrap_or_default(),
        is_live: v.get("is_live").and_then(Value::as_bool).unwrap_or(false),
        source: "yt-dlp",
        ..Default::default()
    };
    for f in v.get("formats").and_then(Value::as_array).into_iter().flatten() {
        let Some(url) = s(f, "/url") else { continue };
        let proto = s(f, "/protocol").unwrap_or_default();
        let vcodec = s(f, "/vcodec").unwrap_or_else(|| "none".into());
        let acodec = s(f, "/acodec").unwrap_or_else(|| "none".into());
        if proto.contains("m3u8") {
            if info.hls_url.is_none() {
                info.hls_url = s(f, "/manifest_url").or(Some(url.clone()));
            }
            continue;
        }
        if proto != "https" && proto != "http" {
            continue;
        }
        let height = f.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
        let sf = StreamFormat {
            itag: s(f, "/format_id").and_then(|x| x.split('-').next().and_then(|n| n.parse().ok())).unwrap_or(0),
            url,
            mime: format!("{}/{}", if vcodec == "none" { "audio" } else { "video" }, s(f, "/ext").unwrap_or_default()),
            codecs: if vcodec == "none" { acodec.clone() } else { vcodec.clone() },
            bitrate: (f.get("tbr").and_then(Value::as_f64).unwrap_or(0.0) * 1000.0) as u64,
            width: f.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
            height,
            fps: f.get("fps").and_then(Value::as_f64).unwrap_or(0.0) as u32,
            quality_label: if height > 0 { format!("{height}p") } else { String::new() },
            audio_language: s(f, "/language"),
            audio_default: f.get("language_preference").and_then(Value::as_i64).unwrap_or(0) >= 0,
            content_length: f.get("filesize").and_then(Value::as_u64),
            is_audio: vcodec == "none",
            is_muxed: vcodec != "none" && acodec != "none",
        };
        if sf.is_muxed {
            info.muxed.push(sf);
        } else if sf.is_audio {
            info.audio.push(sf);
        } else {
            info.video.push(sf);
        }
    }
    for (lang, tracks, auto) in [("subtitles", &v["subtitles"], false), ("automatic_captions", &v["automatic_captions"], true)] {
        let _ = lang;
        if let Some(m) = tracks.as_object() {
            for (code, list) in m {
                if auto && !code.ends_with("-orig") && info.captions.iter().any(|c| c.auto) {
                    continue; // skip the huge list of auto-translations
                }
                if let Some(vtt) = list.as_array().and_then(|a| a.iter().find(|x| s(x, "/ext").as_deref() == Some("vtt"))) {
                    info.captions.push(CaptionTrack {
                        lang: code.clone(),
                        name: s(vtt, "/name").unwrap_or(code.clone()),
                        url: s(vtt, "/url").unwrap_or_default(),
                        auto,
                    });
                }
            }
        }
    }
    sort_formats(&mut info);
    Ok(info)
}

/// Selects the best video + audio pair for a max height and codec preference.
/// YouTube-style quality class ("1080p") for a format, independent of aspect ratio:
/// 1920x960 (2:1) and 1080x1920 (vertical) are both 1080p.
pub fn quality_class(width: u32, height: u32) -> u32 {
    const LADDER: [u32; 9] = [144, 240, 360, 480, 720, 1080, 1440, 2160, 4320];
    let (short, long) = (width.min(height), width.max(height));
    let eq = if width == 0 { height } else { short.max(long * 9 / 16) };
    LADDER.iter().copied().min_by_key(|l| (*l as i64 - eq as i64).abs()).unwrap_or(eq)
}

pub fn fmt_class(f: &StreamFormat) -> u32 {
    quality_class(f.width, f.height)
}

pub fn pick_formats<'a>(
    info: &'a StreamInfo,
    max_height: u32,
    codec_pref: &str,
    audio_lang: Option<&str>,
) -> (Option<&'a StreamFormat>, Option<&'a StreamFormat>) {
    let codec_rank = |f: &StreamFormat| -> u32 {
        let c = f.codecs.to_lowercase();
        let kind = if c.starts_with("av01") { "av1" } else if c.starts_with("vp9") || c.starts_with("vp09") { "vp9" } else { "h264" };
        match (codec_pref, kind) {
            ("av1", "av1") | ("vp9", "vp9") | ("h264", "h264") => 3,
            (_, "vp9") => 2,
            (_, "h264") => 1,
            _ => 0,
        }
    };
    let st = crate::store::settings_snapshot();
    // "Enhanced bitrate": the highest-bitrate stream at the chosen resolution, whatever its codec
    let enhanced = st.enhanced_bitrate;
    let video = info
        .video
        .iter()
        .filter(|f| max_height == 0 || fmt_class(f) <= max_height)
        .max_by_key(|f| if enhanced { (fmt_class(f), f.fps, f.bitrate as u32 / 1000, codec_rank(f)) } else { (fmt_class(f), f.fps, codec_rank(f), f.bitrate as u32 / 1000) })
        .or_else(|| info.video.iter().min_by_key(|f| fmt_class(f)));
    let audio = {
        let mut cands: Vec<&StreamFormat> = info.audio.iter().collect();
        if let Some(lang) = audio_lang {
            let l: Vec<&StreamFormat> = cands.iter().copied().filter(|a| a.audio_language.as_deref() == Some(lang)).collect();
            if !l.is_empty() {
                cands = l;
            }
        } else if !st.original_audio
            && cands.iter().any(|a| a.audio_language.as_deref().map(|l| l.starts_with(&st.language)).unwrap_or(false))
        {
            // dubbed track in the interface language when the original isn't forced
            cands.retain(|a| a.audio_language.as_deref().map(|l| l.starts_with(&st.language)).unwrap_or(false));
        } else {
            let d: Vec<&StreamFormat> = cands.iter().copied().filter(|a| a.audio_default).collect();
            if !d.is_empty() {
                cands = d;
            }
        }
        // prefer opus, then highest bitrate
        cands.into_iter().max_by_key(|a| (a.codecs.contains("opus") as u8, a.bitrate))
    };
    (video, audio)
}

/// Builds a URL mpv can play: EDL combining separate video & audio streams.
pub fn mpv_source(video: Option<&StreamFormat>, audio: Option<&StreamFormat>, duration: u64) -> Option<String> {
    let _ = duration;
    let edl_part = |u: &str| format!("%{}%{}", u.len(), u);
    let local = |f: &StreamFormat| crate::player::proxy::local_url(&f.url, f.content_length, &f.mime);
    match (video, audio) {
        (Some(v), Some(a)) => Some(format!(
            "edl://!no_clip;!no_chapters;{};!new_stream;!no_clip;!no_chapters;{}",
            edl_part(&local(v)),
            edl_part(&local(a))
        )),
        (Some(v), None) => Some(local(v)),
        (None, Some(a)) => Some(local(a)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore]
    fn live_streams() {
        let id = std::env::var("VID").unwrap_or_else(|_| "dQw4w9WgXcQ".into());
        if let Err(e) = android_vr(&id) {
            println!("android_vr error: {e}");
        }
        let s = get_streams(&id).unwrap();
        println!("heights: {:?}", s.video.iter().map(|v| v.height).collect::<Vec<_>>());
        println!("{} via {} v={} a={} subs={} sb={:?}", s.title, s.source, s.video.len(), s.audio.len(), s.captions.len(), s.storyboard);
        let (v, a) = pick_formats(&s, 1080, "vp9", None);
        println!("{:?} {:?}", v.map(|v| (&v.quality_label, &v.codecs)), a.map(|a| &a.codecs));
        // HLS sources carry their audio in the manifest
        assert!(v.is_some() && (a.is_some() || s.hls_only));
    }
}

#[cfg(test)]
mod sb_tests {
    use super::*;
    #[test]
    #[ignore]
    fn live_storyboard() {
        let s = get_streams("dQw4w9WgXcQ").unwrap();
        let sb = s.storyboard.expect("no storyboard");
        let (url, c, r) = sb.frame_at(60.0, s.length_secs as f64).unwrap();
        println!("{url} {c} {r}");
        let (st, b) = match crate::api::http::get_bytes(&url, &[]) { Ok(b) => (200, b.len()), Err(e) => { println!("{e}"); (0, 0) } };
        println!("status {st} bytes {b}");
        assert!(b > 1000);
    }
}

#[cfg(test)]
mod ua_tests {
    use super::*;
    #[test]
    #[ignore]
    fn live_audio_formats() {
        let id = std::env::var("VID").unwrap_or_else(|_| "q0aFOxT6TNw".into());
        let s = android_vr(&id).unwrap();
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        for (a, rg) in s.audio.iter().flat_map(|a| ["bytes=0-1000", "bytes=0-524287", "bytes=0-1048575", "bytes=1048576-2097151"].into_iter().map(move |r| (a, r))) {
            let r = agent.get(&a.url).header("User-Agent", http::BROWSER_UA).header("Range", rg).call().unwrap();
            print!("{rg} ");
            let itag = a.url.split("itag=").nth(1).and_then(|x| x.split('&').next()).unwrap_or("?");
            println!("itag {itag} {} {} br={} default={} lang={:?} drc={} -> {}", a.mime, a.codecs, a.bitrate, a.audio_default, a.audio_language, a.url.contains("drc"), r.status());
        }
    }

    #[test]
    #[ignore]
    fn live_parallel() {
        let id = std::env::var("VID").unwrap_or_else(|_| "q0aFOxT6TNw".into());
        let s = android_vr(&id).unwrap();
        let (v, a) = pick_formats(&s, 1080, "vp9", None);
        let urls = vec![v.unwrap().url.clone(), a.unwrap().url.clone(), v.unwrap().url.clone()];
        let hs: Vec<_> = urls.into_iter().enumerate().map(|(k, u)| std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
            for i in 0..3u64 {
                let r = agent.get(&u).header("User-Agent", http::BROWSER_UA).header("Range", &format!("bytes={}-{}", i * 1048576, i * 1048576 + 1048575)).call().unwrap();
                let st = r.status();
                let _ = r.into_body().with_config().limit(u64::MAX).read_to_vec();
                println!("stream{k} #{i}: {st}");
            }
        })).collect();
        for h in hs { h.join().unwrap(); }
    }

    #[test]
    #[ignore]
    fn live_url_access() {
        let id = std::env::var("VID").unwrap_or_else(|_| "q0aFOxT6TNw".into());
        let s = android_vr(&id).unwrap();
        let f = &s.video[0];
        std::fs::write(std::env::temp_dir().join("ft_url.txt"), &f.url).ok();
        for (name, ua) in [("browser", http::BROWSER_UA), ("vr", VR_UA)] {
            let s = android_vr(&id).unwrap();
            let f = &s.video[0];
            let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
            for i in 0..4u64 {
                let r = agent.get(&f.url).header("User-Agent", ua).header("Range", &format!("bytes={}-{}", i * 1048576, i * 1048576 + 1048575)).call().unwrap();
                let st = r.status();
                let _ = r.into_body().read_to_vec();
                println!("{name} #{i}: {st}");
            }
        }
    }
}


#[cfg(test)]
mod kids_tests {
    /// A video made for kids (YouTube Kids) resolves and its HLS segments download.
    #[test]
    #[ignore]
    fn live_made_for_kids() {
        let info = super::resolve("e_04ZrNroTo").unwrap();
        println!("source={} variants={} hls={}", info.source, info.video.len(), info.hls_url.is_some());
        assert!(info.hls_only && !info.video.is_empty());
        let best = info.video.iter().max_by_key(|f| f.height).unwrap();
        assert!(best.height >= 720, "{}", best.height);
        let pl = super::http::get_text(&best.url).unwrap();
        let seg = pl.lines().find(|l| !l.is_empty() && !l.starts_with('#')).unwrap();
        let bytes = super::http::get_bytes(seg, &[]).unwrap();
        println!("first segment {} bytes", bytes.len());
        assert!(bytes.len() > 100_000);
        // a regular video still goes through visionOS
        let regular = super::resolve("dQw4w9WgXcQ").unwrap();
        assert_eq!(regular.source, "visionos");
    }
}
