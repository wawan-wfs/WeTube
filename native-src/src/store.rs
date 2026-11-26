//! Persistent storage: settings (JSON) + SQLite for subscriptions, history, playlists, caches.
use crate::api::types::VideoItem;
use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Appearance
    pub theme: String,
    pub accent: u32,
    pub start_page: String,
    pub grid_columns: u32,
    // Region
    pub region: String,
    pub language: String,
    // Backend
    pub stream_backend: String,
    /// Linux: "auto", "x11" or "wayland" (see display.rs)
    pub display_server: String,
    pub ytdlp_path: String,
    pub proxy: String,
    // Player
    pub default_quality: u32,
    pub codec_pref: String,
    pub default_speed: f64,
    pub remember_speed_per_channel: bool,
    pub autoplay_next: bool,
    pub resume_playback: bool,
    pub volume: f64,
    pub hwdec: bool,
    pub audio_only: bool,
    pub captions_enabled: bool,
    pub captions_lang: String,
    pub loop_video: bool,
    pub seek_step: f64,
    // SponsorBlock / DeArrow / RYD
    pub sponsorblock: bool,
    pub sponsorblock_server: String,
    /// category -> "skip" | "ask" | "show" | "off"
    pub sb_actions: HashMap<String, String>,
    pub sb_min_duration: f64,
    pub sb_notify: bool,
    pub dearrow_titles: bool,
    pub dearrow_thumbnails: bool,
    pub ryd: bool,
    // Distraction-free / content filtering
    pub hide_shorts: bool,
    pub hide_live: bool,
    pub hide_upcoming: bool,
    pub hide_comments: bool,
    pub hide_related: bool,
    pub hide_watched_in_feed: bool,
    pub hide_view_counts: bool,
    pub hide_description: bool,
    pub hide_channel_avatars: bool,
    pub hide_chapters: bool,
    pub hide_explore: bool,
    pub blocked_keywords: Vec<String>,
    /// (channel_id, name)
    pub blocked_channels: Vec<(String, String)>,
    // Privacy
    pub wetube_restore_tabs: bool,
    pub wetube_recover_playback: bool,
    pub history_enabled: bool,
    pub search_history_enabled: bool,
    /// look for a new version on GitHub at startup
    pub check_updates: bool,
    // Subscriptions
    pub feed_source: String,
    pub feed_include_shorts: bool,
    pub feed_include_live: bool,
    // Downloads / external
    pub download_dir: String,
    /// The user picked (or accepted) the download folder; until then the first download asks.
    pub download_dir_chosen: bool,
    pub download_format: String,
    pub external_player: String,
    // appearance / player customization
    pub ui_scale: f32,
    pub density: String,
    pub thumb_radius: f32,
    pub caption_size: u32,
    pub caption_background: bool,
    /// "sans", "serif", "mono" or "casual"
    pub caption_font: String,
    /// 0xRRGGBB
    pub caption_color: u32,
    /// percent
    pub caption_opacity: u32,
    pub caption_bg_color: u32,
    pub caption_bg_opacity: u32,
    /// edge around the text when there's no background:
    /// "outline-shadow", "outline", "shadow" or "none"
    pub caption_edge: String,
    pub controls_hide_ms: u64,
    pub show_storyboard: bool,
    // SponsorBlock account (private ID, used for voting)
    pub sb_user_id: String,
    // Premium-style features
    pub show_heatmap: bool,
    pub jump_ahead: bool,
    pub enhanced_bitrate: bool,
    /// Connect to YouTube over IPv4 only. Many ISPs' IPv6 ranges get the
    /// "confirm you're not a bot" wall while IPv4 from the same line doesn't.
    pub ipv4_only: bool,
    pub background_play: bool,
    // Tweaks (from YouTube patch sets like Morphe/ReVanced)
    pub ambient_mode: bool,
    pub shorts_in_player: bool,
    pub shorts_autoadvance: bool,
    pub remember_quality: bool,
    pub exit_fullscreen_at_end: bool,
    /// "light" | "default" | "strong": gradient behind the player controls
    pub controls_shade: String,
    pub double_click_fullscreen: bool,
    pub original_audio: bool,
    /// play a muted preview when the pointer rests on a thumbnail
    pub hover_preview: bool,
    /// show archived annotations (notes and links YouTube removed in 2019)
    pub annotations: bool,
    /// keep page scrollbars on screen instead of showing them on hover
    pub always_scrollbars: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let mut sb = HashMap::new();
        for (c, _, _) in crate::api::services::SB_CATEGORIES {
            let a = match *c {
                "sponsor" | "selfpromo" | "interaction" => "skip",
                "poi_highlight" | "intro" | "outro" | "preview" | "filler" | "music_offtopic" | "hook" => "show",
                _ => "off",
            };
            sb.insert(c.to_string(), a.to_string());
        }
        Self {
            theme: "dark".into(),
            accent: 0xf0307e,
            start_page: "home".into(),
            grid_columns: 0,
            region: "US".into(),
            language: "en".into(),
            stream_backend: "auto".into(),
            display_server: "auto".into(),
            ytdlp_path: String::new(),
            proxy: String::new(),
            default_quality: 1080,
            codec_pref: "vp9".into(),
            default_speed: 1.0,
            remember_speed_per_channel: true,
            autoplay_next: true,
            resume_playback: true,
            volume: 80.0,
            hwdec: true,
            audio_only: false,
            captions_enabled: false,
            captions_lang: "en".into(),
            loop_video: false,
            seek_step: 5.0,
            sponsorblock: false,
            sponsorblock_server: "https://sponsor.ajay.app".into(),
            sb_actions: sb,
            sb_min_duration: 0.0,
            sb_notify: true,
            annotations: true,
            always_scrollbars: false,
            dearrow_titles: false,
            dearrow_thumbnails: false,
            ryd: false,
            hide_shorts: false,
            hide_live: false,
            hide_upcoming: false,
            hide_comments: false,
            hide_related: false,
            hide_watched_in_feed: false,
            hide_view_counts: false,
            hide_description: false,
            hide_channel_avatars: false,
            hide_chapters: false,
            hide_explore: false,
            blocked_keywords: Vec::new(),
            blocked_channels: Vec::new(),
            wetube_restore_tabs: false,
            wetube_recover_playback: false,
            history_enabled: false,
            search_history_enabled: false,
            check_updates: false,
            feed_source: "full".into(),
            feed_include_shorts: false,
            feed_include_live: true,
            download_dir: dirs::download_dir()
                .map(|d| d.join("WeTube Desktop").to_string_lossy().into_owned())
                .unwrap_or_default(),
            download_dir_chosen: false,
            download_format: "1080".into(),
            external_player: String::new(),
            ui_scale: 1.0,
            density: "default".into(),
            thumb_radius: 12.0,
            caption_size: 40,
            caption_background: true,
            caption_font: "sans".into(),
            caption_color: 0xFFFFFF,
            caption_opacity: 100,
            caption_bg_color: 0x000000,
            caption_bg_opacity: 75,
            caption_edge: "outline-shadow".into(),
            controls_hide_ms: 2500,
            show_storyboard: true,
            sb_user_id: String::new(),
            show_heatmap: true,
            jump_ahead: true,
            enhanced_bitrate: false,
            ipv4_only: true,
            background_play: true,
            ambient_mode: true,
            shorts_in_player: false,
            shorts_autoadvance: false,
            remember_quality: false,
            exit_fullscreen_at_end: true,
            controls_shade: "default".into(),
            double_click_fullscreen: true,
            original_audio: true,
            hover_preview: true,
        }
    }
}

