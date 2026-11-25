//! Video annotations (the notes, speech bubbles and clickable boxes YouTube removed
//! in 2019), from the public archive the Annotations Restored project serves.
//!
//! Positions and sizes are percentages of the video picture; text size is a
//! percentage of the picture's height.
use super::http;
use anyhow::{Result, anyhow};

const ARCHIVE: &str = "https://storage.googleapis.com/biggest_bucket/annotations";

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A note: a box with text (popup, speech bubble, title, label, ...)
    Note,
    /// A clickable outline; its rollover text shows while the pointer is on it
    Highlight,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Jump to a time in this video
    Seek(f64),
    /// Open another YouTube video, channel or playlist in the app
    YouTube(String),
    /// Open a website in the browser
    Web(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: String,
    pub kind: Kind,
    pub style: String,
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub start: f64,
    pub end: f64,
    /// 0xRRGGBB
    pub bg: u32,
    pub bg_alpha: f32,
    pub fg: u32,
    pub text_size: f32,
    pub action: Option<Action>,
    /// Text shown only while the pointer is on this highlight (its id)
    pub rollover_of: Option<String>,
}

impl Annotation {
    pub fn visible_at(&self, t: f64) -> bool {
        self.rollover_of.is_none() && t >= self.start && t < self.end
    }
}

/// Archive address of a video's annotation file.
fn url_for(video_id: &str) -> String {
    let first = &video_id[..1];
    // the archive keeps ids that start with "-" in a differently named folder
    let dir = if first == "-" { "-/ar-".to_string() } else { first.to_string() };
    format!("{ARCHIVE}/{dir}/{}/{video_id}.xml.gz", &video_id[..3])
}

/// The archived annotations of a video; empty when it never had any.
pub fn fetch(video_id: &str) -> Result<Vec<Annotation>> {
    if video_id.len() != 11 || !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(anyhow!("not a video id"));
    }
    // served with Content-Encoding: gzip, which the HTTP client decodes
    let bytes = match http::get_bytes(&url_for(video_id), &[]) {
        Ok(b) => b,
        // the archive answers 404 (or 403) for videos it has nothing for
        Err(e) if e.to_string().starts_with("HTTP 404 ") || e.to_string().starts_with("HTTP 403 ") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(parse(&String::from_utf8_lossy(&bytes), video_id))
}

