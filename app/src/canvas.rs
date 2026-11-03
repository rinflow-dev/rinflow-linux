//! The arrangement: this device's screens and the peers around them, to
//! scale, in the theme's colours. Dragging a peer asks the agent where it
//! would snap (shown as a ghost) and places it there on release.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use rinflow_core::layout::{Display, Offset};
use rinflow_shell::state::{DeviceView, LayoutView};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

const PADDING: f64 = 16.0;
/// Space between the arrangement and the row of unplaced devices.
const SHELF_GAP: f64 = 0.35;

type PlaceHandler = Box<dyn Fn(&str, Offset)>;
type PreviewHandler = Box<dyn Fn(&str, Offset)>;

#[derive(Clone, Debug)]
pub(crate) struct Drag {
    peer_id: String,
    /// Where in the device (world units) the pointer grabbed it.
    grab: (f64, f64),
    /// The device's top-left now, world units.
    at: (f64, f64),
    displays: Vec<Display>,
}

/// The view's mapping from world (logical points) to pixels.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct View {
    left: f64,
    top: f64,
    scale: f64,
}

impl View {
    fn to_pixels(&self, x: f64, y: f64) -> (f64, f64) {
        (
            PADDING + (x - self.left) * self.scale,
            PADDING + (y - self.top) * self.scale,
        )
    }