fn settings_lock() -> &'static RwLock<Arc<Settings>> {
    static S: OnceLock<RwLock<Arc<Settings>>> = OnceLock::new();
    S.get_or_init(|| {
        let mut s = load_settings();
        normalize_settings(&mut s);
        RwLock::new(Arc::new(s))
    })
}

fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

/// Settings live in settings.json (written atomically) and are mirrored into the
/// database. The file is read first, so a damaged or replaced database never
/// takes the settings with it, and a field that no longer parses (a type changed
/// between versions) only resets that one field instead of all of them.
fn load_settings() -> Settings {
    let from_file = std::fs::read_to_string(settings_file())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v| v.is_object());
    let raw = from_file.or_else(|| {
        load_setting_json::<serde_json::Value>("settings").map(|v| match v {
            serde_json::Value::String(t) => serde_json::from_str(&t).unwrap_or(serde_json::Value::Null),
            v => v,
        })
    });
    let Some(raw) = raw else { return Settings::default() };
    let s = settings_from_value(raw);
    // the file is the copy that survives; write it the first time
    if !settings_file().exists() {
        let _ = write_settings_file(&s);
    }
    s
}

/// Builds settings from stored JSON, keeping every field that parses.
pub(crate) fn settings_from_value(raw: serde_json::Value) -> Settings {
    if let Ok(s) = serde_json::from_value::<Settings>(raw.clone()) {
        return s;
    }
    let serde_json::Value::Object(stored) = raw else { return Settings::default() };
    let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(Settings::default()) else {
        return Settings::default();
    };
    for (k, v) in stored {
        if !merged.contains_key(&k) {
            continue;
        }
        let prev = merged.insert(k.clone(), v);
        if serde_json::from_value::<Settings>(serde_json::Value::Object(merged.clone())).is_err() {
            // this field doesn't fit the current type: keep the default
            if let Some(p) = prev {
                merged.insert(k, p);
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(merged)).unwrap_or_default()
}

fn write_settings_file(s: &Settings) -> Result<()> {
    let path = settings_file();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn normalize_settings(s: &mut Settings) {
    const LADDER: [u32; 7] = [144, 240, 360, 480, 720, 1080, 1440];
    if s.default_quality != 2160 && s.default_quality != 4320 && !LADDER.contains(&s.default_quality) {
        s.default_quality = LADDER.iter().copied().filter(|l| *l <= s.default_quality.max(144)).max().unwrap_or(1080);
        if s.default_quality < 720 {
            s.default_quality = 1080;
        }
    }
    s.ui_scale = s.ui_scale.clamp(0.75, 1.5);
    if s.ui_scale == 0.0 {
        s.ui_scale = 1.0;
    }
}

pub fn settings_snapshot() -> Arc<Settings> {
    settings_lock().read().clone()
}

pub fn update_settings(f: impl FnOnce(&mut Settings)) {
    // one writer at a time, so two quick changes from different threads
    // can't both start from the same old copy and drop one of them
    static WRITE: Mutex<()> = Mutex::new(());
    let _w = WRITE.lock();
    let mut s = (*settings_snapshot()).clone();
    f(&mut s);
    let _ = write_settings_file(&s);
    let _ = save_setting_json("settings", &s);
    *settings_lock().write() = Arc::new(s);
}

pub fn ip_family() -> ureq::config::IpFamily {
    if settings_snapshot().ipv4_only { ureq::config::IpFamily::Ipv4Only } else { ureq::config::IpFamily::Any }
}

/// Whether mpv can use this proxy itself (it only speaks HTTP proxies).
pub fn mpv_http_proxy(p: &str) -> bool {
    let p = p.trim().to_ascii_lowercase();
    p.starts_with("http://") || p.starts_with("https://")
}

pub fn proxy_setting() -> Option<String> {
    let p = settings_snapshot().proxy.clone();
    (!p.trim().is_empty()).then_some(p)
}

pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // a separate profile (portable use, clean test runs)
        if let Some(d) = std::env::var_os("WETUBE_NATIVE_DATA_DIR").filter(|d| !d.is_empty()) {
            let d = PathBuf::from(d);
            let _ = std::fs::create_dir_all(&d);
            return d;
        }
        // tests never touch the real profile
        #[cfg(test)]
        {
            let d = std::env::temp_dir().join(format!("streamtube-test-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&d);
            return d;
        }
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        let d = base.join("WeTubeDesktopNative");
        // carry over data from the app's previous name
        // WeTube never relocates another application profile.
        let _ = std::fs::create_dir_all(&d);
        // A -wal/-shm left without its database belongs to some other copy of it;
        // SQLite would replay it onto whatever database is put there next and
        // damage it, so drop it before anything opens the database.
        if !d.join("streamtube.db").exists() {
            let _ = std::fs::remove_file(d.join("streamtube.db-wal"));
            let _ = std::fs::remove_file(d.join("streamtube.db-shm"));
        }
        if !d.join("streamtube.db").exists() && d.join("ferrotube.db").exists() {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::rename(d.join(format!("ferrotube.db{ext}")), d.join(format!("streamtube.db{ext}")));
            }
        }
        d
    })
    .clone()
}

const LIBRARY_INDEXES: &str = "CREATE INDEX IF NOT EXISTS wetube_history_recent ON history(watched_at DESC, video_id);
    CREATE INDEX IF NOT EXISTS wetube_history_cursor ON history(watched_at DESC, video_id DESC);
    CREATE INDEX IF NOT EXISTS wetube_playlist_order ON playlist_items(playlist_id, position, video_id);
    CREATE INDEX IF NOT EXISTS wetube_saved_recent ON saved_playlists(added_at DESC, playlist_id);
    CREATE INDEX IF NOT EXISTS wetube_search_recent ON search_history(at DESC, query);";

const TABLES: &str = "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS subscriptions (channel_id TEXT PRIMARY KEY, name TEXT NOT NULL, avatar TEXT, added_at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, color INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS profile_channels (profile_id INTEGER NOT NULL, channel_id TEXT NOT NULL, PRIMARY KEY(profile_id, channel_id));
     CREATE TABLE IF NOT EXISTS history (video_id TEXT PRIMARY KEY, json TEXT NOT NULL, watched_at INTEGER NOT NULL, progress REAL NOT NULL DEFAULT 0, duration REAL NOT NULL DEFAULT 0);
     CREATE TABLE IF NOT EXISTS playlists (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, created_at INTEGER NOT NULL, protected INTEGER NOT NULL DEFAULT 0, yt_id TEXT);
     CREATE TABLE IF NOT EXISTS playlist_items (playlist_id INTEGER NOT NULL, video_id TEXT NOT NULL, json TEXT NOT NULL, added_at INTEGER NOT NULL, position INTEGER NOT NULL, PRIMARY KEY(playlist_id, video_id));
     CREATE TABLE IF NOT EXISTS search_history (query TEXT PRIMARY KEY, at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS feed_cache (channel_id TEXT PRIMARY KEY, json TEXT NOT NULL, fetched_at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS channel_speed (channel_id TEXT PRIMARY KEY, speed REAL NOT NULL);
     CREATE TABLE IF NOT EXISTS saved_playlists (playlist_id TEXT PRIMARY KEY, json TEXT NOT NULL, added_at INTEGER NOT NULL);
     CREATE TABLE IF NOT EXISTS wetube_download_jobs (job_id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, json TEXT NOT NULL);
     CREATE INDEX IF NOT EXISTS wetube_download_order ON wetube_download_jobs(ordinal,job_id);";

/// Opens the database and checks it. A damaged one (its pages no longer agree
/// with each other, so lookups can return another row's data) is rebuilt from
/// every row that can still be read, and the damaged file is kept beside it.
fn open_checked(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).expect("open db");
    let ok = conn
        .query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
        .map(|r| r == "ok")
        .unwrap_or(false);
    if ok {
        return conn;
    }
    eprintln!("streamtube: the database failed its check, rebuilding it");
    let fresh = path.with_extension("db.rebuild");
    let _ = std::fs::remove_file(&fresh);
    if rebuild_into(&conn, &fresh).is_err() {
        let _ = std::fs::remove_file(&fresh);
        return conn;
    }
    drop(conn);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let damaged = path.with_extension(format!("db.damaged-{stamp}"));
    if std::fs::rename(path, &damaged).is_err() {
        let _ = std::fs::remove_file(&fresh);
        return Connection::open(path).expect("open db");
    }
    for ext in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", path.display()));
        let _ = std::fs::rename(&from, PathBuf::from(format!("{}{ext}", damaged.display())));
    }
    let _ = std::fs::rename(&fresh, path);
    Connection::open(path).expect("open db")
}

fn rebuild_into(old: &Connection, fresh: &std::path::Path) -> Result<()> {
    let new = Connection::open(fresh)?;
    new.execute_batch(TABLES)?;
    let tables: Vec<String> = {
        let mut st = new.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?;
        st.query_map([], |r| r.get(0))?.filter_map(Result::ok).collect()
    };
    new.execute_batch("BEGIN")?;
    for t in tables {
        let Ok(st) = old.prepare(&format!("SELECT * FROM \"{t}\" LIMIT 0")) else { continue };
        let n = st.column_count();
        let cols: Vec<String> = st.column_names().map(|c| format!("\"{c}\"")).collect();
        drop(st);
        let marks = vec!["?"; n].join(",");
        let insert = format!("INSERT OR IGNORE INTO \"{t}\"({}) VALUES ({marks})", cols.join(","));
        // Walk the table itself in rowid order (not an index: a damaged index points
        // keys at the wrong rows). When a page can't be read, start again a little
        // past the last good row, so one bad page doesn't lose everything after it.
        let select = format!("SELECT rowid, * FROM \"{t}\" NOT INDEXED WHERE rowid > ?1 ORDER BY rowid");
        let mut after = i64::MIN;
        let mut skip = 1i64;
        let mut tries = 0;
        while tries < 64 {
            let Ok(mut st) = old.prepare(&select) else { break };
            let Ok(mut rows) = st.query([after]) else { break };
            let mut progressed = false;
            let finished = loop {
                match rows.next() {
                    Ok(Some(r)) => {
                        let Ok(id) = r.get::<_, i64>(0) else { continue };
                        after = id;
                        progressed = true;
                        let vals: Vec<rusqlite::types::Value> =
                            (1..=n).map(|i| r.get(i).unwrap_or(rusqlite::types::Value::Null)).collect();
                        let _ = new.execute(&insert, rusqlite::params_from_iter(vals));
                    }
                    Ok(None) => break true,
                    Err(_) => break false,
                }
            };
            if finished {
                break;
            }
            // jump over the unreadable part, further each time it fails again
            skip = if progressed { 1 } else { skip.saturating_mul(4) };
            after = after.saturating_add(skip);
            tries += 1;
        }
    }
    new.execute_batch("COMMIT")?;
    Ok(())
}

fn db() -> &'static Mutex<Connection> {
    static DB: OnceLock<Mutex<Connection>> = OnceLock::new();
    DB.get_or_init(|| {
        let conn = open_checked(&data_dir().join("streamtube.db"));
        conn.execute_batch(&format!("PRAGMA journal_mode=WAL; {TABLES}"))
        .expect("init db");
        conn.busy_timeout(std::time::Duration::from_secs(3)).expect("db busy timeout");
        conn.pragma_update(None,"synchronous","FULL").expect("durable SQLite commits");
        if let Err(e) = conn.execute_batch(LIBRARY_INDEXES) {
            eprintln!("WeTube: could not create library indexes: {e}");
        }
        // built-in playlists
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM playlists WHERE protected=1", [], |r| r.get(0))
            .unwrap_or(0);
        if n == 0 {
            let now = crate::api::json::now();
            let _ = conn.execute("INSERT INTO playlists(name, created_at, protected) VALUES ('Watch later', ?1, 1)", [now]);
            let _ = conn.execute("INSERT INTO playlists(name, created_at, protected) VALUES ('Favorites', ?1, 1)", [now + 1]);
        }
        Mutex::new(conn)
    })
}

