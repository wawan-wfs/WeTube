//! Innertube (YouTube internal API) client: search, browse, watch-next, comments, resolve.
use super::http;
use super::json::*;
use super::types::*;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

const BASE: &str = "https://www.youtube.com/youtubei/v1";
pub const WEB_VERSION: &str = "2.20251120.01.00";

fn region() -> (String, String) {
    let s = crate::store::settings_snapshot();
    (s.language.clone(), s.region.clone())
}

fn web_context() -> Value {
    let (hl, gl) = region();
    json!({ "client": { "clientName": "WEB", "clientVersion": WEB_VERSION, "hl": hl, "gl": gl } })
}

pub fn call(endpoint: &str, mut body: Value) -> Result<Value> {
    body["context"] = web_context();
    http::post_json(
        &format!("{BASE}/{endpoint}?prettyPrint=false"),
        &body,
        &[
            ("X-YouTube-Client-Name", "1"),
            ("X-YouTube-Client-Version", WEB_VERSION),
            ("Origin", "https://www.youtube.com"),
        ],
    )
}

// ───────────────────────── item parsing ─────────────────────────

/// Walks any Innertube response and collects every recognizable item plus the last continuation token.
pub fn collect_items(v: &Value) -> Page {
    let mut page = Page::default();
    walk_items(v, &mut page);
    page
}

fn walk_items(v: &Value, page: &mut Page) {
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                if AD_RENDERERS.contains(&k.as_str()) {
                    continue;
                }
                match k.as_str() {
                    "videoRenderer" | "compactVideoRenderer" | "gridVideoRenderer" | "playlistVideoRenderer"
                    | "playlistPanelVideoRenderer" | "videoWithContextRenderer" => {
                        if let Some(vi) = parse_video_renderer(c) {
                            page.items.push(Item::Video(vi));
                        }
                    }
                    "reelItemRenderer" => {
                        if let Some(vi) = parse_reel_item(c) {
                            page.items.push(Item::Video(vi));
                        }
                    }
                    "shortsLockupViewModel" => {
                        if let Some(vi) = parse_shorts_lockup(c) {
                            page.items.push(Item::Video(vi));
                        }
                    }
                    "tileRenderer" => {
                        if let Some(v) = parse_tile(c) {
                            page.items.push(Item::Video(v));
                        }
                    }
                    "nextContinuationData" => {
                        if let Some(t) = c.get("continuation").and_then(Value::as_str) {
                            page.continuation = Some(t.to_string());
                        }
                    }
                    "lockupViewModel" => {
                        if let Some(it) = parse_lockup(c) {
                            page.items.push(it);
                        }
                    }
                    "channelRenderer" | "gridChannelRenderer" => {
                        if let Some(ch) = parse_channel_renderer(c) {
                            page.items.push(Item::Channel(ch));
                        }
                    }
                    "playlistRenderer" | "gridPlaylistRenderer" | "radioRenderer" | "compactPlaylistRenderer" => {
                        if let Some(p) = parse_playlist_renderer(c) {
                            page.items.push(Item::Playlist(p));
                        }
                    }
                    "continuationItemRenderer" => {
                        if let Some(t) = continuation_token(c) {
                            page.continuation = Some(t);
                        }
                    }
                    // Skip noisy sections that duplicate items or are UI chrome.
                    "frameworkUpdates" | "topbar" | "header" | "engagementPanels" | "playerOverlays" | "menu"
                    | "endScreen" | "onResponseReceivedCommands" => {}
                    _ => walk_items(c, page),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|c| walk_items(c, page)),
        _ => {}
    }
}

