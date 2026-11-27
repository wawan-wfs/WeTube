//! Bounded body-reader channel. The download worker can checkpoint and return
//! on cancellation/idle even while the network reader is blocked. The HTTP
//! client's own phase deadline bounds the lifetime of the abandoned reader.
use std::{io::{self,Read},sync::{atomic::{AtomicUsize,Ordering},mpsc::{self,Receiver,SyncSender,RecvTimeoutError}},thread,time::{Duration,Instant}};
const MAX_READERS:usize=6;
const CHUNK_BYTES:usize=256*1024;
static READERS:AtomicUsize=AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn acquire()->io::Result<Self>{
        READERS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n<MAX_READERS).then_some(n+1))
            .map(|_|Self).map_err(|_|io::Error::other("Media readers are busy; retry after existing readers exit"))
    }
}
impl Drop for Permit {fn drop(&mut self){READERS.fetch_sub(1,Ordering::AcqRel);}}
pub enum Message{Chunk(Vec<u8>),Eof,Error(io::Error)}
pub struct Pump{rx:Receiver<Message>}
impl Pump {
    pub fn spawn(producer:impl FnOnce(SyncSender<Message>)+Send+'static)->io::Result<Self>{
        let permit=Permit::acquire()?;
        let(tx,rx)=mpsc::sync_channel(2);
        thread::Builder::new().name("wetube-media-body".into()).spawn(move||{
            let _permit=permit;
            producer(tx);
        })?;
        Ok(Self{rx})
    }
    pub fn next(&self,idle:Duration,mut keep:impl FnMut()->bool)->io::Result<Option<Vec<u8>>>{
        let deadline=Instant::now()+idle;
        loop {
            if !keep(){return Err(io::Error::new(io::ErrorKind::Interrupted,"cancelled"));}
            let remaining=deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero(){return Err(io::Error::new(io::ErrorKind::TimedOut,"Media body idle timeout"));}
            match self.rx.recv_timeout(remaining.min(Duration::from_millis(100))){
                Ok(Message::Chunk(data)) if !data.is_empty()=>return Ok(Some(data)),
                Ok(Message::Chunk(_))=>return Err(io::Error::other("Empty media chunk")),
                Ok(Message::Eof)=>return Ok(None),
                Ok(Message::Error(e))=>return Err(e),
                Err(RecvTimeoutError::Timeout)=>continue,
                Err(RecvTimeoutError::Disconnected)=>return Err(io::Error::other("Media body reader exited without EOF")),
            }
        }
    }
}
pub fn read_to_channel(reader:&mut impl Read,tx:SyncSender<Message>){
    loop {
        let mut buf=vec![0u8;CHUNK_BYTES];
        match reader.read(&mut buf){
            Ok(0)=>{let _=tx.send(Message::Eof);return;}
            Ok(n)=>{buf.truncate(n);if tx.send(Message::Chunk(buf)).is_err(){return;}}
            Err(e) if e.kind()==io::ErrorKind::Interrupted=>continue,
            Err(e)=>{let _=tx.send(Message::Error(e));return;}
        }
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn bounded_channel_preserves_bytes_and_eof(){
        let bytes=vec![7u8;CHUNK_BYTES*3+11];let expected=bytes.clone();
        let pump=Pump::spawn(move|tx|read_to_channel(&mut io::Cursor::new(bytes),tx)).unwrap();
        let mut actual=Vec::new();
        while let Some(b)=pump.next(Duration::from_secs(5),||true).unwrap(){assert!(b.len()<=CHUNK_BYTES);actual.extend(b);}
        assert_eq!(actual,expected);
    }
    #[test]
    fn idle_and_cancel_do_not_wait_for_producer(){
        let (release,hold)=mpsc::channel::<()>();
        let pump=Pump::spawn(move|_tx|{let _=hold.recv_timeout(Duration::from_secs(5));}).unwrap();
        let start=Instant::now();
        let err=pump.next(Duration::from_millis(80),||true).unwrap_err();
        assert_eq!(err.kind(),io::ErrorKind::TimedOut);
        assert!(start.elapsed()<Duration::from_secs(2));
        let err=pump.next(Duration::from_secs(10),||false).unwrap_err();
        assert_eq!(err.kind(),io::ErrorKind::Interrupted);
        drop(pump);let _=release.send(());
    }
    #[test]
    fn unexpected_reader_exit_is_not_eof(){
        let pump=Pump::spawn(|_tx|{}).unwrap();
        assert!(pump.next(Duration::from_secs(2),||true).is_err());
    }
}
