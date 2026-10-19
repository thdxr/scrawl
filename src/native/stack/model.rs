//! The capture stack's state with no AppKit in it: the cards, their springs, where each variant
//! puts them, what the pointer is over, and the rects each card draws at (`CardFrame`).
//!
//! Coordinates are points, top-left origin, local to the display the stack is on.

use std::path::PathBuf;

/// Where finished captures collect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Under the capsule at the top centre: the newest card on top, older ones peeking out below
    /// it; hovering unfolds them into a column.
    Dock,
    /// CleanShot's place: a column in the bottom-right corner, the newest at the bottom.
    Edge,
}

/// One finished capture.
#[derive(Clone, Debug)]
pub struct CardSpec {
    pub id: u64,
    pub bundle: PathBuf,
    pub sheet: PathBuf,
    /// The last keyframe at full size: the picture that lifts off the screen.
    pub frame: Option<PathBuf>,
    /// The contact sheet's size in pixels.
    pub sheet_size: [f32; 2],
    /// Where `frame` sits in the contact sheet, normalized (x, y, w, h).
    pub tile: [f32; 4],
    pub duration_ms: u64,
    /// "Copied", "Sent to SCRAWL", …
    pub destination: String,
}

/// Points.
pub const CARD_W: f32 = 208.0;
pub const INSET: f32 = 4.0;
pub const FOOTER_H: f32 = 24.0;
pub const RADIUS: f32 = 12.0;
pub const GAP: f32 = 10.0;
/// Below the capsule's bottom edge.
pub const DOCK_GAP: f32 = 10.0;
pub const EDGE_MARGIN: f32 = 16.0;
/// Collapsed dock: how much of each older card shows below the one in front, and how much
/// smaller it is.
pub const PEEK: f32 = 7.0;
pub const PEEK_SCALE: f32 = 0.05;
pub const PEEK_VISIBLE: usize = 3;
pub const MAX_CARDS: usize = 5;
pub const BUTTON_H: f32 = 26.0;
pub const PASTE_W: f32 = 64.0;
pub const OPEN_W: f32 = 58.0;
pub const BUTTON_GAP: f32 = 8.0;
pub const CLOSE_D: f32 = 20.0;

/// Seconds, as `duration / bounce 0` springs (ω = 2π / duration).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Frame → card.
    pub flight: f32,
    /// Siblings moving to make room, unfolding, folding.
    pub reflow: f32,
    /// A card closed in place.
    pub exit: f32,
    pub hover: f32,
    /// The stack leaves on its own after this long without a new card or the pointer on it.
    pub auto_close: f32,
    /// Reduce motion: everything is a fade this long.
    pub reduced_fade: f32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            flight: 0.5,
            reflow: 0.4,
            exit: 0.25,
            hover: 0.2,
            auto_close: 8.0,
            reduced_fade: 0.18,
        }
    }
}

/// A critically damped spring (bounce 0) that keeps its velocity when retargeted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    pub x: f32,
    pub v: f32,
}

impl Spring {
    pub fn at(x: f32) -> Self {
        Self { x, v: 0.0 }
    }