pub fn init() {
    let _ = db();
    let _ = settings_snapshot();
}

fn load_setting_json<T: for<'de> Deserialize<'de>>(key: &str) -> Option<T> {
    let conn = db().lock();
    let v: Option<String> = conn
        .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    v.and_then(|s| serde_json::from_str(&s).ok())
}

fn save_setting_json<T: Serialize>(key: &str, v: &T) -> Result<()> {
    db().lock().execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, serde_json::to_string(v)?],
    )?;
    Ok(())
}

pub(crate) fn wetube_import_legacy(v: &serde_json::Value) -> anyhow::Result<crate::import::ImportReport> {
    let mut conn = db().lock();
    crate::wetube_migration::import(&mut conn, v)
}

pub fn wetube_imported_queue() -> Option<i64> {
    let id = load_setting_json::<i64>("wetube_migration:queue")?;
    playlists().into_iter().find(|p| p.id == id && !p.protected).map(|p| p.id)
}

pub fn wetube_unresolved_channels() -> Vec<(String, String)> {
    load_setting_json("wetube_unresolved_channels_v1").unwrap_or_default()
}

pub fn kv_get(key: &str) -> Option<String> {
    load_setting_json(key)
}

pub fn wetube_kv_set(key: &str, v: &str) -> bool { save_setting_json(key, &v).is_ok() }

