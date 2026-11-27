//! Export production logical-pixel geometry, NOT screenshots or interaction results.
#[path="../src/wetube_workspace.rs"] mod workspace;
use workspace::*;
fn main(){
    println!("width,scale,workspace,content_width,surface_width,compact,columns,card_width,rail_width,thumbnail,geometry_status,windows_visual_status");
    for w in [720.,856.,1280.,1920.] {for s in [0.75,1.,1.25,1.5] {for page in WORKSPACES {
        let content=content_width(w,sidebar_width(w,true,page==Workspace::Watch,s));
        let a=surface(page,content,s);
        assert!(a.width<=content&&a.rail_width<=a.width&&a.thumbnail<=a.width);
        assert!(a.card_width*a.columns as f32+16.*(a.columns-1) as f32<=a.width+0.001);
        println!("{w},{s},{page:?},{content},{},{},{},{},{},{},PASS,NOT TESTED",a.width,a.compact,a.columns,a.card_width,a.rail_width,a.thumbnail);
    }}}
}