    /// Exact step toward `target` with ω = 2π / `duration`.
    pub fn step(&mut self, target: f32, duration: f32, dt: f32) {
        let w = std::f32::consts::TAU / duration.max(0.01);
        let d = self.x - target;
        let e = (-w * dt).exp();
        let c = self.v + w * d;
        self.x = target + (d + c * dt) * e;
        self.v = (self.v - w * c * dt) * e;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    Body,
    Paste,
    Open,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub card: u64,
    pub elem: Elem,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Leave {
    /// Closed: shrinks a little in place and fades.
    InPlace,
    /// Slides past the right edge of the display.
    Edge,
    /// Folds up into the capsule.
    Capsule,
}

pub struct Card {
    pub spec: CardSpec,
    rect: [Spring; 4],
    /// 0 when it's the frame on screen, 1 when it's a card.
    flight: Spring,
    opacity: Spring,
    hover: Spring,
    leave: Option<Leave>,
    /// The rect's spring: the flight's until it lands, then the reflow's.
    landing: bool,
    /// A sideways swipe, applied 1:1 while the fingers are down.
    swipe: f32,
    swiping: bool,
    /// Being dragged out to another app.
    dragging: bool,
}

/// Where the capsule and the usable area are.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub display: [f32; 2],
    /// The capsule's island at rest.
    pub capsule: [f32; 4],
    /// The bottom-right of the usable area (above the Dock).
    pub usable_max: [f32; 2],
}

pub struct StackModel {
    pub variant: Variant,
    pub geometry: Geometry,
    pub timing: Timing,
    pub reduced: bool,
    /// Newest first.
    cards: Vec<Card>,
    pointer: Option<[f32; 2]>,
    pub pressed: Option<Hit>,
    expanded: bool,
    /// Seconds the pointer has been off the unfolded stack.
    away: f32,
    /// Seconds since the last card or the pointer on the stack.
    idle: f32,
}

/// What one card draws this frame.
#[derive(Clone, Debug)]
pub struct CardFrame {
    pub id: u64,
    /// The card's visible rect (the clip).
    pub rect: [f32; 4],
    pub radius: f32,
    /// The contact sheet's rect (clipped to `rect`).
    pub sheet: [f32; 4],
    /// The full-size frame over its tile, fading out as the card gets small.
    pub frame: [f32; 4],
    pub frame_alpha: f32,
    pub opacity: f32,
    pub shadow: f32,
    /// Footer text and the thumbnail's own corner.
    pub chrome: f32,
    /// The card's scale against a resting card (text and buttons scale with it).
    pub k: f32,
    pub thumb: [f32; 4],
    pub hover: f32,
    pub paste: [f32; 4],
    pub open: [f32; 4],
    pub close: [f32; 4],
    pub pressed: Option<Elem>,
    pub hovered: Option<Elem>,
    pub leaving: bool,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn contains(r: [f32; 4], p: [f32; 2], pad: f32) -> bool {
    p[0] >= r[0] - pad && p[0] <= r[0] + r[2] + pad && p[1] >= r[1] - pad && p[1] <= r[1] + r[3] + pad
}

fn scaled_about(r: [f32; 4], s: f32, cx: f32, cy: f32) -> [f32; 4] {
    [cx + (r[0] - cx) * s, cy + (r[1] - cy) * s, r[2] * s, r[3] * s]
}

impl CardSpec {
    fn sheet_aspect(&self) -> f32 {
        (self.sheet_size[0] / self.sheet_size[1].max(1.0)).max(0.1)
    }

    /// The thumbnail's height on a resting card: the sheet fitted to the card's width, kept
    /// between square and 2.4:1 (wider sheets sit on the card's own dark ground).
    fn thumb_h(&self) -> f32 {
        let w = CARD_W - 2.0 * INSET;
        w / self.sheet_aspect().clamp(1.0, 2.4)
    }

