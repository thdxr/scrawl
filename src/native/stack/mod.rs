//! The capture stack (experiment `captureStack`): finished captures lift off the screen as their
//! contact sheet and collect as cards, under the capsule (`Variant::Dock`) or in the bottom-right
//! corner like CleanShot X's Quick Access Overlay (`Variant::Edge`).
//!
//! One transparent borderless `NSPanel` covers the display and never moves or resizes; each card
//! is a small tree of Core Animation layers placed every display-link tick inside one
//! `CATransaction` with actions off. The panel ignores the mouse except while the pointer is on a
//! card. Cards take hover, clicks (Paste, Open, ✕), a sideways two-finger swipe to dismiss, and a
//! drag that carries the contact sheet (⌥: the whole bundle) into another app as a file.

pub mod model;
mod trace;

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use core_video::base::{CVOptionFlags, CVTimeStamp};
use core_video::display_link::{CVDisplayLink, CVDisplayLinkRef};
use core_video::r#return::CVReturn;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSCursor, NSDragOperation, NSDraggingContext, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventPhase, NSFont,
    NSImage, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_quartz_core::{CALayer, CATextLayer, CATransaction, CATransform3D};

pub use self::model::{CardSpec, Geometry, Variant};
pub use self::model::CardFrame;
use self::model::{Elem, FOOTER_H, Hit, INSET, StackModel};
use crate::ui::motion;

/// The contact sheet's own ground (`render::SHEET_BG`), so a card and its sheet are one surface.
const SHEET_BG: [f64; 3] = [28.0 / 255.0, 29.0 / 255.0, 34.0 / 255.0];

unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

#[repr(C)]
struct OpaqueCGColor {
    _private: [u8; 0],
}

unsafe impl objc2::RefEncode for OpaqueCGColor {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
}

/// What a card asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackAction {
    Paste(PathBuf),
    Open(PathBuf),
    Closed(u64),
    DraggedOut(u64),
}

struct CardLayers {
    root: Retained<CALayer>,
    clip: Retained<CALayer>,
    sheet: Retained<CALayer>,
    frame: Retained<CALayer>,
    chrome: Retained<CALayer>,
    scrim: Retained<CALayer>,
    paste: Retained<CALayer>,
    open: Retained<CALayer>,
    destination: Retained<CATextLayer>,
    destination_text: String,
    close: Retained<CALayer>,
}

struct Runtime {
    model: StackModel,
    window: Retained<NSPanel>,
    host: Retained<CALayer>,
    layers: HashMap<u64, CardLayers>,
    /// The display's top-left in global points, and its size.
    display: [f32; 4],
    scale: f64,
    ignores_mouse: bool,
    /// `scrawl lab stack`: a scripted pointer instead of the mouse, drawn as a cursor.
    pretend: Option<Option<[f32; 2]>>,
    cursor: Option<Retained<CALayer>>,
    /// Where the mouse went down (local points) and on what.
    down: Option<([f32; 2], Hit)>,
    drag_started: bool,
    last_tick: f64,
    shown: bool,
    trace: Option<trace::StackTrace>,
    action_tx: flume::Sender<StackAction>,
}

// Touched only on the main thread (checked with `MainThreadMarker` before every use).
unsafe impl Send for Runtime {}

struct Shared {
    state: Mutex<Runtime>,
    tick_queued: AtomicBool,
    alive: AtomicBool,
}

struct ViewIvars {
    shared: std::cell::OnceCell<Arc<Shared>>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    struct ScrawlStackView;

    unsafe impl NSObjectProtocol for ScrawlStackView {}

    impl ScrawlStackView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(shared) = self.ivars().shared.get() {
                shared.mouse_down(event);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(shared) = self.ivars().shared.get() else { return };
            if let Some((items, id)) = shared.drag_items(event) {
                let source = ProtocolObject::from_ref(self);
                let _session = self.beginDraggingSessionWithItems_event_source(&items, event, source);
                if let Ok(mut st) = shared.state.lock() {
                    st.model.set_dragging(id, true);
                }
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            if let Some(shared) = self.ivars().shared.get() {
                shared.mouse_up(event);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            if let Some(shared) = self.ivars().shared.get() {
                shared.scroll(event);
            }
        }
    }

    unsafe impl NSDraggingSource for ScrawlStackView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            if let Some(shared) = self.ivars().shared.get() {
                shared.drag_ended(operation != NSDragOperation::None);
            }
        }
    }
);

