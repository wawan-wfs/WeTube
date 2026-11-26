use crate::downloads::{Downloads, JobStatus};
use crate::store;
use crate::ui::app::{Route, navigate};
use crate::ui::widgets::*;
use crate::ui::*;

pub struct DownloadsPage {
    scroll: ScrollHandle,
    filter: u8,
    search: Entity<gpui_kit::component::input::InputState>,
}

impl DownloadsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx).placeholder("Search downloads"));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        Self { scroll: ScrollHandle::new(), filter: 0, search }
    }
}

impl Render for DownloadsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        let jobs = cx.default_global::<Downloads>().jobs.lock().clone();
        let dir = store::settings_snapshot().download_dir.clone();
        let ytdlp = crate::api::streams::ytdlp_path().is_some();
        let query = self.search.read(cx).value().trim().to_lowercase();
        let active = jobs.iter().filter(|j| matches!(j.status, JobStatus::Queued | JobStatus::Running)).count();
        let done = jobs.iter().filter(|j| matches!(j.status, JobStatus::Done(_))).count();
        let failed = jobs.iter().filter(|j| matches!(j.status, JobStatus::Failed(_) | JobStatus::Cancelled)).count();
        let interrupted = crate::downloads::interrupted_count(cx);
        let journal_blocked = crate::downloads::history_blocked(cx);
        let filter = self.filter;
        let visible = jobs.iter().rev().filter(|j| {
            let category = match &j.status { JobStatus::Queued | JobStatus::Running => 1, JobStatus::Done(_) => 2, _ => 3 };
            (filter == 0 || filter == category) && (query.is_empty() || j.video.title.to_lowercase().contains(&query) || j.video.channel_name.to_lowercase().contains(&query))
        });
        let mut shown = 0;
        let mut list = div().flex().flex_col().gap_3();
        for j in visible {
            shown += 1;
            let id = j.id;
            let vid = j.video.id.clone();
            let (status, color): (String, Hsla) = match &j.status {
                JobStatus::Queued => ("Queued".into(), p.text2),
                JobStatus::Running => (if j.detail.is_empty() { "Starting…".into() } else { j.detail.clone() }, p.text2),
                JobStatus::Done(path) => (format!("Saved to {path}"), rgb(0x2ba640).into()),
                JobStatus::Failed(e) => (format!("Failed: {e}"), rgb(0xff4e45).into()),
                JobStatus::Cancelled => ("Cancelled".into(), p.text3),
            };
            let done_path = if let JobStatus::Done(path) = &j.status { Some(path.clone()) } else { None };
            let running = matches!(j.status, JobStatus::Running | JobStatus::Queued);
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .p_3()
                    .rounded_xl()
                    .bg(p.surface)
                    .child(
                        div()
                            .id(SharedString::from(format!("dlt-{id}")))
                            .cursor_pointer()
                            .on_click(move |_, window, cx| navigate(Route::Watch { id: vid.clone(), playlist: None, start: None }, window, cx))
                            .child(img(SharedString::from(j.video.thumbnail.clone())).w(px(160.)).h(px(90.)).rounded_lg().object_fit(ObjectFit::Cover)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).truncate().child(j.video.title.clone()))
                            .child(div().text_xs().text_color(p.text2).child(format!("{} • format: {}", j.video.channel_name, j.format)))
                            .when(running, |d| {
                                d.child(div().w_full().h(px(4.)).rounded_full().bg(p.hover).child(div().h_full().rounded_full().w(relative(j.progress.clamp(0.0, 1.0))).bg(p.accent)))
                            })
                            .child(div().text_xs().text_color(color).truncate().child(status))
                            .when(!j.directory.is_empty(), |d| d.child(div().text_xs().text_color(p.text3).truncate().child(format!("Folder: {}",j.directory)))),
                    )
                    .when(running, |d| d.child(pill(SharedString::from(format!("dlc-{id}")), Some("x"), "Cancel", cx).on_click(move |_, _, cx| crate::downloads::cancel(id, cx))))
                    .when_some(done_path, |d, path| {
                        let p2 = path.clone();
                        d.child(pill(SharedString::from(format!("dlo-{id}")), Some("play"), "Open", cx).on_click(move |_, _, cx| crate::downloads::open_file(&path, cx)))
                            .child(icon_button(SharedString::from(format!("dlf-{id}")), "folder-open", cx).on_click(move |_, _, cx| crate::ui::app::reveal_in_folder(&p2, cx)))
                            .child(icon_button(SharedString::from(format!("dld-{id}")), "trash-2", cx).on_click(move |_, window, cx| crate::ui::app::confirm("Delete downloaded file?", "This permanently removes the selected file from disk.", "Delete file", window, cx, move |_, cx| crate::downloads::delete_file(id, cx))))
                    })
                    .when(matches!(j.status, JobStatus::Failed(_) | JobStatus::Cancelled), |d| d.child(pill(SharedString::from(format!("retry-{id}")), Some("refresh-cw"), "Retry", cx).on_click(move |_, _, cx| crate::downloads::retry(id, cx))))
                    .when(matches!(j.status, JobStatus::Failed(_) | JobStatus::Cancelled), |d| d.child(pill(SharedString::from(format!("restart-{id}")), Some("rotate-ccw"), "Start over", cx).on_click(move |_,window,cx| crate::ui::app::confirm("Start a new transfer?", "Existing partial files will remain in the original folder. The next attempt starts from zero.", "Start over", window,cx,move |_,cx| crate::downloads::restart(id,cx)))))
                    .when(!running, |d| d.child(icon_button(SharedString::from(format!("dlr-{id}")), "x", cx).on_click(move |_, _, cx| crate::downloads::remove(id, cx)))),
            );
        }
        if shown == 0 {
            list = list.child(empty_state("download", if jobs.is_empty() { "No downloads yet" } else { "No matching downloads" }, "", cx));
        }
        let mut filters = div().flex().flex_wrap().items_center().gap_2();
        for (i,label,n) in [(0u8,"All",jobs.len()),(1,"Active",active),(2,"Completed",done),(3,"Failed / cancelled",failed)] {
            filters = filters.child(chip(SharedString::from(format!("dl-filter-{i}")),format!("{label} ({n})"),filter == i,cx)
                .on_click(cx.listener(move |this, _, _, cx| { this.filter=i; cx.notify(); })));
        }
        let dir2 = dir.clone();
        page_scroll("dl-scroll", &self.scroll).child(
            div().flex().justify_center().px_6().pt_6().pb_24().child(
                div()
                    .w_full().max_w(px(1100.))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_3()
                            .justify_between()
                            .child(workspace_header("Downloads", "Manage active, finished and interrupted downloads", "download", cx))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .when(jobs.iter().any(|j| !matches!(j.status, JobStatus::Running | JobStatus::Queued)), |d| {
                                        d.child(pill("dl-clear", Some("list-x"), "Clear list", cx).on_click(|_, _, cx| crate::downloads::clear_history(cx)))
                                    })
                                    .child(pill("dl-change", Some("folder-cog"), "Change folder", cx).on_click(|_, _, cx| {
                                        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Save downloads here".into()) });
                                        cx.spawn(async move |cx| {
                                            if let Ok(Ok(Some(paths))) = rx.await {
                                                if let Some(d) = paths.into_iter().next() {
                                                    store::update_settings(|s| {
                                                        s.download_dir = d.to_string_lossy().into_owned();
                                                        s.download_dir_chosen = true;
                                                    });
                                                    let _ = cx.update(|cx| cx.refresh_windows());
                                                }
                                            }
                                        })
                                        .detach();
                                    }))
                                    .child(pill("dl-folder", Some("folder-open"), "Open folder", cx).on_click(move |_, _, _| {
                                        let _ = std::fs::create_dir_all(&dir2);
                                        let _ = open::that(&dir2);
                                    })),
                            ),
                    )
                    .child(div().text_sm().text_color(p.text2).child(format!(
                        "Saving to {dir}. Engine: {}",
                        if ytdlp { "yt-dlp (all qualities, SponsorBlock chapters)" } else { "built-in (combined 360p/720p or audio only — install yt-dlp for more)" }
                    )))
                    .child(div().flex().flex_col().gap_3().p_4().rounded_xl().border_1().border_color(p.border).bg(p.surface)
                        .child(div().text_sm().text_color(p.text).font_weight(FontWeight::SEMIBOLD).child(format!("{active} active · {done} completed · {failed} failed or cancelled")))
                        .child(gpui_kit::component::input::Input::new(&self.search).appearance(false).cleanable(true))
                        .child(filters)
                        .when(journal_blocked, |d| d.child(div().p_3().rounded_lg().bg(p.surface2).border_1().border_color(p.border)
                            .text_sm().text_color(p.text).child("Saved download history could not be read. New downloads are paused; existing history has not been overwritten.")))
                        .when(interrupted > 0, |d| d.child(div().p_3().rounded_lg().bg(p.surface2).border_1().border_color(p.border)
                            .text_sm().text_color(p.text).child(format!("{interrupted} interrupted downloads recovered. Use Retry to recover selected items in their original folder; no automatic network activity.")))))
                    .child(list),
            ),
        )
    }
}
