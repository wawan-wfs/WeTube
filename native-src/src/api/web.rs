//! Browser sign-in: the YouTube website's own session cookies.
//!
//! The TV sign-in can read the account (feeds, history, likes), but YouTube no
//! longer lets it play videos, so nothing watched through it reaches the account.
//! With the website's cookies the app reports playback the way youtube.com does:
//! the video lands in the account's watch history and the position you stopped at
//! is what other devices resume from.
//!
//! Signing in happens in a real browser (Edge, Chrome, Brave or Chromium) started
//! with its own empty profile. The app reads that profile's youtube.com cookies over
//! the browser's local debugging port once sign-in is done, then closes the browser
//! and deletes the profile. Nothing else from the user's browsers is read.
use super::http;
use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

const WEB_VERSION: &str = "2.20251120.01.00";
const ORIGIN: &str = "https://www.youtube.com";
const SIGN_IN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&uilel=3&passive=true&continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26app%3Ddesktop%26next%3Dhttps%253A%252F%252Fwww.youtube.com%252F";

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Session {
    cookies: BTreeMap<String, String>,
    account: Option<String>,
}

fn session() -> &'static Mutex<Option<Session>> {
    static S: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    S.get_or_init(|| {
        let s: Option<Session> = crate::store::kv_get("web_session").and_then(|s| serde_json::from_str(&s).ok());
        Mutex::new(s.filter(|s| has_login(&s.cookies)))
    })
}

fn save(s: &Option<Session>) {
    crate::store::kv_set("web_session", &s.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()).unwrap_or_default());
}

fn has_login(c: &BTreeMap<String, String>) -> bool {
    (c.contains_key("SAPISID") || c.contains_key("__Secure-3PAPISID")) && (c.contains_key("SID") || c.contains_key("__Secure-3PSID"))
}

/// Whether the browser sign-in is connected.
pub fn connected() -> bool {
    return false;

    session().lock().is_some()
}

/// The account name the browser session belongs to.
pub fn account_name() -> Option<String> {
    session().lock().as_ref().and_then(|s| s.account.clone())
}

pub fn disconnect() {
    let mut s = session().lock();
    *s = None;
    save(&s);
}

/// Stores a set of youtube.com cookies and checks they belong to a signed-in account.
fn connect(cookies: BTreeMap<String, String>) -> Result<String> {
    if !has_login(&cookies) {
        return Err(anyhow!("These cookies aren't from a signed-in YouTube session."));
    }
    {
        let mut s = session().lock();
        *s = Some(Session { cookies, account: None });
    }
    let name = match fetch_account_name() {
        Ok(n) => n,
        Err(e) => {
            *session().lock() = None;
            return Err(e.context("YouTube didn't accept the session"));
        }
    };
    let mut s = session().lock();
    if let Some(st) = s.as_mut() {
        st.account = Some(name.clone());
    }
    save(&s);
    Ok(name)
}

