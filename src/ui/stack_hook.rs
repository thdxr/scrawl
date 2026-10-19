//! The capture stack in the app, behind `"experiments": {"captureStack": true}` (under the
//! capsule) or `{"captureStackEdge": true}` (bottom-right corner). A finished local capture flies
//! off the screen into a card (`crate::native::stack`); the card takes over the Completed
//! capsule's Paste, Open and ✕, so the capsule folds away soon after.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::Context;

use super::{IslandPhase, ScrawlCoordinator, close_capture_windows, completed_label, open_review_window};
use crate::capture::scrawl_log;
use crate::model::ScrawlBundle;
use crate::native::stack::{self, CaptureStack, StackAction, Variant};

pub const CAPTURE_STACK: &str = "captureStack";
pub const CAPTURE_STACK_EDGE: &str = "captureStackEdge";

/// With the stack on, the Completed capsule stays this long, then folds.
const CAPSULE_AFTER_CARD: Duration = Duration::from_millis(1_300);

pub(super) struct StackState {
    stack: CaptureStack,
    variant: Variant,
    cards: Vec<(u64, PathBuf)>,
    next_id: u64,
}

pub(super) fn variant() -> Option<Variant> {
    if crate::handoff::experiment(CAPTURE_STACK_EDGE) {
        Some(Variant::Edge)
    } else if crate::handoff::experiment(CAPTURE_STACK) {
        Some(Variant::Dock)
    } else {
        None
    }
}

/// What decides whether the Completed capsule folds now that a card holds the capture.
#[derive(Clone, Copy, Debug, Default)]
struct Fold {
    since_completed: Duration,
    pointer_on_pill: bool,
    /// The route is still being worked out, or a send is waiting out its undo window: chips
    /// (or "Sending to …") may be about to show or showing.
    route_pending: bool,
    /// Route chips are on the capsule; they exist only there, so it stays until one is
    /// clicked or the Completed island's own 25 s timeout.
    route_chips: bool,
}

fn capsule_may_fold(f: Fold) -> bool {
    f.since_completed >= CAPSULE_AFTER_CARD && !f.pointer_on_pill && !f.route_pending && !f.route_chips
}

fn contains(r: [f32; 4], p: [f32; 2]) -> bool {
    p[0] >= r[0] && p[0] < r[0] + r[2] && p[1] >= r[1] && p[1] < r[1] + r[3]
}

impl ScrawlCoordinator {
    /// A local capture finished into `dir`: it lifts off the screen and joins the stack.
    pub(super) fn stack_finished(&mut self, dir: &Path, bundle: &ScrawlBundle) {
        let Some(variant) = variant() else { return };
        let f = &bundle.target.frame;
        let target = [f.x as f32, f.y as f32, f.width as f32, f.height as f32];
        let centre = [target[0] + target[2] * 0.5, target[1] + target[3] * 0.5];
        let stale = self.capture_stack.as_ref().is_some_and(|s| {
            s.variant != variant || (s.stack.is_empty() && !contains(s.stack.display(), centre))
        });
        if stale {
            self.capture_stack = None;
        }
        if self.capture_stack.is_none() {
            match stack::open_on(variant, Some([f64::from(centre[0]), f64::from(centre[1])])) {
                Ok(stack) => {
                    self.telemetry
                        .excluded_window_ids
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(stack.window_number());
                    stack.set_reduced_motion(self.reduced_motion);
                    self.capture_stack = Some(StackState {
                        stack,
                        variant,
                        cards: Vec::new(),
                        next_id: 1,
                    });
                }
                Err(err) => {
                    scrawl_log(format!("capture stack: {err}"));
                    return;
                }
            }
        }
        let Some(state) = self.capture_stack.as_mut() else { return };
        let id = state.next_id;
        state.next_id += 1;
        let label = completed_label(0, self.completed_note.as_deref());
        let Some(spec) = stack::card_for_bundle(id, dir, bundle.duration_ms, &label) else {
            scrawl_log(format!("capture stack: no card for {}", dir.display()));
            return;
        };
        // The frame starts where it was: the whole target, or the last keyframe's crop of it.
        let cropped = spec.frame.as_deref().is_some_and(|p| {
            !p.components().any(|c| c.as_os_str() == "frames-full")
        });
        let last = bundle.keyframes.len().saturating_sub(1);
        let crop = bundle
            .autocrop
            .as_ref()
            .and_then(|a| a.crops.iter().find(|c| c.index == last));
        let from = match crop {
            Some(c) if cropped => [
                target[0] + c.x as f32,
                target[1] + c.y as f32,
                c.width as f32,
                c.height as f32,
            ],
            _ => target,
        };
        state.stack.push(spec, from);
        state.cards.push((id, dir.to_path_buf()));
    }