/// Checked local state read. Existing legacy KV format remains unchanged.
const JOB_JOURNAL_MARKER:&str="wetube_download_journal_v2";
const JOB_ROW_LIMIT:usize=64*1024;
const JOB_COUNT_LIMIT:usize=50_000;
/// None means legacy migration has not committed. Empty Some means an empty
/// migrated journal, never a reason to resurrect the legacy snapshot.
pub fn download_rows()->Result<Option<Vec<(String,String)>>> {
    let c=db().lock();
    let marked:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)",[JOB_JOURNAL_MARKER],|r|r.get(0))?;
    if !marked {
        let count:i64=c.query_row("SELECT COUNT(*) FROM wetube_download_jobs",[],|r|r.get(0))?;
        anyhow::ensure!(count==0,"Unmarked download journal is not empty");
        return Ok(None);
    }
    let count:i64=c.query_row("SELECT COUNT(*) FROM wetube_download_jobs",[],|r|r.get(0))?;
    anyhow::ensure!((0..=JOB_COUNT_LIMIT as i64).contains(&count),"Download journal exceeds row limit");
    let mut stmt=c.prepare("SELECT job_id, CASE WHEN length(CAST(json AS BLOB))<=?1 THEN json ELSE NULL END FROM wetube_download_jobs ORDER BY ordinal,job_id")?;
    let mut rows=stmt.query([JOB_ROW_LIMIT as i64])?;
    let mut result=Vec::with_capacity(count as usize);
    while let Some(row)=rows.next()? {
        let id:String=row.get(0)?;
        let payload:Option<String>=row.get(1)?;
        result.push((id,payload.ok_or_else(||anyhow::anyhow!("Oversized download row"))?));
    }
    Ok(Some(result))
}
/// Atomic snapshot reconciliation, updating only changed rows; retain the
/// original legacy KV as a migration backup, not as a second source of truth.
pub fn write_download_rows(rows:&[(String,String)])->Result<()> {
    write_download_rows_to(&mut db().lock(),rows)
}
/// Worker state transitions only touch the existing row. A missing row is an
/// error: a late worker must not resurrect a deleted or restarted job.
pub fn update_download_row(id:&str,payload:&str)->Result<()> {
    update_download_row_in(&db().lock(),id,payload)
}
fn update_download_row_in(c:&Connection,id:&str,payload:&str)->Result<()> {
    anyhow::ensure!(payload.len()<=JOB_ROW_LIMIT,"Oversized download row");
    let changed=c.execute("UPDATE wetube_download_jobs SET json=?2 WHERE job_id=?1",params![id,payload])?;
    anyhow::ensure!(changed==1,"Download job not found in journal");
    Ok(())
}
fn write_download_rows_to(c:&mut Connection,rows:&[(String,String)])->Result<()> {
    anyhow::ensure!(rows.len()<=JOB_COUNT_LIMIT,"Download journal is full");
    let tx=c.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS wetube_seen_jobs(job_id TEXT PRIMARY KEY); DELETE FROM wetube_seen_jobs;")?;
    {
        let mut seen=tx.prepare_cached("INSERT INTO wetube_seen_jobs(job_id) VALUES(?1)")?;
        let mut up=tx.prepare_cached("INSERT INTO wetube_download_jobs(job_id,ordinal,json) VALUES(?1,?2,?3)
            ON CONFLICT(job_id) DO UPDATE SET ordinal=excluded.ordinal,json=excluded.json
            WHERE wetube_download_jobs.ordinal<>excluded.ordinal OR wetube_download_jobs.json<>excluded.json")?;
        for (i,(id,payload)) in rows.iter().enumerate() {
            anyhow::ensure!(id.len()==32&&id.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))&&payload.len()<=JOB_ROW_LIMIT,"Invalid download journal row");
            seen.execute([id])?;
            up.execute(params![id,i as i64,payload])?;
        }
    }
    tx.execute("DELETE FROM wetube_download_jobs WHERE job_id NOT IN (SELECT job_id FROM wetube_seen_jobs)",[])?;
    tx.execute("INSERT OR IGNORE INTO kv(key,value) VALUES(?1,'2')",[JOB_JOURNAL_MARKER])?;
    tx.commit()?;
    Ok(())
}
#[cfg(test)]
mod download_row_tests {
    use super::*;
    #[test]
    fn migration_marker_and_rollback(){
        let mut c=Connection::open_in_memory().unwrap();
        c.execute_batch(TABLES).unwrap();
        let a="a".repeat(32);let b="b".repeat(32);
        write_download_rows_to(&mut c,&[(a.clone(),"one".into()),(b.clone(),"two".into())]).unwrap();
        assert!(write_download_rows_to(&mut c,&[(a.clone(),"changed".into()),(a.clone(),"duplicate".into())]).is_err());
        let old:String=c.query_row("SELECT json FROM wetube_download_jobs WHERE job_id=?1",[&a],|r|r.get(0)).unwrap();
        assert_eq!(old,"one");
        update_download_row_in(&c,&a,"new").unwrap();
        assert!(update_download_row_in(&c,&"c".repeat(32),"missing").is_err());
        let unchanged:String=c.query_row("SELECT json FROM wetube_download_jobs WHERE job_id=?1",[&b],|r|r.get(0)).unwrap();
        assert_eq!(unchanged,"two");
        write_download_rows_to(&mut c,&[]).unwrap();
        let count:i64=c.query_row("SELECT COUNT(*) FROM wetube_download_jobs",[],|r|r.get(0)).unwrap();
        let marker:i64=c.query_row("SELECT COUNT(*) FROM kv WHERE key=?1",[JOB_JOURNAL_MARKER],|r|r.get(0)).unwrap();
        assert_eq!((count,marker),(0,1));
    }
}