// ───────────────────────── a small XML reader ─────────────────────────
// The files are simple and machine-written: elements, attributes and text.

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    fn find(&self, name: &str) -> Option<&Node> {
        self.children.iter().find_map(|c| if c.name == name { Some(c) } else { c.find(name) })
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_xml(s: &str) -> Node {
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] != b'<' {
            let end = s[i..].find('<').map(|e| i + e).unwrap_or(b.len());
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(&s[i..end]));
            }
            i = end;
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|e| e + 3).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").unwrap_or(rest.len());
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&rest[9..end]);
            }
            i += (end + 3).min(rest.len());
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i += rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
            continue;
        }
        // find the end of the tag, skipping '>' inside quoted attribute values
        let mut j = 1;
        let mut quote: Option<u8> = None;
        let rb = rest.as_bytes();
        while j < rb.len() {
            match (quote, rb[j]) {
                (None, b'"' | b'\'') => quote = Some(rb[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        let tag = &rest[1..j.min(rest.len())];
        i += j + 1;
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            // close up to the matching element (tolerates stray tags)
            if stack.iter().skip(1).any(|n| n.name == name) {
                while stack.len() > 1 {
                    let n = stack.pop().unwrap();
                    let done = n.name == name;
                    stack.last_mut().unwrap().children.push(n);
                    if done {
                        break;
                    }
                }
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
        let mut node = Node { name: tag[..name_end].to_string(), ..Default::default() };
        let mut a = &tag[name_end..];
        loop {
            a = a.trim_start();
            let Some(eq) = a.find('=') else { break };
            let key = a[..eq].trim().to_string();
            let v = a[eq + 1..].trim_start();
            let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(close) = v[1..].find(q) else { break };
            node.attrs.push((key, unescape(&v[1..1 + close])));
            a = &v[close + 2..];
        }
        if self_closing {
            stack.last_mut().unwrap().children.push(node);
        } else {
            stack.push(node);
        }
    }
    while stack.len() > 1 {
        let n = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(n);
    }
    stack.pop().unwrap_or_default()
}

// ───────────────────────── annotations ─────────────────────────

/// "1:02:03.5", "2:03.5", "3.5" in seconds; None for "never".
fn parse_time(t: &str) -> Option<f64> {
    if t.is_empty() || t == "never" {
        return None;
    }
    let mut total = 0.0;
    for part in t.split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(total)
}

/// "#t=1m30s", "t=90" in seconds.
fn url_time(url: &str) -> Option<f64> {
    let t = url.split(['#', '&', '?']).find_map(|p| p.strip_prefix("t="))?;
    if let Ok(s) = t.trim_end_matches('s').parse::<f64>() {
        return Some(s);
    }
    let (mut total, mut num) = (0.0, String::new());
    for c in t.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'h' | 'm' | 's' => {
                let n: f64 = num.parse().ok()?;
                num.clear();
                total += n * if c == 'h' { 3600.0 } else if c == 'm' { 60.0 } else { 1.0 };
            }
            _ => return None,
        }
    }
    Some(total)
}

fn action_for(url: &str, video_id: &str) -> Option<Action> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let url = if url.starts_with("//") {
        format!("https:{url}")
    } else if url.starts_with('/') {
        format!("https://www.youtube.com{url}")
    } else {
        url.to_string()
    };
    // YouTube's redirect links wrap the real address in q=
    if url.contains("youtube.com/redirect") {
        if let Some(q) = url.split(['?', '&']).find_map(|kv| kv.strip_prefix("q=")) {
            return Some(Action::Web(http::urldecode(q)));
        }
    }
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    let youtube = host == "youtu.be" || host == "youtube.com" || host.ends_with(".youtube.com");
    if !youtube {
        return Some(Action::Web(url));
    }
    let target = url.split(['?', '&', '#']).find_map(|kv| kv.strip_prefix("v="));
    if target == Some(video_id) || (target.is_none() && url.contains(&format!("youtu.be/{video_id}"))) {
        return Some(Action::Seek(url_time(&url).unwrap_or(0.0)));
    }
    Some(Action::YouTube(url))
}

fn color(v: Option<&str>, default: u32) -> u32 {
    v.and_then(|c| c.trim().parse::<u64>().ok()).map(|c| (c & 0xFF_FFFF) as u32).unwrap_or(default)
}