impl ScrawlStackView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(ViewIvars {
            shared: std::cell::OnceCell::new(),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

pub struct CaptureStack {
    shared: Arc<Shared>,
    display_link: Option<CVDisplayLink>,
    display_link_ctx: Option<*const Shared>,
    action_rx: flume::Receiver<StackAction>,
    window_num: u32,
}

impl CaptureStack {
    /// Opens the (empty, invisible) stack over the display at `display` (global top-left points:
    /// x, y, width, height). `capsule` and `usable_max` are local to that display.
    pub fn open(
        variant: Variant,
        display: [f32; 4],
        capsule: [f32; 4],
        usable_max: [f32; 2],
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("CaptureStack::open needs the main thread")?;
        let primary_h = NSScreen::screens(mtm)
            .firstObject()
            .map_or(900.0, |s| s.frame().size.height);
        let scale = NSScreen::screens(mtm)
            .iter()
            .map(|s| s.backingScaleFactor())
            .fold(2.0_f64, f64::max);
        let [ax, ay, w, h] = crate::screens::flip(display.map(f64::from), primary_h);
        let rect = NSRect::new(NSPoint::new(ax, ay), NSSize::new(w, h));

        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            rect,
            NSWindowStyleMask::Borderless
                | NSWindowStyleMask::NonactivatingPanel
                | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe {
            let responds: bool =
                msg_send![&*window, respondsToSelector: sel!(_setPreventsActivation:)];
            if responds {
                let _: () = msg_send![&*window, _setPreventsActivation: true];
            }
            window.setReleasedWhenClosed(false);
        }
        let local = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
        let view = ScrawlStackView::new(mtm, local);
        let host = CALayer::new();
        host.setContentsScale(scale);
        // A layer-hosting view: the layer is set before `wantsLayer`.
        view.setLayer(Some(&host));
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setOpaque(false);
        window.setHasShadow(false);
        window.setHidesOnDeactivate(false);
        window.setCanHide(false);
        window.setIgnoresMouseEvents(true);
        // Under the capsule (+5).
        window.setLevel(NSStatusWindowLevel + 4);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        window.setFrame_display(rect, false);
        let window_num = window.windowNumber().max(0) as u32;

        let (action_tx, action_rx) = flume::unbounded();
        let geometry = Geometry {
            display: [display[2], display[3]],
            capsule,
            usable_max,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(Runtime {
                model: StackModel::new(variant, geometry),
                window,
                host,
                layers: HashMap::new(),
                display,
                scale,
                ignores_mouse: true,
                pretend: None,
                cursor: None,
                down: None,
                drag_started: false,
                last_tick: motion::now(),
                shown: false,
                trace: trace::StackTrace::open(
                    &format!("stack.{variant:?}").to_lowercase(),
                    [display[2], display[3]],
                ),
                action_tx,
            }),
            tick_queued: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        let _ = view.ivars().shared.set(shared.clone());

        let mut display_link = None;
        let mut display_link_ctx = None;
        if let Ok(dl) = CVDisplayLink::from_active_cg_displays() {
            let ctx = Arc::into_raw(shared.clone());
            if unsafe { dl.set_output_callback(display_link_cb, ctx.cast_mut().cast::<c_void>()) }
                .is_ok()
            {
                let _ = dl.start();
                display_link = Some(dl);
                display_link_ctx = Some(ctx);
            } else {
                unsafe { drop(Arc::from_raw(ctx)) };
            }
        }
        Ok(Self {
            shared,
            display_link,
            display_link_ctx,
            action_rx,
            window_num,
        })
    }

    pub fn window_number(&self) -> u32 {
        self.window_num
    }

    /// The display it covers (global top-left points).
    pub fn display(&self) -> [f32; 4] {
        self.shared.state.lock().map_or([0.0; 4], |st| st.display)
    }

    /// A finished capture, starting as the frame at `from` (global top-left points).
    pub fn push(&self, spec: CardSpec, from: [f32; 4]) {
        if let Ok(mut st) = self.shared.state.lock() {
            let local = [from[0] - st.display[0], from[1] - st.display[1], from[2], from[3]];
            st.model.push(spec, local);
            if !st.shown {
                st.shown = true;
                st.window.orderFrontRegardless();
            }
        }
        if self.display_link.is_none() {
            self.shared.tick();
        }
    }

    pub fn set_destination(&self, id: u64, text: &str) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.model.set_destination(id, text);
        }
    }

    pub fn set_reduced_motion(&self, reduced: bool) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.model.reduced = reduced;
        }
    }

    pub fn set_auto_close(&self, seconds: f32) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.model.timing.auto_close = seconds;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.shared.state.lock().map_or(true, |st| st.model.is_empty())
    }

    /// `scrawl lab stack`: drive the pointer from a script (global top-left points), drawn as the
    /// arrow cursor; `None` takes it off the display.
    pub fn pretend_pointer(&self, p: Option<[f32; 2]>) {
        if let Ok(mut st) = self.shared.state.lock() {
            let local = p.map(|p| [p[0] - st.display[0], p[1] - st.display[1]]);
            st.pretend = Some(local);
        }
    }

    /// `scrawl lab stack`: a click at the pretend pointer, as if the mouse went down and up.
    pub fn pretend_click(&self) {
        let Ok(mut st) = self.shared.state.lock() else { return };
        let Some(Some(p)) = st.pretend else { return };
        if let Some(hit) = st.model.hit(p) {
            st.perform(hit);
        }
    }

    /// `scrawl lab stack`: a two-finger swipe over the card under the pretend pointer.
    pub fn pretend_swipe(&self, dx: f32, ended: bool) {
        let Ok(mut st) = self.shared.state.lock() else { return };
        if let Some(id) = st.model.swipe(dx, ended) {
            let _ = st.action_tx.try_send(StackAction::Closed(id));
        }
    }

    /// The cards still in the stack, back to front, in global points.
    pub fn frames_global(&self) -> Vec<CardFrame> {
        let Ok(st) = self.shared.state.lock() else { return Vec::new() };
        let [dx, dy, _, _] = st.display;
        let shift = |r: [f32; 4]| [r[0] + dx, r[1] + dy, r[2], r[3]];
        st.model
            .frames()
            .into_iter()
            .filter(|f| !f.leaving)
            .map(|mut f| {
                f.rect = shift(f.rect);
                f.paste = shift(f.paste);
                f.open = shift(f.open);
                f.close = shift(f.close);
                f.thumb = shift(f.thumb);
                f
            })
            .collect()
    }

    pub fn drain_actions(&self) -> Vec<StackAction> {
        self.action_rx.try_iter().collect()
    }
}

