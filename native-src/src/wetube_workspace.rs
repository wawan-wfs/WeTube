//! GPUI-independent geometry and queue projection. Rendering code consumes the
//! same finite, bounded layout calculations exercised by portable regression.
use std::ops::Range;

pub fn finite_extent(value: f32) -> f32 {
    if value.is_finite() { value.clamp(1.0, 32768.0) } else { 1.0 }
}
pub fn scale(value: f32) -> f32 {
    if value.is_finite() { value.clamp(0.75, 1.5) } else { 1.0 }
}
pub fn compact(width: f32, threshold: f32, ui_scale: f32) -> bool {
    finite_extent(width) < finite_extent(threshold) * scale(ui_scale).max(1.0)
}
pub fn toolbar_height(width: f32, ui_scale: f32) -> f32 {
    if compact(width, 1050.0, ui_scale) { 104.0 } else { 56.0 }
}
pub fn sidebar_width(width: f32, expanded: bool, watching: bool, ui_scale: f32) -> f32 {
    if watching { 0.0 } else if expanded && !compact(width, 1000.0, ui_scale) { 232.0 } else { 72.0 }
}
pub fn content_width(width: f32, sidebar: f32) -> f32 {
    (finite_extent(width) - sidebar.max(0.0) - 48.0).max(1.0)
}
pub fn grid(width: f32, min_card: f32, preferred: usize, ui_scale: f32) -> (usize, f32) {
    let width=finite_extent(width);
    let min_card=if min_card.is_finite() { min_card.max(120.0) } else { 240.0 } * scale(ui_scale).max(1.0);
    let fit=(((width+16.0)/(min_card+16.0)).floor() as usize).clamp(1,6);
    let cols=if preferred==0 {fit} else {preferred.min(fit).max(1)};
    (cols,((width-16.0*(cols-1) as f32)/cols as f32).floor().max(1.0))
}
pub fn fraction(value: f64, total: f64) -> f32 {
    if value.is_finite() && total.is_finite() && total>0.0 {(value/total).clamp(0.0,1.0) as f32} else {0.0}
}
pub fn fit_ratio(width: f32, height: f32, aspect: f32) -> (f32,f32) {
    let width=finite_extent(width);let height=finite_extent(height);
    let aspect=if aspect.is_finite() {aspect.clamp(0.1,10.0)}else{16.0/9.0};
    let w=width.min(height*aspect);
    (w,w/aspect)
}
#[derive(Clone,Copy,Debug)]
pub struct WatchLayout {pub primary:f32,pub side:f32,pub height:f32,pub two_columns:bool}
pub fn watch(width:f32,height:f32,theater:bool,ui_scale:f32)->WatchLayout {
    let avail=content_width(width,0.0);
    let two_columns=!compact(avail,1120.0,ui_scale);
    let side=if two_columns{402.0}else{avail};
    let primary=if two_columns{(avail-side-24.0).max(1.0).min(1280.0)}else{avail};
    let room=(finite_extent(height)-toolbar_height(width,ui_scale)-if theater{120.0}else{40.0}).max(1.0);
    let video_width=if theater{finite_extent(width)}else{primary};
    let (_,h)=fit_ratio(video_width,room,16.0/9.0);
    WatchLayout{primary,side,height:h,two_columns}
}
#[derive(Clone,Copy,Debug)]
pub struct ShortsLayout {pub width:f32,pub height:f32,pub comments_width:f32,pub comments_overlay:bool}
pub fn shorts(content:f32,viewport:f32,height:f32,comments:bool,ui_scale:f32)->ShortsLayout{
    let content=finite_extent(content);
    let overlay=comments&&compact(content,700.0,ui_scale);
    let comments_width=if comments{(content-24.0).max(1.0).min(380.0)}else{0.0};
    let reserved=if comments&&!overlay{comments_width+164.0}else{164.0};
    let (width,height)=fit_ratio((content-reserved).max(1.0),(finite_extent(height)-toolbar_height(viewport,ui_scale)-32.0).max(1.0),9.0/16.0);
    ShortsLayout{width,height,comments_width,comments_overlay:overlay}
}
/// Stable ordinal slices: never deduplicate repeated videos or lose the active
/// occurrence. None follows the playing entry; Some selects a page explicitly.
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct QueuePage {pub page:usize,pub pages:usize,pub range:Range<usize>}
pub fn queue_page(len:usize,active:Option<usize>,requested:Option<usize>,size:usize)->QueuePage {
    let size=size.clamp(1,200);
    let pages=len.div_ceil(size).max(1);
    let page=requested.unwrap_or_else(||active.filter(|&i|i<len).unwrap_or(0)/size).min(pages-1);
    let start=page.saturating_mul(size).min(len);
    QueuePage{page,pages,range:start..start.saturating_add(size).min(len)}
}
/// Preserve original history resume semantics but reject nonfinite/invalid
/// positions and keep checkpoints consistent before async stream startup.
pub fn resume_position(explicit:Option<f64>,history:Option<(f64,f64)>,enabled:bool)->Option<f64>{
    if let Some(t)=explicit.filter(|t|t.is_finite()&&(0.0..=604800.0).contains(t)){return Some(t);}
    if !enabled{return None;}
    history.and_then(|(p,d)|(p.is_finite()&&d.is_finite()&&p>10.0&&p<=604800.0&&d>=0.0&&(d==0.0||p<d-15.0)).then_some(p))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn grid_matrix_fits_and_respects_user_limit(){
        for w in [1.0,240.0,480.0,856.0,1024.0,1920.0,3840.0] {for s in [0.75,1.0,1.25,1.5] {for n in 0..=9 {
            let(c,card)=grid(w,260.0,n,s);assert!((1..=6).contains(&c));assert!(n==0||c<=n);assert!(card*c as f32+16.0*(c-1) as f32<=w+0.001);
        }}}
    }
    #[test] fn shell_consistent_at_dpi_boundaries(){
        assert_eq!(sidebar_width(856.0,true,false,1.0),72.0);assert_eq!(sidebar_width(1200.0,true,false,1.5),72.0);
        assert_eq!(sidebar_width(1800.0,true,false,1.5),232.0);assert_eq!(sidebar_width(1800.0,true,true,1.0),0.0);
        assert_eq!(toolbar_height(1200.0,1.5),104.0);assert_eq!(content_width(40.0,72.0),1.0);
    }
    #[test] fn player_geometry_stays_finite(){
        for w in [320.0,856.0,1280.0,1920.0,3840.0] {for h in [240.0,661.0,1080.0] {for s in [1.0,1.5] {for theater in [true,false] {
            let l=watch(w,h,theater,s);assert!(l.primary>0.0&&l.height>0.0&&l.height<=h);assert!(l.primary<=w);
            if l.two_columns{assert!(l.primary+l.side+24.0<=content_width(w,0.0));}
        }}}}
    }
    #[test] fn portrait_comments_overlay_at_small_width(){
        let small=shorts(480.0,856.0,661.0,true,1.0);assert!(small.comments_overlay);assert!(small.width+164.0<=480.0);assert!(small.comments_width<=480.0);
        let large=shorts(1400.0,1920.0,1080.0,true,1.0);assert!(!large.comments_overlay);assert!(large.width+large.comments_width+164.0<=1400.0);
        assert!((large.width/large.height-9.0/16.0).abs()<0.001);
    }
    #[test] fn fractions_and_ratio_reject_invalid_geometry(){
        assert_eq!(fraction(f64::NAN,1.0),0.0);assert_eq!(fraction(5.0,0.0),0.0);assert_eq!(fraction(-1.0,1.0),0.0);assert_eq!(fraction(10.0,1.0),1.0);
        assert_eq!(fit_ratio(480.0,270.0,16.0/9.0),(480.0,270.0));assert!(fit_ratio(f32::NAN,f32::INFINITY,0.0).0.is_finite());
    }
    #[test] fn queue_projection_preserves_active_and_bounds(){
        assert_eq!(queue_page(10001,Some(9999),None,50).range,9950..10000);assert_eq!(queue_page(10001,Some(9999),Some(usize::MAX),50).range,10000..10001);
        assert_eq!(queue_page(0,None,None,50).range,0..0);assert_eq!(queue_page(51,Some(50),None,50).range,50..51);
    }
    #[test] fn download_page_bounds_at_journal_capacity(){
        let len=50_000;
        let mut count=0;
        for page in 0..500 {
            let p=queue_page(len,None,Some(page),100);
            assert_eq!(p.pages,500);assert_eq!(p.range.start,count);
            assert_eq!(p.range.len(),100);count=p.range.end;
        }
        assert_eq!(count,len);
        assert_eq!(queue_page(49_999,None,Some(500),100).range,49_900..49_999);
    }
    #[test] fn resume_never_overwrites_explicit_or_accepts_invalid(){
        assert_eq!(resume_position(Some(0.0),Some((30.0,100.0)),true),Some(0.0));assert_eq!(resume_position(None,Some((30.0,100.0)),true),Some(30.0));
        assert_eq!(resume_position(None,Some((98.0,100.0)),true),None);assert_eq!(resume_position(None,Some((30.0,100.0)),false),None);
        assert_eq!(resume_position(None,Some((f64::INFINITY,0.0)),true),None);assert_eq!(resume_position(None,Some((30.0,f64::NAN)),true),None);
    }
}