pub fn wetube_kv_read(key: &str) -> Result<Option<String>> {
    wetube_kv_read_bounded(key, 1024 * 1024)
}
/// Read and decode the legacy JSON-string envelope with a SQL-side size check.
/// Corrupt or oversized state is an error, not an empty journal.
pub fn wetube_kv_read_bounded(key: &str, limit: usize) -> Result<Option<String>> {
    let conn = db().lock();
    read_bounded_kv(&conn, key, limit)
}
fn read_bounded_kv(conn: &Connection, key: &str, limit: usize) -> Result<Option<String>> {
    anyhow::ensure!((1..=16 * 1024 * 1024).contains(&limit), "Invalid local state size limit");
    // JSON escaping can expand a single control byte into six characters.
    let outer_limit = (limit * 6 + 2) as i64;
    let row: Option<(i64, Option<String>)> = conn.query_row(
        "SELECT length(CAST(value AS BLOB)), CASE WHEN length(CAST(value AS BLOB))<=?2 THEN value ELSE NULL END FROM kv WHERE key=?1",
        params![key,outer_limit], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((size,raw))=row else { return Ok(None) };
    anyhow::ensure!(size<=outer_limit,"Saved local state exceeds size limit");
    let value:String=serde_json::from_str(&raw.ok_or_else(|| anyhow::anyhow!("Missing saved state envelope"))?)?;
    anyhow::ensure!(value.len()<=limit,"Saved local state exceeds size limit");
    Ok(Some(value))
}

#[cfg(test)]
mod bounded_kv_tests {
    use super::*;
    #[test] fn missing_corrupt_and_oversized_are_distinct() {
        let c=Connection::open_in_memory().unwrap();
        c.execute_batch(TABLES).unwrap();
        assert_eq!(read_bounded_kv(&c,"missing",16).unwrap(),None);
        for (key,raw) in [("valid",serde_json::to_string("a\"b").unwrap()),("broken","not json".into()),("large",serde_json::to_string(&"x".repeat(17)).unwrap())] {
            c.execute("INSERT INTO kv VALUES(?1,?2)",params![key,raw]).unwrap();
        }
        assert_eq!(read_bounded_kv(&c,"valid",16).unwrap(),Some("a\"b".into()));
        assert!(read_bounded_kv(&c,"broken",16).is_err());
        assert!(read_bounded_kv(&c,"large",16).is_err());
    }
}

pub fn kv_set(key: &str, v: &str) {
    let _ = save_setting_json(key, &v);
}

// ───────────────────────── subscriptions & profiles ─────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub channel_id: String,
    pub name: String,
    pub avatar: Option<String>,
}

pub fn subscriptions() -> Vec<Subscription> {
    let conn = db().lock();
    let mut st = conn
        .prepare("SELECT channel_id, name, avatar FROM subscriptions ORDER BY name COLLATE NOCASE")
        .unwrap();
    st.query_map([], |r| Ok(Subscription { channel_id: r.get(0)?, name: r.get(1)?, avatar: r.get(2)? }))
        .unwrap()
        .filter_map(Result::ok)
        .collect()
}

pub fn is_subscribed(channel_id: &str) -> bool {
    db().lock()
        .query_row("SELECT 1 FROM subscriptions WHERE channel_id=?1", [channel_id], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

pub fn subscribe(channel_id: &str, name: &str, avatar: Option<&str>) {
    let _ = db().lock().execute(
        "INSERT INTO subscriptions(channel_id, name, avatar, added_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(channel_id) DO UPDATE SET name=excluded.name, avatar=COALESCE(excluded.avatar, avatar)",
        params![channel_id, name, avatar, crate::api::json::now()],
    );
}

pub fn unsubscribe(channel_id: &str) {
    let conn = db().lock();
    let _ = conn.execute("DELETE FROM subscriptions WHERE channel_id=?1", [channel_id]);
    let _ = conn.execute("DELETE FROM profile_channels WHERE channel_id=?1", [channel_id]);
    let _ = conn.execute("DELETE FROM feed_cache WHERE channel_id=?1", [channel_id]);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub color: u32,
    pub channels: Vec<String>,
}

pub fn profiles() -> Vec<Profile> {
    let conn = db().lock();
    let mut st = conn.prepare("SELECT id, name, color FROM profiles ORDER BY id").unwrap();
    let mut ps: Vec<Profile> = st
        .query_map([], |r| Ok(Profile { id: r.get(0)?, name: r.get(1)?, color: r.get::<_, i64>(2)? as u32, channels: vec![] }))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    for p in ps.iter_mut() {
        let mut st = conn.prepare("SELECT channel_id FROM profile_channels WHERE profile_id=?1").unwrap();
        p.channels = st.query_map([p.id], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
    }
    ps
}

pub fn create_profile(name: &str, color: u32) -> i64 {
    let conn = db().lock();
    let _ = conn.execute("INSERT INTO profiles(name, color) VALUES (?1, ?2)", params![name, color as i64]);
    conn.last_insert_rowid()
}

pub fn delete_profile(id: i64) {
    let conn = db().lock();
    let _ = conn.execute("DELETE FROM profiles WHERE id=?1", [id]);
    let _ = conn.execute("DELETE FROM profile_channels WHERE profile_id=?1", [id]);
}

pub fn set_profile_channel(profile_id: i64, channel_id: &str, member: bool) {
    let conn = db().lock();
    let _ = if member {
        conn.execute("INSERT OR IGNORE INTO profile_channels VALUES (?1, ?2)", params![profile_id, channel_id])
    } else {
        conn.execute("DELETE FROM profile_channels WHERE profile_id=?1 AND channel_id=?2", params![profile_id, channel_id])
    };
}

// ───────────────────────── feed cache ─────────────────────────

pub fn feed_cache_get(channel_id: &str) -> Option<(Vec<VideoItem>, i64)> {
    let conn = db().lock();
    conn.query_row(
        "SELECT json, fetched_at FROM feed_cache WHERE channel_id=?1",
        [channel_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|(j, t)| serde_json::from_str(&j).ok().map(|v| (v, t)))
}

pub fn feed_cache_put(channel_id: &str, videos: &[VideoItem]) {
    if let Ok(j) = serde_json::to_string(videos) {
        let _ = db().lock().execute(
            "INSERT INTO feed_cache VALUES (?1, ?2, ?3) ON CONFLICT(channel_id) DO UPDATE SET json=excluded.json, fetched_at=excluded.fetched_at",
            params![channel_id, j, crate::api::json::now()],
        );
    }
}

// ───────────────────────── history ─────────────────────────

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub video: VideoItem,
    pub watched_at: i64,
    pub progress: f64,
    pub duration: f64,
}

pub fn history_add(v: &VideoItem) {
    if !settings_snapshot().history_enabled {
        return;
    }
    if let Ok(j) = serde_json::to_string(v) {
        let _ = db().lock().execute(
            "INSERT INTO history(video_id, json, watched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(video_id) DO UPDATE SET json=excluded.json, watched_at=excluded.watched_at",
            params![v.id, j, crate::api::json::now()],
        );
    }
}

pub fn history_progress(video_id: &str, progress: f64, duration: f64) {
    if !settings_snapshot().history_enabled {
        return;
    }
    if !progress.is_finite() || !duration.is_finite() || progress < 0.0 || duration < 0.0 { return; }
    let progress = if duration > 0.0 { progress.min(duration) } else { progress.min(604800.0) };
    let _ = db().lock().execute(
        "UPDATE history SET progress=?2, duration=?3 WHERE video_id=?1",
        params![video_id, progress, duration],
    );
}

pub fn history_list(query: &str, limit: usize) -> Vec<HistoryEntry> {
    let conn = db().lock();
    let mut st = conn
        .prepare("SELECT json, watched_at, progress, duration FROM history ORDER BY watched_at DESC, video_id")
        .unwrap();
    let q = query.to_lowercase();
    st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?))
    })
    .unwrap()
    .filter_map(Result::ok)
    .filter_map(|(j, t, p, d)| {
        let v: VideoItem = serde_json::from_str(&j).ok()?;
        Some(HistoryEntry { video: v, watched_at: t, progress: p, duration: d })
    })
    .filter(|h| q.is_empty() || h.video.title.to_lowercase().contains(&q) || h.video.channel_name.to_lowercase().contains(&q))
    .take(limit)
    .collect()
}