pub fn continuation_token(c: &Value) -> Option<String> {
    c.pointer("/continuationEndpoint/continuationCommand/token")
        .or_else(|| c.pointer("/button/buttonRenderer/command/continuationCommand/token"))
        .or_else(|| find(c, "continuationCommand").and_then(|x| x.get("token")))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// TV client tiles (home / subscriptions feeds of a signed-in account).
pub(crate) fn parse_tile(t: &Value) -> Option<VideoItem> {
    if is_ad(t) {
        return None;
    }
    let ctype = t.get("contentType").and_then(Value::as_str).unwrap_or("");
    if !ctype.is_empty() && ctype != "TILE_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let id = t
        .get("contentId")
        .and_then(Value::as_str)
        .or_else(|| t.pointer("/onSelectCommand/watchEndpoint/videoId").and_then(Value::as_str))?
        .to_string();
    let meta = t.pointer("/metadata/tileMetadataRenderer")?;
    let title = meta.get("title").map(text).unwrap_or_default();
    let thumbnail = t.pointer("/header/tileHeaderRenderer/thumbnail").and_then(best_thumb).unwrap_or_else(|| default_thumb(&id));
    let mut overlays = Vec::new();
    find_all(t, "thumbnailOverlayTimeStatusRenderer", &mut overlays);
    let badge = overlays.first().and_then(|o| o.get("text")).map(text);
    let is_live = overlays.iter().any(|o| o.get("style").and_then(Value::as_str) == Some("LIVE"));
    let mut parts: Vec<String> = Vec::new();
    let mut channel_id = None;
    for line in meta.get("lines").and_then(Value::as_array).into_iter().flatten() {
        for item in line.pointer("/lineRenderer/items").and_then(Value::as_array).into_iter().flatten() {
            let r = item.get("lineItemRenderer").unwrap_or(item);
            let tx = r.get("text").map(text).unwrap_or_default();
            if channel_id.is_none() {
                channel_id = find(r, "browseEndpoint").and_then(|b| b.get("browseId")).and_then(Value::as_str).map(str::to_string);
            }
            let tx = tx.trim().trim_matches('•').trim().to_string();
            if !tx.is_empty() {
                parts.push(tx);
            }
        }
    }
    if channel_id.is_none() {
        let mut eps = Vec::new();
        find_all(t, "browseEndpoint", &mut eps);
        channel_id = eps.iter().filter_map(|b| b.get("browseId").and_then(Value::as_str)).find(|b| b.starts_with("UC")).map(str::to_string);
    }
    // in any language: the date is the part that reads as a relative time, the
    // views the other part that starts with a number, and the channel comes first
    let published = parts
        .iter()
        .find(|p| p.to_lowercase().contains("ago") || p.to_lowercase().contains("streamed"))
        .or_else(|| parts.iter().find(|p| crate::api::json::parse_relative_time(p).is_some()))
        .cloned();
    let views = parts
        .iter()
        .find(|p| p.to_lowercase().contains("view") || p.to_lowercase().contains("watching"))
        .or_else(|| {
            parts.iter().skip(1).find(|p| Some(*p) != published.as_ref() && p.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .cloned();
    let channel_name = parts.iter().find(|p| Some(*p) != views.as_ref() && Some(*p) != published.as_ref()).cloned().unwrap_or_default();
    let duration_secs = badge.as_deref().and_then(parse_duration);
    Some(VideoItem {
        id,
        title,
        thumbnail,
        channel_name,
        channel_id,
        duration_text: badge.filter(|_| duration_secs.is_some()),
        duration_secs,
        views_text: views,
        published_text: published,
        is_live,
        ..Default::default()
    })
}

fn parse_video_renderer(r: &Value) -> Option<VideoItem> {
    if is_ad(r) {
        return None;
    }
    let id = r.get("videoId")?.as_str()?.to_string();
    let title = opt_text(r.get("title")).or_else(|| opt_text(r.get("headline"))).unwrap_or_default();
    let owner = r
        .get("ownerText")
        .or_else(|| r.get("longBylineText"))
        .or_else(|| r.get("shortBylineText"));
    let channel_name = owner.map(text).unwrap_or_default();
    let channel_id = owner
        .and_then(|o| o.pointer("/runs/0/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut duration_text = opt_text(r.get("lengthText"));
    if duration_text.is_none() {
        // overlay time status
        let mut ov = Vec::new();
        find_all(r, "thumbnailOverlayTimeStatusRenderer", &mut ov);
        duration_text = ov.first().and_then(|o| opt_text(o.get("text")));
    }
    let badges = r.get("badges").map(|b| b.to_string()).unwrap_or_default();
    let is_live = badges.contains("LIVE_NOW")
        || badges.contains("\"LIVE\"")
        || duration_text.as_deref().map(|d| d.eq_ignore_ascii_case("live")).unwrap_or(false);
    let is_upcoming = r.get("upcomingEventData").is_some();
    let duration_secs = duration_text.as_deref().and_then(parse_duration);
    let published_text = opt_text(r.get("publishedTimeText"));
    let views_text = opt_text(r.get("shortViewCountText")).or_else(|| opt_text(r.get("viewCountText")));
    let snippet = r
        .pointer("/detailedMetadataSnippets/0/snippetText")
        .map(text)
        .or_else(|| opt_text(r.get("descriptionSnippet")));
    let avatar = r
        .pointer("/channelThumbnailSupportedRenderers/channelThumbnailWithLinkRenderer/thumbnail")
        .or_else(|| r.get("channelThumbnail"))
        .and_then(small_thumb);
    let is_short = r
        .pointer("/navigationEndpoint/reelWatchEndpoint")
        .is_some();
    Some(VideoItem {
        thumbnail: r.get("thumbnail").and_then(best_thumb).unwrap_or_else(|| default_thumb(&id)),
        published_ts: published_text.as_deref().and_then(parse_relative_time),
        id,
        title,
        channel_name,
        channel_id,
        channel_avatar: avatar,
        duration_text: if is_live { None } else { duration_text },
        duration_secs,
        views_text,
        published_text,
        description_snippet: snippet.filter(|s| !s.is_empty()),
        is_live,
        is_short,
        is_upcoming,
    })
}

pub fn default_thumb(id: &str) -> String {
    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
}

fn parse_reel_item(r: &Value) -> Option<VideoItem> {
    let id = r.get("videoId")?.as_str()?.to_string();
    Some(VideoItem {
        title: opt_text(r.get("headline")).unwrap_or_default(),
        thumbnail: format!("https://i.ytimg.com/vi/{id}/oar2.jpg"),
        views_text: opt_text(r.get("viewCountText")),
        id,
        is_short: true,
        ..Default::default()
    })
}

fn parse_shorts_lockup(r: &Value) -> Option<VideoItem> {
    let id = r
        .pointer("/onTap/innertubeCommand/reelWatchEndpoint/videoId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            r.get("entityId")
                .and_then(Value::as_str)
                .map(|e| e.trim_start_matches("shorts-shelf-item-").to_string())
        })?;
    let title = r
        .pointer("/overlayMetadata/primaryText/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let views = s(r, "/overlayMetadata/secondaryText/content");
    Some(VideoItem {
        thumbnail: r
            .pointer("/thumbnail")
            .and_then(best_thumb)
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/oar2.jpg")),
        id,
        title,
        views_text: views,
        is_short: true,
        ..Default::default()
    })
}

fn parse_lockup(l: &Value) -> Option<Item> {
    if is_ad(l) {
        return None;
    }
    let id = l.get("contentId")?.as_str()?.to_string();
    let ctype = l.get("contentType").and_then(Value::as_str).unwrap_or("");
    let meta = l.pointer("/metadata/lockupMetadataViewModel")?;
    let title = meta.pointer("/title/content").and_then(Value::as_str).unwrap_or("").to_string();
    let thumb_vm = l
        .pointer("/contentImage/thumbnailViewModel")
        .or_else(|| l.pointer("/contentImage/collectionThumbnailViewModel/primaryThumbnail/thumbnailViewModel"));
    let thumbnail = thumb_vm.and_then(|t| t.get("image")).and_then(best_thumb);
    // badges (duration / LIVE / "N videos")
    let mut badge_texts = Vec::new();
    if let Some(t) = thumb_vm {
        let mut found = Vec::new();
        find_all(t, "thumbnailBadgeViewModel", &mut found);
        for b in found {
            if let Some(tx) = b.get("text").and_then(Value::as_str) {
                badge_texts.push(tx.to_string());
            }
        }
    }
    // metadata rows
    let mut channel_name = None;
    let mut channel_id = None;
    let mut views = None;
    let mut published = None;
    let mut others = Vec::new();
    if let Some(rows) = meta
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array)
    {
        for row in rows {
            for part in row.get("metadataParts").and_then(Value::as_array).into_iter().flatten() {
                let Some(tx) = part.pointer("/text/content").and_then(Value::as_str) else { continue };
                let tx = tx.trim().to_string();
                if tx.is_empty() {
                    continue;
                }
                let browse = find(part, "browseEndpoint")
                    .and_then(|b| b.get("browseId"))
                    .and_then(Value::as_str);
                let low = tx.to_lowercase();
                if let Some(b) = browse.filter(|b| b.starts_with("UC")) {
                    channel_name = Some(tx);
                    channel_id = Some(b.to_string());
                } else if low.contains("view") || low.contains("watching") || low.contains("waiting") {
                    views = Some(tx);
                } else if low.contains("ago") || low.contains("streamed") || low.contains("premiere") || low.contains("scheduled") {
                    published = Some(tx);
                } else {
                    others.push(tx);
                }
            }
        }
    }
    if channel_id.is_none() {
        channel_id = meta
            .pointer("/image/decoratedAvatarViewModel/rendererContext/commandContext/onTap/innertubeCommand/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let avatar = meta
        .pointer("/image/decoratedAvatarViewModel/avatar/avatarViewModel/image")
        .and_then(small_thumb);
    if channel_id.is_none() {
        // TV lockups keep the channel link in the long-press menu
        let mut eps = Vec::new();
        find_all(l, "browseEndpoint", &mut eps);
        channel_id = eps
            .iter()
            .filter_map(|b| b.get("browseId").and_then(Value::as_str))
            .find(|b| b.starts_with("UC"))
            .map(str::to_string);
    }
    if channel_name.is_none() {
        channel_name = others.first().cloned();
    }
    match ctype {
        "LOCKUP_CONTENT_TYPE_VIDEO" | "LOCKUP_CONTENT_TYPE_MUSIC" | "LOCKUP_CONTENT_TYPE_SHORT" => {
            let is_short = ctype == "LOCKUP_CONTENT_TYPE_SHORT";
            let is_live = badge_texts.iter().any(|b| b.eq_ignore_ascii_case("live"));
            let is_upcoming = badge_texts.iter().any(|b| b.eq_ignore_ascii_case("upcoming"));
            let duration_text = badge_texts.iter().find(|b| parse_duration(b).is_some()).cloned();
            let published = published.or_else(|| others.iter().find(|o| parse_relative_time(o).is_some()).cloned());
            let views = views.or_else(|| {
                // compact form "1B" with PLAY_ARROW icon, or another language: no "views" word;
                // pick the numeric-ish part that isn't the date or the channel
                others
                    .iter()
                    .filter(|o| Some(*o) != published.as_ref() && Some(*o) != channel_name.as_ref())
                    .find(|o| o.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
                    .cloned()
            });
            let views = views.map(|v| {
                if v.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',' || "KMB".contains(c)) {
                    format!("{v} views")
                } else {
                    v
                }
            });
            Some(Item::Video(VideoItem {
                thumbnail: thumbnail.unwrap_or_else(|| default_thumb(&id)),
                duration_secs: duration_text.as_deref().and_then(parse_duration),
                duration_text,
                published_ts: published.as_deref().and_then(parse_relative_time),
                published_text: published,
                views_text: views,
                channel_name: channel_name.unwrap_or_default(),
                channel_id,
                channel_avatar: avatar,
                title,
                id,
                is_live,
                is_upcoming,
                is_short,
                ..Default::default()
            }))
        }
        "LOCKUP_CONTENT_TYPE_PLAYLIST" | "LOCKUP_CONTENT_TYPE_PODCAST" | "LOCKUP_CONTENT_TYPE_ALBUM" | "LOCKUP_CONTENT_TYPE_COURSE" | "LOCKUP_CONTENT_TYPE_SHOW" => {
            let first_video_id = find(l, "watchEndpoint")
                .and_then(|w| w.get("videoId"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(Item::Playlist(PlaylistItem {
                id,
                title,
                thumbnail,
                channel_name,
                channel_id,
                video_count_text: badge_texts.first().cloned(),
                first_video_id,
            }))
        }
        "LOCKUP_CONTENT_TYPE_CHANNEL" => Some(Item::Channel(ChannelItem {
            id,
            name: title,
            avatar: thumbnail,
            ..Default::default()
        })),
        _ => None,
    }
}

fn parse_channel_renderer(r: &Value) -> Option<ChannelItem> {
    let id = r.get("channelId")?.as_str()?.to_string();
    // In search results "subscriberCountText" often holds the handle and "videoCountText" holds subscribers.
    let a = opt_text(r.get("subscriberCountText"));
    let b = opt_text(r.get("videoCountText"));
    let (handle, subs) = match (&a, &b) {
        (Some(x), y) if x.starts_with('@') => (Some(x.clone()), y.clone()),
        _ => (None, a.clone()),
    };
    Some(ChannelItem {
        id,
        name: opt_text(r.get("title")).unwrap_or_default(),
        avatar: r.get("thumbnail").and_then(best_thumb),
        handle,
        subscribers_text: subs,
        video_count_text: if a.as_deref().map(|x| x.starts_with('@')).unwrap_or(false) { None } else { b },
        description: opt_text(r.get("descriptionSnippet")),
    })
}

fn parse_playlist_renderer(r: &Value) -> Option<PlaylistItem> {
    let id = r.get("playlistId")?.as_str()?.to_string();
    let thumbnail = r
        .pointer("/thumbnails/0")
        .and_then(best_thumb)
        .or_else(|| r.get("thumbnail").and_then(best_thumb))
        .or_else(|| r.pointer("/thumbnailRenderer/playlistVideoThumbnailRenderer/thumbnail").and_then(best_thumb));
    let owner = r.get("longBylineText").or_else(|| r.get("shortBylineText"));
    Some(PlaylistItem {
        id,
        title: opt_text(r.get("title")).unwrap_or_default(),
        thumbnail,
        channel_name: owner.map(text),
        channel_id: owner
            .and_then(|o| o.pointer("/runs/0/navigationEndpoint/browseEndpoint/browseId"))
            .and_then(Value::as_str)
            .map(str::to_string),
        video_count_text: opt_text(r.get("videoCountText"))
            .or_else(|| opt_text(r.get("videoCountShortText")))
            .or_else(|| r.get("videoCount").and_then(Value::as_str).map(|c| format!("{c} videos"))),
        first_video_id: r
            .pointer("/navigationEndpoint/watchEndpoint/videoId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

// ───────────────────────── search ─────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchFilter {
    /// 0 relevance, 1 rating, 2 upload date, 3 view count
    pub sort: u8,
    /// 0 any, 1 hour, 2 today, 3 week, 4 month, 5 year
    pub date: u8,
    /// 0 any, 1 video, 2 channel, 3 playlist, 4 movie
    pub kind: u8,
    /// 0 any, 1 short (<4m), 2 long (>20m), 3 medium (4–20m)
    pub duration: u8,
    pub hd: bool,
    pub subtitles: bool,
    pub creative_commons: bool,
    pub live: bool,
    pub four_k: bool,
    pub hdr: bool,
}

impl SearchFilter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Encodes the protobuf `sp` parameter.
    pub fn encode(&self) -> Option<String> {
        if self.is_default() {
            return None;
        }
        fn varint(out: &mut Vec<u8>, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    break;
                }
                out.push(b | 0x80);
            }
        }
        fn field(out: &mut Vec<u8>, num: u64, v: u64) {
            varint(out, num << 3);
            varint(out, v);
        }
        let mut filters = Vec::new();
        if self.date > 0 { field(&mut filters, 1, self.date as u64); }
        if self.kind > 0 { field(&mut filters, 2, self.kind as u64); }
        if self.duration > 0 { field(&mut filters, 3, self.duration as u64); }
        if self.hd { field(&mut filters, 4, 1); }
        if self.subtitles { field(&mut filters, 5, 1); }
        if self.creative_commons { field(&mut filters, 6, 1); }
        if self.live { field(&mut filters, 8, 1); }
        if self.four_k { field(&mut filters, 14, 1); }
        if self.hdr { field(&mut filters, 25, 1); }
        let mut out = Vec::new();
        if self.sort > 0 { field(&mut out, 1, self.sort as u64); }
        if !filters.is_empty() {
            varint(&mut out, (2 << 3) | 2);
            varint(&mut out, filters.len() as u64);
            out.extend(filters);
        }
        use base64::Engine;
        Some(base64::engine::general_purpose::STANDARD.encode(out))
    }
}

pub fn search(query: &str, filter: &SearchFilter) -> Result<Page> {
    let mut body = json!({ "query": query });
    if let Some(p) = filter.encode() {
        body["params"] = json!(p);
    }
    let r = call("search", body)?;
    Ok(collect_items(&r))
}

pub fn search_continue(token: &str) -> Result<Page> {
    let r = call("search", json!({ "continuation": token }))?;
    Ok(collect_items(&r))
}

/// The Shorts player's own feed: a fresh, varied sequence each time.
/// `token` is `None` for the first page, then the returned continuation.
/// Items only carry ids; titles and channels come from `watch_next` when played.
/// The Shorts feed. With the browser sign-in it is the account's own Shorts
/// recommendations; otherwise it starts from `seed` (a Short from the local
/// history) so YouTube follows on from what was watched, or from nothing.
pub fn shorts_feed(token: Option<&str>, seed: Option<&str>) -> Result<(Vec<VideoItem>, Option<String>)> {
    let personal = crate::api::web::connected();
    let call = |ep: &str, body: Value| if personal { crate::api::web::web_call(ep, body) } else { call(ep, body) };
    let mut ids: Vec<String> = Vec::new();
    let seq = match token {
        Some(t) => t.to_string(),
        // a sequence that follows on from a Short: protobuf { 1: video id }
        None if seed.is_some() && !personal => {
            use base64::Engine;
            let id = seed.unwrap_or_default();
            let mut pb = vec![0x0a, id.len() as u8];
            pb.extend(id.as_bytes());
            base64::engine::general_purpose::URL_SAFE.encode(&pb)
        }
        None => {
            let body = json!({ "params": "CA8%3D", "disablePlayerResponse": true, "inputType": "REEL_WATCH_INPUT_TYPE_SEEDLESS" });
            let r = call("reel/reel_item_watch", body)?;
            if let Some(id) = r.pointer("/replacementEndpoint/reelWatchEndpoint/videoId").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
            r.get("sequenceContinuation").and_then(Value::as_str).ok_or_else(|| anyhow!("no shorts sequence"))?.to_string()
        }
    };
    let s = call("reel/reel_watch_sequence", json!({ "sequenceParams": seq }))?;
    for e in s.get("entries").and_then(Value::as_array).into_iter().flatten() {
        if let Some(id) = e.pointer("/command/reelWatchEndpoint/videoId").and_then(Value::as_str) {
            if !ids.iter().any(|x| x == id) {
                ids.push(id.to_string());
            }
        }
    }
    let cont = s.pointer("/continuationEndpoint/continuationCommand/token").and_then(Value::as_str).map(str::to_string);
    let items = ids
        .into_iter()
        .map(|id| VideoItem { thumbnail: default_thumb(&id), id, is_short: true, ..Default::default() })
        .collect();
    Ok((items, cont))
}

pub fn suggestions(query: &str) -> Result<Vec<String>> {
    let (hl, gl) = region();
    let q = http::urlencode(query);
    // the endpoint youtube.com itself uses; answers with JSONP
    let yt = http::get_text(&format!(
        "https://suggestqueries-clients6.youtube.com/complete/search?client=youtube&ds=yt&hl={hl}&gl={gl}&q={q}"
    ))
    .ok()
    .and_then(|t| parse_suggestions_jsonp(&t));
    if let Some(v) = yt.filter(|v| !v.is_empty()) {
        return Ok(v);
    }
    let v = http::get_json(&format!("https://suggestqueries.google.com/complete/search?client=firefox&ds=yt&hl={hl}&gl={gl}&q={q}"))?;
    Ok(v.get(1)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

/// `window.google.ac.h(["q",[["sugg",0,[..]],..],{..}])` -> the suggestion strings.
fn parse_suggestions_jsonp(t: &str) -> Option<Vec<String>> {
    let start = t.find('(')? + 1;
    let end = t.rfind(')')?;
    let v: Value = serde_json::from_str(t.get(start..end)?).ok()?;
    Some(
        v.get(1)?
            .as_array()?
            .iter()
            .filter_map(|e| e.get(0).and_then(Value::as_str).or_else(|| e.as_str()).map(str::to_string))
            .collect(),
    )
}

#[cfg(test)]
mod suggest_tests {
    #[test]
    fn jsonp() {
        let t = r#"window.google.ac.h(["lofi",[["lofi",0,[512]],["lofi girl",0,[512,433]]],{"k":1}])"#;
        assert_eq!(super::parse_suggestions_jsonp(t).unwrap(), vec!["lofi", "lofi girl"]);
    }

    #[test]
    #[ignore]
    fn live() {
        let v = super::suggestions("lofi").unwrap();
        assert!(!v.is_empty(), "{v:?}");
    }
}

// ───────────────────────── browse ─────────────────────────

pub fn browse(browse_id: &str, params: Option<&str>) -> Result<Value> {
    let mut body = json!({ "browseId": browse_id });
    if let Some(p) = params {
        body["params"] = json!(p);
    }
    call("browse", body)
}

pub fn browse_continue(token: &str) -> Result<Page> {
    let r = call("browse", json!({ "continuation": token }))?;
    Ok(collect_items(&r))
}

/// Resolves a YouTube URL (/@handle, /c/name, /channel/UC.., /playlist?list=, /watch?v=) via the API.
pub fn resolve_url(url: &str) -> Result<ResolvedUrl> {
    let r = call("navigation/resolve_url", json!({ "url": url }))?;
    let ep = r.get("endpoint").ok_or_else(|| anyhow!("unresolvable url"))?;
    if let Some(id) = ep.pointer("/browseEndpoint/browseId").and_then(Value::as_str) {
        if let Some(pl) = id.strip_prefix("VL") {
            return Ok(ResolvedUrl::Playlist(pl.to_string()));
        }
        return Ok(ResolvedUrl::Channel(id.to_string()));
    }
    if let Some(id) = ep.pointer("/watchEndpoint/videoId").and_then(Value::as_str) {
        return Ok(ResolvedUrl::Video(id.to_string()));
    }
    Err(anyhow!("unsupported url"))
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedUrl {
    Video(String),
    Channel(String),
    Playlist(String),
}

/// Fast local parse of common URL shapes before falling back to resolve_url.
pub fn parse_youtube_url(input: &str) -> Option<ResolvedUrl> {
    let (host, path, query) = crate::wetube_links::split(input)?;
    let video = |id: &str| crate::wetube_session::valid_id(id).then(|| ResolvedUrl::Video(id.to_string()));
    if host.eq_ignore_ascii_case("youtu.be") { return video(path.strip_prefix('/')?); }
    for marker in ["/shorts/", "/live/", "/embed/"] {
        if let Some(id) = path.strip_prefix(marker) { return video(id); }
    }
    if path == "/watch" {
        if let Some(id) = crate::wetube_links::param(query, "v") { return video(id); }
        if query.split('&').any(|kv| kv.starts_with("v=")) { return None; }
    }
    if matches!(path, "/playlist" | "/watch") {
        return crate::wetube_links::param(query, "list").filter(|s| crate::wetube_links::identifier(s)).map(|s| ResolvedUrl::Playlist(s.to_string()));
    }
    if let Some(id) = path.strip_prefix("/channel/") {
        let id = id.split('/').next()?;
        if id.starts_with("UC") && id.len() == 24 && crate::wetube_links::identifier(id) { return Some(ResolvedUrl::Channel(id.to_string())); }
    }
    None
}

pub fn channel(channel_id: &str, tab_params: Option<&str>) -> Result<ChannelPage> {
    let r = browse(channel_id, tab_params)?;
    let mut info = ChannelItem { id: channel_id.to_string(), ..Default::default() };
    if let Some(md) = r.pointer("/metadata/channelMetadataRenderer") {
        info.name = s(md, "/title").unwrap_or_default();
        info.description = s(md, "/description");
        info.avatar = md.get("avatar").and_then(best_thumb);
        if let Some(ext) = s(md, "/externalId") {
            info.id = ext;
        }
        info.handle = s(md, "/vanityChannelUrl").and_then(|u| u.rsplit('/').next().map(str::to_string));
    }
    let mut banner = None;
    if let Some(ph) = r.pointer("/header/pageHeaderRenderer/content/pageHeaderViewModel") {
        if info.name.is_empty() {
            info.name = s(ph, "/title/dynamicTextViewModel/text/content").unwrap_or_default();
        }
        if info.avatar.is_none() {
            info.avatar = ph
                .pointer("/image/decoratedAvatarViewModel/avatar/avatarViewModel/image")
                .and_then(best_thumb);
        }
        banner = ph.pointer("/banner/imageBannerViewModel/image").and_then(best_thumb);
        if let Some(rows) = ph.pointer("/metadata/contentMetadataViewModel/metadataRows").and_then(Value::as_array) {
            for row in rows {
                for part in row.get("metadataParts").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(t) = part.pointer("/text/content").and_then(Value::as_str) {
                        let low = t.to_lowercase();
                        if t.starts_with('@') {
                            info.handle = Some(t.to_string());
                        } else if low.contains("subscriber") {
                            info.subscribers_text = Some(t.to_string());
                        } else if low.contains("video") {
                            info.video_count_text = Some(t.to_string());
                        }
                    }
                }
            }
        }
    } else if let Some(c4) = r.pointer("/header/c4TabbedHeaderRenderer") {
        banner = c4.get("banner").and_then(best_thumb);
        info.subscribers_text = opt_text(c4.get("subscriberCountText"));
    }
    let mut tabs = Vec::new();
    let mut page = Page::default();
    if let Some(arr) = r.pointer("/contents/twoColumnBrowseResultsRenderer/tabs").and_then(Value::as_array) {
        for t in arr {
            let tr = t.get("tabRenderer").or_else(|| t.get("expandableTabRenderer"));
            let Some(tr) = tr else { continue };
            let title = s(tr, "/title").unwrap_or_default();
            let selected = tr.get("selected").and_then(Value::as_bool).unwrap_or(false);
            let params = s(tr, "/endpoint/browseEndpoint/params");
            if selected {
                if let Some(content) = tr.get("content") {
                    page = collect_items(content);
                }
            }
            if !title.is_empty() {
                tabs.push(ChannelTab { title, params, selected });
            }
        }
    }
    // Fill channel info on videos that lack it
    for it in page.items.iter_mut() {
        if let Item::Video(v) = it {
            if v.channel_name.is_empty() {
                v.channel_name = info.name.clone();
            }
            if v.channel_id.is_none() {
                v.channel_id = Some(info.id.clone());
            }
            if v.channel_avatar.is_none() {
                v.channel_avatar = info.avatar.clone();
            }
        }
    }
    Ok(ChannelPage { info, banner, tabs, page })
}

/// Just a channel's avatar, for feeds whose items don't include one.
pub fn channel_avatar(channel_id: &str) -> Result<Option<String>> {
    let r = browse(channel_id, None)?;
    Ok(r
        .pointer("/metadata/channelMetadataRenderer/avatar")
        .and_then(small_thumb)
        .or_else(|| {
            r.pointer("/header/pageHeaderRenderer/content/pageHeaderViewModel/image/decoratedAvatarViewModel/avatar/avatarViewModel/image")
                .and_then(small_thumb)
        }))
}

/// Finds a channel by its exact display name: (channel id, avatar).
pub fn channel_by_name(name: &str) -> Result<Option<(String, Option<String>)>> {
    let page = search(name, &SearchFilter { kind: 2, ..Default::default() })?;
    let want = name.trim().to_lowercase();
    Ok(page.items.into_iter().find_map(|i| match i {
        Item::Channel(c) if c.name.trim().to_lowercase() == want => Some((c.id, c.avatar)),
        _ => None,
    }))
}

/// Searches within a channel.
pub fn channel_search(channel_id: &str, query: &str) -> Result<Page> {
    let r = call("browse", json!({ "browseId": channel_id, "params": "EgZzZWFyY2jyBgQKAloA", "query": query }))?;
    Ok(collect_items(&r))
}

pub fn playlist(playlist_id: &str) -> Result<PlaylistPage> {
    let r = browse(&format!("VL{playlist_id}"), None)?;
    let content = r.get("contents").cloned().unwrap_or(Value::Null);
    let page = collect_items(&content);
    let mut info = PlaylistItem { id: playlist_id.to_string(), ..Default::default() };
    if let Some(md) = r.pointer("/metadata/playlistMetadataRenderer") {
        info.title = s(md, "/title").unwrap_or_default();
    }
    let mut description = None;
    if let Some(h) = r.pointer("/header/pageHeaderRenderer") {
        if info.title.is_empty() {
            info.title = s(h, "/pageTitle").unwrap_or_default();
        }
        let mut parts = Vec::new();
        find_all(h, "metadataParts", &mut parts);
        for arr in parts.iter().filter_map(|p| p.as_array()) {
            for p in arr {
                if let Some(t) = p.pointer("/text/content").and_then(Value::as_str) {
                    if t.to_lowercase().contains("video") {
                        info.video_count_text = Some(t.to_string());
                    }
                    if let Some(b) = find(p, "browseEndpoint").and_then(|b| b.get("browseId")).and_then(Value::as_str) {
                        if b.starts_with("UC") {
                            info.channel_id = Some(b.to_string());
                            info.channel_name = Some(t.trim_start_matches("by ").to_string());
                        }
                    }
                }
            }
        }
        description = find(h, "descriptionPreviewViewModel")
            .and_then(|d| d.pointer("/description/content"))
            .and_then(Value::as_str)
            .map(str::to_string);
    } else if let Some(h) = r.pointer("/header/playlistHeaderRenderer") {
        info.video_count_text = opt_text(h.get("numVideosText"));
        info.channel_name = opt_text(h.get("ownerText"));
        description = opt_text(h.get("descriptionText"));
    }
    let videos: Vec<VideoItem> = page.items.into_iter().filter_map(|i| match i {
        Item::Video(v) => Some(v),
        _ => None,
    }).collect();
    info.thumbnail = videos.first().map(|v| v.thumbnail.clone());
    info.first_video_id = videos.first().map(|v| v.id.clone());
    Ok(PlaylistPage { info, description, videos, continuation: page.continuation })
}

// ───────────────────────── watch / next ─────────────────────────

pub fn watch_next(video_id: &str, playlist_id: Option<&str>) -> Result<WatchInfo> {
    let mut body = json!({ "videoId": video_id, "contentCheckOk": true, "racyCheckOk": true });
    if let Some(p) = playlist_id {
        body["playlistId"] = json!(p);
    }
    let r = call("next", body)?;
    let mut w = WatchInfo { id: video_id.to_string(), ..Default::default() };
    let results = r.pointer("/contents/twoColumnWatchNextResults").cloned().unwrap_or(Value::Null);
    if let Some(p) = find(&results, "videoPrimaryInfoRenderer") {
        w.title = opt_text(p.get("title")).unwrap_or_default();
        w.views_text = p
            .pointer("/viewCount/videoViewCountRenderer/viewCount")
            .map(text)
            .unwrap_or_default();
        w.date_text = opt_text(p.get("dateText")).unwrap_or_default();
        w.is_live = p
            .pointer("/viewCount/videoViewCountRenderer/isLive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(like) = find(p, "likeButtonViewModel") {
            w.likes_text = find(like, "defaultButtonViewModel")
                .and_then(|b| b.pointer("/buttonViewModel/title"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    if let Some(sec) = find(&results, "videoSecondaryInfoRenderer") {
        if let Some(o) = sec.pointer("/owner/videoOwnerRenderer") {
            w.channel.name = opt_text(o.get("title")).unwrap_or_default();
            w.channel.id = o
                .pointer("/navigationEndpoint/browseEndpoint/browseId")
                .or_else(|| o.pointer("/title/runs/0/navigationEndpoint/browseEndpoint/browseId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            w.channel.avatar = o.get("thumbnail").and_then(best_thumb);
            w.channel.subscribers_text = opt_text(o.get("subscriberCountText"));
            // collaborations: "A and B", with the channels listed in a dialog
            if w.channel.name.is_empty() {
                if let Some(t) = o.pointer("/attributedTitle/content").and_then(Value::as_str) {
                    w.channel.name = t.to_string();
                }
            }
            if w.channel.id.is_empty() || w.channel.avatar.is_none() {
                let mut list = Vec::new();
                find_all(o, "listItemViewModel", &mut list);
                if let Some(first) = list.first() {
                    if w.channel.id.is_empty() {
                        w.channel.id = find(first, "browseEndpoint")
                            .and_then(|b| b.get("browseId"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                    }
                    if w.channel.subscribers_text.is_none() {
                        w.channel.subscribers_text = first
                            .pointer("/subtitle/content")
                            .and_then(Value::as_str)
                            .and_then(|s| s.split('\u{2022}').nth(1))
                            .map(|s| s.chars().filter(|c| !('\u{2066}'..='\u{2069}').contains(c) && *c != '\u{200e}').collect::<String>().trim().to_string())
                            .filter(|s| !s.is_empty());
                    }
                }
                if w.channel.avatar.is_none() {
                    w.channel.avatar = o
                        .pointer("/avatarStack/avatarStackViewModel/avatars/0/avatarViewModel/image/sources/0/url")
                        .and_then(Value::as_str)
                        .map(|u| if u.starts_with("//") { format!("https:{u}") } else { u.to_string() });
                }
            }
        }
        w.description = sec
            .pointer("/attributedDescription/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| opt_text(sec.get("description")))
            .unwrap_or_default();
    }
    // Related
    if let Some(sr) = results.pointer("/secondaryResults") {
        let page = collect_items(sr);
        w.related = page.items;
        w.related_continuation = page.continuation;
    }
    // Comments token
    let mut sections = Vec::new();
    find_all(&results, "itemSectionRenderer", &mut sections);
    for sec in sections {
        if sec.get("sectionIdentifier").and_then(Value::as_str) == Some("comment-item-section") {
            w.comments_token = sec.pointer("/contents/0/continuationItemRenderer").and_then(continuation_token);
        }
    }
    if let Some(panels) = r.get("engagementPanels") {
        if let Some(items) = find(panels, "sortFilterSubMenuRenderer").and_then(|m| m.get("subMenuItems")).and_then(Value::as_array) {
            let tok = |i: usize| {
                items
                    .get(i)
                    .and_then(|x| x.pointer("/serviceEndpoint/continuationCommand/token"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            w.comments_sort = (tok(0), tok(1));
        }
    }
    if let Some(lc) = find(&r, "liveChatRenderer").filter(|lc| !lc.get("isReplay").and_then(Value::as_bool).unwrap_or(false)) {
        // Only the main token works on its own; "Top chat" and "Live chat" differ
        // by one flag inside it (field 1 = 4 for top, 1 for everything).
        let first = lc.pointer("/continuations/0/reloadContinuationData/continuation").and_then(Value::as_str).map(str::to_string);
        let top = first.clone();
        let all = first.map(|t| t.replacen("ggEMCAQY", "ggEMCAEY", 1));
        if let (Some(t), Some(a)) = (top, all) {
            w.live_chat = Some((t, a));
        }
    }
    if let Some(cnt) = find(&results, "commentsEntryPointHeaderRenderer") {
        w.comment_count_text = opt_text(cnt.get("commentCount"));
    }
    // Chapters from player bar markers
    let mut chapters = Vec::new();
    let mut found = Vec::new();
    find_all(&r, "chapterRenderer", &mut found);
    for c in found {
        let start = c.get("timeRangeStartMillis").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        chapters.push(Chapter {
            title: opt_text(c.get("title")).unwrap_or_default(),
            start,
            thumbnail: c.get("thumbnail").and_then(best_thumb),
        });
    }
    if chapters.is_empty() {
        let mut m = Vec::new();
        find_all(&r, "macroMarkersListItemRenderer", &mut m);
        for c in m {
            let t = opt_text(c.get("timeDescription")).and_then(|t| parse_duration(&t));
            if let Some(t) = t {
                chapters.push(Chapter {
                    title: opt_text(c.get("title")).unwrap_or_default(),
                    start: t as f64,
                    thumbnail: c.get("thumbnail").and_then(best_thumb),
                });
            }
        }
        // macro markers may include "key moments" duplicates; keep unique sorted
        chapters.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
        chapters.dedup_by(|a, b| a.start == b.start);
    }
    if chapters.is_empty() {
        chapters = chapters_from_description(&w.description);
    }
    w.chapters = chapters;
    // "Most replayed" heatmap
    let mut lists = Vec::new();
    find_all(&r, "macroMarkersListEntity", &mut lists);
    for l in lists {
        if l.pointer("/markersList/markerType").and_then(Value::as_str) != Some("MARKER_TYPE_HEATMAP") {
            continue;
        }
        let num = |v: Option<&Value>| v.and_then(|x| x.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| x.as_f64())).unwrap_or(0.0);
        for m in l.pointer("/markersList/markers").and_then(Value::as_array).into_iter().flatten() {
            w.heatmap.push((
                num(m.get("startMillis")) / 1000.0,
                num(m.get("durationMillis")) / 1000.0,
                num(m.get("intensityScoreNormalized")) as f32,
            ));
        }
        break;
    }
    // Playlist panel
    if let Some(pl) = results.pointer("/playlist/playlist") {
        let page = collect_items(pl.get("contents").unwrap_or(&Value::Null));
        w.playlist = Some(WatchPlaylist {
            id: s(pl, "/playlistId").unwrap_or_default(),
            title: s(pl, "/title").unwrap_or_default(),
            items: page.items.into_iter().filter_map(|i| match i {
                Item::Video(v) => Some(v),
                _ => None,
            }).collect(),
        });
    }
    Ok(w)
}

pub fn related_continue(token: &str) -> Result<Page> {
    let r = call("next", json!({ "continuation": token }))?;
    Ok(collect_items(&r))
}

/// Parses "0:00 Intro" style lines from a description.
pub fn chapters_from_description(desc: &str) -> Vec<Chapter> {
    let mut out = Vec::new();
    for line in desc.lines() {
        let line = line.trim();
        let mut ts_end = None;
        for (i, w) in line.split_whitespace().enumerate() {
            let w2 = w.trim_matches(|c: char| c == '(' || c == ')' || c == '[' || c == ']' || c == '-');
            if w2.contains(':') && parse_duration(w2).is_some() {
                ts_end = Some((i, parse_duration(w2).unwrap()));
                break;
            }
            if i > 1 {
                break;
            }
        }
        if let Some((idx, secs)) = ts_end {
            let title: String = line
                .split_whitespace()
                .enumerate()
                .filter(|(i, _)| *i != idx)
                .map(|(_, w)| w)
                .collect::<Vec<_>>()
                .join(" ")
                .trim_start_matches(|c: char| c == '-' || c == '–' || c == ':' || c == '|' || c.is_whitespace())
                .to_string();
            out.push(Chapter { title, start: secs as f64, thumbnail: None });
        }
    }
    if out.len() >= 3 && out[0].start == 0.0 { out } else { Vec::new() }
}

// ───────────────────────── comments ─────────────────────────

/// Channel (custom) emojis in a comment's text. Standard emojis are left alone:
/// they're already Unicode characters in the text.
fn comment_emojis(props: &Value) -> Vec<(String, String)> {
    let Some(content) = props.pointer("/content/content").and_then(Value::as_str) else { return Vec::new() };
    let Some(runs) = props.pointer("/content/attachmentRuns").and_then(Value::as_array) else { return Vec::new() };
    // run offsets count UTF-16 units
    let units: Vec<u16> = content.encode_utf16().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for r in runs {
        let Some(url) = r.pointer("/element/type/imageType/image/sources/0/url").and_then(Value::as_str) else { continue };
        if url.contains("/s/gaming/emoji/") {
            continue;
        }
        let start = r.get("startIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
        let len = r.get("length").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(span) = units.get(start..start + len) else { continue };
        let code = String::from_utf16_lossy(span);
        if code.trim().is_empty() || out.iter().any(|(c, _)| *c == code) {
            continue;
        }
        // sharper than the 16px default, drawn at text size
        let url = match url.rsplit_once('=') {
            Some((base, _)) if url.contains("googleusercontent.com") || url.contains("ggpht.com") => format!("{base}=w40-h40-c-k-nd"),
            _ => url.to_string(),
        };
        out.push((code, url));
    }
    out
}

pub fn comments(token: &str) -> Result<CommentPage> {
    let r = call("next", json!({ "continuation": token }))?;
    let mut page = CommentPage::default();
    // entity payloads
    let mut payloads = std::collections::HashMap::new();
    let mut toolbar_states = std::collections::HashMap::new();
    let mut surfaces = std::collections::HashMap::new();
    if let Some(muts) = r.pointer("/frameworkUpdates/entityBatchUpdate/mutations").and_then(Value::as_array) {
        for m in muts {
            let key = m.get("entityKey").and_then(Value::as_str).unwrap_or_default().to_string();
            if let Some(p) = m.pointer("/payload/commentEntityPayload") {
                payloads.insert(key, p.clone());
            } else if let Some(p) = m.pointer("/payload/engagementToolbarStateEntityPayload") {
                toolbar_states.insert(key, p.clone());
            } else if let Some(p) = m.pointer("/payload/commentSurfaceEntityPayload") {
                surfaces.insert(key, p.clone());
            }
        }
    }
    let mut lists = Vec::new();
    for key in ["onResponseReceivedEndpoints", "onResponseReceivedActions"] {
        if let Some(arr) = r.get(key).and_then(Value::as_array) {
            for e in arr {
                for k in ["reloadContinuationItemsCommand", "appendContinuationItemsAction"] {
                    if let Some(items) = e.pointer(&format!("/{k}/continuationItems")).and_then(Value::as_array) {
                        lists.extend(items.iter().cloned());
                    }
                }
            }
        }
    }
    for it in &lists {
        if let Some(h) = it.get("commentsHeaderRenderer") {
            page.count_text = opt_text(h.get("countText"));
            if let Some(items) = h
                .pointer("/sortMenu/sortFilterSubMenuRenderer/subMenuItems")
                .and_then(Value::as_array)
            {
                page.sort_top_token = items
                    .first()
                    .and_then(|i| i.pointer("/serviceEndpoint/continuationCommand/token"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                page.sort_new_token = items
                    .get(1)
                    .and_then(|i| i.pointer("/serviceEndpoint/continuationCommand/token"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        let (vm, thread) = if let Some(t) = it.get("commentThreadRenderer") {
            (t.get("commentViewModel").map(|c| c.get("commentViewModel").unwrap_or(c)), Some(t))
        } else if let Some(c) = it.get("commentViewModel") {
            (Some(c), None)
        } else {
            (None, None)
        };
        if let Some(vm) = vm {
            let key = vm.get("commentKey").and_then(Value::as_str).unwrap_or_default();
            if let Some(p) = payloads.get(key) {
                let props = p.get("properties").cloned().unwrap_or(Value::Null);
                let author = p.get("author").cloned().unwrap_or(Value::Null);
                let toolbar = p.get("toolbar").cloned().unwrap_or(Value::Null);
                let state_key = vm.get("toolbarStateKey").and_then(Value::as_str).unwrap_or_default();
                let hearted = toolbar_states
                    .get(state_key)
                    .and_then(|s| s.get("heartState"))
                    .and_then(Value::as_str)
                    .map(|s| s.contains("HEARTED") && !s.contains("UNHEARTED"))
                    .unwrap_or(false);
                let replies_token = thread
                    .and_then(|t| t.pointer("/replies/commentRepliesRenderer/contents"))
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find_map(|c| c.get("continuationItemRenderer")))
                    .and_then(continuation_token);
                page.comments.push(Comment {
                    id: s(&props, "/commentId").unwrap_or_default(),
                    author: s(&author, "/displayName").unwrap_or_default(),
                    author_id: s(&author, "/channelId"),
                    author_avatar: s(&author, "/avatarThumbnailUrl"),
                    text: s(&props, "/content/content").unwrap_or_default(),
                    published: s(&props, "/publishedTime").unwrap_or_default(),
                    likes: s(&toolbar, "/likeCountNotliked").unwrap_or_default().trim().to_string(),
                    reply_count: s(&toolbar, "/replyCount").unwrap_or_default(),
                    replies_token,
                    is_creator: author.get("isCreator").and_then(Value::as_bool).unwrap_or(false),
                    is_verified: author.get("isVerified").and_then(Value::as_bool).unwrap_or(false),
                    is_pinned: vm.get("pinnedText").is_some(),
                    hearted,
                    emojis: comment_emojis(&props),
                    image: vm
                        .get("commentSurfaceKey")
                        .and_then(Value::as_str)
                        .and_then(|k| surfaces.get(k))
                        .and_then(|s| s.pointer("/imageAttachmentViewModel/commentImageAttachmentViewModel"))
                        .and_then(|a| {
                            let url = a.pointer("/image/sources/0/url")?.as_str()?.to_string();
                            let ratio = a.get("aspectRatio").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                            Some((url, ratio.clamp(0.2, 5.0)))
                        }),
                });
            }
        }
        if let Some(c) = it.get("continuationItemRenderer") {
            page.continuation = continuation_token(c);
        }
    }
    Ok(page)
}

// ───────────────────────── home / explore ─────────────────────────

/// Category "explore" channels (YouTube removed the Trending page in 2025).
pub const EXPLORE: &[(&str, &str)] = &[
    ("Music", "UC-9-kyTW8ZkZNDHQJ6FgpwQ"),
    ("Gaming", "UCOpNcN46UbXVtpKMrmU4Abg"),
    ("News", "UCYfdidRxbB8Qhf0Nx7ioOYw"),
    ("Sports", "UCEgdi0XIXXZ-qJOFPf4JSKw"),
    ("Live", "UC4R8DWoMoI7CAwX8_LjQHig"),
    ("Fashion & Beauty", "UCrpQ4p1Ql_hG8rKXIKM1MOQ"),
    ("Learning", "UCtFRv9O2AHqOZjjynzrv-xg"),
];

pub fn explore(channel_id: &str) -> Result<Page> {
    let r = browse(channel_id, None)?;
    Ok(collect_items(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lockup(id: &str, title: &str) -> Value {
        json!({ "lockupViewModel": {
            "contentId": id, "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "metadata": { "lockupMetadataViewModel": { "title": { "content": title } } }
        }})
    }

    #[test]
    fn ads_are_skipped() {
        let mut ad_lockup = lockup("adVid000001", "Buy now");
        ad_lockup["lockupViewModel"]["rendererContext"] = json!({ "loggingContext": { "adClientParams": { "x": 1 } } });
        let feed = json!({ "contents": [
            { "richItemRenderer": { "content": lockup("realVid0001", "A real video") } },
            // the home feed's in-feed ad slot
            { "richItemRenderer": { "content": { "adSlotRenderer": { "fulfillmentContent": { "fulfilledLayout": {
                "inFeedAdLayoutRenderer": { "renderingContent": lockup("adVid000002", "OLX") } } } } } } },
            // an ad drawn as a plain lockup, recognisable only by its ad fields
            { "richItemRenderer": { "content": ad_lockup } },
            // search: promoted video and a regular one marked with an ad badge
            { "searchPyvRenderer": { "ads": [{ "videoRenderer": { "videoId": "adVid000003", "title": { "simpleText": "Ad" } } }] } },
            { "videoRenderer": { "videoId": "adVid000004", "title": { "simpleText": "Sponsored" },
                "badges": [{ "metadataBadgeRenderer": { "style": "BADGE_STYLE_TYPE_AD", "label": "Ad" } }] } },
            { "videoRenderer": { "videoId": "realVid0002", "title": { "simpleText": "Another real one" },
                "badges": [{ "metadataBadgeRenderer": { "style": "BADGE_STYLE_TYPE_SIMPLE", "label": "New" } }] } },
        ]});
        let ids: Vec<String> = collect_items(&feed).items.iter().filter_map(|i| i.as_video().map(|v| v.id.clone())).collect();
        assert_eq!(ids, vec!["realVid0001", "realVid0002"]);
    }

    #[test]
    fn filter_encoding() {
        let f = SearchFilter { kind: 1, ..Default::default() };
        assert_eq!(f.encode().unwrap(), "EgIQAQ==");
        let f = SearchFilter { sort: 2, ..Default::default() };
        assert_eq!(f.encode().unwrap(), "CAI=");
    }

    #[test]
    fn url_parse_rejects_untrusted_routes() {
        for bad in ["https://evil.test/youtu.be/abcdefghijk", "https://youtube.com.evil.test/watch?v=abcdefghijk", "https://youtu.be/abcdefghijkEXTRA", "https://www.youtube.com/watch?v=abcdefghijk&v=12345678901", "https://www.youtube.com/playlist?list=a/b"] { assert!(parse_youtube_url(bad).is_none(), "{bad}"); }
    }

    #[test]
    fn url_parse() {
        assert_eq!(parse_youtube_url("https://youtu.be/dQw4w9WgXcQ?t=1"), Some(ResolvedUrl::Video("dQw4w9WgXcQ".into())));
        assert_eq!(parse_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=x"), Some(ResolvedUrl::Video("dQw4w9WgXcQ".into())));
        assert_eq!(parse_youtube_url("https://www.youtube.com/shorts/oImytUi5y-w"), Some(ResolvedUrl::Video("oImytUi5y-w".into())));
    }

    #[test]
    #[ignore]
    fn live_search() {
        let p = search("rust programming", &SearchFilter::default()).unwrap();
        assert!(p.items.len() > 5, "{:?}", p.items.len());
        let v = p.items.iter().filter_map(|i| i.as_video()).next().unwrap();
        println!("{v:#?}");
        assert!(!v.title.is_empty());
        assert!(p.continuation.is_some());
    }

    #[test]
    #[ignore]
    fn live_watch() {
        let w = watch_next("dQw4w9WgXcQ", None).unwrap();
        println!("{} | {} | {:?} | {} related | likes {:?} | chapters {}", w.title, w.channel.name, w.channel.subscribers_text, w.related.len(), w.likes_text, w.chapters.len());
        assert!(!w.title.is_empty());
        assert!(w.related.len() > 3);
        let c = comments(w.comments_token.as_ref().unwrap()).unwrap();
        println!("{} comments, first: {:?}", c.comments.len(), c.comments.first());
        assert!(c.comments.len() > 5);
        let first_with_replies = c.comments.iter().find(|c| c.replies_token.is_some()).unwrap();
        let replies = comments(first_with_replies.replies_token.as_ref().unwrap()).unwrap();
        assert!(!replies.comments.is_empty());
    }

    #[test]
    #[ignore]
    fn live_channel_playlist() {
        let c = channel("UCsBjURrPoezykLs9EqgamOA", None).unwrap();
        println!("{:?} tabs={:?}", c.info, c.tabs.iter().map(|t| &t.title).collect::<Vec<_>>());
        let vt = c.tabs.iter().find(|t| t.title == "Videos").unwrap();
        let v = channel("UCsBjURrPoezykLs9EqgamOA", vt.params.as_deref()).unwrap();
        assert!(v.page.items.len() > 10);
        println!("{:#?}", v.page.items[0]);
        let p = playlist("PLBCF2DAC6FFB574DE").unwrap();
        println!("{:?} {}", p.info, p.videos.len());
        assert!(!p.videos.is_empty());
        for (name, id) in EXPLORE {
            let e = explore(id).map(|p| p.items.len()).unwrap_or(0);
            println!("explore {name}: {e}");
        }
    }
}

#[cfg(test)]
mod chapter_tests {
    use super::*;
    #[test]
    #[ignore]
    fn live_chapters() {
        for q in ["bayes theorem in artificial intelligence sudhakar atchala", "iphone 17 pro review", "rust tutorial full course"] {
            let p = search(q, &SearchFilter::default()).unwrap();
            for it in p.items.iter().take(4) {
                if let Item::Video(v) = it {
                    let w = watch_next(&v.id, None).unwrap();
                    println!("{} [{}] chapters={} first={:?}", v.title, v.id, w.chapters.len(), w.chapters.get(1).map(|c| (&c.title, c.start)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tv_parse_tests {
    use super::*;
    #[test]
    #[ignore]
    fn parse_tv_file() {
        let path = std::env::var("TVJSON").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let p = collect_items(&v);
        for it in p.items.iter().take(6) {
            if let Item::Video(v) = it {
                println!("{} | {} | {:?} | {:?} | {:?} | {:?}", v.id, v.title, v.channel_name, v.channel_id, v.views_text, v.duration_text);
            }
        }
        println!("items={} cont={}", p.items.len(), p.continuation.is_some());
    }
}

// ───────────────────────── live chat ─────────────────────────

fn chat_text(msg: Option<&Value>) -> String {
    let Some(runs) = msg.and_then(|m| m.get("runs")).and_then(Value::as_array) else {
        return msg.map(text).unwrap_or_default();
    };
    let mut out = String::new();
    for r in runs {
        if let Some(t) = r.get("text").and_then(Value::as_str) {
            out.push_str(t);
        } else if let Some(e) = r.get("emoji") {
            let custom = e.get("isCustomEmoji").and_then(Value::as_bool).unwrap_or(false);
            let s = if custom {
                e.pointer("/shortcuts/0").and_then(Value::as_str)
            } else {
                e.get("emojiId").and_then(Value::as_str)
            };
            out.push_str(s.unwrap_or(""));
        }
    }
    out
}

fn chat_message(item: &Value) -> Option<ChatMessage> {
    let (r, paid) = if let Some(r) = item.get("liveChatTextMessageRenderer") {
        (r, None)
    } else if let Some(r) = item.get("liveChatPaidMessageRenderer") {
        (r, opt_text(r.get("purchaseAmountText")))
    } else {
        return None;
    };
    let mut role = ChatRole::Viewer;
    let mut verified = false;
    for b in r.get("authorBadges").and_then(Value::as_array).into_iter().flatten() {
        let b = b.get("liveChatAuthorBadgeRenderer").unwrap_or(b);
        match b.pointer("/icon/iconType").and_then(Value::as_str) {
            Some("OWNER") => role = ChatRole::Owner,
            Some("MODERATOR") if role != ChatRole::Owner => role = ChatRole::Moderator,
            Some("VERIFIED") => verified = true,
            None if b.get("customThumbnail").is_some() && role == ChatRole::Viewer => role = ChatRole::Member,
            _ => {}
        }
    }
    Some(ChatMessage {
        id: r.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
        author: opt_text(r.get("authorName")).unwrap_or_default(),
        author_photo: r.get("authorPhoto").and_then(best_thumb),
        text: chat_text(r.get("message")),
        role,
        verified,
        paid,
    })
}

/// One poll of a live chat. Reading needs no account.
pub fn live_chat(token: &str) -> Result<ChatBatch> {
    let r = call("live_chat/get_live_chat", json!({ "continuation": token }))?;
    let c = r.pointer("/continuationContents/liveChatContinuation").ok_or_else(|| anyhow!("chat has ended"))?;
    let mut batch = ChatBatch { timeout_ms: 5000, ..Default::default() };
    for a in c.get("actions").and_then(Value::as_array).into_iter().flatten() {
        if let Some(m) = a.pointer("/addChatItemAction/item").and_then(chat_message) {
            batch.messages.push(m);
        }
    }
    if let Some(next) = c.pointer("/continuations/0").and_then(Value::as_object).and_then(|o| o.values().next()) {
        batch.next = next.get("continuation").and_then(Value::as_str).map(str::to_string);
        batch.timeout_ms = next.get("timeoutMs").and_then(Value::as_u64).unwrap_or(5000);
    }
    Ok(batch)
}

#[cfg(test)]
mod heatmap_tests {
    #[test]
    #[ignore]
    fn live_heatmap() {
        for id in ["dQw4w9WgXcQ", "kJQP7kiw5Fk", "LXb3EKWsInQ"] {
            let w = super::watch_next(id, None).unwrap();
            println!("{id}: heatmap={} chapters={}", w.heatmap.len(), w.chapters.len());
        }
    }
}
