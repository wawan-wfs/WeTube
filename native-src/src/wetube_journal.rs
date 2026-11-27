//! GPUI-independent durable download journal. All production operations use
//! the same SQLite transactions exercised by the portable core harness.
use anyhow::{ensure,Result};
use rusqlite::{Connection,params};

const MARKER:&str="wetube_download_journal_v2";
const ROW_LIMIT:usize=64*1024;
const COUNT_LIMIT:usize=50_000;
const BYTE_LIMIT:usize=64*1024*1024;
const SQL_STATS:&str=r#"SELECT COUNT(*),COALESCE(SUM(length(CAST(json AS BLOB))),0),COALESCE(MAX(length(CAST(json AS BLOB))),0) FROM wetube_download_jobs"#;
const SQL_INSERT:&str=r#"INSERT INTO wetube_download_jobs(job_id,ordinal,json) SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2 FROM wetube_download_jobs"#;
const SQL_UPDATE:&str=r#"UPDATE wetube_download_jobs SET json=?2 WHERE job_id=?1"#;
const SQL_CLEAR_TERMINAL:&str=r#"DELETE FROM kv WHERE key IN (?1,?2)"#;
const SQL_TEMP:&str=r#"CREATE TEMP TABLE IF NOT EXISTS wetube_seen_jobs(job_id TEXT PRIMARY KEY); DELETE FROM wetube_seen_jobs;"#;
const SQL_SEEN:&str=r#"INSERT INTO wetube_seen_jobs(job_id) VALUES(?1)"#;
const SQL_UPSERT:&str=r#"INSERT INTO wetube_download_jobs(job_id,ordinal,json) VALUES(?1,?2,?3)
ON CONFLICT(job_id) DO UPDATE SET ordinal=excluded.ordinal,json=excluded.json
WHERE wetube_download_jobs.ordinal<>excluded.ordinal OR wetube_download_jobs.json<>excluded.json"#;
const SQL_CLEAN:&str=r#"DELETE FROM kv WHERE key IN (SELECT 'wetube_publish_'||job_id FROM wetube_download_jobs WHERE job_id NOT IN (SELECT job_id FROM wetube_seen_jobs)) OR key IN (SELECT 'wetube_resume_'||job_id FROM wetube_download_jobs WHERE job_id NOT IN (SELECT job_id FROM wetube_seen_jobs))"#;
const SQL_PRUNE:&str=r#"DELETE FROM wetube_download_jobs WHERE job_id NOT IN (SELECT job_id FROM wetube_seen_jobs)"#;
const SQL_MARK:&str=r#"INSERT OR IGNORE INTO kv(key,value) VALUES(?1,'2')"#;
const SQL_LEGACY:&str=r#"DELETE FROM kv WHERE key='download_history'"#;
const SQL_READ:&str=r#"SELECT job_id, CASE WHEN length(CAST(json AS BLOB))<=?1 THEN json ELSE NULL END FROM wetube_download_jobs ORDER BY ordinal,job_id"#;

