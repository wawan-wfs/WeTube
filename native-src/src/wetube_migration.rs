//! Local, transactional import of WeTube v0.1–v0.5 backups. No network or credentials.
use anyhow::{anyhow, bail, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use crate::{api::types::VideoItem, import::ImportReport};

pub const QUEUE_NAME: &str = "WeTube imported queue";
const MAX_MS: i64 = 253_402_300_799_999;
const INSERT_HISTORY: &str = "INSERT INTO history(video_id,json,watched_at,progress,duration) VALUES (?1,?2,?3,0,0) ON CONFLICT(video_id) DO NOTHING";
const INSERT_ITEM: &str = "INSERT INTO playlist_items(playlist_id,video_id,json,added_at,position) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(playlist_id,video_id) DO NOTHING";
const INSERT_SUB: &str = "INSERT INTO subscriptions(channel_id,name,avatar,added_at) VALUES (?1,?2,NULL,?3) ON CONFLICT(channel_id) DO NOTHING";
const INSERT_SEARCH: &str = "INSERT INTO search_history(query,at) VALUES (?1,?2) ON CONFLICT(query) DO NOTHING";
const INSERT_KV: &str = "INSERT INTO kv(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value";

struct Entry { video: VideoItem, at: i64 }
struct List { key: String, name: String, items: Vec<Entry>, protected: bool }
struct Plan { lists: Vec<List>, history: Vec<Entry>, subscriptions: Vec<(String,String)>, handles: Vec<(String,String)>, searches: Vec<String> }
fn array<'a>(v: &'a Value, limit: usize) -> Result<&'a [Value]> {
    let a = v.as_array().ok_or_else(|| anyhow!("Expected array"))?;
    ensure!(a.len() <= limit, "Too many entries"); Ok(a)
}
fn name(v: &Value, max: usize) -> Result<&str> {
    let s=v.as_str().ok_or_else(|| anyhow!("Expected text"))?;
    ensure!(s.chars().count() <= max && !s.chars().any(char::is_control), "Invalid text"); Ok(s)
}
fn ascii_id(s: &str, n: usize) -> bool { s.len()==n && s.bytes().all(|b| b.is_ascii_alphanumeric() || b==b'-' || b==b'_') }
// Old exported backups always use canonical HTTPS URLs. Reject other paths, userinfo,
// query duplicates, port overrides, whitespace and redirects rather than guessing.
fn video_id(url: &str) -> Option<(&str,bool)> {
    for (prefix, short) in [("https://www.youtube.com/watch?v=",false),("https://www.youtube.com/shorts/",true),("https://www.youtube.com/live/",false),("https://youtu.be/",false)] {
        if let Some(id)=url.strip_prefix(prefix) { return ascii_id(id,11).then_some((id,short)); }
    } None
}
fn timestamp(v: &Value) -> Result<i64> {
    if v.is_null() { return Ok(0); }
    let ms=v.as_i64().ok_or_else(|| anyhow!("Invalid legacy timestamp"))?;
    ensure!((0..=MAX_MS).contains(&ms), "Invalid legacy timestamp"); Ok(ms/1000)
}
fn entry(v: &Value) -> Result<Entry> {
    let url=name(&v["url"],512)?;
    let (id,short)=video_id(url).ok_or_else(|| anyhow!("Invalid video URL"))?;
    if let Some(given)=v.get("id") { ensure!(given.as_str()==Some(id),"Video ID mismatch"); }
    let title=if v["title"].is_null() { "" } else { name(&v["title"],120)? };
    Ok(Entry {video:VideoItem {id:id.to_owned(),title:if title.trim().is_empty(){id.to_owned()}else{title.to_owned()},thumbnail:format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),is_short:short,..Default::default()},at:timestamp(&v["at"])?})
}
fn entries(v: &Value) -> Result<Vec<Entry>> {
    let mut seen=std::collections::HashSet::new(); let mut out=Vec::new();
    for v in array(v,200)? { let e=entry(v)?; if seen.insert(e.video.id.clone()) {out.push(e);} } Ok(out)
}
fn channel(url:&str) -> Option<(&str,bool)> {
    let path=url.strip_prefix("https://www.youtube.com/")?;
    if let Some(id)=path.strip_prefix("channel/") { return (id.starts_with("UC") && ascii_id(id,24)).then_some((id,false)); }
    // Percent-encoded Unicode handles are left unresolved, never used as channel IDs.
    let h=path.strip_prefix('@')?;
    if h.is_empty() || h.len()>300 { return None; }
    let b=h.as_bytes(); let mut i=0;
    while i<b.len() { if b[i]==b'%' { if i+2>=b.len() || !b[i+1].is_ascii_hexdigit() || !b[i+2].is_ascii_hexdigit(){return None;} i+=3; } else if b[i].is_ascii_alphanumeric() || b"_.-".contains(&b[i]) {i+=1;} else {return None;} }
    Some((url,true))
}
fn parse(v:&Value)->Result<Plan> {
    let empty=json!([]); let (lib,c)=match (v["format"].as_str(),v["version"].as_u64()) {
        (Some("wetube-library"),Some(1))=>(v,None),
        (Some("wetube-collections"),Some(2))=>(&v["library"],Some(&v["collections"])),
        _=>bail!("Unsupported WeTube backup format"),
    };
    ensure!(lib["format"]=="wetube-library" && lib["version"]==1,"Invalid library version");
    let mut plan=Plan {lists:vec![List{key:"favorites".into(),name:"Favorites".into(),items:entries(&lib["favorites"])?,protected:true}],history:entries(&lib["history"])?,subscriptions:Vec::new(),handles:Vec::new(),searches:Vec::new()};
    if let Some(c)=c {
        plan.lists.push(List{key:"later".into(),name:"Watch later".into(),items:entries(&c["later"])?,protected:true});
        plan.lists.push(List{key:"queue".into(),name:QUEUE_NAME.into(),items:entries(&c["queue"])?,protected:false});
        let mut ids=std::collections::HashSet::new();
        for p in array(&c["playlists"],30)? {
            let id=name(&p["id"],64)?; ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()||b==b'-'||b==b'_') && ids.insert(id),"Invalid or duplicate playlist ID");
            let n=name(&p["name"],80)?.trim(); ensure!(!n.is_empty(),"Empty playlist name");
            plan.lists.push(List{key:format!("playlist:{id}"),name:format!("Imported · {n}"),items:entries(&p["items"])?,protected:false});
        }
        for s in array(&c["subscriptions"],200)? {
            let url=name(&s["url"],512)?; let n=name(&s["name"],80)?;
            let (id,handle)=channel(url).ok_or_else(||anyhow!("Invalid channel link"))?;
            if handle {plan.handles.push((id.into(),n.into()));} else {plan.subscriptions.push((id.into(),n.into()));}
        }
    }
    // v0.5 did not export search history. Explicit optional extension, never inferred.
    for s in array(v.get("searches").unwrap_or(&empty),30)? {
        let q=name(s,200)?.trim(); ensure!(!q.is_empty() && !q.contains("://") && !q.to_ascii_lowercase().starts_with("wetube:"),"Invalid search text"); plan.searches.push(q.into());
    }
    Ok(plan)
}
fn mapped_playlist(tx:&rusqlite::Transaction<'_>, l:&List, now:i64)->Result<i64> {
    if l.protected {
        let id=tx.query_row("SELECT id FROM playlists WHERE protected=1 AND name=?1 ORDER BY id LIMIT 1",[&l.name],|r|r.get(0)).optional()?;
        return id.ok_or_else(||anyhow!("Missing protected playlist; initialize a native profile first"));
    }
    let key=format!("wetube_migration:{}",l.key);
    let saved:Option<String>=tx.query_row("SELECT value FROM kv WHERE key=?1",[&key],|r|r.get(0)).optional()?;
    if let Some(raw)=saved {if let Ok(id)=serde_json::from_str::<i64>(&raw) {
        let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM playlists WHERE id=?1 AND protected=0)",[id],|r|r.get(0))?;
        if live {return Ok(id);}
    }}
    tx.execute("INSERT INTO playlists(name,created_at,protected) VALUES(?1,?2,0)",params![l.name,now])?;
    let id=tx.last_insert_rowid(); tx.execute(INSERT_KV,params![key,serde_json::to_string(&id)?])?; Ok(id)
}
pub fn import(conn:&mut Connection, value:&Value)->Result<ImportReport> {
    let plan=parse(value)?; // validate everything before opening transaction
    let now=crate::api::json::now(); let tx=conn.transaction()?; let mut rep=ImportReport::default();
    for e in plan.history {rep.history+=tx.execute(INSERT_HISTORY,params![e.video.id,serde_json::to_string(&e.video)?,e.at])?;}
    for l in plan.lists {
        if l.items.is_empty() && (l.protected || l.key=="queue") {continue;}
        let id=mapped_playlist(&tx,&l,now)?; rep.playlists+=1;
        let mut pos:i64=tx.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM playlist_items WHERE playlist_id=?1",[id],|r|r.get(0))?;
        for e in l.items {let n=tx.execute(INSERT_ITEM,params![id,e.video.id,serde_json::to_string(&e.video)?,e.at,pos])?;pos+=n as i64;rep.playlist_videos+=n;}
    }
    for (id,n) in plan.subscriptions {rep.subscriptions+=tx.execute(INSERT_SUB,params![id,n,now])?;}
    for (i,q) in plan.searches.into_iter().enumerate() {rep.searches+=tx.execute(INSERT_SEARCH,params![q,now-i as i64])?;}
    // Keep unresolved channel handles separate from the subscriptions table; merge by URL.
    let key="wetube_unresolved_channels_v1";
    let previous:Option<String>=tx.query_row("SELECT value FROM kv WHERE key=?1",[key],|r|r.get(0)).optional()?;
    let mut handles:Vec<(String,String)>=previous.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default();
    for h in plan.handles {if !handles.iter().any(|x|x.0==h.0){ensure!(handles.len()<200,"Unresolved handle limit reached");handles.push(h);rep.unresolved+=1;}}
    // Private structured value; accessed via load_setting_json in store.rs.
    tx.execute(INSERT_KV,params![key,serde_json::to_string(&handles)?])?;
    tx.commit()?; Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn strict_urls(){assert_eq!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),Some(("dQw4w9WgXcQ",false)));assert!(video_id("https://www.youtube.com.evil/watch?v=dQw4w9WgXcQ").is_none());assert!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&v=bad").is_none());}
    #[test] fn timestamps(){assert_eq!(timestamp(&json!(1700000000123)).unwrap(),1700000000);assert!(timestamp(&json!(-1)).is_err());}
    #[test] fn handles_are_not_ids(){assert_eq!(channel("https://www.youtube.com/@name").unwrap().1,true);assert!(channel("https://www.youtube.com/@x/path").is_none());}
    #[test] fn rejects_before_db(){let bad=json!({"format":"wetube-library","version":1,"favorites":[],"history":[{"url":"file:///x"}]});assert!(parse(&bad).is_err());}
    fn db()->Connection {
        let c=Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("wetube_import_schema.sql")).unwrap();
        c.execute_batch("INSERT INTO playlists(name,created_at,protected) VALUES('Favorites',1,1),('Watch later',1,1);").unwrap();
        c
    }
    fn legacy()->Value {
        let v=json!({"id":"dQw4w9WgXcQ","url":"https://www.youtube.com/watch?v=dQw4w9WgXcQ","title":"Test","at":1700000000123i64});
        json!({"format":"wetube-collections","version":2,"library":{"format":"wetube-library","version":1,"favorites":[v.clone()],"history":[v.clone()]},"collections":{"later":[v.clone()],"queue":[v.clone()],"playlists":[{"id":"p1","name":"Same","items":[v.clone()]},{"id":"p2","name":"Same","items":[v]}],"subscriptions":[{"url":"https://www.youtube.com/channel/UCabcdefghijklmnopqrstuv","name":"Channel"},{"url":"https://www.youtube.com/@handle","name":"Handle"}]},"searches":["rust"]})
    }
    #[test] fn full_transaction_and_repeat(){
        let mut c=db();let v=legacy();let a=import(&mut c,&v).unwrap();
        assert_eq!((a.history,a.playlist_videos,a.subscriptions,a.unresolved,a.searches),(1,5,1,1,1));
        let b=import(&mut c,&v).unwrap();assert_eq!((b.history,b.playlist_videos,b.subscriptions,b.unresolved,b.searches),(0,0,0,0,0));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM playlists",[],|r|r.get::<_,i64>(0)).unwrap(),5);
        assert_eq!(c.query_row("SELECT watched_at FROM history",[],|r|r.get::<_,i64>(0)).unwrap(),1700000000);
    }
    #[test] fn transaction_failure_rolls_back(){
        let mut c=db();c.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON playlist_items BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(import(&mut c,&legacy()).is_err());
        assert_eq!(c.query_row("SELECT COUNT(*) FROM history",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }

}