    pub fn card_size(&self) -> [f32; 2] {
        [CARD_W, self.thumb_h() + 2.0 * INSET + FOOTER_H]
    }
}

impl StackModel {
    pub fn new(variant: Variant, geometry: Geometry) -> Self {
        Self {
            variant,
            geometry,
            timing: Timing::default(),
            reduced: false,
            cards: Vec::new(),
            pointer: None,
            pressed: None,
            expanded: false,
            away: 0.0,
            idle: 0.0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    pub fn card(&self, id: u64) -> Option<&CardSpec> {
        self.cards.iter().find(|c| c.spec.id == id).map(|c| &c.spec)
    }

    #[cfg(test)]
    pub fn ids(&self) -> Vec<u64> {
        self.cards.iter().map(|c| c.spec.id).collect()
    }

    pub fn set_destination(&mut self, id: u64, text: &str) -> bool {
        match self.cards.iter_mut().find(|c| c.spec.id == id) {
            Some(c) if c.spec.destination != text => {
                c.spec.destination = text.to_owned();
                true
            }
            _ => false,
        }
    }

    /// A new capture: it starts as the frame at `from` (where it was on screen) and flies to the
    /// top of the stack. With Reduce motion it fades in where it lands.
    pub fn push(&mut self, spec: CardSpec, from: [f32; 4]) {
        self.cards.insert(
            0,
            Card {
                spec,
                rect: from.map(Spring::at),
                flight: Spring::at(0.0),
                opacity: Spring::at(1.0),
                hover: Spring::at(0.0),
                leave: None,
                landing: true,
                swipe: 0.0,
                swiping: false,
                dragging: false,
            },
        );
        if self.reduced {
            let target = self.target_for(0, &self.cards[0].spec);
            let c = &mut self.cards[0];
            c.rect = target.map(Spring::at);
            c.flight = Spring::at(1.0);
            c.opacity = Spring::at(0.0);
            c.landing = false;
        }
        self.idle = 0.0;
        for i in self.live_indices().into_iter().skip(MAX_CARDS) {
            self.cards[i].leave = Some(Leave::InPlace);
        }
    }

    /// Closes one card (✕, a swipe, a paste or a drop somewhere).
    pub fn close(&mut self, id: u64) {
        let variant = self.variant;
        if let Some(c) = self.cards.iter_mut().find(|c| c.spec.id == id) {
            c.leave = Some(match variant {
                Variant::Dock if c.swipe.abs() > 1.0 => Leave::Edge,
                Variant::Dock => Leave::InPlace,
                Variant::Edge => Leave::Edge,
            });
            c.swiping = false;
        }
        if self.pressed.is_some_and(|h| h.card == id) {
            self.pressed = None;
        }
    }

    /// Everything leaves: into the capsule (dock) or past the edge (corner).
    pub fn close_all(&mut self) {
        let leave = match self.variant {
            Variant::Dock => Leave::Capsule,
            Variant::Edge => Leave::Edge,
        };
        for c in &mut self.cards {
            if c.leave.is_none() {
                c.leave = Some(leave);
            }
        }
        self.expanded = false;
    }

    pub fn set_pointer(&mut self, p: Option<[f32; 2]>) {
        self.pointer = p;
    }

    pub fn set_dragging(&mut self, id: u64, dragging: bool) {
        if let Some(c) = self.cards.iter_mut().find(|c| c.spec.id == id) {
            c.dragging = dragging;
        }
    }

    /// Two-finger horizontal scroll over a card: it follows 1:1; `ended` decides whether it goes.
    /// Returns the id of a card that was swiped away.
    pub fn swipe(&mut self, dx: f32, ended: bool) -> Option<u64> {
        // The card under the fingers when the swipe began, wherever it has moved since.
        let id = match self.cards.iter().find(|c| c.swiping && c.leave.is_none()) {
            Some(c) => c.spec.id,
            None => self.hit(self.pointer?)?.card,
        };
        let variant = self.variant;
        let c = self.cards.iter_mut().find(|c| c.spec.id == id)?;
        if !ended {
            c.swiping = true;
            // Toward the edge freely; the other way with resistance.
            let toward_edge = match variant {
                Variant::Edge => dx > 0.0 || c.swipe > 0.0,
                Variant::Dock => true,
            };
            c.swipe += if toward_edge { dx } else { dx * 0.3 };
            c.rect[0].x += if toward_edge { dx } else { dx * 0.3 };
            c.rect[0].v = 0.0;
            return None;
        }
        c.swiping = false;
        let gone = match variant {
            Variant::Edge => c.swipe > 56.0,
            Variant::Dock => c.swipe.abs() > 56.0,
        };
        let id = c.spec.id;
        if gone {
            self.close(id);
            Some(id)
        } else {
            c.swipe = 0.0;
            None
        }
    }

    fn live_indices(&self) -> Vec<usize> {
        (0..self.cards.len()).filter(|&i| self.cards[i].leave.is_none()).collect()
    }

    /// The resting rect of the `slot`-th live card (0 = newest).
    fn target_for(&self, slot: usize, spec: &CardSpec) -> [f32; 4] {
        let [w, h] = spec.card_size();
        let g = &self.geometry;
        match self.variant {
            Variant::Dock => {
                let cx = g.capsule[0] + g.capsule[2] * 0.5;
                let top = g.capsule[1] + g.capsule[3] + DOCK_GAP;
                if self.expanded {
                    // A column down from the capsule, newest at the top, each card at its own
                    // height.
                    let mut y = top;
                    for c in self.cards.iter().filter(|c| c.leave.is_none()).take(slot) {
                        y += c.spec.card_size()[1] + GAP;
                    }
                    [cx - w * 0.5, y, w, h]
                } else {
                    let i = slot.min(PEEK_VISIBLE - 1) as f32;
                    let front_h = self
                        .cards
                        .iter()
                        .find(|c| c.leave.is_none())
                        .map_or(h, |c| c.spec.card_size()[1]);
                    // A card taller than the front one shrinks to its height, so it only ever
                    // shows below it, never above.
                    let s = (1.0 - PEEK_SCALE * i) * (front_h / h).min(1.0);
                    // Older cards are smaller and their bottom edges show below the front one.
                    let bottom = top + front_h + PEEK * i;
                    [cx - w * s * 0.5, bottom - h * s, w * s, h * s]
                }
            }
            Variant::Edge => {
                let right = g.usable_max[0] - EDGE_MARGIN;
                let mut bottom = g.usable_max[1] - EDGE_MARGIN;
                for c in self.cards.iter().filter(|c| c.leave.is_none()).take(slot) {
                    bottom -= c.spec.card_size()[1] + GAP;
                }
                [right - w, bottom - h, w, h]
            }
        }
    }

    fn leave_target(&self, card: &Card, leave: Leave) -> ([f32; 4], f32) {
        let r = card.rect.map(|s| s.x);
        match leave {
            Leave::InPlace => (
                scaled_about(r, 0.96, r[0] + r[2] * 0.5, r[1] + r[3] * 0.5),
                0.0,
            ),
            Leave::Edge => {
                let x = if card.swipe < 0.0 && self.variant == Variant::Dock {
                    -r[2] - 24.0
                } else {
                    self.geometry.display[0] + 24.0
                };
                ([x, r[1], r[2], r[3]], 1.0)
            }
            Leave::Capsule => {
                let g = &self.geometry;
                let cx = g.capsule[0] + g.capsule[2] * 0.5;
                let cy = g.capsule[1] + g.capsule[3] * 0.5;
                let s = (g.capsule[3] / r[3].max(1.0)).clamp(0.1, 1.0);
                ([cx - r[2] * s * 0.5, cy - r[3] * s * 0.5, r[2] * s, r[3] * s], 0.0)
            }
        }
    }

    fn stack_hover_zone(&self) -> Option<[f32; 4]> {
        // A card still in flight starts as the whole screen; counting it would unfold the dock
        // wherever the pointer happens to be.
        let rects: Vec<[f32; 4]> = self
            .cards
            .iter()
            .filter(|c| c.leave.is_none() && !c.landing)
            .map(|c| c.rect.map(|s| s.x))
            .collect();
        let first = *rects.first()?;
        Some(rects.iter().fold(first, |a, r| {
            let x0 = a[0].min(r[0]);
            let y0 = a[1].min(r[1]);
            let x1 = (a[0] + a[2]).max(r[0] + r[2]);
            let y1 = (a[1] + a[3]).max(r[1] + r[3]);
            [x0, y0, x1 - x0, y1 - y0]
        }))
    }

    pub fn advance(&mut self, dt: f32) {
        let t = self.timing;
        let pointer_on = self
            .pointer
            .zip(self.stack_hover_zone())
            .is_some_and(|(p, z)| contains(z, p, if self.expanded { 12.0 } else { 0.0 }));
        if self.variant == Variant::Dock {
            if pointer_on {
                self.expanded = true;
                self.away = 0.0;
            } else if self.expanded {
                self.away += dt;
                if self.away > 0.3 && self.pressed.is_none() {
                    self.expanded = false;
                }
            }
        }
        let dragging = self.cards.iter().any(|c| c.dragging);
        if pointer_on || dragging {
            self.idle = 0.0;
        } else if !self.live_indices().is_empty() {
            self.idle += dt;
            if self.idle > t.auto_close {
                self.close_all();
            }
        }

        let hovered = self.pointer.and_then(|p| self.hit(p)).map(|h| h.card);
        let live = self.live_indices();
        let targets: Vec<(usize, [f32; 4])> = live
            .iter()
            .enumerate()
            .map(|(slot, &i)| (i, self.target_for(slot, &self.cards[i].spec)))
            .collect();
        let peek_hidden: Vec<usize> = if self.variant == Variant::Dock && !self.expanded {
            live.iter().skip(PEEK_VISIBLE).copied().collect()
        } else {
            Vec::new()
        };
        let leaving: Vec<(usize, [f32; 4], f32)> = (0..self.cards.len())
            .filter_map(|i| {
                let c = &self.cards[i];
                c.leave.map(|l| {
                    let (r, o) = self.leave_target(c, l);
                    (i, r, o)
                })
            })
            .collect();

        let reduced = self.reduced;
        for (i, target) in targets {
            let c = &mut self.cards[i];
            let opacity_target = if peek_hidden.contains(&i) { 0.0 } else { 1.0 };
            if reduced {
                c.rect = target.map(Spring::at);
                c.flight = Spring::at(1.0);
                c.opacity.step(opacity_target, t.reduced_fade, dt);
            } else {
                let d = if c.landing { t.flight } else { t.reflow };
                for (k, s) in c.rect.iter_mut().enumerate() {
                    if k == 0 && c.swiping {
                        continue;
                    }
                    s.step(target[k], d, dt);
                }
                c.flight.step(1.0, t.flight, dt);
                if c.landing && c.flight.x > 0.985 {
                    c.landing = false;
                }
                c.opacity.step(opacity_target, t.reflow * 0.5, dt);
            }
            let h = if hovered == Some(c.spec.id) && !c.dragging { 1.0 } else { 0.0 };
            c.hover.step(h, t.hover, dt);
        }
        for (i, target, opacity) in leaving {
            let c = &mut self.cards[i];
            c.hover.step(0.0, t.hover, dt);
            if reduced {
                c.opacity.step(0.0, t.reduced_fade, dt);
                continue;
            }
            let d = match c.leave {
                Some(Leave::Capsule) => t.reflow,
                _ => t.exit,
            };
            for (k, s) in c.rect.iter_mut().enumerate() {
                s.step(target[k], d, dt);
            }
            c.flight.step(1.0, t.flight, dt);
            // Exits fade a little ahead of their travel.
            c.opacity.step(opacity, d * 0.8, dt);
        }
        let display_w = self.geometry.display[0];
        self.cards.retain(|c| {
            let Some(leave) = c.leave else { return true };
            let r = c.rect.map(|s| s.x);
            let offscreen = r[0] > display_w + 2.0 || r[0] + r[2] < -2.0;
            !(c.opacity.x < 0.01 || (leave == Leave::Edge && offscreen))
        });
    }

    fn layout(&self, c: &Card) -> CardFrame {
        let v = c.rect.map(|s| s.x);
        let q = c.flight.x.clamp(0.0, 1.0);
        let spec = &c.spec;
        let [cw, _] = spec.card_size();
        let k = v[2] / cw;
        let thumb_h = spec.thumb_h();
        let thumb = [
            v[0] + INSET * k,
            v[1] + INSET * k,
            (cw - 2.0 * INSET) * k,
            thumb_h * k,
        ];
        let aspect = spec.sheet_aspect();
        let tile = spec.tile;
        // The sheet's width against the card's: the tile filling the frame at first, the whole
        // sheet fitted in the thumbnail at the end.
        let fit_w = (thumb[2]).min(thumb[3] * aspect);
        let r0 = 1.0 / tile[2].max(0.01);
        let r1 = fit_w / v[2].max(0.01);
        let sw = lerp(r0, r1, q) * v[2];
        let sh = sw / aspect;
        // The sheet point that stays put: the tile's centre at first, the sheet's at the end; it
        // sits on the rect's centre at first and the thumbnail's at the end.
        let ax = lerp(tile[0] + tile[2] * 0.5, 0.5, q);
        let ay = lerp(tile[1] + tile[3] * 0.5, 0.5, q);
        let bx = lerp(v[0] + v[2] * 0.5, thumb[0] + thumb[2] * 0.5, q);
        let by = lerp(v[1] + v[3] * 0.5, thumb[1] + thumb[3] * 0.5, q);
        let sheet = [bx - ax * sw, by - ay * sh, sw, sh];
        let frame = [
            sheet[0] + tile[0] * sw,
            sheet[1] + tile[1] * sh,
            tile[2] * sw,
            tile[3] * sh,
        ];
        let bcx = thumb[0] + thumb[2] * 0.5;
        let by_ = thumb[1] + (thumb[3] - BUTTON_H * k) * 0.5;
        let total = (PASTE_W + BUTTON_GAP + OPEN_W) * k;
        let paste = [bcx - total * 0.5, by_, PASTE_W * k, BUTTON_H * k];
        let open = [
            paste[0] + (PASTE_W + BUTTON_GAP) * k,
            by_,
            OPEN_W * k,
            BUTTON_H * k,
        ];
        let close = [
            v[0] - CLOSE_D * 0.3,
            v[1] - CLOSE_D * 0.3,
            CLOSE_D,
            CLOSE_D,
        ];
        let hovered = if c.hover.x > 0.5 {
            self.pointer.and_then(|p| {
                if contains(close, p, 2.0) {
                    Some(Elem::Close)
                } else if contains(paste, p, 0.0) {
                    Some(Elem::Paste)
                } else if contains(open, p, 0.0) {
                    Some(Elem::Open)
                } else if contains(v, p, 0.0) {
                    Some(Elem::Body)
                } else {
                    None
                }
            })
        } else {
            None
        };
        CardFrame {
            id: spec.id,
            rect: v,
            radius: RADIUS * k * smoothstep(0.0, 0.5, q),
            sheet,
            frame,
            frame_alpha: if spec.frame.is_some() {
                1.0 - smoothstep(0.3, 0.7, q)
            } else {
                0.0
            },
            opacity: c.opacity.x.clamp(0.0, 1.0) * if c.dragging { 0.5 } else { 1.0 },
            shadow: smoothstep(0.0, 0.3, q),
            chrome: smoothstep(0.7, 1.0, q),
            k,
            thumb,
            hover: c.hover.x.clamp(0.0, 1.0),
            paste,
            open,
            close,
            pressed: self.pressed.filter(|h| h.card == spec.id).map(|h| h.elem),
            hovered,
            leaving: c.leave.is_some(),
        }
    }

    /// Back to front.
    pub fn frames(&self) -> Vec<CardFrame> {
        self.cards.iter().rev().map(|c| self.layout(c)).collect()
    }

    /// What's under `p`, front to back. Only cards that can be seen and have landed take the
    /// pointer; in the folded dock only the front card does (with its peeking edges).
    pub fn hit(&self, p: [f32; 2]) -> Option<Hit> {
        for c in self.cards.iter() {
            if c.leave.is_some() || c.landing || c.opacity.x < 0.5 {
                continue;
            }
            let f = self.layout(c);
            if c.hover.x > 0.5 && contains(f.close, p, 2.0) {
                return Some(Hit { card: c.spec.id, elem: Elem::Close });
            }
            if !contains(f.rect, p, 0.0) {
                continue;
            }
            let elem = if c.hover.x > 0.5 && contains(f.paste, p, 0.0) {
                Elem::Paste
            } else if c.hover.x > 0.5 && contains(f.open, p, 0.0) {
                Elem::Open
            } else {
                Elem::Body
            };
            return Some(Hit { card: c.spec.id, elem });
        }
        None
    }

    /// The pointer is over something the stack should take clicks for.
    pub fn takes_pointer(&self, p: [f32; 2]) -> bool {
        self.hit(p).is_some()
            || (self.variant == Variant::Dock
                && self.stack_hover_zone().is_some_and(|z| contains(z, p, 0.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> Geometry {
        Geometry {
            display: [1512.0, 982.0],
            capsule: [656.0, 56.0, 200.0, 40.0],
            usable_max: [1512.0, 982.0],
        }
    }

    fn spec(id: u64) -> CardSpec {
        CardSpec {
            id,
            bundle: PathBuf::from("/tmp/x.scrawl"),
            sheet: PathBuf::from("/tmp/x.scrawl/contact-sheet.png"),
            frame: Some(PathBuf::from("/tmp/x.scrawl/frames/frame-02.png")),
            sheet_size: [1600.0, 506.0],
            tile: [0.5, 0.0, 0.5, 1.0],
            duration_ms: 9_000,
            destination: "Copied".into(),
        }
    }

    fn run(m: &mut StackModel, seconds: f32) {
        let mut t = 0.0;
        while t < seconds {
            m.advance(1.0 / 120.0);
            t += 1.0 / 120.0;
        }
    }

    #[test]
    fn the_flight_starts_as_the_frame_on_screen() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        // The frame's shape: the tile is 800 × 506 px.
        let from = [0.0, 0.0, 1512.0, 1512.0 * 506.0 / 800.0];
        m.push(spec(1), from);
        let f = &m.frames()[0];
        for k in 0..4 {
            assert!((f.rect[k] - from[k]).abs() < 0.01, "{:?}", f.rect);
            assert!((f.frame[k] - from[k]).abs() < 0.5, "{:?}", f.frame);
        }
        assert_eq!(f.radius, 0.0);
    }

    #[test]
    fn the_flight_is_a_straight_line_to_its_dock() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        let from = [0.0, 0.0, 1512.0, 982.0];
        m.push(spec(1), from);
        let mut centres = Vec::new();
        for _ in 0..120 {
            m.advance(1.0 / 120.0);
            let r = m.frames()[0].rect;
            centres.push([r[0] + r[2] * 0.5, r[1] + r[3] * 0.5]);
        }
        let end = *centres.last().unwrap();
        let start = [756.0, 491.0];
        for c in &centres {
            // Distance from the line start → end.
            let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
            let off = ((c[0] - start[0]) * dy - (c[1] - start[1]) * dx).abs() / dx.hypot(dy);
            assert!(off < 0.5, "{c:?} is {off} pt off the line");
        }
        // Monotonic: always getting closer.
        for w in centres.windows(2) {
            let d0 = (w[0][0] - end[0]).hypot(w[0][1] - end[1]);
            let d1 = (w[1][0] - end[0]).hypot(w[1][1] - end[1]);
            assert!(d1 <= d0 + 1e-3);
        }
        let f = &m.frames()[0];
        assert!(f.rect[1] > 96.0 && (f.rect[0] + f.rect[2] * 0.5 - 756.0).abs() < 0.5);
    }

    #[test]
    fn a_new_card_pushes_the_others_back_and_closing_reflows() {
        let mut m = StackModel::new(Variant::Edge, geometry());
        m.push(spec(1), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        let first = m.frames()[0].rect;
        m.push(spec(2), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        let frames = m.frames();
        let old = frames.iter().find(|f| f.id == 1).unwrap().rect;
        let new = frames.iter().find(|f| f.id == 2).unwrap().rect;
        assert!((new[1] - first[1]).abs() < 0.5, "the newest takes the bottom slot");
        assert!(old[1] + old[3] < new[1], "the older one moved up");
        m.close(2);
        run(&mut m, 1.0);
        assert_eq!(m.ids(), vec![1]);
        assert!((m.frames()[0].rect[1] - first[1]).abs() < 0.5);
    }

    #[test]
    fn the_dock_unfolds_under_the_pointer_and_folds_back() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        m.timing.auto_close = 100.0;
        for id in 1..=3 {
            m.push(spec(id), [0.0, 0.0, 1512.0, 982.0]);
            run(&mut m, 1.0);
        }
        let folded: Vec<[f32; 4]> = m.frames().iter().map(|f| f.rect).collect();
        let front = folded[2];
        m.set_pointer(Some([front[0] + 20.0, front[1] + 20.0]));
        run(&mut m, 1.0);
        let unfolded: Vec<[f32; 4]> = m.frames().iter().map(|f| f.rect).collect();
        assert!((unfolded[2][1] - front[1]).abs() < 0.5, "the front card stays put");
        assert!(unfolded[0][1] > unfolded[1][1] + 50.0, "older cards go down the column");
        m.set_pointer(None);
        run(&mut m, 1.0);
        let refolded: Vec<[f32; 4]> = m.frames().iter().map(|f| f.rect).collect();
        assert!((refolded[0][1] - folded[0][1]).abs() < 0.5);
    }

    #[test]
    fn a_card_in_flight_does_not_unfold_the_dock_under_a_resting_pointer() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        m.timing.auto_close = 100.0;
        m.set_pointer(Some([300.0, 600.0]));
        m.push(spec(1), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        // The second card starts as the whole screen, over the pointer.
        m.push(spec(2), [0.0, 0.0, 1512.0, 982.0]);
        let mut lowest = f32::MIN;
        for _ in 0..240 {
            m.advance(1.0 / 120.0);
            let old = m.frames().iter().find(|f| f.id == 1).unwrap().rect;
            lowest = lowest.max(old[1]);
        }
        let settled = m.frames().iter().find(|f| f.id == 1).unwrap().rect[1];
        // Straight to its peek slot: never further down the screen (into a column) first.
        assert!(lowest <= settled + 0.5, "the older card went down to {lowest}, then back to {settled}");
    }

    #[test]
    fn older_cards_in_the_folded_dock_only_show_below_the_front_one() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        m.timing.auto_close = 100.0;
        let mut tall = spec(1);
        tall.sheet_size = [1600.0, 1100.0];
        m.push(tall, [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        m.push(spec(2), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        let frames = m.frames();
        let old = frames.iter().find(|f| f.id == 1).unwrap().rect;
        let front = frames.iter().find(|f| f.id == 2).unwrap().rect;
        assert!(old[1] >= front[1] - 0.5, "older top {} above the front's {}", old[1], front[1]);
        assert!(old[1] + old[3] > front[1] + front[3] + 5.0, "its bottom edge peeks out");
    }

    #[test]
    fn it_leaves_on_its_own_unless_the_pointer_is_on_it() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        m.timing.auto_close = 2.0;
        m.push(spec(1), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        let r = m.frames()[0].rect;
        m.set_pointer(Some([r[0] + 10.0, r[1] + 10.0]));
        run(&mut m, 3.0);
        assert_eq!(m.ids(), vec![1]);
        m.set_pointer(None);
        run(&mut m, 3.5);
        assert!(m.is_empty());
    }

    #[test]
    fn reduced_motion_fades_in_where_it_lands() {
        let mut m = StackModel::new(Variant::Dock, geometry());
        m.reduced = true;
        m.push(spec(1), [0.0, 0.0, 1512.0, 982.0]);
        let f = &m.frames()[0];
        assert!(f.rect[1] > 96.0 && f.opacity < 0.01);
        run(&mut m, 0.5);
        let g = &m.frames()[0];
        assert_eq!(f.rect, g.rect);
        assert!(g.opacity > 0.99);
    }

    #[test]
    fn a_swipe_follows_the_fingers_and_past_the_threshold_dismisses() {
        let mut m = StackModel::new(Variant::Edge, geometry());
        m.push(spec(1), [0.0, 0.0, 1512.0, 982.0]);
        run(&mut m, 1.0);
        let r = m.frames()[0].rect;
        m.set_pointer(Some([r[0] + 30.0, r[1] + 30.0]));
        run(&mut m, 0.3);
        let r = m.frames()[0].rect;
        m.swipe(20.0, false);
        let moved = m.frames()[0].rect;
        assert!((moved[0] - (r[0] + 20.0)).abs() < 0.01, "{r:?} → {moved:?}");
        m.swipe(0.0, true);
        run(&mut m, 1.0);
        assert!((m.frames()[0].rect[0] - r[0]).abs() < 0.5, "springs back");
        m.swipe(80.0, false);
        assert_eq!(m.swipe(0.0, true), Some(1));
        run(&mut m, 1.0);
        assert!(m.is_empty());
    }
}
