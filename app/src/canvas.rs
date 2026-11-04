//! The arrangement: this device's screens and the peers placed around them,
//! to scale on a dotted canvas, with a strip along the bottom holding the
//! user's online devices that aren't placed yet. The agent's core owns
//! placements (it snaps every drop to a valid spot and syncs it to the
//! peer), so the canvas only proposes positions and draws what comes back.
//! It behaves like the Windows app's arrangement:
//!
//! - a press that moves less than `DRAG_THRESHOLD` is a click, not a drag;
//! - the view holds still while a device moves or settles, and glides to fit
//!   the new arrangement once the agent has it;
//! - while dragging, the agent says where the device would land (the core's
//!   own snapping): close to that spot the device jumps there like a magnet,
//!   farther away a dashed outline shows it;
//! - dropped, the device glides to that spot and stays there until the
//!   agent's snapshot has it;
//! - dropped on the strip, a placed device is unplaced;
//! - Escape, or the pointer taken away, puts it back where it was.

use crate::window::{platform_icon, platform_name};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use rinflow_core::layout::{Display, Offset};
use rinflow_shell::i18n::tr;
use rinflow_shell::state::{DeviceView, LayoutView};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

/// Room around the drawing (pixels), and the strip at the bottom.
const PAD_SIDE: f64 = 24.0;
const PAD_TOP: f64 = 14.0;
const STRIP_HEIGHT: f64 = 44.0;
const PAD_BOTTOM: f64 = STRIP_HEIGHT + 14.0;
/// Room for placing around this device's screens, as a share of their size.
const ROOM_X: f64 = 0.6;
const ROOM_Y: f64 = 0.35;
const DOT_SPACING: f64 = 22.0;
/// A press that moves less than this (pixels) is a click, not a drag.
const DRAG_THRESHOLD: f64 = 6.0;
/// The magnet (pixels): a device dragged this close to where it would land
/// jumps there, and stays until the pointer is this far away, so it does not
/// flicker at the edge.
const SNAP_IN: f64 = 14.0;
const SNAP_OUT: f64 = 24.0;
/// How long a dropped device glides to where it lands, and the view to its
/// new fit (microseconds).
const SETTLE_US: i64 = 180_000;
const VIEW_US: i64 = 220_000;
/// How long a settled device waits for the snapshot with its placement.
const SNAPSHOT_WAIT_US: i64 = 500_000;
/// A preview the agent hasn't answered in this long is given up.
const PREVIEW_TIMEOUT_US: i64 = 1_000_000;
/// Screens are inset by this much (pixels) on sides that don't meet another
/// screen of the same device, so devices that touch show a hairline gap.
const SCREEN_GAP: f64 = 1.0;
const RADIUS: f32 = 6.0;
const CHIP_HEIGHT: f64 = 28.0;
const CHIP_MAX_WIDTH: f64 = 200.0;

type PlaceHandler = Box<dyn Fn(&str, Offset)>;
type PeerHandler = Box<dyn Fn(&str)>;

/// A rectangle, in world units (logical points) or pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }

    fn bottom(&self) -> f64 {
        self.y + self.h
    }

    fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect { x, y, w: self.right().max(other.right()) - x, h: self.bottom().max(other.bottom()) - y }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    fn of(display: &Display) -> Rect {
        Rect { x: display.x, y: display.y, w: display.width, h: display.height }
    }

    /// The bounds of a device's displays.
    fn around(displays: &[Display]) -> Option<Rect> {
        displays.iter().map(Rect::of).reduce(|a, b| a.union(&b))
    }

    fn graphene(&self) -> graphene::Rect {
        graphene::Rect::new(self.x as f32, self.y as f32, self.w.max(1.0) as f32, self.h.max(1.0) as f32)
    }
}

/// The mapping from world units to pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct View {
    left: f64,
    top: f64,
    scale: f64,
}

impl View {
    fn px(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.left) * self.scale, (y - self.top) * self.scale)
    }

    fn world(&self, px: f64, py: f64) -> (f64, f64) {
        (px / self.scale + self.left, py / self.scale + self.top)
    }

    fn lerp(&self, to: &View, t: f64) -> View {
        View {
            left: self.left + (to.left - self.left) * t,
            top: self.top + (to.top - self.top) * t,
            scale: self.scale + (to.scale - self.scale) * t,
        }
    }

    /// Whether the two draw the same within a pixel.
    fn close(&self, other: &View) -> bool {
        (self.scale - other.scale).abs() * 2000.0 < 1.0
            && ((self.left - other.left) * self.scale).abs() < 0.5
            && ((self.top - other.top) * self.scale).abs() < 0.5
    }
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// A device being dragged. Positions are its top-left corner in pixels: the
/// view holds still meanwhile.
#[derive(Clone, Debug)]
struct Drag {
    peer_id: String,
    displays: Vec<Display>,
    bounds: Rect,
    /// Picked up from the strip (not placed yet).
    from_strip: bool,
    /// Where it was drawn when picked up, if placed.
    origin: Option<(f64, f64)>,
    /// The pointer at the press.
    start: (f64, f64),
    /// The pointer relative to the top-left corner.
    grab: (f64, f64),
    /// Where the pointer has it.
    free: (f64, f64),
    /// Past the threshold: a drag, not a click.
    moved: bool,
    /// Over the strip: dropping unplaces it.
    over_strip: bool,
    /// Where the agent says it would land.
    landing: Option<(f64, f64)>,
    /// Held by the magnet at `landing`.
    snapped: bool,
}

impl Drag {
    fn drawn_at(&self) -> (f64, f64) {
        match self.landing {
            Some(landing) if self.snapped => landing,
            _ => self.free,
        }
    }

