//! Explicit isolated native visual probe. No fabricated screenshots or provider
//! bypass: normal pages are rendered, including loading/error states.
use anyhow::{ensure,Result};
use std::{path::Path,sync::{Arc,Mutex}};
#[derive(Clone,Debug)]
pub struct Probe {
    pub page:String,pub width:u32,pub height:u32,pub scale:f32,pub target:String,
    last:Arc<Mutex<Option<String>>>,
}
impl Probe {
    pub fn parse(args:&[String])->Result<Option<Self>>{
        if args.first().map(String::as_str)!=Some("--visual-probe"){return Ok(None);}
        ensure!(args.len()==6,"Usage: --visual-probe PAGE WIDTH HEIGHT SCALE TARGET");
        let page=args[1].as_str();
        ensure!(matches!(page,"Home"|"Watch"|"Shorts"|"Search"|"Channel"|"Subscriptions"|"Library"|"Playlist"|"Downloads"|"Settings"),"Unknown visual workspace");
        let width:u32=args[2].parse()?;let height:u32=args[3].parse()?;let scale:f32=args[4].parse()?;let target=&args[5];
        ensure!([720,856,1280,1920].contains(&width)&&(480..=2160).contains(&height)&&[0.75,1.0,1.25,1.5].contains(&scale),"Invalid visual matrix dimensions");
        match page {
            "Watch"|"Shorts"|"Playlist"=>ensure!(crate::wetube_session::valid_id(target),"An authorized video ID is required"),
            "Channel"=>ensure!(target.len()==24&&target.starts_with("UC")&&target.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'),"A channel ID is required"),
            "Search"=>ensure!(!target.trim().is_empty()&&target.len()<=512&&!target.chars().any(char::is_control),"Invalid search query"),
            _=>ensure!(target=="-","Unexpected target"),
        }
        Ok(Some(Self{page:page.into(),width,height,scale,target:target.clone(),last:Default::default()}))
    }
    /// Require a dedicated, already-created empty profile. Do not use this mode
    /// on the user's ordinary profile or overwrite existing state.
    pub fn validate_profile(path:&Path)->Result<()> {
        ensure!(path.is_absolute()&&path.is_dir(),"Visual probe requires an absolute empty directory");
        let meta=std::fs::symlink_metadata(path)?;
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;ensure!(meta.file_attributes()&0x400==0,"Visual probe profile must not be a reparse point");}
        ensure!(!meta.file_type().is_symlink()&&std::fs::read_dir(path)?.next().is_none(),"Visual probe profile must be empty and not a symlink");
        Ok(())
    }
    pub fn report(&self,root:&Path,page:&str,width:f32,height:f32,os_scale:f32,ui_scale:f32,root_focus:bool,search_focus:bool)->Result<()> {
        ensure!(page==self.page,"Rendered route does not match requested workspace");
        ensure!([width,height,os_scale,ui_scale].iter().all(|v|v.is_finite()&&*v>0.0),"Invalid measured viewport");
        let raw=serde_json::to_string(&serde_json::json!({"schema":1,"page":page,"requested_width":self.width,"requested_height":self.height,"requested_scale":self.scale,"viewport_width":width,"viewport_height":height,"os_scale":os_scale,"ui_scale":ui_scale,"root_focused":root_focus,"search_focused":search_focus}))?;
        let mut last=self.last.lock().map_err(|_|anyhow::anyhow!("Visual report lock unavailable"))?;
        if last.as_deref()==Some(&raw){return Ok(());}
        let tmp=root.join("visual-state.tmp");std::fs::write(&tmp,&raw)?;std::fs::rename(tmp,root.join("visual-state.json"))?;*last=Some(raw);Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(page:&str,width:&str,scale:&str,target:&str)->Vec<String>{["--visual-probe",page,width,"720",scale,target].iter().map(|s|s.to_string()).collect()}
    #[test]fn validates_matrix_and_targets(){
        for w in ["720","856","1280","1920"] {for s in ["0.75","1","1.25","1.5"] {assert!(Probe::parse(&args("Home",w,s,"-")).unwrap().is_some());}}
        assert!(Probe::parse(&args("Watch","856","1","abcdefghijk")).is_ok());
        for a in [args("Watch","856","1","bad"),args("Home","999","1","-"),args("Home","856","NaN","-"),args("Unknown","856","1","-"),args("Search","856","1","bad\nquery"),args("Channel","856","1","bad"),args("Home","856","1","unexpected")]{assert!(Probe::parse(&a).is_err());}
    }
    #[test]fn isolated_profile_and_measured_state(){
        let root=std::env::temp_dir().join(format!("wetube-visual-{:032x}",rand::random::<u128>()));std::fs::create_dir(&root).unwrap();
        assert!(Probe::validate_profile(&root).is_ok());let p=Probe::parse(&args("Home","856","1","-")).unwrap().unwrap();
        assert!(p.report(&root,"Watch",856.,720.,1.,1.,true,false).is_err());
        assert!(p.report(&root,"Home",f32::NAN,720.,1.,1.,true,false).is_err());
        p.report(&root,"Home",856.,720.,1.25,1.,true,false).unwrap();
        assert!(Probe::validate_profile(&root).is_err());
        p.report(&root,"Home",856.,720.,1.25,1.,false,true).unwrap();
        let v:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(root.join("visual-state.json")).unwrap()).unwrap();assert_eq!(v["search_focused"],true);assert_eq!(v["os_scale"],1.25);std::fs::remove_dir_all(root).unwrap();
    }
}
