//! Pure validation for exact byte range recovery.
use serde::{Deserialize,Serialize};
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Validator { pub version:u8,pub itag:u32,pub total:u64,pub etag:String }
pub const MAX_BYTES:u64=32*1024*1024*1024;
pub fn strong_etag(s:&str)->bool {
    s.len()>=2 && s.len()<=512 && s.starts_with('"') && s.ends_with('"')
        && s[1..s.len()-1].bytes().all(|b| (0x21..=0x7e).contains(&b) && b!=b'"')
}
impl Validator {
    pub fn valid(&self,itag:u32)->bool {
        self.version==1 && self.itag==itag && self.total>0 && self.total<=MAX_BYTES && strong_etag(&self.etag)
    }
}
pub fn range_matches(value:&str,offset:u64,total:u64)->bool {
    if offset>=total || total==0{return false;}
    let Some((bounds,size))=value.strip_prefix("bytes ").and_then(|s|s.split_once('/')) else{return false;};
    let Some((start,end))=bounds.split_once('-') else{return false;};
    start.parse::<u64>().ok()==Some(offset) && end.parse::<u64>().ok()==Some(total-1) && size.parse::<u64>().ok()==Some(total)
}
pub fn validate_append(v:&Validator,offset:u64,status:u16,range:&str,etag:&str,length:Option<u64>)->bool {
    v.valid(v.itag) && status==206 && etag==v.etag && range_matches(range,offset,v.total) && length.is_none_or(|n|n==v.total-offset)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn v()->Validator{Validator{version:1,itag:18,total:1000,etag:"\"abc\"".into()}}
    #[test] fn exact_range(){assert!(validate_append(&v(),100,206,"bytes 100-999/1000","\"abc\"",Some(900)));}
    #[test] fn changed_or_wrong_range(){
        for (status,range,tag,len) in [(200,"bytes 100-999/1000","\"abc\"",Some(900)),(206,"bytes 99-999/1000","\"abc\"",Some(901)),(206,"bytes 100-998/1000","\"abc\"",Some(899)),(206,"bytes 100-999/1001","\"abc\"",Some(900)),(206,"bytes 100-999/1000","\"changed\"",Some(900)),(206,"bytes 100-999/1000","\"abc\"",Some(901))] {
            assert!(!validate_append(&v(),100,status,range,tag,len));
        }
    }
    #[test] fn invalid_validator(){
        for t in ["W/\"abc\"","abc","\"a\nb\"","\"a\"b\""]{assert!(!strong_etag(t));}
        assert!(!range_matches("bytes 0-0/0",0,0));
        assert!(!range_matches("bytes 0-1/2",2,2));
        let mut m=v();m.total=MAX_BYTES+1;assert!(!m.valid(18));
    }
}