fn cookie_header() -> Option<String> {
    let s = session().lock();
    let s = s.as_ref()?;
    Some(s.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
}

fn sha1_hex(s: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The `Authorization` header youtube.com sends with its API calls.
fn sapisid_auth() -> Option<String> {
    let s = session().lock();
    let c = &s.as_ref()?.cookies;
    let ts = chrono::Utc::now().timestamp();
    let parts: Vec<String> = [("SAPISIDHASH", "SAPISID"), ("SAPISID1PHASH", "__Secure-1PAPISID"), ("SAPISID3PHASH", "__Secure-3PAPISID")]
        .iter()
        .filter_map(|(scheme, name)| {
            let v = c.get(*name).or_else(|| if *name == "SAPISID" { c.get("__Secure-3PAPISID") } else { None })?;
            Some(format!("{scheme} {ts}_{}", sha1_hex(&format!("{ts} {v} {ORIGIN}"))))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// A call to the YouTube website's API as the signed-in user.
pub fn web_call(endpoint: &str, mut body: Value) -> Result<Value> {
    let cookie = cookie_header().ok_or_else(|| anyhow!("browser sign-in not connected"))?;
    let auth = sapisid_auth().ok_or_else(|| anyhow!("browser sign-in not connected"))?;
    let st = crate::store::settings_snapshot();
    body["context"] = json!({ "client": { "clientName": "WEB", "clientVersion": WEB_VERSION, "hl": st.language, "gl": st.region } });
    let (status, v) = http::post_json_status(
        &format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"),
        &body,
        &[
            ("Cookie", &cookie),
            ("Authorization", &auth),
            ("Origin", ORIGIN),
            ("X-Origin", ORIGIN),
            ("Referer", "https://www.youtube.com/"),
            ("X-Goog-AuthUser", "0"),
            ("X-Youtube-Bootstrap-Logged-In", "true"),
            ("X-Youtube-Client-Name", "1"),
            ("X-Youtube-Client-Version", WEB_VERSION),
        ],
    )?;
    if status == 401 || status == 403 {
        return Err(anyhow!("YouTube refused the browser session (HTTP {status}). Connect it again."));
    }
    if status >= 400 {
        return Err(anyhow!("HTTP {status}"));
    }
    Ok(v)
}

fn fetch_account_name() -> Result<String> {
    let r = web_call("account/account_menu", json!({}))?;
    let header = crate::api::json::find(&r, "activeAccountHeaderRenderer").ok_or_else(|| anyhow!("not signed in"))?;
    let name = header.get("accountName").map(crate::api::json::text).filter(|s| !s.is_empty());
    let handle = header.get("channelHandle").map(crate::api::json::text).filter(|s| !s.is_empty());
    name.or(handle).ok_or_else(|| anyhow!("not signed in"))
}

/// Regular videos of a page, without the Shorts shelf (its items carry no channel or length).
fn long_videos(p: crate::api::types::Page) -> crate::api::types::Page {
    use crate::api::types::Item;
    let mut seen = std::collections::HashSet::new();
    crate::api::types::Page {
        items: p.items.into_iter().filter(|i| matches!(i, Item::Video(v) if !v.is_short && !v.channel_name.is_empty() && seen.insert(v.id.clone()))).collect(),
        continuation: p.continuation,
    }
}

/// The account's own home feed, the same one youtube.com shows.
pub fn home() -> Result<crate::api::types::Page> {
    let r = web_call("browse", json!({ "browseId": "FEwhat_to_watch" }))?;
    Ok(long_videos(crate::api::innertube::collect_items(&r)))
}

/// Next page of a feed loaded with the browser session.
pub fn browse_continue(token: &str) -> Result<crate::api::types::Page> {
    let r = web_call("browse", json!({ "continuation": token }))?;
    Ok(long_videos(crate::api::innertube::collect_items(&r)))
}

// ───────────── sign-in through a browser ─────────────

/// Chromium-based browsers the sign-in can use, most likely first.
fn find_browser() -> Option<(String, PathBuf)> {
    #[cfg(windows)]
    {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidates = [
            ("Google Chrome", format!(r"{pf}\Google\Chrome\Application\chrome.exe")),
            ("Google Chrome", format!(r"{pf86}\Google\Chrome\Application\chrome.exe")),
            ("Google Chrome", format!(r"{local}\Google\Chrome\Application\chrome.exe")),
            ("Microsoft Edge", format!(r"{pf86}\Microsoft\Edge\Application\msedge.exe")),
            ("Microsoft Edge", format!(r"{pf}\Microsoft\Edge\Application\msedge.exe")),
            ("Brave", format!(r"{pf}\BraveSoftware\Brave-Browser\Application\brave.exe")),
            ("Brave", format!(r"{local}\BraveSoftware\Brave-Browser\Application\brave.exe")),
            ("Chromium", format!(r"{local}\Chromium\Application\chrome.exe")),
        ];
        candidates.into_iter().map(|(n, p)| (n.to_string(), PathBuf::from(p))).find(|(_, p)| p.is_file())
    }
    #[cfg(not(windows))]
    {
        let names = [
            ("Google Chrome", "google-chrome"),
            ("Google Chrome", "google-chrome-stable"),
            ("Chromium", "chromium"),
            ("Chromium", "chromium-browser"),
            ("Brave", "brave-browser"),
            ("Brave", "brave"),
            ("Microsoft Edge", "microsoft-edge"),
            ("Microsoft Edge", "microsoft-edge-stable"),
            ("Vivaldi", "vivaldi"),
        ];
        let path = std::env::var_os("PATH")?;
        for (n, bin) in names {
            for dir in std::env::split_paths(&path) {
                let p = dir.join(bin);
                if p.is_file() {
                    return Some((n.to_string(), p));
                }
            }
        }
        None
    }
}

/// Name of the browser the sign-in will open, if there is one.
pub fn browser_name() -> Option<String> {
    find_browser().map(|(n, _)| n)
}

fn local_agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).http_status_as_error(false).build().into()
}

/// Asks the browser for its cookies over the DevTools protocol.
fn cdp_cookies(port: u16) -> Result<Vec<Value>> {
    let mut resp = local_agent().get(&format!("http://127.0.0.1:{port}/json/version")).call()?;
    let v: Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
    let ws = v.get("webSocketDebuggerUrl").and_then(Value::as_str).ok_or_else(|| anyhow!("no debugger url"))?.to_string();
    let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let (mut sock, _) = tungstenite::client(ws.as_str(), stream).map_err(|e| anyhow!("devtools: {e}"))?;
    sock.send(tungstenite::Message::text(json!({ "id": 1, "method": "Storage.getCookies" }).to_string()))?;
    loop {
        let msg = sock.read()?;
        if let tungstenite::Message::Text(t) = msg {
            let v: Value = serde_json::from_str(t.as_str())?;
            if v.get("id").and_then(Value::as_i64) == Some(1) {
                let _ = sock.close(None);
                return Ok(v.pointer("/result/cookies").and_then(Value::as_array).cloned().unwrap_or_default());
            }
        }
    }
}

fn cdp_close(port: u16) {
    let Ok(mut resp) = local_agent().get(&format!("http://127.0.0.1:{port}/json/version")).call() else { return };
    let Ok(v) = serde_json::from_str::<Value>(&resp.body_mut().read_to_string().unwrap_or_default()) else { return };
    let Some(ws) = v.get("webSocketDebuggerUrl").and_then(Value::as_str) else { return };
    let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return };
    if let Ok((mut sock, _)) = tungstenite::client(ws, stream) {
        let _ = sock.send(tungstenite::Message::text(json!({ "id": 2, "method": "Browser.close" }).to_string()));
        let _ = sock.flush();
    }
}

fn youtube_cookies(list: &[Value]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for c in list {
        let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
        if !(domain == "youtube.com" || domain.ends_with(".youtube.com")) {
            continue;
        }
        if let (Some(n), Some(v)) = (c.get("name").and_then(Value::as_str), c.get("value").and_then(Value::as_str)) {
            out.insert(n.to_string(), v.to_string());
        }
    }
    out
}

/// Opens a browser window for the user to sign in to YouTube and waits until they
/// have. Returns the account name. `cancel` stops waiting and closes the browser.
pub fn browser_sign_in(cancel: Arc<AtomicBool>) -> Result<String> {
    return Err(anyhow::anyhow!("Browser session import disabled"));

    let (_, exe) = find_browser().ok_or_else(|| {
        anyhow!("No supported browser found. Install Google Chrome, Microsoft Edge, Brave or Chromium, or import a cookies.txt file instead.")
    })?;
    let profile = crate::store::data_dir().join("signin-browser");
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile)?;
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--remote-allow-origins=http://127.0.0.1")
        .args(["--no-first-run", "--no-default-browser-check", "--disable-sync", "--new-window"])
        .arg(SIGN_IN_URL);
    if let Some(p) = crate::store::proxy_setting() {
        cmd.arg(format!("--proxy-server={p}"));
    }
    let mut child = cmd.spawn().context("couldn't start the browser")?;
    let started = Instant::now();
    let result = loop {
        std::thread::sleep(Duration::from_millis(1500));
        if cancel.load(Ordering::Relaxed) {
            break Err(anyhow!("cancelled"));
        }
        if let Ok(Some(_)) = child.try_wait() {
            // the user closed the window; the browser may have handed off to an
            // already running copy, but with our own profile that doesn't happen
            break Err(anyhow!("The browser was closed before sign-in finished."));
        }
        if started.elapsed() > Duration::from_secs(20 * 60) {
            break Err(anyhow!("Sign-in timed out."));
        }
        let Ok(list) = cdp_cookies(port) else { continue };
        let cookies = youtube_cookies(&list);
        if has_login(&cookies) && cookies.contains_key("LOGIN_INFO") {
            break Ok(cookies);
        }
    };
    cdp_close(port);
    for _ in 0..20 {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..8 {
        if std::fs::remove_dir_all(&profile).is_ok() || !profile.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    connect(result?)
}

/// Reads a Netscape-format cookies.txt (as saved by "Get cookies.txt" style
/// browser extensions) and connects with its youtube.com cookies.
pub fn import_cookies_txt(path: &std::path::Path) -> Result<String> {
    return Err(anyhow::anyhow!("Cookie import disabled"));

    let text = std::fs::read_to_string(path)?;
    let mut cookies = BTreeMap::new();
    for line in text.lines() {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        let domain = f[0].trim_start_matches('.');
        if domain == "youtube.com" || domain.ends_with(".youtube.com") {
            cookies.insert(f[5].to_string(), f[6].trim_end_matches('\r').to_string());
        }
    }
    if cookies.is_empty() {
        return Err(anyhow!("No youtube.com cookies in that file."));
    }
    connect(cookies)
}

// ───────────── reporting what is watched ─────────────

fn sts() -> Option<u64> {
    static STS: Mutex<Option<(u64, Instant)>> = Mutex::new(None);
    if let Some((v, at)) = *STS.lock() {
        if at.elapsed() < Duration::from_secs(6 * 3600) {
            return Some(v);
        }
    }
    let api = String::from_utf8(http::get_bytes("https://www.youtube.com/iframe_api", &[]).ok()?).ok()?;
    let id: String = api
        .split("/s/player/")
        .nth(1)
        .or_else(|| api.split(r"player\/").nth(1))?
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    let js = String::from_utf8(http::get_bytes(&format!("https://www.youtube.com/s/player/{id}/player_ias.vflset/en_US/base.js"), &[]).ok()?).ok()?;
    let v: u64 = ["signatureTimestamp:", "sts:"]
        .iter()
        .find_map(|k| js.split(k).nth(1).and_then(|t| t.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()))?;
    *STS.lock() = Some((v, Instant::now()));
    Some(v)
}

struct Watch {
    id: String,
    playback: String,
    watchtime: String,
    cpn: String,
    len: f64,
    started: Instant,
    last_ping: Instant,
    last_cmt: f64,
    pings: u32,
    playing: bool,
}

impl Watch {
    fn begin(id: &str, len: f64) -> Result<Self> {
        let mut body = json!({ "videoId": id, "racyCheckOk": true, "contentCheckOk": true });
        if let Some(sts) = sts() {
            body["playbackContext"] = json!({ "contentPlaybackContext": { "signatureTimestamp": sts, "html5Preference": "HTML5_PREF_WANTS" } });
        }
        let r = web_call("player", body)?;
        let url = |k: &str| r.pointer(&format!("/playbackTracking/{k}/baseUrl")).and_then(Value::as_str).map(str::to_string);
        let playback = url("videostatsPlaybackUrl").ok_or_else(|| anyhow!("no playback tracking"))?;
        let watchtime = url("videostatsWatchtimeUrl").ok_or_else(|| anyhow!("no watchtime tracking"))?;
        let len = r
            .pointer("/videoDetails/lengthSeconds")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .filter(|l: &f64| *l > 0.0)
            .unwrap_or(len);
        let cpn: String = {
            use rand::Rng;
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut rng = rand::rng();
            (0..16).map(|_| A[rng.random_range(0..A.len())] as char).collect()
        };
        let w = Watch { id: id.to_string(), playback, watchtime, cpn, len, started: Instant::now(), last_ping: Instant::now(), last_cmt: 0.0, pings: 0, playing: true };
        w.get(&w.playback, &[("cmt", "0".into()), ("rtn", "0".into()), ("state", "playing".into())])?;
        Ok(w)
    }

    /// Sends a stats ping: the base URL from the player response with the fields
    /// youtube.com's own player adds. Fields already in the base URL are replaced,
    /// never repeated.
    fn get(&self, base: &str, fields: &[(&str, String)]) -> Result<()> {
        // the ping carries the sign-in cookies: only ever to YouTube itself
        if !is_youtube_https(base) {
            return Err(anyhow!("unexpected stats host"));
        }
        let rt = self.started.elapsed().as_secs_f64();
        let mut add: Vec<(&str, String)> = vec![
            ("ver", "2".into()),
            ("cpn", self.cpn.clone()),
            ("c", "WEB".into()),
            ("cver", WEB_VERSION.into()),
            ("cplayer", "UNIPLAYER".into()),
            ("cbr", "Chrome".into()),
            ("cbrver", "140.0.0.0".into()),
            ("cos", "Windows".into()),
            ("cosver", "10.0".into()),
            ("cplatform", "DESKTOP".into()),
            ("fs", "0".into()),
            ("volume", "100".into()),
            ("muted", "0".into()),
            ("lact", "1200".into()),
            ("len", format!("{:.3}", self.len)),
            ("rt", format!("{rt:.3}")),
        ];
        add.extend(fields.iter().cloned());
        let (path, query) = base.split_once('?').unwrap_or((base, ""));
        let mut pairs: Vec<(String, String)> = query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (k.to_string(), v.to_string())
            })
            .collect();
        for (k, v) in add {
            match pairs.iter_mut().find(|(pk, _)| pk == k) {
                Some(p) => p.1 = v,
                None => pairs.push((k.to_string(), v)),
            }
        }
        let url = format!("{path}?{}", pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&"));
        let cookie = cookie_header().unwrap_or_default();
        let referer = format!("https://www.youtube.com/watch?v={}", self.id);
        http::get_bytes(&url, &[("Cookie", &cookie), ("Referer", &referer), ("Origin", ORIGIN)])?;
        Ok(())
    }

    /// Reports the stretch watched since the last report, ending at `cmt`.
    fn report(&mut self, cmt: f64, playing: bool, last: bool) {
        let st = if cmt >= self.last_cmt && cmt - self.last_cmt < 120.0 { self.last_cmt } else { cmt };
        let rtn = self.started.elapsed().as_secs_f64();
        let mut fields = vec![
            ("cmt", format!("{cmt:.3}")),
            ("st", format!("{st:.3}")),
            ("et", format!("{cmt:.3}")),
            ("state", if playing { "playing" } else { "paused" }.to_string()),
            ("rtn", format!("{:.0}", rtn.max(1.0))),
            ("rti", format!("{:.0}", rtn.max(1.0))),
            ("idpj", "-1".into()),
            ("ldpj", "-1".into()),
        ];
        if last {
            fields.push(("final", "1".into()));
        }
        if let Err(e) = self.get(&self.watchtime, &fields) {
            if cfg!(test) {
                eprintln!("watchtime ping failed: {e}");
            }
        }
        self.last_cmt = cmt;
        self.last_ping = Instant::now();
        self.pings += 1;
        self.playing = playing;
    }

    /// youtube.com reports at 10, 20 and 30 seconds, then every 40 seconds.
    fn due(&self) -> bool {
        let every = if self.pings < 3 { 10 } else { 40 };
        self.last_ping.elapsed() >= Duration::from_secs(every)
    }
}

enum Event {
    Start { id: String, len: f64 },
    Progress { id: String, t: f64, playing: bool },
    Stop { id: String, t: f64 },
}

fn reporter() -> &'static Mutex<mpsc::Sender<Event>> {
    static TX: OnceLock<Mutex<mpsc::Sender<Event>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Event>();
        std::thread::Builder::new()
            .name("watch-report".into())
            .spawn(move || {
                let mut cur: Option<Watch> = None;
                while let Ok(ev) = rx.recv() {
                    match ev {
                        Event::Start { id, len } => {
                            if let Some(mut w) = cur.take() {
                                let t = w.last_cmt;
                                w.report(t, false, true);
                            }
                            cur = Watch::begin(&id, len).ok();
                        }
                        Event::Progress { id, t, playing } => {
                            if let Some(w) = cur.as_mut().filter(|w| w.id == id) {
                                if w.due() || playing != w.playing {
                                    w.report(t, playing, false);
                                }
                            }
                        }
                        Event::Stop { id, t } => {
                            if let Some(mut w) = cur.take() {
                                if w.id == id {
                                    w.report(t, false, true);
                                } else {
                                    cur = Some(w);
                                }
                            }
                        }
                    }
                }
            })
            .ok();
        Mutex::new(tx)
    })
}