/// Stable descending keyset cursor for local watch history. No OFFSET scan.
pub type HistoryCursor=(i64,String);
pub fn history_page(query:&str,before:Option<&HistoryCursor>,limit:usize)->Result<(Vec<HistoryEntry>,Option<HistoryCursor>)> {
    history_page_from(&db().lock(),query,before,limit)
}
fn history_page_from(conn:&Connection,query:&str,before:Option<&HistoryCursor>,limit:usize)->Result<(Vec<HistoryEntry>,Option<HistoryCursor>)> {
    let limit=limit.clamp(1,200);
    let q=query.to_lowercase();
    let start=(i64::MAX,"\u{10ffff}".to_string());
    let (at,id)=before.unwrap_or(&start);
    let mut st=conn.prepare_cached("SELECT video_id,json,watched_at,progress,duration FROM history
        WHERE (watched_at,video_id)<(?1,?2) ORDER BY watched_at DESC,video_id DESC")?;
    let mut rows=st.query(params![at,id])?;
    let mut page=Vec::with_capacity(limit);
    let mut cursor=None;
    while let Some(row)=rows.next()? {
        let id:String=row.get(0)?;
        let raw:String=row.get(1)?;
        let Ok(video)=serde_json::from_str::<VideoItem>(&raw) else{continue;};
        if video.id!=id {continue;}
        if !q.is_empty()&&!video.title.to_lowercase().contains(&q)&&!video.channel_name.to_lowercase().contains(&q){continue;}
        if page.len()==limit {return Ok((page,cursor));}
        let watched_at:i64=row.get(2)?;
        let progress:f64=row.get(3)?;
        let duration:f64=row.get(4)?;
        cursor=Some((watched_at,id));
        page.push(HistoryEntry{video,watched_at,progress:if progress.is_finite(){progress.max(0.0)}else{0.0},duration:if duration.is_finite(){duration.max(0.0)}else{0.0}});
    }
    Ok((page,None))
}
#[cfg(test)]
mod history_cursor_tests {
    use super::*;
    #[test]
    fn keyset_ties_filter_and_boundaries(){
        let c=Connection::open_in_memory().unwrap();
        c.execute_batch(TABLES).unwrap();c.execute_batch(LIBRARY_INDEXES).unwrap();
        for i in 0..505 {
            let id=format!("{i:011}");
            let v=VideoItem{id:id.clone(),title:if i%2==0{"Épisode".into()}else{"Other".into()},..Default::default()};
            c.execute("INSERT INTO history(video_id,json,watched_at) VALUES(?1,?2,?3)",params![id,serde_json::to_string(&v).unwrap(),i/3]).unwrap();
        }
        let mut before=None;let mut ids=std::collections::HashSet::new();
        loop {
            let (page,next)=history_page_from(&c,"épisode",before.as_ref(),50).unwrap();
            assert!(page.len()<=50);
            for e in page{assert!(ids.insert(e.video.id));}
            if next.is_none(){break;}before=next;
        }
        assert_eq!(ids.len(),253);
    }
}

/// Indexed point lookup, without reading the entire history.
pub fn history_position(id: &str) -> Option<(f64, f64)> {
    db().lock().query_row("SELECT progress, duration FROM history WHERE video_id=?1", [id],
        |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))
        .optional().ok().flatten()
        .filter(|(p,d)| p.is_finite() && d.is_finite() && *p >= 0.0 && *d >= 0.0)
}
pub fn history_count() -> usize {
    db().lock().query_row("SELECT COUNT(*) FROM history", [], |r| r.get::<_, i64>(0)).unwrap_or(0).max(0) as usize
}

/// video_id -> (progress, duration)
pub fn progress_map() -> HashMap<String, (f64, f64)> {
    let conn = db().lock();
    let mut st = conn.prepare("SELECT video_id, progress, duration FROM history").unwrap();
    st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, f64>(1)?, r.get::<_, f64>(2)?))))
        .unwrap()
        .filter_map(Result::ok)
        .collect()
}

