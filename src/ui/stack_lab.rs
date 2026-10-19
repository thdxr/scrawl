//! `scrawl lab stack <scene> [--variant dock|edge|none]`: the capture-stack experiment over a
//! pretend screen. The real capsule records pretend captures (`lab::Scene` style, nothing is
//! captured, recorded or sent); each one finishes into a card made from a generated bundle, and a
//! scripted pointer (drawn as the arrow cursor) hovers, closes, pastes and swipes. `none` plays
//! today's finish for comparison.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, Application, prelude::*};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSImage, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;

use super::{ScrawlCoordinator, SelectionMode, motion};
use crate::capture::SharedCaptureTelemetry;
use crate::native::stack::{self, CaptureStack, StackAction, Variant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scene {
    /// One capture finishing.
    One,
    /// Three captures in a row.
    Three,
    /// Three captures, then the pointer: unfold, close one, paste one, leave, auto-close.
    Story,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum LabVariant {
    Dock,
    Edge,
    /// Today: the capsule turns to Copied and folds away.
    None,
}

pub struct Options {
    pub scene: Scene,
    pub variant: LabVariant,
    pub repeat: u32,
    pub reduced: bool,
    /// Record the display to this movie with `screencapture -v` (a child of the lab, so it runs
    /// under the lab app's Screen Recording grant when the lab is launched as an app).
    pub record: Option<PathBuf>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGDisplayHideCursor(display: u32) -> i32;
}

const LEAD_IN_S: f32 = 0.85;
const TAIL_S: f32 = 1.0;
/// One pretend capture: tap, fold the chips, finish (then `lab::PROCESSING_S` to Completed).
const TAP_TO_FINISH_S: f32 = 1.6;
const CAPTURE_GAP_S: f32 = 4.2;
/// Shorter than the real 8 s so a clip ends soon; longer than the gap between captures when
/// there are several, so they pile up (the whole stack shares one idle timer).
fn auto_close_s(scene: Scene) -> f32 {
    match scene {
        Scene::One => 2.6,
        Scene::Three | Scene::Story => CAPTURE_GAP_S + 0.8,
    }
}

#[derive(Clone, Debug)]
enum Step {
    Screen(usize),
    Tap,
    Finish(usize),
    Dismiss,
    /// Move the pointer to a target over `secs`.
    Point(Target, f32),
    Click,
    Swipe(f32),
    PointerOff,
}

#[derive(Clone, Copy, Debug)]
enum Target {
    /// Somewhere off to the side of the stack.
    Aside,
    /// The `n`th newest card's body, Paste button or ✕.
    Body(usize),
    Paste(usize),
    Close(usize),
    Away,
}

fn steps(scene: Scene, variant: LabVariant) -> Vec<(f32, Step)> {
    let captures = match scene {
        Scene::One => 1,
        Scene::Three | Scene::Story => 3,
    };
    let mut out = Vec::new();
    let mut t = 0.0;
    for k in 0..captures {
        out.push((t, Step::Screen(k)));
        out.push((t, Step::Tap));
        out.push((t + TAP_TO_FINISH_S, Step::Finish(k)));
        let completed = t + TAP_TO_FINISH_S + super::lab::PROCESSING_S;
        let dismiss = if variant == LabVariant::None { 1.4 } else { 0.9 };
        out.push((completed + dismiss, Step::Dismiss));
        t += CAPTURE_GAP_S;
    }
    if scene != Scene::Story && variant != LabVariant::None {
        // Stay for the cards' auto-close.
        let last = t - CAPTURE_GAP_S + TAP_TO_FINISH_S + super::lab::PROCESSING_S;
        out.push((last + auto_close_s(scene) + 0.8, Step::PointerOff));
    }
    if scene == Scene::Story && variant != LabVariant::None {
        let mut s = t - 0.6;
        let mut at = |d: f32, step: Step, out: &mut Vec<(f32, Step)>| {
            out.push((s, step));
            s += d;
        };
        at(0.0, Step::Point(Target::Aside, 0.0), &mut out);
        at(0.15, Step::Point(Target::Body(0), 0.55), &mut out);
        at(0.9, Step::Point(Target::Body(1), 0.4), &mut out);
        if variant == LabVariant::Edge {
            // A two-finger swipe toward the edge takes the card away.
            for _ in 0..10 {
                at(0.025, Step::Swipe(9.0), &mut out);
            }
            at(0.9, Step::Swipe(0.0), &mut out);
        } else {
            at(0.7, Step::Point(Target::Close(1), 0.35), &mut out);
            at(0.9, Step::Click, &mut out);
        }
        at(0.5, Step::Point(Target::Paste(0), 0.45), &mut out);
        at(0.6, Step::Click, &mut out);
        at(0.55, Step::Point(Target::Away, 0.5), &mut out);
        at(0.0, Step::PointerOff, &mut out);
        // Then the auto-close.
        out.push((s + auto_close_s(scene) + 1.4, Step::PointerOff));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

fn real(s: f32) -> Duration {
    Duration::from_secs_f32((s / motion::time_scale()).max(0.0))
}

/// A pretend capture: a generated bundle and the "screen" it was taken from.
struct LabCapture {
    screen: PathBuf,
    card: stack::CardSpec,
}

/// Three bundles made from Scrawl's synthetic app screen, each a little different, sized to the
/// display so the last keyframe lifts off exactly where the screen showed it.
fn make_captures(dir: &Path, size: [u32; 2]) -> anyhow::Result<Vec<LabCapture>> {
    use crate::model::{
        CaptureMode, InteractionEvent, Stroke, TargetFrame, TargetInfo, TranscriptData, SCALE,
    };
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let [w, h] = size;
    let destinations = ["Copied", "Sent to SCRAWL", "Sent to OpenCode iOS"];
    let durations = [9_400_u64, 14_200, 6_800];
    let mut out = Vec::new();
    for k in 0..3 {
        let bundle = dir.join(format!("lab-{k}.scrawl"));
        let duration = durations[k];
        let phase = k as u32;
        let frames = vec![
            crate::render::render_synthetic_demo_frame(w, h, 300, phase),
            crate::render::render_synthetic_demo_frame(w, h, duration / 2, (phase + 1) % 3),
            crate::render::render_synthetic_demo_frame(w, h, duration - 400, phase),
        ];
        let screen = crate::render::render_synthetic_demo_frame(w, h, duration, phase);
        let target = TargetInfo {
            mode: CaptureMode::Window,
            app: Some("OpenCode".into()),
            bundle_id: Some("ai.opencode.desktop".into()),
            window_title: Some("Lab".into()),
            window_id: Some(1),
            display_id: 1,
            frame: TargetFrame {
                x: 0.0,
                y: 0.0,
                width: f64::from(w),
                height: f64::from(h),
                scale: 1.0,
            },
        };
        let events = (0..=k.min(1))
            .map(|i| InteractionEvent::Click {
                t: 1_000 + i as u64 * (duration / 3),
                x: 0.3 + 0.25 * i as f32,
                y: 0.36 + 0.1 * k as f32,
                button: Some("left".into()),
                label: None,
                role: None,
            })
            .collect();
        let strokes = if k == 1 {
            let mut pts = Vec::new();
            for i in 0..=36 {
                let a = i as f32 / 36.0 * std::f32::consts::TAU;
                pts.extend_from_slice(&[
                    ((0.62 + 0.12 * a.cos()) * SCALE as f32) as i32,
                    ((0.2 + 0.06 * a.sin()) * SCALE as f32) as i32,
                    i * 16,
                ]);
            }
            vec![Stroke {
                t0: duration - 2_000,
                t1: duration - 2_000 + 36 * 16,
                fw: w,
                fh: h,
                on: None,
                pin: None,
                laser: false,
                pts,
            }]
        } else {
            Vec::new()
        };
        let transcript = TranscriptData {
            source: "lab".into(),
            model: None,
            text: String::new(),
            segments: Vec::new(),
            words: Vec::new(),
            estimated: false,
        };
        crate::build_bundle_from_parts(
            &bundle,
            target,
            duration,
            frames,
            strokes,
            events,
            transcript,
            None,
            None,
            crate::SourceExtras::default(),
        )?;
        let screen_path = bundle.join("lab-screen.png");
        crate::render::save_png(&screen, &screen_path)?;
        let mut card = stack::card_for_bundle(k as u64 + 1, &bundle, duration, destinations[k])
            .ok_or_else(|| anyhow::anyhow!("no card for {}", bundle.display()))?;
        card.frame = Some(screen_path.clone());
        out.push(LabCapture {
            screen: screen_path,
            card,
        });
    }
    Ok(out)
}

/// The pretend screen: a borderless window over the whole display (menu bar included) showing a
/// capture's screen image. It takes no input.
struct Backdrop {
    _window: Retained<NSPanel>,
    layer: Retained<CALayer>,
}

impl Backdrop {
    fn open(display: [f32; 4]) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let primary_h = NSScreen::screens(mtm).firstObject()?.frame().size.height;
        let [ax, ay, w, h] = crate::screens::flip(display.map(f64::from), primary_h);
        let rect = NSRect::new(NSPoint::new(ax, ay), NSSize::new(w, h));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { window.setReleasedWhenClosed(false) };
        let view = NSView::initWithFrame(
            mtm.alloc(),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        );
        let layer = CALayer::new();
        view.setLayer(Some(&layer));
        view.setWantsLayer(true);
        layer.setMinificationFilter(&NSString::from_str("trilinear"));
        window.setContentView(Some(&view));
        window.setBackgroundColor(Some(&NSColor::blackColor()));
        window.setIgnoresMouseEvents(true);
        window.setLevel(NSStatusWindowLevel + 1);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Stationary,
        );
        window.setFrame_display(rect, true);
        window.orderFrontRegardless();
        Some(Self {
            _window: window,
            layer,
        })
    }

    fn show(&self, path: &Path) {
        if let Some(image) = NSImage::initWithContentsOfFile(
            NSImage::alloc(),
            &NSString::from_str(&path.to_string_lossy()),
        ) {
            objc2_quartz_core::CATransaction::begin();
            objc2_quartz_core::CATransaction::setDisableActions(true);
            let obj: &AnyObject = image.as_ref();
            unsafe { self.layer.setContents(Some(obj)) };
            objc2_quartz_core::CATransaction::commit();
        }
    }
}

struct PointerPath {
    from: [f32; 2],
    to: [f32; 2],
    start: f64,
    secs: f32,
}

pub fn run(options: Options) {
    let app = Application::new().with_assets(crate::icons::Assets);
    app.run(move |cx: &mut App| {
        motion::poll_tune();
        let screens = crate::screens::current();
        let Some(screen) = screens.first().cloned() else {
            eprintln!("no display");
            std::process::exit(1);
        };
        let display = screen.frame.map(|v| v as f32);
        let size = [
            (display[2] * screen.scale as f32).round() as u32,
            (display[3] * screen.scale as f32).round() as u32,
        ];
        let lab_dir = std::env::temp_dir().join("scrawl-lab-stack");
        let captures = match make_captures(&lab_dir, size) {
            Ok(c) => c,
            Err(err) => {
                eprintln!("couldn't make the lab captures: {err:#}");
                std::process::exit(1);
            }
        };
        motion::trace_open(
            &format!("stack.{:?}.{:?}", options.scene, options.variant).to_lowercase(),
            [display[2], display[3]],
        );
        let backdrop = Backdrop::open(display);
        if let Some(b) = &backdrop {
            b.show(&captures[0].screen);
        }
        let (sender, _receiver) = flume::unbounded();
        let telemetry = SharedCaptureTelemetry::new(sender);
        let coordinator = cx.new(|_| {
            let mut coord = ScrawlCoordinator::new(telemetry, None);
            coord.lab = true;
            coord
        });
        coordinator.update(cx, |coord, _| coord.ensure_native_pill());
        let variant = match options.variant {
            LabVariant::Dock => Some(Variant::Dock),
            LabVariant::Edge => Some(Variant::Edge),
            LabVariant::None => None,
        };
        let stack = variant.and_then(|v| match stack::open_on(v, Some([1.0, 1.0])) {
            Ok(s) => Some(s),
            Err(err) => {
                eprintln!("stack: {err}");
                None
            }
        });
        if let Some(s) = &stack {
            s.set_auto_close(auto_close_s(options.scene));
            s.set_reduced_motion(options.reduced);
        }
        // For `perf/stack/record.sh`: the whole display.
        let region = format!("{},{},{},{}", display[0], display[1], display[2], display[3]);
        println!("region {region}");
        let record_path = options.record.clone();
        let start_recorder = move || {
            record_path.as_ref().and_then(|path| {
                let _ = std::fs::remove_file(path);
                // The real pointer stays out of the recording (the scripted one is drawn): park
                // it in the bottom-left corner, where only its tip is on screen, away from the
                // capsule and both stacks, and hide it if macOS lets us.
                unsafe {
                    CGWarpMouseCursorPosition(CGPoint {
                        x: f64::from(display[0]),
                        y: f64::from(display[1]) + f64::from(display[3]) - 1.0,
                    });
                    CGDisplayHideCursor(0);
                }
                std::process::Command::new("/usr/sbin/screencapture")
                    .args(["-v", "-x", &format!("-R{region}")])
                    .arg(path)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|err| eprintln!("screencapture: {err}"))
                    .ok()
            })
        };

        let stack = std::rc::Rc::new(stack);
        let backdrop = std::rc::Rc::new(backdrop);
        let captures = std::rc::Rc::new(captures);
        let pointer: std::rc::Rc<std::cell::RefCell<Option<PointerPath>>> = Default::default();

        let loop_coord = coordinator.clone();
        let loop_stack = stack.clone();
        let loop_pointer = pointer.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(8)).await;
                let alive = cx.update(|cx| {
                    motion::poll_tune();
                    loop_coord.update(cx, |coord, cx| coord.island_tick(cx));
                    if let Some(s) = loop_stack.as_ref() {
                        if let Some(path) = loop_pointer.borrow().as_ref() {
                            let u = (((motion::now() - path.start) as f32) / path.secs.max(0.001))
                                .clamp(0.0, 1.0);
                            // A hand's move: quick out, slow in.
                            let e = 1.0 - (1.0 - u).powi(3);
                            s.pretend_pointer(Some([
                                path.from[0] + (path.to[0] - path.from[0]) * e,
                                path.from[1] + (path.to[1] - path.from[1]) * e,
                            ]));
                        }
                        for action in s.drain_actions() {
                            motion::trace_event(format!("stack:{action:?}"));
                            if let StackAction::Paste(dir) | StackAction::Open(dir) = &action {
                                eprintln!("lab: {action:?} ({})", dir.display());
                            }
                        }
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let scene = options.scene;
        let lab_variant = options.variant;
        let repeat = options.repeat;
        cx.spawn(async move |cx| {
            // Start recording only once the backdrop has drawn, so no desktop frame leaks in,
            // and give the recorder a moment before the scene starts.
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let recorder = start_recorder();
            if recorder.is_some() {
                cx.background_executor().timer(Duration::from_millis(700)).await;
            }
            let mut run = 0;
            loop {
                cx.background_executor().timer(real(LEAD_IN_S)).await;
                let mut at = 0.0;
                for (when, step) in steps(scene, lab_variant) {
                    cx.background_executor().timer(real(when - at)).await;
                    at = when;
                    motion::trace_event(format!("lab:{step:?}"));
                    let stack = stack.clone();
                    let backdrop = backdrop.clone();
                    let captures = captures.clone();
                    let pointer = pointer.clone();
                    let _ = coordinator.update(cx, |coord, cx| match step {
                        Step::Screen(k) => {
                            if let Some(b) = backdrop.as_ref() {
                                b.show(&captures[k].screen);
                            }
                        }
                        Step::Tap => coord.record_screen_now(cx),
                        Step::Finish(k) => {
                            coord.switch_recording(SelectionMode::Screen, cx);
                            coord.finish_recording(cx);
                            if let Some(s) = stack.as_ref() {
                                let card = captures[k].card.clone();
                                let wait = real(super::lab::PROCESSING_S);
                                let s_display = s.display();
                                let stack = stack.clone();
                                cx.spawn(async move |_, cx| {
                                    cx.background_executor().timer(wait).await;
                                    let _ = cx.update(|_| {
                                        if let Some(s) = stack.as_ref() {
                                            s.push(card, s_display);
                                        }
                                    });
                                })
                                .detach();
                            }
                        }
                        Step::Dismiss => coord.dismiss_island(cx),
                        Step::Point(target, secs) => {
                            if let Some(s) = stack.as_ref() {
                                let to = pointer_target(s, target);
                                let from = pointer
                                    .borrow()
                                    .as_ref()
                                    .map(|p| current(p))
                                    .unwrap_or(to);
                                *pointer.borrow_mut() = Some(PointerPath {
                                    from,
                                    to,
                                    start: motion::now(),
                                    secs,
                                });
                            }
                        }
                        Step::Click => {
                            if let Some(s) = stack.as_ref() {
                                s.pretend_click();
                            }
                        }
                        Step::Swipe(dx) => {
                            if let Some(s) = stack.as_ref() {
                                s.pretend_swipe(dx, dx == 0.0);
                            }
                        }
                        Step::PointerOff => {
                            *pointer.borrow_mut() = None;
                            if let Some(s) = stack.as_ref() {
                                s.pretend_pointer(None);
                            }
                        }
                    });
                }
                cx.background_executor().timer(real(TAIL_S)).await;
                run += 1;
                if repeat != 0 && run >= repeat {
                    break;
                }
            }
            let _ = cx.update(|cx| {
                let _ = coordinator.update(cx, |coord, _| coord.native_pill = None);
                motion::trace_close();
                if let Some(mut rec) = recorder {
                    unsafe { libc::kill(rec.id() as i32, libc::SIGINT) };
                    let _ = rec.wait();
                }
                cx.quit();
                std::process::exit(0);
            });
        })
        .detach();
    });
}

fn current(p: &PointerPath) -> [f32; 2] {
    let u = (((motion::now() - p.start) as f32) / p.secs.max(0.001)).clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - u).powi(3);
    [
        p.from[0] + (p.to[0] - p.from[0]) * e,
        p.from[1] + (p.to[1] - p.from[1]) * e,
    ]
}

/// Where a pointer target is now, in global points.
fn pointer_target(s: &CaptureStack, target: Target) -> [f32; 2] {
    let d = s.display();
    let frames = s.frames_global();
    // Newest first.
    let card = |n: usize| frames.iter().rev().nth(n);
    let centre = |r: [f32; 4]| [r[0] + r[2] * 0.5, r[1] + r[3] * 0.5];
    match target {
        Target::Aside => match frames.last() {
            Some(f) => [f.rect[0] + f.rect[2] + 140.0, f.rect[1] + f.rect[3] + 120.0],
            None => [d[0] + d[2] * 0.6, d[1] + d[3] * 0.5],
        },
        Target::Body(n) => card(n).map_or([d[0], d[1]], |f| {
            [f.rect[0] + f.rect[2] * 0.72, f.rect[1] + f.rect[3] * 0.38]
        }),
        Target::Paste(n) => card(n).map_or([d[0], d[1]], |f| centre(f.paste)),
        Target::Close(n) => card(n).map_or([d[0], d[1]], |f| centre(f.close)),
        Target::Away => match frames.last() {
            Some(f) => [f.rect[0] + f.rect[2] + 260.0, f.rect[1] + f.rect[3] + 220.0],
            None => [d[0] + d[2] * 0.7, d[1] + d[3] * 0.6],
        },
    }
}