    /// Into the magnet when close, out of it only farther away.
    fn update_magnet(&mut self) {
        self.snapped = match self.landing {
            Some(landing) if !self.over_strip => {
                distance(self.free, landing) <= if self.snapped { SNAP_OUT } else { SNAP_IN }
            }
            _ => false,
        };
    }
}

/// A dropped (or put back) device gliding to its spot, then waiting there
/// for the agent's snapshot.
#[derive(Clone, Debug)]
struct Settle {
    peer_id: String,
    displays: Vec<Display>,
    bounds: Rect,
    from: (f64, f64),
    to: (f64, f64),
    /// Frame time of the first frame, microseconds.
    start: Option<i64>,
    /// The layout when it was dropped: a different one is the agent's
    /// answer. `None` when there is nothing to wait for (put back).
    waits_for: Option<LayoutView>,
    answered: bool,
    /// Unplaced: not drawn while waiting.
    gone: bool,
}

/// What a press can pick up.
struct Target {
    peer_id: String,
    from_strip: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArrangementCanvas {
        pub layout: RefCell<LayoutView>,
        pub local_name: RefCell<String>,
        pub connected: RefCell<HashSet<String>>,
        /// Devices on the network now: only those wait on the strip.
        pub online: RefCell<HashSet<String>>,
        pub driving: RefCell<Option<String>>,
        pub(super) drag: RefCell<Option<Drag>>,
        pub(super) settle: RefCell<Option<Settle>>,
        /// The view drawn now. It holds still while a device moves or settles.
        pub(super) view: Cell<View>,
        /// The view gliding: from, to, and the frame time it started.
        pub(super) view_glide: Cell<Option<(View, View, Option<i64>)>>,
        pub size: Cell<(i32, i32)>,
        /// The device or chip under the pointer.
        pub hover: RefCell<Option<String>>,
        /// The chips on the strip as last drawn, by device ID (pixels).
        pub(super) chips: RefCell<Vec<(String, Rect)>>,
        /// When the preview being waited for was asked (monotonic time).
        pub preview_sent: Cell<Option<i64>>,
        /// The device moved on meanwhile: ask again when the answer comes.
        pub preview_queued: Cell<bool>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub gesture: OnceCell<gtk::GestureDrag>,
        pub on_place: RefCell<Option<PlaceHandler>>,
        pub on_unplace: RefCell<Option<PeerHandler>>,
        pub on_preview: RefCell<Option<PlaceHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArrangementCanvas {
        const NAME: &'static str = "RinflowArrangementCanvas";
        type Type = super::ArrangementCanvas;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("arrangement");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for ArrangementCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            let widget = self.obj();
            widget.add_css_class("arrangement-canvas");
            widget.add_css_class("card");
            widget.set_hexpand(true);
            widget.set_overflow(gtk::Overflow::Hidden);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                widget,
                move |gesture, x, y| widget.drag_begin(gesture, x, y)
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak]
                widget,
                move |gesture, dx, dy| {
                    if let Some((x, y)) = gesture.start_point() {
                        widget.drag_update(x + dx, y + dy);
                    }
                }
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak]
                widget,
                move |_, _, _| widget.drag_end()
            ));
            // The pointer taken away (a grab, the window hidden): put back.
            drag.connect_cancel(glib::clone!(
                #[weak]
                widget,
                move |_, _| widget.cancel_drag()
            ));
            widget.add_controller(drag.clone());
            let _ = self.gesture.set(drag);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak]
                widget,
                move |_, x, y| widget.hover_at(Some((x, y)))
            ));
            motion.connect_leave(glib::clone!(
                #[weak]
                widget,
                move |_| widget.hover_at(None)
            ));
            widget.add_controller(motion);

            // Escape puts a dragged device back, wherever the focus is.
            let keys = gtk::ShortcutController::new();
            keys.set_scope(gtk::ShortcutScope::Global);
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string("Escape"),
                Some(gtk::CallbackAction::new(|widget, _| {
                    let Some(canvas) = widget.downcast_ref::<super::ArrangementCanvas>() else {
                        return glib::Propagation::Proceed;
                    };
                    if canvas.imp().drag.borrow().is_none() {
                        return glib::Propagation::Proceed;
                    }
                    canvas.cancel_drag();
                    if let Some(gesture) = canvas.imp().gesture.get() {
                        gesture.set_state(gtk::EventSequenceState::Denied);
                    }
                    glib::Propagation::Stop
                })),
            ));
            widget.add_controller(keys);
        }

        fn dispose(&self) {
            if let Some(tick) = self.tick.borrow_mut().take() {
                tick.remove();
            }
        }
    }

    impl WidgetImpl for ArrangementCanvas {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (280, 520, -1, -1),
                _ => (260, 340, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct ArrangementCanvas(ObjectSubclass<imp::ArrangementCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArrangementCanvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArrangementCanvas {
    /// A device dropped at this offset: the agent snaps and stores it.
    pub fn connect_place(&self, handler: impl Fn(&str, Offset) + 'static) {
        *self.imp().on_place.borrow_mut() = Some(Box::new(handler));
    }

    /// A placed device dropped on the strip.
    pub fn connect_unplace(&self, handler: impl Fn(&str) + 'static) {
        *self.imp().on_unplace.borrow_mut() = Some(Box::new(handler));
    }

    /// Where would a device dropped at this offset land? The answer comes
    /// back through `set_preview`.
    pub fn connect_preview(&self, handler: impl Fn(&str, Offset) + 'static) {
        *self.imp().on_preview.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_layout(
        &self,
        layout: LayoutView,
        local_name: &str,
        connected: HashSet<String>,
        online: HashSet<String>,
        driving: Option<String>,
    ) {
        let imp = self.imp();
        // Redraw only when something drawn changed, not on every snapshot.
        if *imp.layout.borrow() == layout
            && *imp.local_name.borrow() == local_name
            && *imp.connected.borrow() == connected
            && *imp.online.borrow() == online
            && *imp.driving.borrow() == driving
        {
            return;
        }
        if let Some(settle) = imp.settle.borrow_mut().as_mut()
            && settle.waits_for.as_ref().is_some_and(|before| *before != layout)
        {
            settle.answered = true;
        }
        // The dragged device was forgotten meanwhile: nothing to drag.
        let dragged = imp.drag.borrow().as_ref().map(|drag| drag.peer_id.clone());
        let gone = dragged.is_some_and(|peer| !layout.devices.iter().any(|d| d.peer_id == peer));
        *imp.layout.borrow_mut() = layout;
        *imp.local_name.borrow_mut() = local_name.to_owned();
        *imp.connected.borrow_mut() = connected;
        *imp.online.borrow_mut() = online;
        *imp.driving.borrow_mut() = driving;
        if gone {
            imp.drag.borrow_mut().take();
            self.set_cursor_from_name(None);
        }
        self.update_description();
        self.queue_draw();
    }

    /// Where the agent says the dragged device would land (`None`: nowhere
    /// from there).
    pub fn set_preview(&self, peer_id: &str, offset: Option<Offset>) {
        let imp = self.imp();
        imp.preview_sent.set(None);
        let queued = imp.preview_queued.replace(false);
        {
            let mut drag = imp.drag.borrow_mut();
            let Some(drag) = drag.as_mut().filter(|drag| drag.peer_id == peer_id) else {
                return;
            };
            // Asked before it reached the strip.
            if drag.over_strip {
                return;
            }
            let view = imp.view.get();
            drag.landing = offset.map(|o| view.px(drag.bounds.x + o.x, drag.bounds.y + o.y));
            drag.update_magnet();        }
        if queued {
            self.request_preview();
        }
        self.queue_draw();
    }

    /// For screen readers: what the drawing shows.
    fn update_description(&self) {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let placed: Vec<&str> = layout
            .devices
            .iter()
            .filter(|d| d.offset.is_some())
            .map(|d| d.name.as_str())
            .collect();
        let mut text = tr("Arrangement");
        if !placed.is_empty() {
            text.push_str(": ");
            text.push_str(&placed.join(", "));
        }
        self.update_property(&[gtk::accessible::Property::Label(&text)]);
    }

    // ---- The view ------------------------------------------------------

    /// The view that fits this device's screens, with room around them for
    /// placing, and every placed peer.
    fn fit(&self) -> View {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let local = Rect::around(&layout.displays).unwrap_or(Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 });
        let mut world = Rect {
            x: local.x - local.w * ROOM_X,
            y: local.y - local.h * ROOM_Y,
            w: local.w * (1.0 + 2.0 * ROOM_X),
            h: local.h * (1.0 + 2.0 * ROOM_Y),
        };
        for device in &layout.devices {
            if let (Some(offset), Some(bounds)) = (device.offset, Rect::around(&device.displays)) {
                world = world.union(&Rect { x: bounds.x + offset.x, y: bounds.y + offset.y, ..bounds });
            }
        }
        let width = (f64::from(self.width()) - 2.0 * PAD_SIDE).max(40.0);
        let height = (f64::from(self.height()) - PAD_TOP - PAD_BOTTOM).max(40.0);
        let scale = (width / world.w.max(1.0)).min(height / world.h.max(1.0)).max(1e-4);
        View {
            left: world.x - (width / scale - world.w) / 2.0 - PAD_SIDE / scale,
            top: world.y - (height / scale - world.h) / 2.0 - PAD_TOP / scale,
            scale,
        }
    }

    fn frame_time(&self) -> i64 {
        self.frame_clock().map_or_else(glib::monotonic_time, |clock| clock.frame_time())
    }

    /// The view to draw with now: still while a device moves or settles,
    /// else gliding to the fit (at once when the canvas changed size).
    fn current_view(&self) -> View {
        let imp = self.imp();
        if imp.drag.borrow().is_some() || imp.settle.borrow().is_some() {
            return imp.view.get();
        }
        let size = (self.width(), self.height());
        let target = self.fit();
        let view = imp.view.get();
        if view.scale == 0.0 || imp.size.replace(size) != size {
            imp.view.set(target);
            imp.view_glide.set(None);
            return target;
        }
        match imp.view_glide.get() {
            Some((from, to, start)) if to.close(&target) => {
                let now = self.frame_time();
                let start = start.unwrap_or(now);
                let t = (now - start) as f64 / VIEW_US as f64;
                let view = if t >= 1.0 {
                    imp.view_glide.set(None);
                    to
                } else {
                    imp.view_glide.set(Some((from, to, Some(start))));
                    from.lerp(&to, ease_out(t))
                };
                imp.view.set(view);
                view
            }
            _ if view.close(&target) => {
                imp.view_glide.set(None);
                view
            }
            _ => {
                imp.view_glide.set(Some((view, target, None)));
                self.ensure_ticking();
                view
            }
        }
    }

    /// Frames while something glides.
    fn ensure_ticking(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|canvas, clock| {
            if canvas.tick(clock.frame_time()) {
                glib::ControlFlow::Continue
            } else {
                canvas.imp().tick.borrow_mut().take();
                glib::ControlFlow::Break
            }
        });
        *imp.tick.borrow_mut() = Some(id);
    }

    /// One frame: ends a settle that has glided and has its answer (or
    /// waited long enough). Whether to keep ticking.
    fn tick(&self, now: i64) -> bool {
        let imp = self.imp();
        let settled = imp.settle.borrow_mut().as_mut().is_some_and(|settle| {
            let elapsed = now - *settle.start.get_or_insert(now);
            elapsed >= SETTLE_US
                && (settle.waits_for.is_none() || settle.answered || elapsed >= SETTLE_US + SNAPSHOT_WAIT_US)
        });
        if settled {
            imp.settle.borrow_mut().take();
        }
        self.queue_draw();
        imp.settle.borrow().is_some() || imp.view_glide.get().is_some()
    }

    // ---- Dragging ------------------------------------------------------

    /// What is under this point: a chip on the strip, or a placed device.
    fn target_at(&self, px: f64, py: f64) -> Option<Target> {
        let imp = self.imp();
        if let Some((peer_id, _)) = imp.chips.borrow().iter().find(|(_, rect)| rect.contains(px, py)) {
            return Some(Target { peer_id: peer_id.clone(), from_strip: true });
        }
        if py >= f64::from(self.height()) - STRIP_HEIGHT {
            return None;
        }
        let view = imp.view.get();
        if view.scale == 0.0 {
            return None;
        }
        let (wx, wy) = view.world(px, py);
        let settling = imp.settle.borrow().as_ref().map(|settle| settle.peer_id.clone());
        let layout = imp.layout.borrow();
        layout
            .devices
            .iter()
            .filter(|device| settling.as_ref() != Some(&device.peer_id))
            .find(|device| {
                device.offset.is_some_and(|offset| {
                    device.displays.iter().any(|d| Rect { x: d.x + offset.x, y: d.y + offset.y, w: d.width, h: d.height }.contains(wx, wy))
                })
            })
            .map(|device| Target { peer_id: device.peer_id.clone(), from_strip: false })
    }

    fn hover_at(&self, point: Option<(f64, f64)>) {
        let imp = self.imp();
        if imp.drag.borrow().is_some() {
            return;
        }
        let hover = point.and_then(|(x, y)| self.target_at(x, y)).map(|target| target.peer_id);
        self.set_cursor_from_name(hover.as_ref().map(|_| "grab"));
        if *imp.hover.borrow() != hover {
            *imp.hover.borrow_mut() = hover;
            self.queue_draw();
        }
    }

    fn drag_begin(&self, gesture: &gtk::GestureDrag, px: f64, py: f64) {
        let imp = self.imp();
        if imp.drag.borrow().is_some() {
            return;
        }
        let Some(target) = self.target_at(px, py) else {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        };
        let device = imp.layout.borrow().devices.iter().find(|d| d.peer_id == target.peer_id).cloned();
        let Some(device) = device else { return };
        let Some(bounds) = Rect::around(&device.displays) else { return };
        // Whatever was gliding stops where it is.
        imp.settle.borrow_mut().take();
        imp.view_glide.set(None);
        let view = imp.view.get();
        let (grab, free, origin) = match device.offset.filter(|_| !target.from_strip) {
            Some(offset) => {
                let at = view.px(bounds.x + offset.x, bounds.y + offset.y);
                ((px - at.0, py - at.1), at, Some(at))
            }
            // From a chip: the device at full size, held by its middle.
            None => {
                let grab = (bounds.w * view.scale / 2.0, bounds.h * view.scale / 2.0);
                (grab, (px - grab.0, py - grab.1), None)
            }
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        *imp.drag.borrow_mut() = Some(Drag {
            peer_id: device.peer_id,
            displays: device.displays,
            bounds,
            from_strip: target.from_strip,
            origin,
            start: (px, py),
            grab,
            free,
            moved: false,
            over_strip: false,
            landing: None,
            snapped: false,
        });
        imp.preview_sent.set(None);
        imp.preview_queued.set(false);
        self.set_cursor_from_name(Some("grabbing"));
        self.queue_draw();
    }

    fn drag_update(&self, px: f64, py: f64) {
        let imp = self.imp();
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let ask = {
            let mut drag = imp.drag.borrow_mut();
            let Some(drag) = drag.as_mut() else { return };
            drag.moved |= distance((px, py), drag.start) >= DRAG_THRESHOLD;
            if !drag.moved {
                return;
            }
            drag.free = (px - drag.grab.0, py - drag.grab.1);
            // Over the strip a device lands nowhere: dropped there, a placed
            // one is unplaced and one from a chip goes back.
            drag.over_strip = (0.0..=width).contains(&px) && (height - STRIP_HEIGHT..=height).contains(&py);
            if drag.over_strip {
                drag.landing = None;
            }
            drag.update_magnet();
            !drag.over_strip
        };
        self.queue_draw();
        if ask {
            self.request_preview();
        }
    }

    /// The offset the dragged device has where the pointer has it (not where
    /// the magnet holds it: the core snaps from there, and the magnet lets go
    /// when that is far enough).
    fn dragged_offset(&self, drag: &Drag) -> Offset {
        let (x, y) = self.imp().view.get().world(drag.free.0, drag.free.1);
        Offset { x: x - drag.bounds.x, y: y - drag.bounds.y }
    }

    /// Asks the agent where the dragged device would land; one question at a
    /// time, the latest position once the answer comes.
    fn request_preview(&self) {
        let imp = self.imp();
        let now = glib::monotonic_time();
        if imp.preview_sent.get().is_some_and(|sent| now - sent < PREVIEW_TIMEOUT_US) {
            imp.preview_queued.set(true);
            return;
        }
        let Some((peer_id, offset)) = imp.drag.borrow().as_ref().map(|drag| (drag.peer_id.clone(), self.dragged_offset(drag)))
        else {
            return;
        };
        imp.preview_sent.set(Some(now));
        imp.preview_queued.set(false);
        if let Some(handler) = imp.on_preview.borrow().as_ref() {
            handler(&peer_id, offset);
        }
    }

    fn drag_end(&self) {
        let imp = self.imp();
        let Some(drag) = imp.drag.borrow_mut().take() else { return };
        imp.preview_sent.set(None);
        imp.preview_queued.set(false);
        // What is under the pointer now (the device it held has moved).
        imp.hover.borrow_mut().take();
        self.queue_draw();
        if !drag.moved {
            self.hover_at(Some(drag.start));
            return;
        }
        self.set_cursor_from_name(None);
        let layout = imp.layout.borrow().clone();
        let settle = |to: (f64, f64), gone: bool| Settle {
            peer_id: drag.peer_id.clone(),
            displays: drag.displays.clone(),
            bounds: drag.bounds,
            from: drag.drawn_at(),
            to,
            start: None,
            waits_for: Some(layout.clone()),
            answered: false,
            gone,
        };
        let settle = if drag.over_strip {
            if drag.from_strip {
                // Back on the strip, where it was.
                None
            } else {
                if let Some(handler) = imp.on_unplace.borrow().as_ref() {
                    handler(&drag.peer_id);
                }
                Some(settle(drag.free, true))
            }
        } else {
            // Proposed where it was dropped, even outside the canvas; the
            // core snaps it to the spot the preview showed. Until the
            // snapshot comes it stays there (or where it was dropped, if the
            // agent hasn't said yet).
            if let Some(handler) = imp.on_place.borrow().as_ref() {
                handler(&drag.peer_id, self.dragged_offset(&drag));
            }
            Some(settle(drag.landing.unwrap_or(drag.free), false))
        };
        if let Some(settle) = settle {
            *imp.settle.borrow_mut() = Some(settle);
            self.ensure_ticking();
        }
    }

    /// Puts the dragged device back where it was, gliding.
    fn cancel_drag(&self) {
        let imp = self.imp();
        let Some(drag) = imp.drag.borrow_mut().take() else { return };
        imp.preview_sent.set(None);
        imp.preview_queued.set(false);
        imp.hover.borrow_mut().take();
        self.set_cursor_from_name(None);
        if let (true, Some(origin)) = (drag.moved, drag.origin) {
            *imp.settle.borrow_mut() = Some(Settle {
                peer_id: drag.peer_id.clone(),
                displays: drag.displays.clone(),
                bounds: drag.bounds,
                from: drag.drawn_at(),
                to: origin,
                start: None,
                waits_for: None,
                answered: false,
                gone: false,
            });
            self.ensure_ticking();
        }
        self.queue_draw();
    }

    // ---- Drawing -------------------------------------------------------

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let view = self.current_view();
        imp.view.set(view);
        let colors = Colors::of(self.upcast_ref());
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let strip_top = height - STRIP_HEIGHT;

        self.draw_dots(snapshot, width, strip_top, &colors);

        let layout = imp.layout.borrow();
        let drag = imp.drag.borrow();
        let settle = imp.settle.borrow();
        let hover = imp.hover.borrow();
        let connected = imp.connected.borrow();
        let online = imp.online.borrow();
        let driving = imp.driving.borrow();

        // This device.
        if let Some(local) = Rect::around(&layout.displays) {
            let local_name = imp.local_name.borrow();
            let title = if local_name.is_empty() { tr("This device") } else { local_name.clone() };
            let style = Style {
                fill: colors.accent,
                border: None,
                text: colors.accent_fg,
                text_dim: colors.accent_fg_dim,
                shadow: false,
                dashed: false,
            };
            let at = view.px(local.x, local.y);
            self.draw_device(snapshot, &layout.displays, local, at, view.scale, &style, &title, &tr("This device"));
        }

        // Placed peers, but the one moving.
        let moving = |peer: &str| {
            drag.as_ref().is_some_and(|drag| drag.peer_id == peer) || settle.as_ref().is_some_and(|settle| settle.peer_id == peer)
        };
        for device in &layout.devices {
            let (Some(offset), Some(bounds)) = (device.offset, Rect::around(&device.displays)) else { continue };
            if moving(&device.peer_id) {
                continue;
            }
            let is_connected = connected.contains(&device.peer_id);
            let style = self.peer_style(
                &colors,
                is_connected,
                online.contains(&device.peer_id),
                driving.as_deref() == Some(device.peer_id.as_str()),
                hover.as_deref() == Some(device.peer_id.as_str()),
            );
            let at = view.px(bounds.x + offset.x, bounds.y + offset.y);
            let subtitle = subtitle(device, is_connected, online.contains(&device.peer_id));
            self.draw_device(snapshot, &device.displays, bounds, at, view.scale, &style, &device.name, &subtitle);
        }

        let name_of = |peer: &str| layout.devices.iter().find(|d| d.peer_id == peer);

        // Where the dragged device lands, while it isn't there.
        if let Some(drag) = drag.as_ref().filter(|drag| drag.moved && !drag.snapped && !drag.over_strip)
            && let Some(landing) = drag.landing
        {
            let style = Style {
                fill: colors.accent_soft,
                border: Some(colors.accent),
                text: colors.fg,
                text_dim: colors.fg_dim,
                shadow: false,
                dashed: true,
            };
            self.draw_device(snapshot, &drag.displays, drag.bounds, landing, view.scale, &style, "", "");
        }

        // Gliding to its spot.
        if let Some(settle) = settle.as_ref().filter(|settle| !settle.gone) {
            let t = settle.start.map_or(0.0, |start| (self.frame_time() - start) as f64 / SETTLE_US as f64);
            let t = ease_out(t);
            let at = (settle.from.0 + (settle.to.0 - settle.from.0) * t, settle.from.1 + (settle.to.1 - settle.from.1) * t);
            let device = name_of(&settle.peer_id);
            let peer_connected = connected.contains(&settle.peer_id);
            let peer_online = online.contains(&settle.peer_id);
            let style = self.peer_style(&colors, peer_connected, peer_online, false, false);
            let (title, sub) =
                device.map_or((String::new(), String::new()), |d| (d.name.clone(), subtitle(d, peer_connected, peer_online)));
            self.draw_device(snapshot, &settle.displays, settle.bounds, at, view.scale, &style, &title, &sub);
        }

        // The dragged device, above everything; over the strip, faded under
        // the strip's hint.
        let over_strip = drag.as_ref().is_some_and(|drag| drag.moved && drag.over_strip);
        if !over_strip {
            self.draw_strip(snapshot, &layout, drag.as_ref(), &online, &connected, hover.as_deref(), width, height, &colors);
        }
        if let Some(drag) = drag.as_ref().filter(|drag| drag.moved) {
            let device = name_of(&drag.peer_id);
            let peer_connected = connected.contains(&drag.peer_id);
            let peer_online = online.contains(&drag.peer_id);
            let mut style = self.peer_style(&colors, peer_connected, peer_online, false, true);
            style.shadow = !drag.over_strip;
            if drag.over_strip {
                snapshot.push_opacity(0.45);
            }
            // Over the strip its labels would cross the strip's hint.
            let (title, sub) = device
                .filter(|_| !drag.over_strip)
                .map_or((String::new(), String::new()), |d| (d.name.clone(), subtitle(d, peer_connected, peer_online)));
            self.draw_device(snapshot, &drag.displays, drag.bounds, drag.drawn_at(), view.scale, &style, &title, &sub);
            if drag.over_strip {
                snapshot.pop();
            }
        }
        if over_strip {
            self.draw_strip(snapshot, &layout, drag.as_ref(), &online, &connected, hover.as_deref(), width, height, &colors);
        }
    }

    fn peer_style(&self, colors: &Colors, connected: bool, online: bool, driving: bool, hover: bool) -> Style {
        let (fill, text, text_dim) = if driving {
            (colors.success, colors.success_fg, colors.success_fg)
        } else if connected {
            (colors.card, colors.fg, colors.fg_dim)
        } else if online {
            (colors.card_dim, colors.fg, colors.fg_dim)
        } else {
            (colors.card_dim, colors.fg_dim, colors.fg_dim)
        };
        Style {
            fill,
            border: Some(if hover { colors.accent } else { colors.border }),
            text,
            text_dim,
            shadow: false,
            dashed: false,
        }
    }

    fn draw_dots(&self, snapshot: &gtk::Snapshot, width: f64, bottom: f64, colors: &Colors) {
        let size = 2.2_f32;
        let mut y = DOT_SPACING / 2.0;
        while y < bottom - DOT_SPACING / 4.0 {
            let mut x = DOT_SPACING / 2.0;
            while x < width {
                let rect = graphene::Rect::new(x as f32 - size / 2.0, y as f32 - size / 2.0, size, size);
                let corner = graphene::Size::new(size / 2.0, size / 2.0);
                snapshot.push_rounded_clip(&gsk::RoundedRect::new(rect, corner, corner, corner, corner));
                snapshot.append_color(&colors.dot, &rect);
                snapshot.pop();
                x += DOT_SPACING;
            }
            y += DOT_SPACING;
        }
    }

    /// One device: each display to scale from `at` (the top-left corner of
    /// `bounds`, pixels), the name and a line under it on the main display.
    #[allow(clippy::too_many_arguments)]
    fn draw_device(
        &self,
        snapshot: &gtk::Snapshot,
        displays: &[Display],
        bounds: Rect,
        at: (f64, f64),
        scale: f64,
        style: &Style,
        title: &str,
        subtitle: &str,
    ) {
        let main = displays.iter().position(|d| d.x == 0.0 && d.y == 0.0).unwrap_or(0);
        for (index, display) in displays.iter().enumerate() {
            let shape = screen_shape(displays, index);
            let rect = Rect {
                x: at.0 + (display.x - bounds.x) * scale + shape.insets[0],
                y: at.1 + (display.y - bounds.y) * scale + shape.insets[1],
                w: (display.width * scale - shape.insets[0] - shape.insets[2]).max(2.0),
                h: (display.height * scale - shape.insets[1] - shape.insets[3]).max(2.0),
            };
            let radius = |r: f32| {
                let r = r.min((rect.w / 2.0) as f32).min((rect.h / 2.0) as f32);
                graphene::Size::new(r, r)
            };
            let rounded = gsk::RoundedRect::new(
                rect.graphene(),
                radius(shape.corners[0]),
                radius(shape.corners[1]),
                radius(shape.corners[2]),
                radius(shape.corners[3]),
            );
            if style.shadow {
                snapshot.append_outset_shadow(&rounded, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25), 0.0, 3.0, 0.0, 10.0);
            }
            snapshot.push_rounded_clip(&rounded);
            snapshot.append_color(&style.fill, &rect.graphene());
            snapshot.pop();
            if let Some(border) = style.border {
                if style.dashed {
                    let builder = gsk::PathBuilder::new();
                    builder.add_rounded_rect(&rounded);
                    let stroke = gsk::Stroke::new(1.5);
                    stroke.set_dash(&[5.0, 4.0]);
                    snapshot.append_stroke(&builder.to_path(), &stroke, &border);
                } else {
                    snapshot.append_border(&rounded, &[1.0; 4], &[border; 4]);
                }
            }
            if index == main {
                self.draw_labels(snapshot, rect, title, subtitle, style);
            }
        }
    }

    /// The name (bold) and a dimmer line under it, centred; small screens
    /// show less.
    fn draw_labels(&self, snapshot: &gtk::Snapshot, rect: Rect, title: &str, subtitle: &str, style: &Style) {
        let max_width = rect.w - 10.0;
        if max_width < 12.0 {
            return;
        }
        let title = (!title.is_empty() && rect.h >= 22.0).then(|| self.text(title, true, 0.9, max_width));
        let subtitle = (!subtitle.is_empty() && rect.h >= 48.0).then(|| self.text(subtitle, false, 0.8, max_width));
        let lines: Vec<(&pango::Layout, gdk::RGBA)> = title
            .iter()
            .map(|layout| (layout, style.text))
            .chain(subtitle.iter().map(|layout| (layout, style.text_dim)))
            .collect();
        let total: f64 = lines.iter().map(|(layout, _)| f64::from(layout.pixel_size().1)).sum();
        let mut y = rect.y + (rect.h - total) / 2.0;
        for (layout, color) in lines {
            let (w, h) = layout.pixel_size();
            snapshot.save();
            snapshot.translate(&graphene::Point::new((rect.x + (rect.w - f64::from(w)) / 2.0) as f32, y as f32));
            snapshot.append_layout(layout, &color);
            snapshot.restore();
            y += f64::from(h);
        }
    }

    fn text(&self, text: &str, bold: bool, scale: f64, max_width: f64) -> pango::Layout {
        let layout = self.create_pango_layout(Some(text));
        let attributes = pango::AttrList::new();
        if bold {
            attributes.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        }
        attributes.insert(pango::AttrFloat::new_scale(scale));
        layout.set_attributes(Some(&attributes));
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_width((max_width * f64::from(pango::SCALE)) as i32);
        layout.set_single_paragraph_mode(true);
        // Measured as wide as the text, not as the limit.
        let (w, _) = layout.pixel_size();
        if f64::from(w) < max_width {
            layout.set_width(-1);
        }
        layout
    }

    /// The strip along the bottom: the user's online devices that aren't
    /// placed yet, or a hint; dropping a placed device on it unplaces it.
    #[allow(clippy::too_many_arguments)]
    fn draw_strip(
        &self,
        snapshot: &gtk::Snapshot,
        layout: &LayoutView,
        drag: Option<&Drag>,
        online: &HashSet<String>,
        connected: &HashSet<String>,
        hover: Option<&str>,
        width: f64,
        height: f64,
        colors: &Colors,
    ) {
        let top = height - STRIP_HEIGHT;
        let unplacing = drag.is_some_and(|drag| drag.moved && drag.over_strip && !drag.from_strip);
        if unplacing {
            snapshot.append_color(&colors.accent_soft, &Rect { x: 0.0, y: top, w: width, h: STRIP_HEIGHT }.graphene());
        }
        snapshot.append_color(&colors.border, &Rect { x: 0.0, y: top, w: width, h: 1.0 }.graphene());

        let waiting: Vec<&DeviceView> = layout
            .devices
            .iter()
            .filter(|d| {
                d.offset.is_none() && online.contains(&d.peer_id) && (d.remembered || connected.contains(&d.peer_id))
            })
            .collect();
        let any_placed = layout.devices.iter().any(|d| d.offset.is_some());
        let mut chips = Vec::new();
        if unplacing || waiting.is_empty() {
            let hint = if unplacing {
                tr("Release to unplace")
            } else if any_placed {
                tr("Drag a device here to unplace it")
            } else {
                tr("Connect a device, then drag it next to your screen here")
            };
            let text = self.text(&hint, false, 0.85, width - 2.0 * PAD_SIDE);
            let (w, h) = text.pixel_size();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                ((width - f64::from(w)) / 2.0) as f32,
                (top + (STRIP_HEIGHT - f64::from(h)) / 2.0) as f32,
            ));
            snapshot.append_layout(&text, if unplacing { &colors.accent } else { &colors.fg_dim });
            snapshot.restore();
        } else {
            let mut x = PAD_SIDE / 2.0;
            let y = top + (STRIP_HEIGHT - CHIP_HEIGHT) / 2.0;
            let icon_size = 16.0;
            for device in waiting {
                let text = self.text(&device.name, false, 0.85, CHIP_MAX_WIDTH - icon_size - 30.0);
                let (w, h) = text.pixel_size();
                let rect = Rect { x, y, w: f64::from(w) + icon_size + 30.0, h: CHIP_HEIGHT };
                if rect.right() > width - PAD_SIDE / 2.0 {
                    break;
                }
                let dragged = drag.is_some_and(|drag| drag.peer_id == device.peer_id && drag.moved);
                let corner = graphene::Size::new((CHIP_HEIGHT / 2.0) as f32, (CHIP_HEIGHT / 2.0) as f32);
                let rounded = gsk::RoundedRect::new(rect.graphene(), corner, corner, corner, corner);
                if dragged {
                    snapshot.push_opacity(0.4);
                }
                snapshot.push_rounded_clip(&rounded);
                snapshot.append_color(&colors.card, &rect.graphene());
                snapshot.pop();
                let border = if hover == Some(device.peer_id.as_str()) { colors.accent } else { colors.border };
                snapshot.append_border(&rounded, &[1.0; 4], &[border; 4]);
                self.draw_icon(snapshot, platform_icon(&device.platform), rect.x + 10.0, rect.y + (CHIP_HEIGHT - icon_size) / 2.0, icon_size, &colors.fg);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(
                    (rect.x + 10.0 + icon_size + 8.0) as f32,
                    (rect.y + (CHIP_HEIGHT - f64::from(h)) / 2.0) as f32,
                ));
                snapshot.append_layout(&text, &colors.fg);
                snapshot.restore();
                if dragged {
                    snapshot.pop();
                }
                chips.push((device.peer_id.clone(), rect));
                x = rect.right() + 8.0;
            }
        }
        *self.imp().chips.borrow_mut() = chips;
    }

    fn draw_icon(&self, snapshot: &gtk::Snapshot, name: &str, x: f64, y: f64, size: f64, color: &gdk::RGBA) {
        let theme = gtk::IconTheme::for_display(&self.display());
        let icon = theme.lookup_icon(name, &[], size as i32, self.scale_factor(), gtk::TextDirection::None, gtk::IconLookupFlags::empty());
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x as f32, y as f32));
        icon.snapshot_symbolic(snapshot, size, size, &[*color]);
        snapshot.restore();
    }
}