impl Drop for CaptureStack {
    fn drop(&mut self) {
        self.shared.alive.store(false, Ordering::SeqCst);
        if let Some(ref dl) = self.display_link {
            let _ = dl.stop();
        }
        if let Ok(st) = self.shared.state.lock() {
            st.window.orderOut(None);
            st.window.close();
        }
        if let Some(ctx) = self.display_link_ctx {
            unsafe { drop(Arc::from_raw(ctx)) };
        }
    }
}

extern "C" fn display_link_cb(
    _display_link: CVDisplayLinkRef,
    _now: *const CVTimeStamp,
    _output_time: *const CVTimeStamp,
    _flags_in: CVOptionFlags,
    _flags_out: *mut CVOptionFlags,
    context: *mut c_void,
) -> CVReturn {
    if !context.is_null() {
        let shared = unsafe { &*context.cast::<Shared>() };
        if shared.alive.load(Ordering::Relaxed) && !shared.tick_queued.swap(true, Ordering::AcqRel)
        {
            unsafe {
                Arc::increment_strong_count(context.cast::<Shared>());
                dispatch_async_f(&_dispatch_main_q, context, main_queue_trampoline);
            }
        }
    }
    0
}

extern "C" fn main_queue_trampoline(context: *mut c_void) {
    let shared = unsafe { Arc::from_raw(context.cast::<Shared>()) };
    if shared.alive.load(Ordering::Relaxed) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shared.tick()));
    }
    shared.tick_queued.store(false, Ordering::Release);
}

fn local_point(st: &Runtime, event: &NSEvent) -> [f32; 2] {
    let p = event.locationInWindow();
    [p.x as f32, st.display[3] - p.y as f32]
}

impl Shared {
    fn tick(&self) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        motion::poll_tune();
        let Ok(mut st) = self.state.lock() else { return };
        if !st.shown {
            st.last_tick = motion::now();
            return;
        }
        let now = motion::now();
        let dt = ((now - st.last_tick).max(0.0) as f32).min(0.025 * motion::time_scale());
        if dt < 0.004 * motion::time_scale() {
            return;
        }
        st.last_tick = now;

