//! Bounded collection reads with a consistent SQLite snapshot. The local-name
//! scan preserves Unicode matching; expensive item/thumbnail reads are limited
//! to the selected page. No migration, no remote queries.
use anyhow::Result;
use rusqlite::Connection;
#[cfg(test)]use rusqlite::params;
use crate::{api::types::{VideoItem,PlaylistItem},wetube_catalog::{Window,PageCollector}};
#[derive(Clone,Debug,PartialEq)]
pub struct LocalPlaylist{pub id:i64,pub name:String,pub protected:bool,pub count:usize,pub thumbnail:Option<String>}
pub struct LocalPage{pub window:Window,pub rows:Vec<LocalPlaylist>,pub playlists:usize,pub entries:usize}
pub fn local_page(c:&Connection,query:&str,requested:usize,size:usize)->Result<LocalPage>{
    let tx=c.unchecked_transaction()?;
    let q=query.to_lowercase();let mut collector=PageCollector::new(requested,size);
    let mut playlists=0usize;
    {
        let mut stmt=tx.prepare_cached("SELECT id,name,protected FROM playlists ORDER BY protected DESC,created_at,id")?;
        let mut rows=stmt.query([])?;
        while let Some(row)=rows.next()?{
            let id:i64=row.get(0)?;let name:String=row.get(1)?;let protected:i64=row.get(2)?;
            playlists=playlists.saturating_add(1);
            if name.to_lowercase().contains(&q){collector.push((id,name,protected!=0));}
        }
    }
    let(window,selected)=collector.finish();
    let entries:i64=tx.query_row("SELECT COUNT(*) FROM playlist_items i WHERE EXISTS(SELECT 1 FROM playlists p WHERE p.id=i.playlist_id)",[],|r|r.get(0))?;
    let mut rows=Vec::with_capacity(selected.len());
    {
        let mut count=tx.prepare_cached("SELECT COUNT(*) FROM playlist_items WHERE playlist_id=?1")?;
        let mut first=tx.prepare_cached("SELECT CASE WHEN length(CAST(json AS BLOB))<=1048576 THEN json ELSE NULL END FROM playlist_items WHERE playlist_id=?1 ORDER BY position,video_id LIMIT 1")?;
        for(id,name,protected)in selected{
            let n:i64=count.query_row([id],|r|r.get(0))?;
            let mut it=first.query([id])?;
            let thumb=if let Some(row)=it.next()?{row.get::<_,Option<String>>(0)?.and_then(|s|serde_json::from_str::<VideoItem>(&s).ok()).map(|v|v.thumbnail)}else{None};
            rows.push(LocalPlaylist{id,name,protected,count:n.max(0) as usize,thumbnail:thumb});
        }
    }
    tx.commit()?;Ok(LocalPage{window,rows,playlists,entries:entries.max(0) as usize})
}
pub fn saved_page(c:&Connection,query:&str,requested:usize,size:usize)->Result<(Window,Vec<PlaylistItem>)>{
    let q=query.to_lowercase();let mut collector=PageCollector::new(requested,size);
    let mut stmt=c.prepare_cached("SELECT playlist_id,CASE WHEN length(CAST(json AS BLOB))<=1048576 THEN json ELSE NULL END FROM saved_playlists ORDER BY added_at DESC,playlist_id")?;
    let mut rows=stmt.query([])?;
    while let Some(row)=rows.next()?{
        let id:String=row.get(0)?;let raw:Option<String>=row.get(1)?;
        let Some(v)=raw.and_then(|s|serde_json::from_str::<PlaylistItem>(&s).ok())else{continue;};
        if v.id==id&&v.title.to_lowercase().contains(&q){collector.push(v);}
    }
    Ok(collector.finish())
}
#[cfg(test)]mod tests{
    use super::*;
    fn db()->Connection{let c=Connection::open_in_memory().unwrap();c.execute_batch("CREATE TABLE playlists(id INTEGER PRIMARY KEY,name TEXT NOT NULL,created_at INTEGER NOT NULL,protected INTEGER NOT NULL);CREATE TABLE playlist_items(playlist_id INTEGER NOT NULL,video_id TEXT NOT NULL,json TEXT NOT NULL,position INTEGER NOT NULL);CREATE INDEX items ON playlist_items(playlist_id,position,video_id);CREATE TABLE saved_playlists(playlist_id TEXT PRIMARY KEY,json TEXT NOT NULL,added_at INTEGER NOT NULL);").unwrap();c}
    #[test]fn local_page_scales_and_preserves_unicode_order(){
        let mut c=db();{let tx=c.transaction().unwrap();for i in 0..10000{tx.execute("INSERT INTO playlists VALUES(?1,?2,1,?3)",params![i,if i%2==0{"Épisode"}else{"Other"},if i==9998{1}else{0}]).unwrap();tx.execute("INSERT INTO playlist_items VALUES(?1,'vid','{}',0)",[i]).unwrap();}tx.commit().unwrap();}
        let p=local_page(&c,"éPISODE",0,48).unwrap();assert_eq!(p.playlists,10000);assert_eq!(p.entries,10000);assert_eq!(p.window.total,5000);assert_eq!(p.rows[0].id,9998);assert_eq!(p.rows[1].id,0);assert!(p.rows.iter().all(|p|p.count==1));
        let p=local_page(&c,"épisode",usize::MAX,48).unwrap();assert_eq!(p.rows.len(),8);assert_eq!(p.window.page,104);assert_eq!(p.rows.last().unwrap().id,9996);
        assert!(local_page(&c,"absent",9,48).unwrap().rows.is_empty());
    }
    #[test]fn saved_page_skips_corrupt_and_mismatched_metadata(){
        let c=db();for i in 0..101{let id=format!("PL{i}");let item=PlaylistItem{id:id.clone(),title:format!("Épisode {i}"),..Default::default()};c.execute("INSERT INTO saved_playlists VALUES(?1,?2,?3)",params![id,serde_json::to_string(&item).unwrap(),i]).unwrap();}
        c.execute("INSERT INTO saved_playlists VALUES('bad','{}',9999)",[]).unwrap();
        let(w,v)=saved_page(&c,"épisode",999,48).unwrap();assert_eq!(w.total,101);assert_eq!(v.len(),5);assert_eq!(v.last().unwrap().id,"PL0");
    }
}
