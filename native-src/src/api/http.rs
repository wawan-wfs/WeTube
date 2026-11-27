use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let mut b = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .ip_family(crate::store::ip_family())
            .user_agent(BROWSER_UA);
        if let Some(p) = crate::store::proxy_setting() {
            if let Ok(proxy) = ureq::Proxy::new(&p) {
                b = b.proxy(Some(proxy));
            }
        }
        b.build().into()
    })
}

pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call().with_context(|| format!("GET {url}"))?;
    let status = resp.status().as_u16();
    let mut buf = Vec::new();
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .reader()
        .read_to_end(&mut buf)?;
    if status >= 400 {
        return Err(anyhow!("HTTP {status} for {url}"));
    }
    Ok(buf)
}

pub fn get_text(url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&get_bytes(url, &[])?).into_owned())
}

pub fn get_json(url: &str) -> Result<Value> {
    let b = get_bytes(url, &[("Accept", "application/json")])?;
    serde_json::from_slice(&b).context("invalid json")
}

/// Returns (status, json)
pub fn get_json_status(url: &str) -> Result<(u16, Value)> {
    let mut resp = agent().get(url).header("Accept", "application/json").call()?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

pub fn post_json(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<Value> {
    let mut req = agent().post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req
        .send(serde_json::to_vec(body)?)
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()?;
    if status >= 400 {
        return Err(anyhow!("HTTP {status}: {}", text.chars().take(300).collect::<String>()));
    }
    serde_json::from_str(&text).context("invalid json")
}

/// POST returning the status code and JSON body even for error statuses.
pub fn post_json_status(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<(u16, Value)> {
    let mut req = agent().post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.send(serde_json::to_vec(body)?).with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// Streaming download to a writer with progress callback (downloaded, total).
pub fn download_to(
    url: &str,
    out: &mut dyn std::io::Write,
    mut progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<()> {
    let mut resp = agent().get(url).call()?;
    if resp.status().as_u16() >= 400 {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if !progress(done, total) {
            return Err(anyhow!("cancelled"));
        }
    }
    Ok(())
}

/// Use a fresh authorized media URL. Persist only the strong HTTP validator,
/// representation ID and size, never the URL. Exact 206 required to append.
pub fn download_resumable(url:&str,partial:&std::path::Path,key:&str,itag:u32,
    mut progress:impl FnMut(u64,Option<u64>)->bool)->Result<()> {
    let read_checkpoint=||->Result<Option<crate::wetube_resume::Validator>> {
        crate::store::wetube_kv_read(key)?.filter(|s|!s.is_empty())
            .map(|s|serde_json::from_str::<crate::wetube_resume::Validator>(&s).map_err(Into::into)).transpose()
    };
    // Long downloads use a separate agent: metadata requests retain their 30s deadline.
    let mut cfg=ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(6*3600)))
        // Bound a single body phase. Explicit Retry resumes the durable prefix.
        // This is a total phase deadline, not a true inactivity timer.
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .ip_family(crate::store::ip_family()).user_agent(BROWSER_UA);
    if let Some(p)=crate::store::proxy_setting() {
        if let Ok(proxy)=ureq::Proxy::new(&p){cfg=cfg.proxy(Some(proxy));}
    }
    let media:ureq::Agent=cfg.build().into();
    let started=std::time::Instant::now();
    for _ in 0..180 {
        let prior=read_checkpoint()?;
        let before=prior.as_ref().filter(|v|v.valid(itag)&&v.checkpoint_valid()).map(|v|v.committed).unwrap_or(0);
        let mut cancelled=false;
        let result=transfer_resumable(&media,url,partial,itag,prior,|v| {
            let data=match v {Some(v)=>serde_json::to_string(v)?,None=>String::new()};
            anyhow::ensure!(crate::store::wetube_kv_set(key,&data),"Could not persist media checkpoint");
            Ok(())
        },|done,total|{
            let keep=started.elapsed()<Duration::from_secs(6*3600)&&progress(done,total);
            cancelled=!keep;
            keep
        });
        match result {
            Ok(())=>return Ok(()),
            Err(e)=>{
                if cancelled || started.elapsed()>=Duration::from_secs(6*3600) {return Err(e);}
                // Only a typed I/O timeout with a newly committed strong
                // validator permits automatic continuation. Other errors,
                // provider denials and unknown-length responses stop.
                let timed_out=e.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::TimedOut);
                let advanced=read_checkpoint()?.is_some_and(|v|v.valid(itag)&&v.checkpoint_valid()&&v.committed>before&&v.committed<v.total);
                if !timed_out||!advanced{return Err(e);}
            }
        }
    }
    anyhow::bail!("Media transfer continuation limit reached")
}

/// Dependency-injected checkpoint store and HTTP agent allow true local HTTP tests.
fn transfer_resumable(
    agent:&ureq::Agent,url:&str,partial:&std::path::Path,itag:u32,
    prior:Option<crate::wetube_resume::Validator>,
    mut save:impl FnMut(Option<&crate::wetube_resume::Validator>)->Result<()>,
    mut progress:impl FnMut(u64,Option<u64>)->bool,
)->Result<()> {
    use crate::wetube_resume::{Validator,MAX_BYTES,CHECKPOINT_BYTES,strong_etag,validate_append,hex_digest};
    use sha2::{Digest,Sha256};
    use std::io::{Seek,SeekFrom,Write};
    let mut options=std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let mut file=match std::fs::symlink_metadata(partial) {
        Ok(meta)=>{
            anyhow::ensure!(meta.is_file() && !meta.file_type().is_symlink(),"Unsafe partial file");
            options.open(partial)?
        }
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>options.create_new(true).open(partial)?,
        Err(e)=>return Err(e.into()),
    };
    let existing=file.metadata()?.len();
    anyhow::ensure!(existing<=MAX_BYTES,"Partial download exceeds size limit");
    let mut hash=Sha256::new();
    let mut resume=prior.filter(|v|v.valid(itag)&&v.checkpoint_valid()&&v.committed>0&&v.committed<=existing);
    let mut offset=0;
    if let Some(v)=&resume {
        let mut left=v.committed;
        let mut buf=vec![0u8;256*1024];
        while left>0 {
            let n=left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            hash.update(&buf[..n]);left-=n as u64;
            anyhow::ensure!(progress(v.committed-left,Some(v.total)),"cancelled");
        }
        if hex_digest(hash.clone().finalize())==v.prefix_sha256 {
            offset=v.committed;
            // Only the fsync'd, hashed prefix is trusted. Ignore crash-tail bytes.
            file.set_len(offset)?;
            file.sync_all()?;
        } else {resume=None;hash=Sha256::new();}
    }
    // A completed checkpoint can only be reused after an independent server
    // validation; a full GET is used for that case rather than accepting 416.
    if resume.as_ref().is_some_and(|v|offset==v.total) {resume=None;offset=0;hash=Sha256::new();}
    let mut req=agent.get(url).header("Accept-Encoding","identity");
    if let Some(v)=&resume {
        req=req.header("Range",format!("bytes={offset}-")).header("If-Range",v.etag.as_str());
    }
    let mut resp=req.call().map_err(|_|anyhow!("Media download request failed"))?;
    let status=resp.status().as_u16();
    let header=|name:&str|resp.headers().get(name).and_then(|v|v.to_str().ok()).unwrap_or("").to_owned();
    let etag=header("etag");
    let content_range=header("content-range");
    let encoding=header("content-encoding");
    let length=header("content-length").parse::<u64>().ok();
    anyhow::ensure!(encoding.is_empty()||encoding.eq_ignore_ascii_case("identity"),"Encoded representation cannot be resumed");
    let (total,mut validator)=if status==206 {
        let v=resume.ok_or_else(||anyhow!("Unexpected partial HTTP response"))?;
        anyhow::ensure!(validate_append(&v,offset,status,&content_range,&etag,length),"Resume validator or byte range changed");
        (Some(v.total),Some(v))
    } else if status==200 {
        anyhow::ensure!(length.is_some_and(|n|n>0&&n<=MAX_BYTES),"Native transfer requires a known positive content length");
        save(None)?;file.set_len(0)?;file.sync_all()?;offset=0;hash=Sha256::new();
        let v=length.filter(|_|strong_etag(&etag)).map(|n|Validator{version:2,itag,total:n,etag,committed:0,prefix_sha256:hex_digest(hash.clone().finalize())});
        (length,v)
    } else {return Err(anyhow!("HTTP {status}"));};
    file.seek(SeekFrom::Start(offset))?;
    let mut done=offset;
    let mut durable=offset;
    let mut checkpoint=|file:&std::fs::File,hash:&Sha256,done:u64,v:&mut Option<Validator>|->Result<()> {
        file.sync_all()?;
        if let Some(v)=v {
            v.committed=done;v.prefix_sha256=hex_digest(hash.clone().finalize());
            save(Some(v))?;
        }
        Ok(())
    };
    let result=(||->Result<()> {
        anyhow::ensure!(progress(done,total),"cancelled");
        // The consumer owns all file and journal state. A stalled reader
        // cannot block cancellation or mutate a partial after the worker returns.
        let pump=crate::wetube_transfer::Pump::spawn(move |tx|{
            let mut reader=resp.body_mut().with_config().limit(MAX_BYTES).reader();
            crate::wetube_transfer::read_to_channel(&mut reader,tx);
        })?;
        while let Some(buf)=pump.next(Duration::from_secs(20),||progress(done,total))? {
            let next=done.checked_add(buf.len() as u64).ok_or_else(||anyhow!("Media size overflow"))?;
            anyhow::ensure!(next<=MAX_BYTES&&total.is_none_or(|t|next<=t),"Media response exceeds expected size");
            file.write_all(&buf)?;hash.update(&buf);done=next;
            if done-durable>=CHECKPOINT_BYTES {checkpoint(&file,&hash,done,&mut validator)?;durable=done;}
            anyhow::ensure!(progress(done,total),"cancelled");
        }
        anyhow::ensure!(done>0&&total.is_none_or(|n|done==n),"Incomplete media body");
        Ok(())
    })();
    // On cancellation or network failure, retain only complete written bytes.
    // write_all may fail mid-buffer; do not include that incomplete buffer.
    file.set_len(done)?;
    checkpoint(&file,&hash,done,&mut validator)?;
    result
}

pub fn urlencode(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Decodes %XX escapes and '+' (query strings).
pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}


#[cfg(test)]
mod resume_http_tests {
    use super::*;
    use crate::wetube_resume::Validator;
    use std::{io::{BufRead,BufReader,Write},net::TcpListener,path::PathBuf,cell::RefCell};
    struct FixtureDir(PathBuf);
    impl FixtureDir {
        fn new()->Self {
            let p=std::env::temp_dir().join(format!("wetube-range-test-{:032x}",rand::random::<u128>()));
            std::fs::create_dir(&p).unwrap();Self(p)
        }
    }
    impl Drop for FixtureDir {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
    fn serve(body:Vec<u8>,expected:Option<u64>,bad_range:bool)->(String,std::thread::JoinHandle<()>) {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url=format!("http://{}/fixture",listener.local_addr().unwrap());
        let handle=std::thread::spawn(move||{
            let deadline=std::time::Instant::now()+Duration::from_secs(8);
            let mut sock=loop {
                match listener.accept(){
                    Ok((s,_))=>break s,
                    Err(e) if e.kind()==std::io::ErrorKind::WouldBlock && std::time::Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(10)),
                    Err(e)=>panic!("fixture accept: {e}"),
                }
            };
            sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            sock.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut headers=String::new();
            {
                let mut reader=BufReader::new(&mut sock);
                loop {
                    let mut line=String::new();
                    if reader.read_line(&mut line).unwrap()==0||line=="\r\n"{break;}
                    headers.push_str(&line.to_ascii_lowercase());
                }
            }
            if let Some(off)=expected {
                assert!(headers.contains(&format!("range: bytes={off}-")));
                assert!(headers.contains("if-range: \"fixture-v1\""));
                let payload=&body[off as usize..];
                let range_start=if bad_range{off-1}else{off};
                let head=format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\n",payload.len(),range_start,body.len()-1,body.len());
                sock.write_all(head.as_bytes()).unwrap();
                let _=sock.write_all(payload);
            } else {
                assert!(!headers.contains("\r\nrange:"));
                let head=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\n",body.len());
                sock.write_all(head.as_bytes()).unwrap();
                let _=sock.write_all(&body);
            }
        });
        (url,handle)
    }
    fn local_agent()->ureq::Agent {
        ureq::Agent::config_builder().proxy(None).http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10))).build().into()
    }
    fn payload()->Vec<u8>{(0..(1024*1024)).map(|i|(i%251) as u8).collect()}
    fn interrupted(p:&std::path::Path,data:&[u8])->Validator {
        let (url,server)=serve(data.to_vec(),None,false);
        let saved=RefCell::new(None);
        let result=transfer_resumable(&local_agent(),&url,p,18,None,
            |v|{*saved.borrow_mut()=v.cloned();Ok(())},
            |n,_|n<128*1024);
        server.join().unwrap();
        assert!(result.is_err());
        let v=saved.into_inner().unwrap();
        assert!(v.committed>=128*1024&&v.committed<data.len() as u64);
        assert_eq!(std::fs::metadata(p).unwrap().len(),v.committed);
        v
    }
    #[test]
    fn real_http_cancel_resume_trims_crash_tail() {
        let dir=FixtureDir::new();let p=dir.0.join("partial");let data=payload();
        let v=interrupted(&p,&data);
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"uncommitted").unwrap();
        let (url,server)=serve(data.clone(),Some(v.committed),false);
        transfer_resumable(&local_agent(),&url,&p,18,Some(v),|_|Ok(()),|_,_|true).unwrap();
        server.join().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(),data);
    }
    #[test]
    fn real_http_wrong_range_rejected_without_append() {
        let dir=FixtureDir::new();let p=dir.0.join("partial");let data=payload();
        let v=interrupted(&p,&data);let before=std::fs::read(&p).unwrap();
        let (url,server)=serve(data,Some(v.committed),true);
        assert!(transfer_resumable(&local_agent(),&url,&p,18,Some(v),|_|Ok(()),|_,_|true).is_err());
        server.join().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(),before);
    }
    #[test]
    fn real_http_corrupt_prefix_restarts() {
        let dir=FixtureDir::new();let p=dir.0.join("partial");let data=payload();
        let v=interrupted(&p,&data);
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().write_all(b"corrupt").unwrap();
        let (url,server)=serve(data.clone(),None,false);
        transfer_resumable(&local_agent(),&url,&p,18,Some(v),|_|Ok(()),|_,_|true).unwrap();
        server.join().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(),data);
    }
}