        let pointer = match st.pretend {
            Some(p) => p,
            None => {
                let m = NSEvent::mouseLocation();
                let f = st.window.frame();
                let p = [
                    (m.x - f.origin.x) as f32,
                    (f.size.height - (m.y - f.origin.y)) as f32,
                ];
                (p[0] >= 0.0 && p[1] >= 0.0 && p[0] <= st.display[2] && p[1] <= st.display[3])
                    .then_some(p)
            }
        };
        st.model.set_pointer(pointer);
        let takes = pointer.is_some_and(|p| st.model.takes_pointer(p))
            || st.down.is_some()
            || st.drag_started;
        let ignore = st.pretend.is_some() || !takes;
        if st.ignores_mouse != ignore {
            st.ignores_mouse = ignore;
            st.window.setIgnoresMouseEvents(ignore);
        }
        st.model.advance(dt);
        st.render(pointer);
        if st.trace.is_some() {
            st.trace();
        }
        if st.model.is_empty() && st.pretend.is_none() {
            st.shown = false;
            st.window.orderOut(None);
            st.window.setIgnoresMouseEvents(true);
            st.ignores_mouse = true;
        }
    }

    fn mouse_down(&self, event: &NSEvent) {
        let Ok(mut st) = self.state.lock() else { return };
        let p = local_point(&st, event);
        st.drag_started = false;
        st.down = st.model.hit(p).map(|h| (p, h));
        st.model.pressed = st.down.map(|(_, h)| h);
    }

    /// The items for a drag that has gone far enough from a card's body, once per press.
    fn drag_items(&self, event: &NSEvent) -> Option<(Retained<NSArray<NSDraggingItem>>, u64)> {
        let mtm = MainThreadMarker::new()?;
        let mut st = self.state.lock().ok()?;
        let (start, hit) = st.down?;
        let p = local_point(&st, event);
        if st.drag_started || hit.elem != Elem::Body || (p[0] - start[0]).hypot(p[1] - start[1]) < 4.0
        {
            return None;
        }
        let spec = st.model.card(hit.card)?.clone();
        let frame = st.model.frames().into_iter().find(|f| f.id == hit.card)?;
        st.drag_started = true;
        st.model.pressed = None;
        let whole_bundle = event.modifierFlags().contains(NSEventModifierFlags::Option);
        let path = if whole_bundle { &spec.bundle } else { &spec.sheet };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(
            mtm.alloc(),
            ProtocolObject::from_ref(&*url),
        );
        let r = frame.thumb;
        let drag_frame = NSRect::new(
            NSPoint::new(f64::from(r[0]), f64::from(st.display[3] - r[1] - r[3])),
            NSSize::new(f64::from(r[2]), f64::from(r[3])),
        );
        if let Some(image) = load_image(&spec.sheet) {
            let obj: &AnyObject = image.as_ref();
            unsafe { item.setDraggingFrame_contents(drag_frame, Some(obj)) };
        }
        Some((NSArray::from_retained_slice(&[item]), hit.card))
    }

    fn drag_ended(&self, dropped: bool) {
        let Ok(mut st) = self.state.lock() else { return };
        let id = st.down.take().map(|(_, h)| h.card);
        st.drag_started = false;
        if let Some(id) = id {
            st.model.set_dragging(id, false);
            if dropped {
                st.model.close(id);
                let _ = st.action_tx.try_send(StackAction::DraggedOut(id));
            }
        }
    }

    fn mouse_up(&self, event: &NSEvent) {
        let Ok(mut st) = self.state.lock() else { return };
        let p = local_point(&st, event);
        st.model.pressed = None;
        if st.drag_started {
            return;
        }
        let Some((_, down)) = st.down.take() else { return };
        if st.model.hit(p) == Some(down) {
            st.perform(down);
        }
    }

    fn scroll(&self, event: &NSEvent) {
        let Ok(mut st) = self.state.lock() else { return };
        if event.momentumPhase() != NSEventPhase::empty() {
            return;
        }
        let phase = event.phase();
        let mut dx = event.scrollingDeltaX() as f32;
        // Fingers to the right is positive whatever the natural-scrolling setting.
        if !event.isDirectionInvertedFromDevice() {
            dx = -dx;
        }
        let ended = phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled);
        if phase.is_empty() {
            return;
        }
        if let Some(id) = st.model.swipe(dx, ended) {
            let _ = st.action_tx.try_send(StackAction::Closed(id));
        }
    }
}

impl Runtime {
    fn trace(&mut self) {
        let frames = self.model.frames();
        let [dx, dy, _, _] = self.display;
        let g = |r: [f32; 4]| [r[0] + dx, r[1] + dy, r[2], r[3]];
        let mut out: Vec<trace::Element> = Vec::new();
        for f in &frames {
            let card = g(f.rect);
            out.push((format!("card:{}", f.id), card, f.opacity, f.radius, None, None));
            if f.frame_alpha > 0.001 {
                out.push((format!("frame:{}", f.id), g(f.frame), f.frame_alpha * f.opacity, 0.0, Some("card"), Some(card)));
            }
            if f.hover > 0.001 {
                out.push((format!("paste:{}", f.id), g(f.paste), f.hover * f.opacity, 0.0, Some("card"), Some(card)));
            }
        }
        if let Some(t) = self.trace.as_mut() {
            t.frame(&out, Vec::new());
        }
    }