fn parse_annotation(n: &Node, video_id: &str) -> Option<Annotation> {
    let kind = match n.attr("type")? {
        "text" => Kind::Note,
        "highlight" => Kind::Highlight,
        // pauses, cards, branding and promotions aren't drawn on the video
        _ => return None,
    };
    let style = n.attr("style").unwrap_or("").to_string();
    let segment = n.find("segment")?;
    let region = segment.find("movingRegion")?;
    let rtype = region.attr("type").unwrap_or("rect");
    let regions: Vec<&Node> = region.children.iter().filter(|c| c.name == format!("{rtype}Region") || c.name.ends_with("Region")).collect();
    let first = regions.first()?;
    let last = regions.last()?;
    let num = |k: &str| first.attr(k).and_then(|v| v.parse::<f32>().ok());
    let (x, y, w, h) = (num("x")?, num("y")?, num("w")?, num("h")?);
    let rollover_of = segment.attr("spaceRelative").filter(|_| style == "highlightText").map(str::to_string);
    let (start, end) = match (parse_time(first.attr("t").unwrap_or("")), parse_time(last.attr("t").unwrap_or(""))) {
        (Some(s), Some(e)) => (s, e.max(s)),
        // "never": only shown through its highlight
        _ if rollover_of.is_some() => (0.0, 0.0),
        _ => return None,
    };
    let app = n.child("appearance");
    let a = |k: &str| app.and_then(|a| a.attr(k));
    let text = n.find("TEXT").map(|t| t.text.trim_matches(['\n', '\r']).to_string()).unwrap_or_default();
    let action = n
        .children
        .iter()
        .filter(|c| c.name == "action")
        .find(|c| c.attr("type") == Some("openUrl"))
        .and_then(|c| c.child("url"))
        .and_then(|u| u.attr("value"))
        .and_then(|u| action_for(u, video_id));
    let (bg_default, alpha_default, fg_default) = match (&kind, style.as_str()) {
        (Kind::Highlight, _) => (0x85_7F85, 0.25, 0xFFFFFF),
        (_, "highlightText") => (0x00_0000, 0.0, 0xFFFFFF),
        (_, "title") => (0x00_0000, 0.0, 0xFFFFFF),
        _ => (0xFF_FFFF, 0.8, 0x000000),
    };
    let fg_key = if style == "highlightText" { "highlightFontColor" } else { "fgColor" };
    Some(Annotation {
        id: n.attr("id").unwrap_or("").to_string(),
        kind,
        text,
        x,
        y,
        w,
        h,
        start,
        end,
        bg: color(a("bgColor"), bg_default),
        bg_alpha: a("bgAlpha").or(a("borderAlpha").filter(|_| style.is_empty())).and_then(|v| v.parse().ok()).unwrap_or(alpha_default),
        fg: color(a(fg_key).or(a("fgColor")), fg_default),
        text_size: a("textSize").and_then(|v| v.parse().ok()).filter(|s: &f32| *s > 0.0).unwrap_or(3.15),
        action,
        rollover_of,
        style,
    })
}

