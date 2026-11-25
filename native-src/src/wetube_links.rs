//! Strict local routing for public YouTube links. No redirects or network lookup.
pub fn split(input: &str) -> Option<(&str, &str, &str)> {
    let s = input.trim();
    if s.len() > 4096 || s.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') { return None; }
    let rest = s.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if !["youtube.com", "www.youtube.com", "m.youtube.com", "youtu.be"].iter().any(|h| host.eq_ignore_ascii_case(h)) { return None; }
    let tail = rest[end..].split('#').next()?;
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    Some((host, path, query))
}
pub fn param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    let mut matches = query.split('&').filter_map(|kv| kv.split_once('=')).filter(|(k, _)| *k == key);
    let (_, v) = matches.next()?;
    if matches.next().is_some() || v.is_empty() { return None; }
    Some(v)
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 150 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn hosts() {
        for bad in ["http://www.youtube.com/watch?v=x", "https://youtube.com.evil.test/watch?v=x", "https://youtube.com@evil.test/watch", "https://evil.test/youtu.be/x", "https://www.youtube.com:443/watch", "https://youtube.com\\@evil.test/watch", "https://www.youtube.com/watch\n?v=x"] { assert!(split(bad).is_none(), "{bad}"); }
        assert_eq!(split("https://YOUTU.BE/abcdefghijk?t=42#ignored"), Some(("YOUTU.BE", "/abcdefghijk", "t=42")));
        assert!(split(&format!("https://youtube.com/{}", "a".repeat(4096))).is_none());
    }
    #[test] fn parameters() { assert_eq!(param("v=abc&list=xyz", "v"), Some("abc")); assert_eq!(param("v=a&v=b", "v"), None); assert_eq!(param("v=", "v"), None); assert!(identifier("PL_abc-1")); assert!(!identifier("a/b")); }
}