    fn perform(&mut self, hit: Hit) {
        let Some(spec) = self.model.card(hit.card).cloned() else { return };
        match hit.elem {
            Elem::Close => {
                self.model.close(hit.card);
                let _ = self.action_tx.try_send(StackAction::Closed(hit.card));
            }
            Elem::Paste => {
                self.model.close(hit.card);
                let _ = self.action_tx.try_send(StackAction::Paste(spec.bundle));
            }
            Elem::Open | Elem::Body => {
                let _ = self.action_tx.try_send(StackAction::Open(spec.bundle));
            }
        }
    }

    fn render(&mut self, pointer: Option<[f32; 2]>) {
        let frames = self.model.frames();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let live: Vec<u64> = frames.iter().map(|f| f.id).collect();
        self.layers.retain(|id, l| {
            let keep = live.contains(id);
            if !keep {
                l.root.removeFromSuperlayer();
            }
            keep
        });
        for (z, f) in frames.iter().enumerate() {
            if !self.layers.contains_key(&f.id) {
                let Some(spec) = self.model.card(f.id).cloned() else { continue };
                let layers = build_card(&spec, self.scale);
                self.host.addSublayer(&layers.root);
                self.layers.insert(f.id, layers);
            }
            let dest = self.model.card(f.id).map(|s| s.destination.clone());
            let display_h = self.display[3];
            if let Some(l) = self.layers.get_mut(&f.id) {
                if let Some(dest) = dest
                    && dest != l.destination_text
                {
                    set_text(&l.destination, &dest);
                    l.destination_text = dest;
                }
                l.root.setZPosition(z as f64);
                place_card(l, f, display_h);
            }
        }
        if let Some(pretend) = self.pretend {
            let cursor = self.cursor.get_or_insert_with(cursor_layer).clone();
            if cursor.superlayer().is_none() {
                self.host.addSublayer(&cursor);
            }
            cursor.setZPosition(1000.0);
            cursor.setHidden(pretend.is_none());
            if let Some(p) = pointer {
                let size = cursor.bounds().size;
                cursor.setFrame(NSRect::new(
                    NSPoint::new(
                        f64::from(p[0]) - 4.0,
                        f64::from(self.display[3] - p[1]) - size.height + 4.0,
                    ),
                    size,
                ));
            }
        }
        CATransaction::commit();
    }
}

/// A rect in local top-left points → a layer frame in a parent `parent_h` tall (bottom-left).
fn ns_rect(r: [f32; 4], parent_h: f32) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(r[0]), f64::from(parent_h - r[1] - r[3])),
        NSSize::new(f64::from(r[2].max(0.0)), f64::from(r[3].max(0.0))),
    )
}

fn color(layer: &CALayer, rgba: [f64; 4]) {
    let ns = NSColor::colorWithSRGBRed_green_blue_alpha(rgba[0], rgba[1], rgba[2], rgba[3]);
    unsafe {
        let cg: *const OpaqueCGColor = msg_send![&*ns, CGColor];
        let _: () = msg_send![layer, setBackgroundColor: cg];
    }
}

fn text_color(layer: &CATextLayer, rgba: [f64; 4]) {
    let ns = NSColor::colorWithSRGBRed_green_blue_alpha(rgba[0], rgba[1], rgba[2], rgba[3]);
    unsafe {
        let cg: *const OpaqueCGColor = msg_send![&*ns, CGColor];
        let _: () = msg_send![layer, setForegroundColor: cg];
    }
}

fn set_text(layer: &CATextLayer, text: &str) {
    let s = NSString::from_str(text);
    let obj: &AnyObject = s.as_ref();
    unsafe { layer.setString(Some(obj)) };
}

fn text_layer(text: &str, font: &NSFont, size: f64, rgba: [f64; 4], scale: f64) -> Retained<CATextLayer> {
    let layer = CATextLayer::new();
    set_text(&layer, text);
    unsafe {
        // `CFTypeRef`: an NSFont is toll-free bridged with CTFont.
        let font: *const c_void = (font as *const NSFont).cast();
        let _: () = msg_send![&*layer, setFont: font];
    }
    layer.setFontSize(size);
    text_color(&layer, rgba);
    layer.setContentsScale(scale);
    layer
}

fn load_image(path: &Path) -> Option<Retained<NSImage>> {
    NSImage::initWithContentsOfFile(
        NSImage::alloc(),
        &NSString::from_str(&path.to_string_lossy()),
    )
}