fn valid_id(id:&str)->bool {
    id.len()==32&&id.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))
}
fn marked(c:&Connection)->Result<bool>{
    use rusqlite::OptionalExtension;
    let version:Option<String>=c.query_row("SELECT value FROM kv WHERE key=?1",[MARKER],|r|r.get(0)).optional()?;
    match version.as_deref(){None=>Ok(false),Some("2")=>Ok(true),_=>anyhow::bail!("Unsupported download journal version")}
}
fn bounded(count:i64,bytes:i64,max:i64)->Result<()>{
    ensure!((0..=COUNT_LIMIT as i64).contains(&count),"Download journal exceeds row limit");
    ensure!((0..=BYTE_LIMIT as i64).contains(&bytes),"Download journal exceeds aggregate byte budget");
    ensure!((0..=ROW_LIMIT as i64).contains(&max),"Oversized download journal row");
    Ok(())
}
fn stats(c:&Connection)->Result<(i64,i64,i64)>{
    let (n,b,m)=c.query_row(SQL_STATS,[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    bounded(n,b,m)?;Ok((n,b,m))
}
fn validate(id:&str,payload:&str)->Result<()>{
    ensure!(valid_id(id)&&payload.len()<=ROW_LIMIT,"Invalid download journal row");
    Ok(())
}
pub fn read_rows(c:&Connection)->Result<Option<Vec<(String,String)>>>{
    // A stable read transaction prevents a concurrent writer changing size or
    // identity between the budget check and materialization.
    let tx=c.unchecked_transaction()?;
    let rows=read_snapshot(&tx)?;
    tx.commit()?;
    Ok(rows)
}
fn read_snapshot(c:&Connection)->Result<Option<Vec<(String,String)>>>{
    // Check aggregate size before allocating or loading any JSON.
    let (n,_,_)=stats(c)?;
    if !marked(c)?{
        ensure!(n==0,"Unmarked download journal is not empty");
        return Ok(None);
    }
    let mut st=c.prepare(SQL_READ)?;let mut rows=st.query([ROW_LIMIT as i64])?;
    let mut result=Vec::with_capacity(n as usize);
    while let Some(row)=rows.next()?{
        let id:String=row.get(0)?;let payload:Option<String>=row.get(1)?;
        let payload=payload.ok_or_else(||anyhow::anyhow!("Oversized download row"))?;
        validate(&id,&payload)?;result.push((id,payload));
    }
    Ok(Some(result))
}
pub fn insert_row(c:&mut Connection,id:&str,payload:&str)->Result<()>{
    validate(id,payload)?;
    let tx=c.transaction()?;
    ensure!(marked(&tx)?,"Download journal migration not committed");
    let(n,b,m)=stats(&tx)?;
    bounded(n+1,b+payload.len() as i64,m.max(payload.len() as i64))?;
    tx.execute(SQL_INSERT,params![id,payload])?;
    tx.commit()?;Ok(())
}
pub fn update_row(c:&mut Connection,id:&str,payload:&str,done:bool)->Result<()>{
    validate(id,payload)?;
    let tx=c.transaction()?;
    ensure!(marked(&tx)?,"Download journal migration not committed");
    let old:i64=tx.query_row("SELECT length(CAST(json AS BLOB)) FROM wetube_download_jobs WHERE job_id=?1",[id],|r|r.get(0))?;
    // Ordinary equal/smaller transitions need no full journal scan.
    if payload.len() as i64>old {
        let(n,b,m)=stats(&tx)?;
        bounded(n,b-old+payload.len() as i64,m.max(payload.len() as i64))?;
    }
    ensure!(tx.execute(SQL_UPDATE,params![id,payload])?==1,"Download job not found in journal");
    if done{tx.execute(SQL_CLEAR_TERMINAL,params![format!("wetube_publish_{id}"),format!("wetube_resume_{id}")])?;}
    tx.commit()?;Ok(())
}
/// Remove selected identities and their recovery metadata atomically. Missing
/// rows reject the entire operation; other workers' rows are never rewritten.
pub fn remove_rows(c:&mut Connection,ids:&[String])->Result<()> {
    ensure!(ids.len()<=COUNT_LIMIT,"Too many removal identities");
    let tx=c.transaction()?;
    ensure!(marked(&tx)?,"Download journal migration not committed");
    for id in ids {
        ensure!(valid_id(id),"Invalid download identity");
        ensure!(tx.execute("DELETE FROM wetube_download_jobs WHERE job_id=?1",[id])?==1,"Download job not found in journal");
        tx.execute(SQL_CLEAR_TERMINAL,params![format!("wetube_publish_{id}"),format!("wetube_resume_{id}")])?;
    }
    tx.commit()?;Ok(())
}
/// Replace the identity for an explicit clean retry, preserving display order
/// and atomically clearing only the old job's recovery metadata.
pub fn replace_identity(c:&mut Connection,old:&str,new:&str,payload:&str)->Result<()> {
    validate(new,payload)?;
    ensure!(valid_id(old)&&old!=new,"Invalid replacement identity");
    let tx=c.transaction()?;
    ensure!(marked(&tx)?,"Download journal migration not committed");
    let old_len:i64=tx.query_row("SELECT length(CAST(json AS BLOB)) FROM wetube_download_jobs WHERE job_id=?1",[old],|r|r.get(0))?;
    let(n,b,m)=stats(&tx)?;
    bounded(n,b-old_len+payload.len() as i64,m.max(payload.len() as i64))?;
    ensure!(tx.execute("UPDATE wetube_download_jobs SET job_id=?2,json=?3 WHERE job_id=?1",params![old,new,payload])?==1,"Download job not found in journal");
    tx.execute(SQL_CLEAR_TERMINAL,params![format!("wetube_publish_{old}"),format!("wetube_resume_{old}")])?;
    tx.commit()?;Ok(())
}
pub fn write_rows(c:&mut Connection,rows:&[(String,String)])->Result<()>{
    // Validate proposed snapshot before starting a write transaction.
    let mut total=0usize;let mut max=0usize;
    for(id,payload)in rows{
        validate(id,payload)?;
        total=total.checked_add(payload.len()).ok_or_else(||anyhow::anyhow!("Journal size overflow"))?;
        max=max.max(payload.len());
        ensure!(total<=BYTE_LIMIT,"Download journal exceeds aggregate byte budget");
    }
    bounded(rows.len() as i64,total as i64,max as i64)?;
    let tx=c.transaction()?;
    // Do not silently downgrade or overwrite an unknown journal version.
    marked(&tx)?;
    tx.execute_batch(SQL_TEMP)?;
    {
        let mut seen=tx.prepare_cached(SQL_SEEN)?;
        let mut up=tx.prepare_cached(SQL_UPSERT)?;
        for(i,(id,payload))in rows.iter().enumerate(){
            seen.execute([id])?;
            up.execute(params![id,i as i64,payload])?;
        }
    }
    tx.execute(SQL_CLEAN,[])?;
    tx.execute(SQL_PRUNE,[])?;
    tx.execute(SQL_MARK,[MARKER])?;
    tx.execute(SQL_LEGACY,[])?;
    tx.commit()?;Ok(())
}
#[cfg(test)]
mod download_row_tests{
    use super::*;
    fn db()->Connection{
        let c=Connection::open_in_memory().unwrap();
        c.execute_batch(r#"CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS wetube_download_jobs (job_id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, json TEXT NOT NULL);
     CREATE INDEX IF NOT EXISTS wetube_download_order ON wetube_download_jobs(ordinal,job_id);"#).unwrap();c
    }
    #[test]
    fn migration_marker_and_rollback(){
        let mut c=db();let a="a".repeat(32);let b="b".repeat(32);
        assert!(read_rows(&c).unwrap().is_none());
        assert!(insert_row(&mut c,&a,"unmigrated").is_err());
        c.execute("INSERT INTO kv VALUES('download_history','legacy private history')",[]).unwrap();
        write_rows(&mut c,&[(a.clone(),"one".into()),(b.clone(),"two".into())]).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM kv WHERE key='download_history'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        c.execute("INSERT INTO kv VALUES(?1,'intent')",[format!("wetube_publish_{a}")]).unwrap();
        assert!(write_rows(&mut c,&[(a.clone(),"changed".into()),(a.clone(),"duplicate".into())]).is_err());
        let old:String=c.query_row("SELECT json FROM wetube_download_jobs WHERE job_id=?1",[&a],|r|r.get(0)).unwrap();
        assert_eq!(old,"one");
        update_row(&mut c,&a,"new",false).unwrap();
        assert!(update_row(&mut c,&"c".repeat(32),"missing",false).is_err());
        let unchanged:String=c.query_row("SELECT json FROM wetube_download_jobs WHERE job_id=?1",[&b],|r|r.get(0)).unwrap();
        assert_eq!(unchanged,"two");
        write_rows(&mut c,&[]).unwrap();
        assert_eq!(read_rows(&c).unwrap(),Some(vec![]));
        assert_eq!(c.query_row("SELECT COUNT(*) FROM kv WHERE key=?1",[format!("wetube_publish_{a}")],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn selected_removal_and_identity_replacement(){
        let mut c=db();let a="a".repeat(32);let b="b".repeat(32);let d="d".repeat(32);
        write_rows(&mut c,&[(a.clone(),"one".into()),(b.clone(),"two".into())]).unwrap();
        for k in [format!("wetube_publish_{a}"),format!("wetube_resume_{b}")]{c.execute("INSERT INTO kv VALUES(?1,'intent')",[k]).unwrap();}
        assert!(replace_identity(&mut c,&a,&b,"collision").is_err());
        assert!(remove_rows(&mut c,&[a.clone(),d.clone()]).is_err());
        assert_eq!(read_rows(&c).unwrap().unwrap().len(),2);
        replace_identity(&mut c,&a,&d,"new").unwrap();
        assert_eq!(read_rows(&c).unwrap().unwrap(),vec![(d.clone(),"new".into()),(b.clone(),"two".into())]);
        assert!(update_row(&mut c,&a,"late",false).is_err());
        assert_eq!(c.query_row("SELECT COUNT(*) FROM kv WHERE key=?1",[format!("wetube_publish_{a}")],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM kv WHERE key=?1",[format!("wetube_resume_{b}")],|r|r.get::<_,i64>(0)).unwrap(),1);
        remove_rows(&mut c,&[b]).unwrap();
        assert_eq!(read_rows(&c).unwrap().unwrap(),vec![(d,"new".into())]);
    }
    #[test]
    fn future_marker_is_fail_closed(){
        let mut c=db();let id="a".repeat(32);
        write_rows(&mut c,&[(id.clone(),"one".into())]).unwrap();
        c.execute("UPDATE kv SET value='99' WHERE key=?1",[MARKER]).unwrap();
        assert!(read_rows(&c).is_err());assert!(insert_row(&mut c,&"b".repeat(32),"new").is_err());
        assert!(write_rows(&mut c,&[]).is_err());assert!(remove_rows(&mut c,&[id.clone()]).is_err());
        assert!(update_row(&mut c,&id,"changed",true).is_err());
        assert_eq!(c.query_row("SELECT json FROM wetube_download_jobs WHERE job_id=?1",[id],|r|r.get::<_,String>(0)).unwrap(),"one");
    }
    #[test]
    fn byte_budget_and_atomic_completion(){
        assert!(bounded(1,BYTE_LIMIT as i64+1,ROW_LIMIT as i64).is_err());
        assert!(bounded(COUNT_LIMIT as i64+1,0,0).is_err());
        let mut c=db();let id="a".repeat(32);
        write_rows(&mut c,&[]).unwrap();
        assert!(insert_row(&mut c,&id,&"x".repeat(ROW_LIMIT+1)).is_err());
        insert_row(&mut c,&id,"queued").unwrap();
        for k in [format!("wetube_publish_{id}"),format!("wetube_resume_{id}")]{c.execute("INSERT INTO kv VALUES(?1,'intent')",[k]).unwrap();}
        update_row(&mut c,&id,"done",true).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM kv WHERE key LIKE 'wetube_publish_%' OR key LIKE 'wetube_resume_%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(read_rows(&c).unwrap().unwrap()[0].1,"done");
        // Directly corrupted oversized row is rejected before materialization.
        c.execute(SQL_UPDATE,params![id,"x".repeat(ROW_LIMIT+1)]).unwrap();
        assert!(read_rows(&c).is_err());
    }
}