    /// Every UI tick: the cards' clicks, the newest card's delivery text, and the capsule
    /// folding once its card has landed.
    pub(super) fn stack_step(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.capture_stack.as_ref() else { return };
        let actions = state.stack.drain_actions();
        let dir_of = |id: u64| {
            state
                .cards
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, d)| d.clone())
        };
        let closed: Vec<(StackAction, Option<PathBuf>)> = actions
            .into_iter()
            .map(|a| {
                let dir = match &a {
                    StackAction::Closed(id) | StackAction::DraggedOut(id) => dir_of(*id),
                    StackAction::Paste(d) | StackAction::Open(d) => Some(d.clone()),
                };
                (a, dir)
            })
            .collect();
        if let Some(dir) = self.completed_dir.clone()
            && let Some((id, _)) = state.cards.iter().find(|(_, d)| *d == dir)
        {
            state
                .stack
                .set_destination(*id, &completed_label(0, self.completed_note.as_deref()));
        }
        for (action, dir) in closed {
            let Some(dir) = dir else { continue };
            match action {
                StackAction::Closed(_) => {
                    // ✕ on the card does what ✕ on the capsule did: keep it on the clipboard.
                    self.keep_pending_send(&dir);
                    self.mark_handled(&dir);
                }
                StackAction::DraggedOut(_) => self.mark_handled(&dir),
                StackAction::Paste(_) => {
                    self.keep_pending_send(&dir);
                    if !crate::capture::accessibility_granted() {
                        self.gate
                            .defer(vec![crate::onboarding::Permission::Accessibility]);
                        continue;
                    }
                    self.mark_handled(&dir);
                    std::thread::spawn(move || {
                        if let Err(err) = crate::handoff::paste_bundle_at_target(&dir, None) {
                            scrawl_log(format!("paste_bundle_at_target error: {err}"));
                        }
                    });
                }
                StackAction::Open(_) => {
                    self.mark_handled(&dir);
                    if let Ok((_, bundle)) = crate::handoff::load_bundle(&dir) {
                        self.set_review_bundle((dir, bundle), None);
                        let entity = cx.entity().clone();
                        cx.defer(move |cx| {
                            close_capture_windows(cx);
                            open_review_window(&entity, cx);
                        });
                    }
                }
            }
        }
        if let Some(state) = self.capture_stack.as_mut() {
            let live: Vec<u64> = state
                .stack
                .frames_global()
                .iter()
                .map(|f| f.id)
                .collect();
            state.cards.retain(|(id, _)| live.contains(id));
        }
        let pointer_on_pill = self.native_pill.as_ref().is_some_and(|p| p.pointer_on_pill());
        if let IslandPhase::Completed { finished_at, inbox_count, .. } = self.island_phase
            && inbox_count == 0
            && self.completed_dir.is_some()
            && self
                .capture_stack
                .as_ref()
                .is_some_and(|s| !s.cards.is_empty())
            && capsule_may_fold(Fold {
                since_completed: finished_at.elapsed(),
                pointer_on_pill,
                route_pending: self.routing.is_some() || self.pending_send.is_some(),
                route_chips: !self.route_chips.is_empty(),
            })
        {
            self.dismiss_island(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capsule_folds_after_its_card_lands_unless_routing_needs_it() {
        let landed = Fold {
            since_completed: CAPSULE_AFTER_CARD,
            ..Fold::default()
        };
        assert!(capsule_may_fold(landed));
        assert!(!capsule_may_fold(Fold {
            since_completed: Duration::from_millis(1_000),
            ..landed
        }));
        assert!(!capsule_may_fold(Fold { pointer_on_pill: true, ..landed }));
        assert!(!capsule_may_fold(Fold { route_pending: true, ..landed }));
        assert!(!capsule_may_fold(Fold { route_chips: true, ..landed }));
    }
}