fn image_layer(path: Option<&Path>) -> Retained<CALayer> {
    let layer = CALayer::new();
    if let Some(image) = path.and_then(load_image) {
        let obj: &AnyObject = image.as_ref();
        unsafe { layer.setContents(Some(obj)) };
    }
    layer.setMinificationFilter(&NSString::from_str("trilinear"));
    layer
}

fn cursor_layer() -> Retained<CALayer> {
    let layer = CALayer::new();
    let image = NSCursor::arrowCursor().image();
    let size = image.size();
    let obj: &AnyObject = image.as_ref();
    unsafe { layer.setContents(Some(obj)) };
    layer.setBounds(NSRect::new(NSPoint::new(0.0, 0.0), size));
    layer
}

fn button(label: &str, w: f32, bg: [f64; 4], fg: [f64; 4], scale: f64) -> Retained<CALayer> {
    let layer = CALayer::new();
    color(&layer, bg);
    layer.setCornerRadius(f64::from(model::BUTTON_H) * 0.5);
    let font = NSFont::systemFontOfSize_weight(12.0, unsafe { objc2_app_kit::NSFontWeightSemibold });
    let text = text_layer(label, &font, 12.0, fg, scale);
    text.setAlignmentMode(&NSString::from_str("center"));
    text.setFrame(ns_rect([0.0, 5.0, w, 16.0], model::BUTTON_H));
    layer.addSublayer(&text);
    layer
}