/// Merges an entry from the account's YouTube history: adds it when missing and keeps
/// the furthest resume position. Local entries keep their own date and order.
/// Returns true when the entry was new.
pub fn history_merge_remote(v: &VideoItem, progress: f64, duration: f64, watched_at: i64) -> bool {
    if !settings_snapshot().history_enabled || !crate::wetube_session::valid_id(&v.id)
        || !progress.is_finite() || !duration.is_finite() || progress<0.0 || duration<0.0 { return false; }
    let progress = if duration>0.0 { progress.min(duration) } else { progress.min(604800.0) };
    let Ok(j) = serde_json::to_string(v) else { return false };
    let conn = db().lock();
    let existed = conn
        .query_row("SELECT 1 FROM history WHERE video_id=?1", [&v.id], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some();
    let written = conn.execute(
        "INSERT INTO history(video_id, json, watched_at, progress, duration) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(video_id) DO UPDATE SET
           progress = CASE WHEN excluded.progress > history.progress THEN excluded.progress ELSE history.progress END,
           duration = CASE WHEN history.duration > 0 THEN history.duration ELSE excluded.duration END",
        params![v.id, j, watched_at, progress, duration],
    );
    written.is_ok() && !existed
}

pub fn history_remove(video_id: &str) {
    let _ = db().lock().execute("DELETE FROM history WHERE video_id=?1", [video_id]);
}

pub fn history_clear() {
    let _ = db().lock().execute("DELETE FROM history", []);
}

// ───────────────────────── search history ─────────────────────────

pub fn search_history_add(q: &str) {
    if !settings_snapshot().search_history_enabled || q.trim().is_empty() {
        return;
    }
    let _ = db().lock().execute(
        "INSERT INTO search_history VALUES (?1, ?2) ON CONFLICT(query) DO UPDATE SET at=excluded.at",
        params![q.trim(), crate::api::json::now()],
    );
}

pub fn search_history(prefix: &str, limit: usize) -> Vec<String> {
    let conn = db().lock();
    let mut st = conn
        .prepare("SELECT query FROM search_history WHERE query LIKE ?1 ORDER BY at DESC LIMIT ?2")
        .unwrap();
    st.query_map(params![format!("{}%", prefix.replace('%', "")), limit as i64], |r| r.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect()
}

/// Adds searches made on other devices without moving existing entries;
/// `at` is when they should sort (older than anything searched here).
pub fn search_history_merge_remote(q: &str, at: i64) -> bool {
    if !settings_snapshot().search_history_enabled || q.trim().is_empty() {
        return false;
    }
    db().lock().execute("INSERT OR IGNORE INTO search_history VALUES (?1, ?2)", params![q.trim(), at]).map(|n| n > 0).unwrap_or(false)
}

pub fn search_history_remove(q: &str) {
    let _ = db().lock().execute("DELETE FROM search_history WHERE query=?1", [q]);
}

pub fn search_history_clear() {
    let _ = db().lock().execute("DELETE FROM search_history", []);
}

// ───────────────────────── local playlists ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct LocalPlaylist {
    pub id: i64,
    pub name: String,
    pub protected: bool,
    pub count: usize,
    pub thumbnail: Option<String>,
}

pub fn playlists() -> Vec<LocalPlaylist> {
    let conn = db().lock();
    let mut st = conn
        .prepare(
            "SELECT p.id, p.name, p.protected, (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id=p.id),
                    (SELECT json FROM playlist_items i WHERE i.playlist_id=p.id ORDER BY position LIMIT 1)
             FROM playlists p ORDER BY p.protected DESC, p.created_at",
        )
        .unwrap();
    st.query_map([], |r| {
        let j: Option<String> = r.get(4)?;
        Ok(LocalPlaylist {
            id: r.get(0)?,
            name: r.get(1)?,
            protected: r.get::<_, i64>(2)? != 0,
            count: r.get::<_, i64>(3)? as usize,
            thumbnail: j.and_then(|j| serde_json::from_str::<VideoItem>(&j).ok()).map(|v| v.thumbnail),
        })
    })
    .unwrap()
    .filter_map(Result::ok)
    .collect()
}

pub fn playlist_create(name: &str) -> i64 {
    let conn = db().lock();
    let _ = conn.execute(
        "INSERT INTO playlists(name, created_at) VALUES (?1, ?2)",
        params![name, crate::api::json::now()],
    );
    conn.last_insert_rowid()
}

pub fn playlist_rename(id: i64, name: &str) {
    let _ = db().lock().execute("UPDATE playlists SET name=?2 WHERE id=?1", params![id, name]);
}

pub fn playlist_delete(id: i64) {
    let mut conn = db().lock();
    if let Ok(tx) = conn.transaction() {
        let result = (|| -> rusqlite::Result<()> {
            if tx.execute("DELETE FROM playlists WHERE id=?1 AND protected=0", [id])? == 1 {
                tx.execute("DELETE FROM playlist_items WHERE playlist_id=?1", [id])?;
            }
            Ok(())
        })();
        if result.is_ok() { let _ = tx.commit(); }
    };
}

pub fn playlist_items(id: i64) -> Vec<VideoItem> {
    let conn = db().lock();
    let mut st = conn
        .prepare("SELECT json FROM playlist_items WHERE playlist_id=?1 ORDER BY position, video_id")
        .unwrap();
    st.query_map([id], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|j| serde_json::from_str(&j).ok())
        .collect()
}

pub fn playlist_add(id: i64, v: &VideoItem) {
    let conn = db().lock();
    if !crate::wetube_session::valid_id(&v.id) { return; }
    if let Ok(j) = serde_json::to_string(v) {
        // One statement: validate parent and append in the same SQLite write.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO playlist_items(playlist_id,video_id,json,added_at,position)
             SELECT ?1,?2,?3,?4,(SELECT COALESCE(MAX(position),-1)+1 FROM playlist_items WHERE playlist_id=?1)
             WHERE EXISTS(SELECT 1 FROM playlists WHERE id=?1)",
            params![id,v.id,j,crate::api::json::now()],
        );
    }
}

pub fn playlist_remove(id: i64, video_id: &str) {
    let _ = db().lock().execute(
        "DELETE FROM playlist_items WHERE playlist_id=?1 AND video_id=?2",
        params![id, video_id],
    );
}

pub fn playlist_contains(id: i64, video_id: &str) -> bool {
    db().lock()
        .query_row(
            "SELECT 1 FROM playlist_items WHERE playlist_id=?1 AND video_id=?2",
            params![id, video_id],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// Moves an item up (-1) or down (+1).
pub fn playlist_move(id: i64, video_id: &str, delta: i64) -> bool {
    move_playlist_item(&mut db().lock(), id, video_id, delta).unwrap_or(false)
}

/// Transactional adjacent swap; duplicate legacy positions are normalized only when needed.
fn move_playlist_item(conn: &mut Connection, id: i64, video_id: &str, delta: i64) -> rusqlite::Result<bool> {
    if !matches!(delta, -1 | 1) { return Ok(false); }
    let tx = conn.transaction()?;
    let current: Option<i64> = tx.query_row(
        "SELECT position FROM playlist_items WHERE playlist_id=?1 AND video_id=?2",
        params![id, video_id], |r| r.get(0)).optional()?;
    let Some(current) = current else { return Ok(false) };
    let sql = if delta < 0 {
        "SELECT video_id, position FROM playlist_items WHERE playlist_id=?1 AND (position, video_id) < (?2, ?3) ORDER BY position DESC, video_id DESC LIMIT 1"
    } else {
        "SELECT video_id, position FROM playlist_items WHERE playlist_id=?1 AND (position, video_id) > (?2, ?3) ORDER BY position, video_id LIMIT 1"
    };
    let next: Option<(String,i64)> = tx.query_row(sql, params![id, current, video_id],
        |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((other, target)) = next else { return Ok(false) };
    let duplicates: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM playlist_items WHERE playlist_id=?1 AND position IN (?2, ?3) GROUP BY position HAVING COUNT(*)>1)",
        params![id,current,target], |r| r.get(0))?;
    if duplicates {
        let mut ids: Vec<String> = {
            let mut st = tx.prepare("SELECT video_id FROM playlist_items WHERE playlist_id=?1 ORDER BY position,video_id")?;
            let rows = st.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let Some(i) = ids.iter().position(|v| v == video_id) else { return Ok(false) };
        let Some(j) = ids.iter().position(|v| v == &other) else { return Ok(false) };
        ids.swap(i,j);
        for (i,vid) in ids.iter().enumerate() {
            tx.execute("UPDATE playlist_items SET position=?3 WHERE playlist_id=?1 AND video_id=?2", params![id,vid,i as i64])?;
        }
    } else {
        tx.execute("UPDATE playlist_items SET position=CASE WHEN video_id=?2 THEN ?4 ELSE ?5 END WHERE playlist_id=?1 AND video_id IN (?2,?3)",
            params![id,video_id,other,target,current])?;
    }
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod wetube_library_regression {
    use super::*;
    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(TABLES).unwrap();
        conn.execute_batch(LIBRARY_INDEXES).unwrap();
        for (id,pos) in [("a",0),("b",1),("c",2)] {
            conn.execute("INSERT INTO playlist_items VALUES(1,?1,'{}',0,?2)",params![id,pos]).unwrap();
        }
        conn
    }
    fn ordered(c: &Connection) -> Vec<String> {
        let mut s = c.prepare("SELECT video_id FROM playlist_items WHERE playlist_id=1 ORDER BY position,video_id").unwrap();
        let rows=s.query_map([],|r|r.get(0)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }
    #[test] fn adjacent_move_and_boundaries() {
        let mut c=setup();
        assert!(!move_playlist_item(&mut c,1,"a",-1).unwrap());
        assert!(!move_playlist_item(&mut c,1,"a",2).unwrap());
        assert!(move_playlist_item(&mut c,1,"b",-1).unwrap());
        assert_eq!(ordered(&c),vec!["b","a","c"]);
        assert!(move_playlist_item(&mut c,1,"b",1).unwrap());
        assert_eq!(ordered(&c),vec!["a","b","c"]);
    }
    #[test] fn duplicate_legacy_positions() {
        let mut c=setup();
        c.execute("UPDATE playlist_items SET position=0 WHERE playlist_id=1",[]).unwrap();
        assert!(move_playlist_item(&mut c,1,"b",1).unwrap());
        assert_eq!(ordered(&c),vec!["a","c","b"]);
    }
    #[test] fn rollback_on_database_failure() {
        let mut c=setup();
        c.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE ON playlist_items WHEN NEW.video_id='b' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(move_playlist_item(&mut c,1,"a",1).is_err());
        assert_eq!(ordered(&c),vec!["a","b","c"]);
    }
}

