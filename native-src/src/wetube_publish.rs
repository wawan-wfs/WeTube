//! A durable publication intent bridges the filesystem rename and the SQL
//! terminal-state commit. Recovery validates actual bytes, never guesses by name.
use crate::{downloads::{Job,commit_download,valid_durable_id},store};
use anyhow::{Result,ensure,anyhow};
use serde::{Serialize,Deserialize};
use sha2::{Digest,Sha256};
use std::{path::{Path,PathBuf,Component},io::Read,sync::atomic::{AtomicBool,Ordering}};
#[derive(Serialize,Deserialize,Debug)]
#[serde(deny_unknown_fields)]
struct Intent { version:u8, job:String, target:String, source:String, staged:bool, size:u64, digest:String }
fn key(id:&str)->String{format!("wetube_publish_{id}")}
fn name(s:&str)->bool{
    !s.is_empty()&&s.len()<=1024&&!s.contains(['\\','/',':','\0'])&&
    matches!((Path::new(s).components().next(),Path::new(s).components().nth(1)),(Some(Component::Normal(_)),None))
}
fn paths(root:&Path,i:&Intent,id:&str)->Result<(PathBuf,PathBuf)>{
    ensure!(i.version==1&&i.job==id&&valid_durable_id(id)&&name(&i.target)&&name(&i.source)&&i.size>0&&i.size<=crate::wetube_resume::MAX_BYTES&&i.digest.len()==64&&i.digest.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)),"Invalid publication intent");
    ensure!(!i.target.starts_with(".wetube-"),"Reserved output filename");
    let src=if i.staged{
        let stage=root.join(format!(".wetube-job-{id}"));
        if stage.exists(){
            let meta=std::fs::symlink_metadata(&stage)?;
            ensure!(meta.is_dir()&&!meta.file_type().is_symlink()&&std::fs::canonicalize(&stage)?.parent()==Some(root),"Unsafe publication stage");
        }
        stage.join(&i.source)
    }else{
        ensure!(i.source==format!(".wetube-{id}.part"),"Wrong native partial");
        root.join(&i.source)
    };
    let dst=root.join(&i.target);
    ensure!(src!=dst,"Publication source equals target");
    Ok((src,dst))
}
fn fingerprint(root:&Path,path:&Path,cancel:&AtomicBool)->Result<(u64,String)>{
    let meta=std::fs::symlink_metadata(path)?;
    ensure!(meta.is_file()&&!meta.file_type().is_symlink()&&std::fs::canonicalize(path)?.starts_with(root),"Unsafe output file");
    let mut file=std::fs::File::open(path)?;
    let size=file.metadata()?.len();
    ensure!(size>0&&size<=crate::wetube_resume::MAX_BYTES,"Invalid output size");
    let mut hash=Sha256::new();let mut buf=vec![0u8;256*1024];let mut done=0u64;
    loop {
        ensure!(!cancel.load(Ordering::SeqCst),"cancelled");
        let n=file.read(&mut buf)?;if n==0{break;}
        done=done.checked_add(n as u64).ok_or_else(||anyhow!("Output size overflow"))?;
        ensure!(done<=size,"Output changed during verification");hash.update(&buf[..n]);
    }
    ensure!(done==size,"Output changed during verification");
    Ok((size,crate::wetube_resume::hex_digest(hash.finalize())))
}
fn matches(root:&Path,path:&Path,i:&Intent,cancel:&AtomicBool)->Result<bool>{
    if !path.try_exists()? {return Ok(false);}
    let (size,digest)=fingerprint(root,path,cancel)?;
    Ok(size==i.size&&digest==i.digest)
}
fn finish(root:&Path,i:&Intent,cancel:&AtomicBool)->Result<String>{
    let (src,dst)=paths(root,i,&i.job)?;
    if dst.try_exists()? {
        ensure!(matches(root,&dst,i,cancel)?,"Existing destination differs from publication intent; never overwritten");
    }else{
        ensure!(matches(root,&src,i,cancel)?,"Verified staged output missing or changed");
        ensure!(!cancel.load(Ordering::SeqCst),"cancelled");
        commit_download(&src,&dst)?;
        ensure!(matches(root,&dst,i,cancel)?,"Published file failed verification");
    }
    // Non-Windows hard-link publication leaves the source behind. Preserve it
    // here rather than deleting a path that might have been replaced.
    Ok(dst.to_string_lossy().into_owned())
}
pub fn recover(job:&Job,dir:&Path,cancel:&AtomicBool)->Result<Option<String>>{
    let Some(raw)=store::wetube_kv_read(&key(&job.durable_id))?.filter(|v|!v.is_empty()) else{return Ok(None);};
    let i:Intent=serde_json::from_str(&raw)?;
    let root=std::fs::canonicalize(dir)?;
    paths(&root,&i,&job.durable_id)?;
    Ok(Some(finish(&root,&i,cancel)?))
}
pub fn publish(job:&Job,dir:&Path,source:&Path,target:&Path,cancel:&AtomicBool)->Result<String>{
    let root=std::fs::canonicalize(dir)?;
    let source=std::fs::canonicalize(source)?;
    ensure!(source.starts_with(&root),"Publication outside destination");
    let staged=source.parent()!=Some(root.as_path());
    let filename=|p:&Path|p.file_name().and_then(|s|s.to_str()).map(str::to_owned).ok_or_else(||anyhow!("Invalid output filename"));
    let (size,digest)=fingerprint(&root,&source,cancel)?;
    std::fs::OpenOptions::new().write(true).open(&source)?.sync_all()?;
    let i=Intent{version:1,job:job.durable_id.clone(),target:filename(target)?,source:filename(&source)?,staged,size,digest};
    let (expected,_)=paths(&root,&i,&job.durable_id)?;
    ensure!(expected==source,"Publication source does not belong to job");
    ensure!(store::wetube_kv_set(&key(&job.durable_id),&serde_json::to_string(&i)?),"Could not persist publication intent");
    finish(&root,&i,cancel)
}
pub fn has_intent(id:&str)->Result<bool>{Ok(store::wetube_kv_read(&key(id))?.is_some_and(|v|!v.is_empty()))}
pub fn forget(id:&str){if valid_durable_id(id){let _=store::wetube_kv_set(&key(id),"");}}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_restart_and_collision(){
        let root=std::env::temp_dir().join(format!("wetube-publish-{:032x}",rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();let root=std::fs::canonicalize(root).unwrap();
        let id="a".repeat(32);let source=format!(".wetube-{id}.part");
        let src=root.join(&source);let dst=root.join("video.mp4");std::fs::write(&src,b"original bytes").unwrap();
        let cancel=AtomicBool::new(false);
        let(size,digest)=fingerprint(&root,&src,&cancel).unwrap();
        let i=Intent{version:1,job:id,target:"video.mp4".into(),source,staged:false,size,digest};
        std::fs::write(&dst,b"other bytes").unwrap();
        assert!(finish(&root,&i,&cancel).is_err());assert_eq!(std::fs::read(&dst).unwrap(),b"other bytes");
        std::fs::remove_file(&dst).unwrap();finish(&root,&i,&cancel).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(),b"original bytes");
        assert!(finish(&root,&i,&cancel).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]fn rejects_paths(){for s in ["","..","a/b","a\\b","C:video"]{assert!(!name(s));}}
}
