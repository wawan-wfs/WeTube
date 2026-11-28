//! Bounded catalog presentation, shared by native pages and portable tests.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Window { pub page:usize,pub pages:usize,pub start:usize,pub end:usize,pub total:usize }
impl Window {
    pub fn new(total:usize,requested:usize,size:usize)->Self {
        let size=size.clamp(1,200);
        let pages=total.div_ceil(size).max(1);
        let page=requested.min(pages-1);
        let start=page.saturating_mul(size).min(total);
        Self{page,pages,start,end:start.saturating_add(size).min(total),total}
    }
    pub fn range(self)->std::ops::Range<usize>{self.start..self.end}
    pub fn label(self)->String{if self.total==0 {"No items".into()} else {format!("{}–{} of {} · Page {} / {}",self.start+1,self.end,self.total,self.page+1,self.pages)}}
}
#[derive(Default)]
pub struct Catalog {pub page:usize,pub list:bool,context:String}
impl Catalog {
    pub fn reset(&mut self){self.page=0;}
    pub fn context(&mut self,value:&str){if self.context!=value{self.context=value.into();self.reset();}}
    pub fn window(&mut self,total:usize,size:usize)->Window{let w=Window::new(total,self.page,size);self.page=w.page;w}
}
/// Streaming page projection. A caller may scan metadata for Unicode matching
/// without retaining every matching item or deserializing every thumbnail.
pub struct PageCollector<T>{requested:usize,size:usize,total:usize,target:Vec<T>,tail:Vec<T>}
impl<T> PageCollector<T>{
    pub fn new(requested:usize,size:usize)->Self{Self{requested,size:size.clamp(1,200),total:0,target:Vec::new(),tail:Vec::new()}}
    pub fn push(&mut self,item:T){
        let page=self.total/self.size;
        if page==self.requested{self.target.push(item);}
        else if page<self.requested{
            if self.total%self.size==0{self.tail.clear();}
            self.tail.push(item);
        }
        self.total=self.total.saturating_add(1);
    }
    pub fn finish(self)->(Window,Vec<T>){let w=Window::new(self.total,self.requested,self.size);let v=if w.page==self.requested{self.target}else{self.tail};(w,v)}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn large_catalog_covers_every_item_once(){let mut all=0;let mut cursor=0;for i in 0..1000{let w=Window::new(50000,i,50);assert_eq!(w.start,cursor);all+=w.range().len();cursor=w.end;}assert_eq!(all,50000);}
    #[test]fn clamp_and_empty_are_safe(){let w=Window::new(0,usize::MAX,0);assert_eq!(w.range(),0..0);assert_eq!(w.label(),"No items");let w=Window::new(51,usize::MAX,50);assert_eq!(w.range(),50..51);assert_eq!(w.page,1);let w=Window::new(usize::MAX,usize::MAX,usize::MAX);assert_eq!(w.end,usize::MAX);assert!(w.range().len()<=200);}
    #[test]fn streaming_projection_is_bounded_and_clamps(){
        for (total,page,size) in [(50000,501,48),(50000,usize::MAX,48),(0,usize::MAX,48),(96,9,48),(96,1,48)]{
            let mut c=PageCollector::new(page,size);
            for i in 0..total{c.push(i);assert!(c.target.len()+c.tail.len()<=96);}
            let(w,v)=c.finish();assert_eq!(v,w.range().collect::<Vec<_>>());
        }
    }
    #[test]fn context_resets_page_but_preserves_display_preference(){let mut c=Catalog::default();c.context("one");c.page=2;c.list=true;c.context("one");assert_eq!(c.page,2);c.context("two");assert_eq!(c.page,0);assert!(c.list);c.page=9;assert_eq!(c.window(12,50).page,0);}
}