/// The annotations of a file, in drawing order.
pub fn parse(xml: &str, video_id: &str) -> Vec<Annotation> {
    let doc = parse_xml(xml);
    let Some(list) = doc.find("annotations") else { return Vec::new() };
    let mut out: Vec<Annotation> = list
        .children
        .iter()
        .filter(|c| c.name == "annotation")
        .filter_map(|c| parse_annotation(c, video_id))
        // empty notes that do nothing are leftovers from YouTube's editor
        .filter(|a| a.kind == Kind::Highlight || !a.text.trim().is_empty() || a.action.is_some())
        .collect();
    // rollover text is placed relative to its highlight
    let highlights: Vec<(String, f32, f32)> = out.iter().filter(|a| a.kind == Kind::Highlight).map(|a| (a.id.clone(), a.x, a.y)).collect();
    out.retain_mut(|a| match &a.rollover_of {
        Some(parent) => match highlights.iter().find(|(id, ..)| id == parent) {
            Some((_, px, py)) => {
                a.x += px;
                a.y += py;
                true
            }
            None => false,
        },
        None => true,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" ?><document><annotations itct="x">
<annotation id="hl1" type="highlight" log_data="a=1&amp;b=2">
  <segment><movingRegion type="rect">
    <rectRegion x="10.0" y="20.0" w="30.0" h="10.0" t="0:01:00.5"/>
    <rectRegion x="10.0" y="20.0" w="30.0" h="10.0" t="1:30.0"/>
  </movingRegion></segment>
  <appearance bgColor="16777215" highlightWidth="3" borderAlpha="0.25"/>
  <action type="openUrl" trigger="click">
    <url target="current" value="https://www.youtube.com/watch?annotation_id=hl1&amp;feature=iv&amp;src_vid=abcdefghijk&amp;v=abcdefghijk#t=2m5s" link_class="1"/>
  </action>
</annotation>
<annotation id="tx1" type="text" style="highlightText" logable="false">
  <TEXT>Go to 2:05 &amp; more</TEXT>
  <segment spaceRelative="hl1"><movingRegion type="rect">
    <rectRegion x="1.0" y="-2.0" w="5" h="3" t="never"/>
    <rectRegion x="1.0" y="-2.0" w="5" h="3" t="never"/>
  </movingRegion></segment>
  <appearance bgAlpha="0" textSize="3.9" highlightFontColor="15921906"/>
</annotation>
<annotation id="p1" type="text" style="popup">
  <TEXT>Subscribe!
Thanks</TEXT>
  <segment><movingRegion type="rect">
    <rectRegion x="5" y="5" w="20" h="8" t="0:05.0"/>
    <rectRegion x="5" y="5" w="20" h="8" t="0:10.0"/>
  </movingRegion></segment>
  <appearance bgAlpha="0.6" bgColor="0" fgColor="16777215" textSize="3.9116"/>
  <action type="openUrl" trigger="click"><url value="https://www.youtube.com/watch?v=zzzzzzzzzzz&amp;feature=iv"/></action>
</annotation>
<annotation id="w1" type="text" style="speech">
  <TEXT>site</TEXT>
  <segment><movingRegion type="anchored">
    <anchoredRegion x="50" y="50" w="10" h="10" sx="40" sy="70" t="0:00:01"/>
    <anchoredRegion x="50" y="50" w="10" h="10" sx="40" sy="70" t="0:00:03"/>
  </movingRegion></segment>
  <action type="openUrl"><url value="http://example.org/page?x=1"/></action>
</annotation>
<annotation id="pz" type="pause"><segment><movingRegion type="rect"><rectRegion x="0" y="0" w="1" h="1" t="0:01"/></movingRegion></segment></annotation>
</annotations></document>"#;

    #[test]
    fn parses_annotations() {
        let a = parse(SAMPLE, "abcdefghijk");
        assert_eq!(a.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["hl1", "tx1", "p1", "w1"]);
        let hl = &a[0];
        assert_eq!(hl.kind, Kind::Highlight);
        assert_eq!((hl.start, hl.end), (60.5, 90.0));
        assert_eq!(hl.action, Some(Action::Seek(125.0)));
        let tx = &a[1];
        assert_eq!(tx.rollover_of.as_deref(), Some("hl1"));
        assert_eq!((tx.x, tx.y), (11.0, 18.0));
        assert_eq!(tx.text, "Go to 2:05 & more");
        assert_eq!(tx.fg, 15921906);
        assert!(!tx.visible_at(70.0));
        let p = &a[2];
        assert_eq!(p.text, "Subscribe!\nThanks");
        assert_eq!((p.bg, p.fg), (0, 0xFFFFFF));
        assert!((p.bg_alpha - 0.6).abs() < 1e-6);
        assert!(p.visible_at(7.0) && !p.visible_at(10.0) && !p.visible_at(4.0));
        assert_eq!(p.action, Some(Action::YouTube("https://www.youtube.com/watch?v=zzzzzzzzzzz&feature=iv".into())));
        let w = &a[3];
        assert_eq!(w.style, "speech");
        assert_eq!((w.bg, w.fg, w.text_size), (0xFFFFFF, 0, 3.15));
        assert_eq!(w.action, Some(Action::Web("http://example.org/page?x=1".into())));
    }

    #[test]
    fn archive_paths() {
        assert_eq!(url_for("6Uwd5w1AwYU"), format!("{ARCHIVE}/6/6Uw/6Uwd5w1AwYU.xml.gz"));
        assert_eq!(url_for("-abcdefghij"), format!("{ARCHIVE}/-/ar-/-ab/-abcdefghij.xml.gz"));
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("0:48:00.9"), Some(2880.9));
        assert_eq!(parse_time("20:00.800"), Some(1200.8));
        assert_eq!(parse_time("never"), None);
        assert_eq!(url_time("https://x/watch?v=a#t=40m"), Some(2400.0));
    }

    #[test]
    #[ignore]
    fn live_archive() {
        let a = fetch("6Uwd5w1AwYU").unwrap();
        println!("{} annotations; first: {:?}", a.len(), a.first());
        assert!(a.len() > 100);
        assert!(a.iter().any(|a| a.kind == Kind::Highlight && matches!(a.action, Some(Action::Seek(_)))));
        // a video that never had annotations
        assert!(fetch("aqz-KE-bpKQ").unwrap().is_empty());
    }
}