pub fn saved_playlists() -> Vec<crate::api::types::PlaylistItem> {
    let conn = db().lock();
    let mut st = conn.prepare("SELECT json FROM saved_playlists ORDER BY added_at DESC").unwrap();
    st.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|j| serde_json::from_str(&j).ok())
        .collect()
}

pub fn save_playlist(p: &crate::api::types::PlaylistItem, save: bool) {
    let conn = db().lock();
    let _ = if save {
        conn.execute(
            "INSERT OR REPLACE INTO saved_playlists VALUES (?1, ?2, ?3)",
            params![p.id, serde_json::to_string(p).unwrap_or_default(), crate::api::json::now()],
        )
    } else {
        conn.execute("DELETE FROM saved_playlists WHERE playlist_id=?1", [&p.id])
    };
}

pub fn is_playlist_saved(id: &str) -> bool {
    db().lock()
        .query_row("SELECT 1 FROM saved_playlists WHERE playlist_id=?1", [id], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

// ───────────────────────── per-channel speed ─────────────────────────

pub fn channel_speed(channel_id: &str) -> Option<f64> {
    db().lock()
        .query_row("SELECT speed FROM channel_speed WHERE channel_id=?1", [channel_id], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

pub fn set_channel_speed(channel_id: &str, speed: f64) {
    let _ = db().lock().execute(
        "INSERT INTO channel_speed VALUES (?1, ?2) ON CONFLICT(channel_id) DO UPDATE SET speed=excluded.speed",
        params![channel_id, speed],
    );
}

pub fn speed_reset_available(channel_id: &str) -> bool {
    channel_speed(channel_id).is_some()
}

pub fn clear_channel_speed(channel_id: &str) {
    let _ = db().lock().execute("DELETE FROM channel_speed WHERE channel_id=?1", [channel_id]);
}

// ───────────────────────── content filter ─────────────────────────

/// Returns true if the video should be hidden according to filtering settings.
pub fn is_filtered(v: &VideoItem, s: &Settings) -> bool {
    if s.hide_shorts && v.is_short {
        return true;
    }
    if s.hide_live && v.is_live {
        return true;
    }
    if s.hide_upcoming && v.is_upcoming {
        return true;
    }
    if let Some(cid) = &v.channel_id {
        if s.blocked_channels.iter().any(|(id, _)| id == cid) {
            return true;
        }
    }
    if !s.blocked_keywords.is_empty() {
        let t = v.title.to_lowercase();
        if s.blocked_keywords.iter().any(|k| !k.trim().is_empty() && t.contains(&k.trim().to_lowercase())) {
            return true;
        }
    }
    false
}

/// Local like state of a video: 1 liked, -1 disliked, 0 none.
pub fn rating(video_id: &str) -> i8 {
    kv_get(&format!("rating:{video_id}")).and_then(|s| s.parse().ok()).unwrap_or(0)
}

pub fn set_rating(video_id: &str, r: i8) {
    kv_set(&format!("rating:{video_id}"), &r.to_string());
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn a_bad_field_keeps_the_others() {
        let raw = serde_json::json!({
            "hide_shorts": true,
            "blocked_keywords": ["spoiler"],
            "sb_actions": { "sponsor": "ask" },
            // wrong type, e.g. from another version
            "default_quality": "high",
            "not_a_setting": 1
        });
        let s = settings_from_value(raw);
        assert!(s.hide_shorts);
        assert_eq!(s.blocked_keywords, vec!["spoiler".to_string()]);
        assert_eq!(s.sb_actions.get("sponsor").map(String::as_str), Some("ask"));
        assert_eq!(s.default_quality, Settings::default().default_quality);
    }

    #[test]
    fn damaged_database_is_rebuilt() {
        let dir = std::env::temp_dir().join(format!("st-rebuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("streamtube.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(TABLES).unwrap();
            for i in 0..300 {
                c.execute("INSERT INTO kv VALUES (?1, ?2)", params![format!("k{i}"), "x".repeat(200)]).unwrap();
            }
            c.execute("INSERT INTO subscriptions VALUES ('UC1', 'One', NULL, 1)", []).unwrap();
        }
        // scribble over a page in the middle of the file, as a damaged write would
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            // the header of a page in the middle of the kv table
            f.seek(SeekFrom::Start(4096 * 6)).unwrap();
            f.write_all(&[0xAB; 64]).unwrap();
        }
        let c = open_checked(&path);
        let ok: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0)).unwrap();
        assert!(n > 100, "kept {n} rows");
        let damaged = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().contains("damaged"));
        assert!(damaged, "the damaged file is kept");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
