//! Track libmpv async command replies. A queued command is not an executed
//! command. Only the latest tagged reply for its category is actionable.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum Kind{Load,Seek}
#[derive(Default)]pub struct Commands{next:u64,load:Option<u64>,seek:Option<u64>}
impl Commands{
    pub fn issue(&mut self,kind:Kind)->u64{self.next=self.next.wrapping_add(1).max(1);let id=self.next;match kind{Kind::Load=>{self.load=Some(id);self.seek=None;},Kind::Seek=>self.seek=Some(id)};id}
    pub fn clear(&mut self){self.load=None;self.seek=None;}
    pub fn resolve(&mut self,id:u64)->Option<Kind>{if self.load==Some(id){self.load=None;Some(Kind::Load)}else if self.seek==Some(id){self.seek=None;Some(Kind::Seek)}else{None}}
}
/// Old versions of mpv take the options string as the third loadfile argument.
pub fn legacy_loadfile(version:&str)->bool{
    let v=version.trim().strip_prefix("mpv ").unwrap_or(version.trim());
    let mut parts=v.split('.');let major=parts.next().and_then(|n|n.parse::<u32>().ok());let minor=parts.next().and_then(|n|n.parse::<u32>().ok());
    matches!((major,minor),(Some(0),Some(n)) if n<38)
}
/// Keep provider-supplied text as a single mpv option value; never emit a NUL,
/// comma or control character that changes command parsing.
pub fn media_title(title:&str)->String{title.chars().take(512).map(|c|if c==','||c.is_control(){' '}else{c}).collect()}
#[cfg(test)]mod tests{use super::*;
    #[test]fn stale_and_duplicate_replies_cannot_mutate_current_command(){let mut c=Commands::default();let a=c.issue(Kind::Load);let b=c.issue(Kind::Load);assert_ne!(a,b);assert_eq!(c.resolve(a),None);let s=c.issue(Kind::Seek);let t=c.issue(Kind::Seek);assert_eq!(c.resolve(s),None);assert_eq!(c.resolve(b),Some(Kind::Load));assert_eq!(c.resolve(b),None);assert_eq!(c.resolve(t),Some(Kind::Seek));let a=c.issue(Kind::Load);c.clear();let b=c.issue(Kind::Load);assert_eq!(c.resolve(a),None);assert_eq!(c.resolve(b),Some(Kind::Load));}
    #[test]fn load_replaces_pending_seek(){let mut c=Commands::default();let s=c.issue(Kind::Seek);let l=c.issue(Kind::Load);assert_eq!(c.resolve(s),None);assert_eq!(c.resolve(l),Some(Kind::Load));}
    #[test]fn version_and_title_parsing(){assert!(legacy_loadfile("mpv 0.34.1"));assert!(!legacy_loadfile("mpv 0.38.0"));assert!(!legacy_loadfile("mpv 1.0"));assert!(!legacy_loadfile("unknown"));assert_eq!(media_title("name,pause=yes\0\n"),"name pause=yes  ");assert_eq!(media_title(&"é".repeat(999)).chars().count(),512);}
}