fn build_card(spec: &CardSpec, scale: f64) -> CardLayers {
    let [cw, card_h] = spec.card_size();
    let root = CALayer::new();
    color(&root, [SHEET_BG[0], SHEET_BG[1], SHEET_BG[2], 1.0]);
    root.setShadowOpacity(0.0);
    root.setShadowRadius(16.0);
    root.setShadowOffset(NSSize::new(0.0, -6.0));
    // A hairline so a dark card (and the dock's peeking edges) reads on a dark screen.
    unsafe {
        let edge = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.14);
        let cg: *const OpaqueCGColor = msg_send![&*edge, CGColor];
        let _: () = msg_send![&*root, setBorderColor: cg];
    }
    let clip = CALayer::new();
    clip.setMasksToBounds(true);
    root.addSublayer(&clip);
    let sheet = image_layer(Some(&spec.sheet));
    clip.addSublayer(&sheet);
    let frame = image_layer(spec.frame.as_deref());
    clip.addSublayer(&frame);

    // Footer, scrim and buttons at a resting card's size, scaled with the card as one layer.
    let chrome = CALayer::new();
    chrome.setBounds(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(cw), f64::from(card_h)),
    ));
    chrome.setAnchorPoint(NSPoint::new(0.0, 1.0));
    clip.addSublayer(&chrome);
    let thumb_h = card_h - 2.0 * INSET - FOOTER_H;
    let scrim = CALayer::new();
    color(&scrim, [0.0, 0.0, 0.0, 0.5]);
    scrim.setCornerRadius(8.0);
    scrim.setFrame(ns_rect([INSET, INSET, cw - 2.0 * INSET, thumb_h], card_h));
    scrim.setOpacity(0.0);
    chrome.addSublayer(&scrim);
    let total = model::PASTE_W + model::BUTTON_GAP + model::OPEN_W;
    let bx = (cw - total) * 0.5;
    let by = INSET + (thumb_h - model::BUTTON_H) * 0.5;
    let paste = button("Paste", model::PASTE_W, [1.0, 1.0, 1.0, 0.94], [0.05, 0.05, 0.06, 1.0], scale);
    paste.setFrame(ns_rect([bx, by, model::PASTE_W, model::BUTTON_H], card_h));
    chrome.addSublayer(&paste);
    let open = button("Open", model::OPEN_W, [1.0, 1.0, 1.0, 0.18], [1.0, 1.0, 1.0, 0.95], scale);
    open.setFrame(ns_rect(
        [bx + model::PASTE_W + model::BUTTON_GAP, by, model::OPEN_W, model::BUTTON_H],
        card_h,
    ));
    chrome.addSublayer(&open);
    let digits = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe {
        objc2_app_kit::NSFontWeightMedium
    });
    let regular = NSFont::systemFontOfSize_weight(11.0, unsafe { objc2_app_kit::NSFontWeightMedium });
    let clock = format!("{}:{:02}", spec.duration_ms / 60_000, spec.duration_ms / 1000 % 60);
    let duration = text_layer(&clock, &digits, 11.0, [1.0, 1.0, 1.0, 0.5], scale);
    let text_y = card_h - FOOTER_H + (FOOTER_H - 14.0) * 0.5 - 1.0;
    duration.setFrame(ns_rect([INSET + 6.0, text_y, 36.0, 14.0], card_h));
    chrome.addSublayer(&duration);
    // The destination ends in a soft fade rather than an ellipsis when it doesn't fit.
    let dest_x = INSET + 6.0 + 34.0;
    let dest_w = cw - dest_x - INSET - 4.0;
    let dest_box = CALayer::new();
    dest_box.setFrame(ns_rect([dest_x, text_y, dest_w, 14.0], card_h));
    let fade = objc2_quartz_core::CAGradientLayer::new();
    fade.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(dest_w), 14.0),
    ));
    fade.setStartPoint(NSPoint::new(0.0, 0.5));
    fade.setEndPoint(NSPoint::new(1.0, 0.5));
    unsafe {
        let opaque = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0);
        let clear = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.0);
        let a: *const OpaqueCGColor = msg_send![&*opaque, CGColor];
        let b: *const OpaqueCGColor = msg_send![&*clear, CGColor];
        let colors: Retained<NSArray> = msg_send![
            NSArray::<AnyObject>::class(),
            arrayWithObjects: [a.cast::<AnyObject>(), a.cast::<AnyObject>(), b.cast::<AnyObject>()].as_ptr(),
            count: 3usize
        ];
        fade.setColors(Some(&colors));
        let stop = 1.0 - 18.0 / f64::from(dest_w.max(19.0));
        let locations = NSArray::from_retained_slice(&[
            objc2_foundation::NSNumber::new_f64(0.0),
            objc2_foundation::NSNumber::new_f64(stop),
            objc2_foundation::NSNumber::new_f64(1.0),
        ]);
        fade.setLocations(Some(&locations));
        dest_box.setMask(Some(&fade));
    }
    let destination = text_layer(&spec.destination, &regular, 11.0, [1.0, 1.0, 1.0, 0.86], scale);
    destination.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 14.0)));
    dest_box.addSublayer(&destination);
    chrome.addSublayer(&dest_box);

    let close = CALayer::new();
    color(&close, [0.17, 0.17, 0.19, 0.97]);
    close.setCornerRadius(f64::from(model::CLOSE_D) * 0.5);
    close.setBorderWidth(0.5);
    unsafe {
        let border = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.22);
        let cg: *const OpaqueCGColor = msg_send![&*border, CGColor];
        let _: () = msg_send![&*close, setBorderColor: cg];
    }
    let xmark = NSFont::systemFontOfSize_weight(10.0, unsafe { objc2_app_kit::NSFontWeightBold });
    let x = text_layer("✕", &xmark, 10.0, [1.0, 1.0, 1.0, 0.9], scale);
    x.setAlignmentMode(&NSString::from_str("center"));
    x.setFrame(ns_rect([0.0, 3.5, model::CLOSE_D, 13.0], model::CLOSE_D));
    close.addSublayer(&x);
    close.setOpacity(0.0);
    root.addSublayer(&close);

    CardLayers {
        root,
        clip,
        sheet,
        frame,
        chrome,
        scrim,
        paste,
        open,
        destination,
        destination_text: spec.destination.clone(),
        close,
    }
}

fn place_card(l: &CardLayers, f: &CardFrame, display_h: f32) {
    let v = f.rect;
    l.root.setFrame(ns_rect(v, display_h));
    l.root.setCornerRadius(f64::from(f.radius));
    l.root.setOpacity(f.opacity);
    l.root.setShadowOpacity(0.38 * f.shadow);
    l.root.setBorderWidth(0.5 * f64::from(f.chrome));
    let local = |r: [f32; 4]| [r[0] - v[0], r[1] - v[1], r[2], r[3]];
    l.clip.setFrame(ns_rect([0.0, 0.0, v[2], v[3]], v[3]));
    l.clip.setCornerRadius(f64::from(f.radius));
    l.sheet.setFrame(ns_rect(local(f.sheet), v[3]));
    l.frame.setFrame(ns_rect(local(f.frame), v[3]));
    l.frame.setOpacity(f.frame_alpha);
    l.frame.setHidden(f.frame_alpha <= 0.001);
    l.chrome.setPosition(NSPoint::new(0.0, f64::from(v[3])));
    let mut t = CATransform3D {
        m11: 1.0, m12: 0.0, m13: 0.0, m14: 0.0,
        m21: 0.0, m22: 1.0, m23: 0.0, m24: 0.0,
        m31: 0.0, m32: 0.0, m33: 1.0, m34: 0.0,
        m41: 0.0, m42: 0.0, m43: 0.0, m44: 1.0,
    };
    t.m11 = f64::from(f.k);
    t.m22 = f64::from(f.k);
    l.chrome.setTransform(t);
    l.chrome.setOpacity(f.chrome);
    l.scrim.setOpacity(f.hover);
    let pressed = |e: Elem| f.pressed == Some(e);
    let hovered = |e: Elem| f.hovered == Some(e);
    l.paste.setOpacity(f.hover);
    l.open.setOpacity(f.hover);
    color(
        &l.paste,
        [1.0, 1.0, 1.0, if pressed(Elem::Paste) { 0.78 } else if hovered(Elem::Paste) { 1.0 } else { 0.92 }],
    );
    color(
        &l.open,
        [1.0, 1.0, 1.0, if pressed(Elem::Open) { 0.34 } else if hovered(Elem::Open) { 0.28 } else { 0.18 }],
    );
    l.close.setFrame(ns_rect(local(f.close), v[3]));
    l.close.setOpacity(f.hover * f.chrome);
    color(
        &l.close,
        if hovered(Elem::Close) || pressed(Elem::Close) {
            [0.30, 0.30, 0.33, 0.98]
        } else {
            [0.17, 0.17, 0.19, 0.97]
        },
    );
}