    fn to_world(&self, px: f64, py: f64) -> (f64, f64) {
        (
            (px - PADDING) / self.scale + self.left,
            (py - PADDING) / self.scale + self.top,
        )
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArrangementCanvas {
        pub layout: RefCell<LayoutView>,
        pub connected: RefCell<HashSet<String>>,
        pub driving: RefCell<Option<String>>,
        pub(super) drag: RefCell<Option<Drag>>,
        pub preview: RefCell<Option<(String, Offset)>>,
        pub(super) view: Cell<View>,
        /// Where unplaced devices are drawn, world units, by ID.
        pub shelf: RefCell<Vec<(String, (f64, f64), Vec<Display>)>>,
        pub on_place: RefCell<Option<PlaceHandler>>,
        pub on_preview: RefCell<Option<PreviewHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArrangementCanvas {
        const NAME: &'static str = "RinflowArrangementCanvas";
        type Type = super::ArrangementCanvas;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("arrangement");
        }
    }

    impl ObjectImpl for ArrangementCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            let widget = self.obj();
            widget.add_css_class("arrangement-canvas");
            widget.add_css_class("card");
            widget.set_hexpand(true);
            widget.set_cursor_from_name(Some("default"));

            let drag = gtk::GestureDrag::new();
            drag.set_button(gtk::gdk::BUTTON_PRIMARY);
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                widget,
                move |_, x, y| widget.drag_begin(x, y)
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
            widget.add_controller(drag);
        }
    }

    impl WidgetImpl for ArrangementCanvas {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (240, 480, -1, -1),
                _ => (240, 300, -1, -1),
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
    pub fn connect_place(&self, handler: impl Fn(&str, Offset) + 'static) {
        *self.imp().on_place.borrow_mut() = Some(Box::new(handler));
    }

    pub fn connect_preview(&self, handler: impl Fn(&str, Offset) + 'static) {
        *self.imp().on_preview.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_layout(&self, layout: LayoutView, connected: HashSet<String>, driving: Option<String>) {
        let imp = self.imp();
        *imp.layout.borrow_mut() = layout;
        *imp.connected.borrow_mut() = connected;
        *imp.driving.borrow_mut() = driving;
        // A device that was placed while being dragged elsewhere: the
        // agent's word wins.
        self.queue_draw();
    }

    /// Where the agent says a dragged peer would land.
    pub fn set_preview(&self, peer_id: &str, offset: Option<Offset>) {
        let imp = self.imp();
        let dragging = imp
            .drag
            .borrow()
            .as_ref()
            .is_some_and(|drag| drag.peer_id == peer_id);
        *imp.preview.borrow_mut() = match offset {
            Some(offset) if dragging => Some((peer_id.to_owned(), offset)),
            _ => None,
        };
        self.queue_draw();
    }

    /// The view that fits everything: the arrangement, and a shelf of
    /// unplaced devices beneath it.
    fn fit(&self) -> View {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let drag = imp.drag.borrow();
        let mut world = Bounds::default();
        for display in &layout.displays {
            world.extend(display.x, display.y, display.width, display.height);
        }
        let mut unplaced: Vec<&DeviceView> = Vec::new();
        for device in &layout.devices {
            match device.offset {
                Some(offset) => {
                    for display in &device.displays {
                        world.extend(display.x + offset.x, display.y + offset.y, display.width, display.height);
                    }
                }
                None => unplaced.push(device),
            }
        }
        if world.is_empty() {
            world.extend(0.0, 0.0, 1920.0, 1080.0);
        }
        // The dragged device too, wherever it is.
        if let Some(drag) = drag.as_ref() {
            for display in &drag.displays {
                world.extend(display.x + drag.at.0, display.y + drag.at.1, display.width, display.height);
            }
        }
        let Bounds { left, top, mut right, mut bottom } = world;
        // The shelf: unplaced devices in a row below, at half size.
        let mut shelf = Vec::new();
        let gap = (bottom - top) * SHELF_GAP;
        let mut x = left;
        let shelf_top = bottom + gap;
        let mut shelf_bottom = shelf_top;
        for device in unplaced {
            if drag.as_ref().is_some_and(|drag| drag.peer_id == device.peer_id) {
                continue;
            }
            let displays: Vec<Display> = device
                .displays
                .iter()
                .map(|d| Display {
                    x: d.x * 0.5,
                    y: d.y * 0.5,
                    width: d.width * 0.5,
                    height: d.height * 0.5,
                    scale: d.scale,
                })
                .collect();
            let (l, t, r, b) = bounds(&displays);
            shelf.push((device.peer_id.clone(), (x - l, shelf_top - t), displays));
            x += (r - l) + gap * 0.5;
            shelf_bottom = shelf_bottom.max(shelf_top + (b - t));
        }
        if !shelf.is_empty() {
            right = right.max(x);
            bottom = shelf_bottom;
        }
        *imp.shelf.borrow_mut() = shelf;

        let width = f64::from(self.width()).max(1.0) - 2.0 * PADDING;
        let height = f64::from(self.height()).max(1.0) - 2.0 * PADDING;
        let scale = (width / (right - left).max(1.0))
            .min(height / (bottom - top).max(1.0))
            .max(0.001);
        // Centred.
        let used_w = (right - left) * scale;
        let used_h = (bottom - top) * scale;
        View {
            left: left - (width - used_w) / 2.0 / scale,
            top: top - (height - used_h) / 2.0 / scale,
            scale,
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let view = self.fit();
        imp.view.set(view);
        let layout = imp.layout.borrow();
        let colors = Colors::of(self.upcast_ref());

        for display in &layout.displays {
            self.draw_display(snapshot, &view, display, 0.0, 0.0, &colors.accent, &colors.accent_fg, None);
        }
        if let Some((l, t, r, b)) = Some(bounds(&layout.displays)).filter(|b| b.0 < b.2) {
            let name = gettextrs::gettext("This device");
            self.draw_label(snapshot, &view, (l + r) / 2.0, (t + b) / 2.0, &name, &colors.accent_fg, true);
        }

        let drag = imp.drag.borrow();
        let preview = imp.preview.borrow();
        for device in &layout.devices {
            if drag.as_ref().is_some_and(|drag| drag.peer_id == device.peer_id) {
                continue;
            }
            let Some(offset) = device.offset else { continue };
            let connected = imp.connected.borrow().contains(&device.peer_id);
            let driving = imp.driving.borrow().as_deref() == Some(device.peer_id.as_str());
            let fill = if driving {
                &colors.success
            } else if connected {
                &colors.card
            } else {
                &colors.card_dim
            };
            for display in &device.displays {
                self.draw_display(snapshot, &view, display, offset.x, offset.y, fill, &colors.fg, Some(&colors.border));
            }
            let (l, t, r, b) = bounds(&device.displays);
            self.draw_label(
                snapshot,
                &view,
                (l + r) / 2.0 + offset.x,
                (t + b) / 2.0 + offset.y,
                &device.name,
                if driving { &colors.success_fg } else { &colors.fg },
                connected,
            );
        }
        for (peer_id, at, displays) in imp.shelf.borrow().iter() {
            let name = layout
                .devices
                .iter()
                .find(|d| &d.peer_id == peer_id)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            for display in displays {
                self.draw_display(snapshot, &view, display, at.0, at.1, &colors.card_dim, &colors.fg, Some(&colors.border));
            }
            let (l, t, r, b) = bounds(displays);
            self.draw_label(snapshot, &view, (l + r) / 2.0 + at.0, (t + b) / 2.0 + at.1, &name, &colors.fg_dim, false);
        }
        if let Some((peer_id, offset)) = preview.as_ref()
            && let Some(device) = layout.devices.iter().find(|d| &d.peer_id == peer_id)
        {
            for display in &device.displays {
                self.draw_display(snapshot, &view, display, offset.x, offset.y, &colors.ghost, &colors.fg, Some(&colors.accent));
            }
        }
        if let Some(drag) = drag.as_ref() {
            for display in &drag.displays {
                self.draw_display(snapshot, &view, display, drag.at.0, drag.at.1, &colors.card, &colors.fg, Some(&colors.accent));
            }
            let (l, t, r, b) = bounds(&drag.displays);
            let name = layout
                .devices
                .iter()
                .find(|d| d.peer_id == drag.peer_id)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            self.draw_label(snapshot, &view, (l + r) / 2.0 + drag.at.0, (t + b) / 2.0 + drag.at.1, &name, &colors.fg, true);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_display(
        &self,
        snapshot: &gtk::Snapshot,
        view: &View,
        display: &Display,
        dx: f64,
        dy: f64,
        fill: &gtk::gdk::RGBA,
        _fg: &gtk::gdk::RGBA,
        border: Option<&gtk::gdk::RGBA>,
    ) {
        let (x, y) = view.to_pixels(display.x + dx, display.y + dy);
        let w = display.width * view.scale;
        let h = display.height * view.scale;
        let rect = gtk::graphene::Rect::new(x as f32 + 1.0, y as f32 + 1.0, (w - 2.0).max(1.0) as f32, (h - 2.0).max(1.0) as f32);
        let radius = gtk::graphene::Size::new(6.0, 6.0);
        let rounded = gtk::gsk::RoundedRect::new(rect, radius, radius, radius, radius);
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(fill, &rect);
        snapshot.pop();
        if let Some(border) = border {
            snapshot.append_border(&rounded, &[1.0; 4], &[*border; 4]);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_label(&self, snapshot: &gtk::Snapshot, view: &View, x: f64, y: f64, text: &str, color: &gtk::gdk::RGBA, strong: bool) {
        let layout = self.create_pango_layout(Some(text));
        layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let mut attributes = gtk::pango::AttrList::new();
        if strong {
            attributes.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
        }
        attributes.insert(gtk::pango::AttrFloat::new_scale(0.9));
        layout.set_attributes(Some(&attributes));
        let (px, py) = view.to_pixels(x, y);
        let (width, height) = layout.pixel_size();
        snapshot.save();
        snapshot.translate(&gtk::graphene::Point::new(
            (px - f64::from(width) / 2.0) as f32,
            (py - f64::from(height) / 2.0) as f32,
        ));
        snapshot.append_layout(&layout, color);
        snapshot.restore();
        let _ = &mut attributes;
    }

    fn device_at(&self, px: f64, py: f64) -> Option<(String, (f64, f64), Vec<Display>)> {
        let imp = self.imp();
        let view = imp.view.get();
        let (wx, wy) = view.to_world(px, py);
        let layout = imp.layout.borrow();
        for device in &layout.devices {
            let Some(offset) = device.offset else { continue };
            if device.displays.iter().any(|d| contains(d, offset.x, offset.y, wx, wy)) {
                return Some((device.peer_id.clone(), (offset.x, offset.y), device.displays.clone()));
            }
        }
        for (peer_id, at, displays) in imp.shelf.borrow().iter() {
            if displays.iter().any(|d| contains(d, at.0, at.1, wx, wy)) {
                // Picked up from the shelf: full size from here on.
                let full = layout
                    .devices
                    .iter()
                    .find(|d| &d.peer_id == peer_id)
                    .map(|d| d.displays.clone())
                    .unwrap_or_else(|| displays.clone());
                return Some((peer_id.clone(), *at, full));
            }
        }
        None
    }

    fn drag_begin(&self, px: f64, py: f64) {
        let Some((peer_id, at, displays)) = self.device_at(px, py) else { return };
        let view = self.imp().view.get();
        let (wx, wy) = view.to_world(px, py);
        let grab = (wx - at.0, wy - at.1);
        // From the shelf the device is half size: keep the grab inside.
        let (l, t, r, b) = bounds(&displays);
        let grab = (grab.0.clamp(l, r), grab.1.clamp(t, b));
        *self.imp().drag.borrow_mut() = Some(Drag { peer_id, grab, at, displays });
        self.set_cursor_from_name(Some("grabbing"));
        self.queue_draw();
    }

    fn drag_update(&self, px: f64, py: f64) {
        let imp = self.imp();
        let view = imp.view.get();
        let (wx, wy) = view.to_world(px, py);
        let request = {
            let mut drag = imp.drag.borrow_mut();
            let Some(drag) = drag.as_mut() else { return };
            drag.at = (wx - drag.grab.0, wy - drag.grab.1);
            (drag.peer_id.clone(), Offset { x: drag.at.0, y: drag.at.1 })
        };
        if let Some(handler) = imp.on_preview.borrow().as_ref() {
            handler(&request.0, request.1);
        }
        self.queue_draw();
    }

    fn drag_end(&self) {
        let imp = self.imp();
        self.set_cursor_from_name(Some("default"));
        let Some(drag) = imp.drag.borrow_mut().take() else { return };
        *imp.preview.borrow_mut() = None;
        if let Some(handler) = imp.on_place.borrow().as_ref() {
            handler(&drag.peer_id, Offset { x: drag.at.0, y: drag.at.1 });
        }
        self.queue_draw();
    }
}

/// A growing bounding box in world units.
struct Bounds {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self { left: f64::MAX, top: f64::MAX, right: f64::MIN, bottom: f64::MIN }
    }
}

impl Bounds {
    fn extend(&mut self, x: f64, y: f64, w: f64, h: f64) {
        self.left = self.left.min(x);
        self.top = self.top.min(y);
        self.right = self.right.max(x + w);
        self.bottom = self.bottom.max(y + h);
    }

    fn is_empty(&self) -> bool {
        self.left > self.right
    }
}

fn bounds(displays: &[Display]) -> (f64, f64, f64, f64) {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for d in displays {
        b.0 = b.0.min(d.x);
        b.1 = b.1.min(d.y);
        b.2 = b.2.max(d.x + d.width);
        b.3 = b.3.max(d.y + d.height);
    }
    if b.0 > b.2 {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        b
    }
}

fn contains(display: &Display, dx: f64, dy: f64, x: f64, y: f64) -> bool {
    x >= display.x + dx
        && x < display.x + dx + display.width
        && y >= display.y + dy
        && y < display.y + dy + display.height
}

/// The theme's colours, so the canvas follows light, dark and the accent.
struct Colors {
    accent: gtk::gdk::RGBA,
    accent_fg: gtk::gdk::RGBA,
    card: gtk::gdk::RGBA,
    card_dim: gtk::gdk::RGBA,
    ghost: gtk::gdk::RGBA,
    border: gtk::gdk::RGBA,
    fg: gtk::gdk::RGBA,
    fg_dim: gtk::gdk::RGBA,
    success: gtk::gdk::RGBA,
    success_fg: gtk::gdk::RGBA,
}

impl Colors {
    #[allow(deprecated)]
    fn of(widget: &gtk::Widget) -> Self {
        let context = widget.style_context();
        let named = |name: &str, fallback: gtk::gdk::RGBA| context.lookup_color(name).unwrap_or(fallback);
        let fg = widget.color();
        let accent = named("accent_bg_color", gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
        let accent_fg = named("accent_fg_color", gtk::gdk::RGBA::WHITE);
        let dark = fg.red() > 0.5;
        let card = if dark {
            gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 0.12)
        } else {
            gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.08)
        };
        let card_dim = if dark {
            gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 0.05)
        } else {
            gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.035)
        };
        let border = gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.2);
        let fg_dim = gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.55);
        let success = named("success_bg_color", gtk::gdk::RGBA::new(0.18, 0.76, 0.49, 1.0));
        let success_fg = named("success_fg_color", gtk::gdk::RGBA::WHITE);
        let ghost = gtk::gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.25);
        Self { accent, accent_fg, card, card_dim, ghost, border, fg, fg_dim, success, success_fg }
    }
}
