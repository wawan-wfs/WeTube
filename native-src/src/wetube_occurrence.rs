//! Stable occurrence identity for the live queue and explicit saved queue.
//! The old v1 snapshot remains readable; a v2 save atomically replaces it.
use crate::api::types::VideoItem;
use anyhow::{ensure,Result};
use serde::{Deserialize,Serialize};
use std::{collections::HashMap,ops::Deref};
const KEY:&str="wetube_saved_queue_v1";
const LIMIT:usize=200;
const MAX_BYTES:usize=256*1024;
#[derive(Default)]
pub struct Queue { videos:Vec<VideoItem>, tokens:Vec<u128>, indices:HashMap<u128,usize> }
impl Deref for Queue {type Target=[VideoItem];fn deref(&self)->&Self::Target{&self.videos}}
impl Queue {
    pub fn from_videos(videos:Vec<VideoItem>)->Self{let mut q=Self::default();for v in videos{q.push(v);}q}
    pub fn push(&mut self,v:VideoItem){let token=loop{let t=rand::random::<u128>();if t!=0&&!self.indices.contains_key(&t){break t;}};self.indices.insert(token,self.videos.len());self.tokens.push(token);self.videos.push(v);}
    pub fn clear(&mut self){self.tokens.clear();self.videos.clear();self.indices.clear();}
    pub fn token(&self,index:usize)->Option<u128>{self.tokens.get(index).copied()}
    pub fn index(&self,token:u128)->Option<usize>{self.indices.get(&token).copied()}
    pub fn move_token(&mut self,pos:&mut Option<usize>,token:u128,up:bool)->bool{
        let Some(i)=self.index(token)else{return false;};
        if !crate::wetube_queue::move_item(&mut self.videos,pos,i,up){return false;}
        let to=if up{i-1}else{i+1};self.tokens.swap(i,to);self.indices.insert(self.tokens[i],i);self.indices.insert(self.tokens[to],to);true
    }
    pub fn remove_token(&mut self,pos:&mut Option<usize>,token:u128)->bool{
        let Some(i)=self.index(token)else{return false;};
        if !crate::wetube_queue::remove_item(&mut self.videos,pos,i){return false;}
        self.tokens.remove(i);self.indices.remove(&token);
        for n in i..self.tokens.len(){self.indices.insert(self.tokens[n],n);}true
    }
    /// Bounded recovery window preserving the exact active occurrence. Invalid
    /// neighbors are omitted without changing the selected token.
    pub(crate) fn recovery_window(&self,video:&VideoItem,selected:Option<usize>)->Option<(Self,usize)>{
        let i=selected.filter(|&i|self.get(i).is_some_and(|v|v.id==video.id))?;
        if !crate::wetube_session::valid_id(&video.id){return None;}
        let start=i.saturating_sub(LIMIT/2).min(self.len().saturating_sub(LIMIT));
        let end=start.saturating_add(LIMIT).min(self.len());
        let mut result=Self::default();let mut active=None;
        for n in start..end {
            let v=if n==i{video}else{&self.videos[n]};
            if !crate::wetube_session::valid_id(&v.id){continue;}
            let token=self.tokens[n];let index=result.videos.len();
            if n==i{active=Some(index);}
            result.indices.insert(token,index);result.tokens.push(token);result.videos.push(v.clone());
        }
        active.map(|i|(result,i))
    }
    pub fn shuffle(&mut self,pos:&mut Option<usize>){
        use rand::seq::SliceRandom;
        let active=pos.and_then(|i|self.token(i));
        let mut entries:Vec<_>=std::mem::take(&mut self.tokens).into_iter().zip(std::mem::take(&mut self.videos)).collect();
        entries.shuffle(&mut rand::rng());
        (self.tokens,self.videos)=entries.into_iter().unzip();
        self.indices=self.tokens.iter().enumerate().map(|(i,&t)|(t,i)).collect();
        *pos=active.and_then(|t|self.index(t));
    }
}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry{token:String,id:String,title:String,channel:String}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot{version:u8,entries:Vec<Entry>,selected:usize}
fn short(s:&str)->String{s.chars().filter(|c|!c.is_control()).take(160).collect()}
pub(crate) fn encode(queue:&Queue,selected:Option<usize>)->Result<String>{
    let selected=selected.unwrap_or(0);
    ensure!(!queue.is_empty()&&queue.len()<=LIMIT&&selected<queue.len(),"Save 1–200 videos with a valid position");
    ensure!(queue.iter().all(|v|crate::wetube_session::valid_id(&v.id)),"Invalid video ID");
    let entries=queue.iter().zip(&queue.tokens).map(|(v,t)|Entry{token:format!("{t:032x}"),id:v.id.clone(),title:short(&v.title),channel:short(&v.channel_name)}).collect();
    let raw=serde_json::to_string(&Snapshot{version:2,entries,selected})?;
    ensure!(raw.len()<=MAX_BYTES,"Queue exceeds storage limit");Ok(raw)
}
pub(crate) fn decode(raw:&str)->Result<(Queue,usize)>{
    ensure!(raw.len()<=MAX_BYTES,"Queue exceeds storage limit");
    let s:Snapshot=serde_json::from_str(raw)?;
    ensure!(s.version==2&&!s.entries.is_empty()&&s.entries.len()<=LIMIT&&s.selected<s.entries.len(),"Invalid saved queue");
    let mut q=Queue::default();
    for e in s.entries{
        ensure!(e.token.len()==32&&e.token.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)),"Invalid occurrence token");
        let t=u128::from_str_radix(&e.token,16)?;
        ensure!(t!=0&&!q.indices.contains_key(&t),"Duplicate or zero occurrence token");
        ensure!(crate::wetube_session::valid_id(&e.id)&&e.title==short(&e.title)&&e.channel==short(&e.channel),"Invalid queue metadata");
        q.indices.insert(t,q.videos.len());q.tokens.push(t);q.videos.push(VideoItem{thumbnail:crate::api::innertube::default_thumb(&e.id),id:e.id,title:e.title,channel_name:e.channel,..Default::default()});
    }
    Ok((q,s.selected))
}
pub fn save(queue:&Queue,selected:Option<usize>)->Result<()>{ensure!(crate::store::wetube_kv_set(KEY,&encode(queue,selected)?),"Could not write saved queue");Ok(())}
pub fn load()->Result<Option<(Queue,usize)>>{
    let Some(raw)=crate::store::wetube_kv_read_bounded(KEY,MAX_BYTES)?.filter(|s|!s.is_empty())else{return Ok(None);};
    ensure!(raw.len()<=MAX_BYTES,"Queue exceeds storage limit");
    let value:serde_json::Value=serde_json::from_str(&raw)?;
    match value.get("version").and_then(|v|v.as_u64()){
        Some(1)=>Ok(crate::wetube_queue::load()?.map(|(v,i)|(Queue::from_videos(v),i))),
        Some(2)=>Ok(Some(decode(&raw)?)),
        _=>anyhow::bail!("Unsupported saved queue version"),
    }
}
pub fn clear()->bool{crate::wetube_queue::clear()}
#[cfg(test)]
mod tests{
    use super::*;
    fn q()->Queue{Queue::from_videos(["abcdefghijk","12345678901","abcdefghijk"].iter().map(|id|VideoItem{id:(*id).into(),..Default::default()}).collect())}
    #[test]fn duplicate_occurrences_survive_reorder_and_roundtrip(){
        let mut q=q();let a=q.token(0).unwrap();let b=q.token(2).unwrap();assert_ne!(a,b);let mut pos=Some(2);
        assert!(q.move_token(&mut pos,b,true));assert_eq!(pos,Some(1));assert_eq!(q.index(a),Some(0));
        assert!(!q.remove_token(&mut pos,b));assert!(q.remove_token(&mut pos,a));assert_eq!(pos,Some(0));assert!(q.index(a).is_none());
        q.shuffle(&mut pos);assert_eq!(q.token(pos.unwrap()),Some(b));
        let (restored,i)=decode(&encode(&q,pos).unwrap()).unwrap();assert_eq!(restored.tokens,q.tokens);assert_eq!(restored.token(i),Some(b));
    }
    #[test]fn large_queue_index_consistency(){
        let mut q=Queue::from_videos(vec![VideoItem{id:"abcdefghijk".into(),..Default::default()};50_000]);
        let active=q.token(49_999).unwrap();let mut pos=Some(49_999);
        assert_eq!(q.index(active),pos);assert!(q.move_token(&mut pos,active,true));
        let first=q.token(0).unwrap();assert!(q.remove_token(&mut pos,first));q.shuffle(&mut pos);
        assert_eq!(q.token(pos.unwrap()),Some(active));assert_eq!(q.indices.len(),q.len());
        for i in 0..q.len(){assert_eq!(q.index(q.token(i).unwrap()),Some(i));}
        q.clear();assert!(q.indices.is_empty());assert!(q.index(active).is_none());
    }
    #[test]fn stale_and_corrupt_tokens_fail_closed(){
        let mut q=q();let old=q.token(0).unwrap();q.clear();q.push(VideoItem{id:"abcdefghijk".into(),..Default::default()});
        assert!(!q.remove_token(&mut None,old));assert!(!q.move_token(&mut None,old,true));
        let mut v:serde_json::Value=serde_json::from_str(&encode(&q,None).unwrap()).unwrap();let entry=v["entries"][0].clone();v["entries"].as_array_mut().unwrap().push(entry);
        assert!(decode(&v.to_string()).is_err());v["entries"][0]["token"]=serde_json::json!("00000000000000000000000000000000");assert!(decode(&v.to_string()).is_err());
    }
}
