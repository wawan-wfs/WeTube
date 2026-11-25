//! The main video player lives outside the page tree so that video frames only
//! re-render the player itself, never the (cached) page around it. Pages place a
//! `player_slot()` where the video should appear; `SlotFollower` draws the player
//! over that slot in the same frame (it reads the slot bounds during prepaint).
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct PlayerSlot(pub Rc<Cell<Option<Bounds<Pixels>>>>);

impl Global for PlayerSlot {}

/// Placeholder element that records where the player should be drawn.
pub fn player_slot(cx: &App) -> impl IntoElement {
    let slot = cx.global::<PlayerSlot>().0.clone();
    canvas(move |b, _, _| slot.set(Some(b)), |_, _, _, _| {}).size_full()
}

pub struct SlotFollower {
    slot: Rc<Cell<Option<Bounds<Pixels>>>>,
    child: Option<AnyElement>,
    laid_size: Option<Size<Pixels>>,
}

impl SlotFollower {
    pub fn new(cx: &App, view: AnyView) -> Self {
        Self::with_slot(cx.global::<PlayerSlot>().0.clone(), view)
    }

    /// Follows any slot (e.g. the thumbnail being previewed).
    pub fn with_slot(slot: Rc<Cell<Option<Bounds<Pixels>>>>, view: AnyView) -> Self {
        let laid_size = slot.get().map(|b| b.size);
        let child = laid_size.map(|s| div().w(s.width).h(s.height).child(view).into_any_element());
        Self { slot, child, laid_size }
    }
}

impl IntoElement for SlotFollower {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SlotFollower {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = Position::Absolute;
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        let children: Vec<LayoutId> = self.child.as_mut().map(|c| c.request_layout(window, cx)).into_iter().collect();
        (window.request_layout(style, children, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(slot) = self.slot.get() else {
            window.request_animation_frame();
            return;
        };
        if Some(slot.size) != self.laid_size {
            // slot changed size since layout: draw once more with the new size
            window.request_animation_frame();
        }
        if let Some(child) = self.child.as_mut() {
            let offset = slot.origin - bounds.origin;
            window.with_element_offset(offset, |window| child.prepaint(window, cx));
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
    }
}