/// A video started playing.
pub fn report_start(id: &str, len: f64) {
    if connected() && crate::store::settings_snapshot().history_enabled {
        let _ = reporter().lock().send(Event::Start { id: id.to_string(), len });
    }
}

/// Playback position update; cheap to call often.
pub fn report_progress(id: &str, t: f64, playing: bool) {
    if connected() {
        let _ = reporter().lock().send(Event::Progress { id: id.to_string(), t, playing });
    }
}

/// The video was closed or replaced at position `t`.
pub fn report_stop(id: &str, t: f64) {
    if connected() {
        let _ = reporter().lock().send(Event::Stop { id: id.to_string(), t });
    }
}

/// `https://youtube.com/...` or `https://<anything>.youtube.com/...`, nothing else.
fn is_youtube_https(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // no user info or port tricks ("youtube.com@evil.example")
    !host.contains(['@', ':']) && (host == "youtube.com" || host.ends_with(".youtube.com"))
}

#[cfg(test)]
mod host_tests {
    use super::is_youtube_https;

    #[::core::prelude::v1::test]
    fn only_youtube() {
        assert!(is_youtube_https("https://s.youtube.com/api/stats/playback?x=1"));
        assert!(is_youtube_https("https://www.youtube.com/api/stats/watchtime"));
        assert!(!is_youtube_https("http://s.youtube.com/api"));
        assert!(!is_youtube_https("https://youtube.com.evil.example/api"));
        assert!(!is_youtube_https("https://www.youtube.com@evil.example/api"));
        assert!(!is_youtube_https("https://evilyoutube.com/api"));
    }
}