/// The line under a peer's name: its platform and how it stands.
fn subtitle(device: &DeviceView, connected: bool, online: bool) -> String {
    let status = if connected {
        tr("Connected")
    } else if online {
        tr("Online")
    } else {
        tr("Offline")
    };
    format!("{} · {}", platform_name(&device.platform), status)
}

struct Style {
    fill: gdk::RGBA,
    border: Option<gdk::RGBA>,
    text: gdk::RGBA,
    text_dim: gdk::RGBA,
    shadow: bool,
    dashed: bool,
}

/// How one screen of a device is drawn: insets (left, top, right, bottom)
/// and corner radii (top-left, top-right, bottom-right, bottom-left).
struct ScreenShape {
    insets: [f64; 4],
    corners: [f32; 4],
}

/// Flush and square where the screen meets another screen of the same
/// device (one computer's monitors read as one shape), inset and rounded
/// elsewhere.
fn screen_shape(displays: &[Display], index: usize) -> ScreenShape {
    const TOLERANCE: f64 = 0.5;
    let d = Rect::of(&displays[index]);
    let mut insets = [SCREEN_GAP; 4];
    let mut corners = [RADIUS; 4];
    let within = |value: f64, low: f64, high: f64| value >= low - TOLERANCE && value <= high + TOLERANCE;
    for (other, display) in displays.iter().enumerate() {
        if other == index {
            continue;
        }
        let o = Rect::of(display);
        let shares_rows = d.bottom().min(o.bottom()) - d.y.max(o.y) > TOLERANCE;
        let shares_columns = d.right().min(o.right()) - d.x.max(o.x) > TOLERANCE;
        if shares_rows && (o.right() - d.x).abs() < TOLERANCE {
            insets[0] = 0.0;
            if within(d.y, o.y, o.bottom()) {
                corners[0] = 0.0;
            }
            if within(d.bottom(), o.y, o.bottom()) {
                corners[3] = 0.0;
            }
        }
        if shares_rows && (o.x - d.right()).abs() < TOLERANCE {
            insets[2] = 0.0;
            if within(d.y, o.y, o.bottom()) {
                corners[1] = 0.0;
            }
            if within(d.bottom(), o.y, o.bottom()) {
                corners[2] = 0.0;
            }
        }
        if shares_columns && (o.bottom() - d.y).abs() < TOLERANCE {
            insets[1] = 0.0;
            if within(d.x, o.x, o.right()) {
                corners[0] = 0.0;
            }
            if within(d.right(), o.x, o.right()) {
                corners[1] = 0.0;
            }
        }
        if shares_columns && (o.y - d.bottom()).abs() < TOLERANCE {
            insets[3] = 0.0;
            if within(d.x, o.x, o.right()) {
                corners[3] = 0.0;
            }
            if within(d.right(), o.x, o.right()) {
                corners[2] = 0.0;
            }
        }
    }
    ScreenShape { insets, corners }
}