/// Opens the stack on the display holding `point` (global points), else the primary one, with
/// the capsule where `native::pill` puts it.
pub fn open_on(variant: Variant, point: Option<[f64; 2]>) -> Result<CaptureStack, String> {
    use crate::native::pill::model::{ISLAND_H, PANEL_W, PANEL_Y, TOP};
    let screens = crate::screens::current();
    let index = point
        .and_then(|p| crate::screens::index_at(&screens, p))
        .or((!screens.is_empty()).then_some(0))
        .ok_or("no display")?;
    let s = &screens[index];
    let [px, py] = crate::screens::pill_origin(s, f64::from(PANEL_W), f64::from(PANEL_Y));
    let display = s.frame.map(|v| v as f32);
    let capsule_w = 160.0;
    let capsule = [
        (px - s.frame[0]) as f32 + PANEL_W * 0.5 - capsule_w * 0.5,
        (py - s.frame[1]) as f32 + TOP,
        capsule_w,
        ISLAND_H,
    ];
    let usable_max = [
        (s.visible[0] + s.visible[2] - s.frame[0]) as f32,
        (s.visible[1] + s.visible[3] - s.frame[1]) as f32,
    ];
    CaptureStack::open(variant, display, capsule, usable_max)
}

/// The normalized rect of keyframe `index` in a contact sheet laid out from `sizes`
/// (`render::contact_sheet_cells`).
pub fn tile_of(sizes: &[(u32, u32)], index: usize) -> ([f32; 2], [f32; 4]) {
    let (sheet, cells) = crate::render::contact_sheet_cells(sizes);
    let [w, h] = [sheet.0 as f32, sheet.1 as f32];
    let r = cells.get(index).copied().unwrap_or([0.0, 0.0, w, h]);
    ([w, h], [r[0] / w, r[1] / h, r[2] / w, r[3] / h])
}

/// A card for a finished bundle: its contact sheet, its last keyframe (full-size if the bundle
/// kept one) and where that keyframe sits in the sheet.
pub fn card_for_bundle(id: u64, dir: &Path, duration_ms: u64, destination: &str) -> Option<CardSpec> {
    let sheet = dir.join("contact-sheet.png");
    let mut frames: Vec<PathBuf> = std::fs::read_dir(dir.join("frames"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    frames.sort();
    let sizes: Vec<(u32, u32)> = frames.iter().filter_map(|p| png_size(p)).collect();
    if sizes.is_empty() || sizes.len() != frames.len() {
        return None;
    }
    let last = frames.len() - 1;
    let (sheet_size, tile) = tile_of(&sizes, last);
    // The full frame when the bundle was cropped; the tile shows the crop, so only use it when
    // it has the same shape.
    let full = dir.join("frames-full").join(frames[last].file_name()?);
    let frame = match png_size(&full) {
        Some((w, h)) if ((w as f32 / h as f32) - (sizes[last].0 as f32 / sizes[last].1 as f32)).abs() < 0.02 => full,
        _ => frames[last].clone(),
    };
    Some(CardSpec {
        id,
        bundle: dir.to_path_buf(),
        sheet,
        frame: Some(frame),
        sheet_size,
        tile,
        duration_ms,
        destination: destination.to_owned(),
    })
}

fn png_size(path: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let reader = decoder.read_info().ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