/// The theme's colours, so the canvas follows light, dark and the accent.
struct Colors {
    accent: gdk::RGBA,
    accent_fg: gdk::RGBA,
    accent_fg_dim: gdk::RGBA,
    accent_soft: gdk::RGBA,
    card: gdk::RGBA,
    card_dim: gdk::RGBA,
    border: gdk::RGBA,
    fg: gdk::RGBA,
    fg_dim: gdk::RGBA,
    dot: gdk::RGBA,
    success: gdk::RGBA,
    success_fg: gdk::RGBA,
}

impl Colors {
    #[allow(deprecated)]
    fn of(widget: &gtk::Widget) -> Self {
        let context = widget.style_context();
        let named = |name: &str, fallback: gdk::RGBA| context.lookup_color(name).unwrap_or(fallback);
        let fg = widget.color();
        let with_alpha = |color: gdk::RGBA, alpha: f32| gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha);
        let accent = named("accent_bg_color", gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
        let accent_fg = named("accent_fg_color", gdk::RGBA::WHITE);
        let dark = fg.red() > 0.5;
        let (card, card_dim) = if dark {
            (gdk::RGBA::new(1.0, 1.0, 1.0, 0.13), gdk::RGBA::new(1.0, 1.0, 1.0, 0.05))
        } else {
            (gdk::RGBA::new(0.0, 0.0, 0.0, 0.08), gdk::RGBA::new(0.0, 0.0, 0.0, 0.035))
        };
        Self {
            accent,
            accent_fg,
            accent_fg_dim: with_alpha(accent_fg, 0.75),
            accent_soft: with_alpha(accent, 0.14),
            card,
            card_dim,
            border: with_alpha(fg, 0.2),
            fg,
            fg_dim: with_alpha(fg, 0.55),
            dot: with_alpha(fg, if dark { 0.16 } else { 0.14 }),
            success: named("success_bg_color", gdk::RGBA::new(0.18, 0.76, 0.49, 1.0)),
            success_fg: named("success_fg_color", gdk::RGBA::WHITE),
        }
    }
}
