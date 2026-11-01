//! On-screen touch controls for the 1.16.5 Android target.
//!
//! The phone layout mirrors Bedrock's fixed controls so that every gesture is
//! deterministic and testable without an Android device:
//!
//! - the bottom-left holds a D-pad: a cross of four direction buttons around a
//!   centre cell. Up, down, left and right drive [`Input::forward`],
//!   [`Input::back`], [`Input::left`] and [`Input::right`]. Several buttons
//!   may be held at once and a finger sliding between adjacent buttons ORs
//!   their flags, so a diagonal reads as two directions;
//! - the centre cell toggles sneak. It is sticky: the flag stays set until
//!   the cell is pressed again, and [`TouchFrame::sneak`] reports it every
//!   tick. The cell is tinted while sneak is active;
//! - a large jump button sits in the bottom-right corner; the place and
//!   attack buttons sit in a row just above it, inside the look zone;
//! - the rest of the right half is the look zone: dragging reports degree
//!   deltas in the desktop mouse convention (`yaw -= look[0]`,
//!   `pitch += look[1]`), and a short tap breaks the targeted block;
//! - a nine-slot hotbar sits centred above the D-pad and the right-hand
//!   buttons. Tapping a slot selects it; because the hotbar is hit-tested
//!   first, such a touch never starts a look drag, breaks a block or acts on
//!   a button.
//!
//! [`TouchControls::frame`] drains the one-shot actions and the accumulated
//! look delta for one tick while held movement and the sticky sneak flag keep
//! being reported, so it can be called unconditionally every frame.
//! [`TouchControls::hud_rects`] and [`TouchControls::hotbar_rects`] expose the
//! same layout as normalized rectangles for the HUD renderer.
//!
//! Coordinates are physical pixels from winit with a top-left origin.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::player::Input;

/// Fraction of the width that belongs to the movement pad. Touches left of
/// this line that miss every pad button are inert instead of starting a look
/// drag.
const PAD_ZONE: f32 = 0.45;
/// Side of one D-pad button (and of the centre cell) as a fraction of the
/// smaller screen side.
const PAD_CELL: f32 = 0.18;
/// Distance between the D-pad and the screen edges.
const PAD_MARGIN: f32 = 0.03;
/// Side of the large jump button as a fraction of the smaller screen side.
const JUMP_SIZE: f32 = 0.25;
/// Side of the place and attack buttons as a fraction of the smaller screen
/// side.
const ACTION_SIZE: f32 = 0.18;
/// Distance between the right-hand controls and the screen edges.
const ACTION_MARGIN: f32 = 0.03;
/// Gap between the attack and place buttons, and between the place/attack row
/// and the jump button below it, as a fraction of the smaller screen side.
const ACTION_GAP: f32 = 0.02;
/// Height of the faint break-zone hint at the top of the look zone.
const BREAK_HINT_HEIGHT: f32 = 0.07;
/// Number of hotbar slots.
pub const HOTBAR_SLOTS: usize = 9;
/// Side of one hotbar slot as a fraction of the smaller screen side.
const HOTBAR_SLOT_SIZE: f32 = 0.085;
/// Gap between two hotbar slots, as a fraction of the smaller screen side.
const HOTBAR_GAP: f32 = 0.007;
/// Distance from the bottom edge to the hotbar, as a fraction of the smaller
/// screen side. Larger than the D-pad's `PAD_MARGIN + 3 * PAD_CELL` so the
/// bar always clears the pad and the right-hand buttons.
const HOTBAR_BOTTOM: f32 = 0.60;

/// Longest press that still counts as a tap instead of a hold.
const TAP_TIME: Duration = Duration::from_millis(250);
/// Largest finger travel, in pixels, that still counts as a tap.
const TAP_SLOP: f32 = 10.0;
/// Look sensitivity in degrees per physical pixel.
const LOOK_DEGREES_PER_PIXEL: f32 = 0.15;

/// Direction bits held by D-pad fingers.
const DIR_UP: u8 = 1 << 0;
const DIR_DOWN: u8 = 1 << 1;
const DIR_LEFT: u8 = 1 << 2;
const DIR_RIGHT: u8 = 1 << 3;

// Subtle translucent white controls, drawn over the world.
const PAD_BUTTON_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.18];
/// The sneak cell turns cool blue while the sticky flag is set.
const SNEAK_ACTIVE_COLOR: [f32; 4] = [0.45, 0.85, 1.0, 0.55];
const JUMP_BUTTON_COLOR: [f32; 4] = [0.72, 0.94, 1.0, 0.26];
const PLACE_BUTTON_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.22];
/// The attack button is a warm tint so it reads apart from the place button.
const ATTACK_BUTTON_COLOR: [f32; 4] = [1.0, 0.68, 0.55, 0.24];
const BREAK_HINT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.05];

/// One tick of touch input.
///
/// `movement` and `sneak` are held states and are therefore still set on the
/// next call; everything else is a one-shot (or, for `look`, an accumulated
/// delta) that [`TouchControls::frame`] drains.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TouchFrame {
    /// Held movement direction from the D-pad.
    pub movement: Input,
    /// Sticky sneak toggle from the D-pad's centre cell.
    pub sneak: bool,
    /// Look delta in degrees, desktop mouse convention: the caller applies
    /// `yaw -= look[0]` and `pitch += look[1]`.
    pub look: [f32; 2],
    /// Set once for a short look-zone tap.
    pub break_block: bool,
    /// Set once when the place button is pressed.
    pub place_block: bool,
    /// Set once when the attack button is pressed.
    pub attack: bool,
    /// Set once when the jump button is pressed.
    pub jump: bool,
    /// Hotbar slot selected by a touch on the hotbar, if any.
    pub select: Option<usize>,
}

/// Which gesture a finger owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// One or more held D-pad direction bits.
    Pad(u8),
    /// The centre sneak cell; toggles on press.
    Sneak,
    /// The jump button; jumps once on press.
    Jump,
    Look,
    Place,
    /// The attack button; pressing it breaks/attacks once.
    Attack,
    /// A hotbar press; the slot was selected at press time.
    Hotbar,
}

/// State kept for a finger that is still touching the screen.
#[derive(Clone, Copy, Debug)]
struct Finger {
    role: Role,
    start: [f32; 2],
    last: [f32; 2],
    started: Instant,
    /// Largest distance from `start` seen so far; a tap never exceeds
    /// [`TAP_SLOP`].
    travel: f32,
}

/// Bedrock-style D-pad, look area, jump/place/attack buttons and hotbar for
/// one touch screen.
#[derive(Clone, Debug, Default)]
pub struct TouchControls {
    width: f32,
    height: f32,
    fingers: BTreeMap<u64, Finger>,
    look: [f32; 2],
    /// Sticky sneak state toggled by the D-pad's centre cell.
    sneak: bool,
    jump: bool,
    break_block: bool,
    place_block: bool,
    attack: bool,
    /// Hotbar slot highlighted by the touch HUD.
    selected: usize,
    /// Slot picked since the last [`TouchControls::frame`].
    select: Option<usize>,
}

impl TouchControls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the layout to a new physical size in pixels.
    ///
    /// Fingers keep their roles; only the rectangles they are measured
    /// against change, so a resize never strands a held gesture.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.width = width.max(0.0);
        self.height = height.max(0.0);
    }

    /// Feeds one winit touch event in physical pixels.
    ///
    /// `Ended` and `Cancelled` release the finger; only `Ended` can produce a
    /// tap action.
    pub fn touch(&mut self, id: u64, phase: winit::event::TouchPhase, x: f32, y: f32) {
        self.touch_at(id, phase, [x, y], Instant::now());
    }

    /// Drains the one-shot actions and the look delta for this tick.
    ///
    /// Held movement and the sticky sneak flag are not cleared: they are
    /// derived from the still-touching fingers and the toggle state. Safe to
    /// call every tick, whether or not anything happened.
    pub fn frame(&mut self) -> TouchFrame {
        let pad = self
            .fingers
            .values()
            .fold(0u8, |mask, finger| match finger.role {
                Role::Pad(directions) => mask | directions,
                _ => mask,
            });
        let frame = TouchFrame {
            movement: movement_from_pad(pad),
            sneak: self.sneak,
            look: self.look,
            break_block: self.break_block,
            place_block: self.place_block,
            attack: self.attack,
            jump: self.jump,
            select: self.select.take(),
        };
        self.look = [0.0; 2];
        self.break_block = false;
        self.place_block = false;
        self.attack = false;
        self.jump = false;
        frame
    }

    /// The hotbar slot the touch HUD highlights.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Which of the eight visible buttons read as pressed right now, in the
    /// order [`Self::hud_rects`] returns them: up, left, sneak, right, down,
    /// jump, place, attack.
    pub fn pressed_buttons(&self) -> [bool; 8] {
        let pad = self
            .fingers
            .values()
            .fold(0u8, |mask, finger| match finger.role {
                Role::Pad(directions) => mask | directions,
                _ => mask,
            });
        [
            pad & DIR_UP != 0,
            pad & DIR_LEFT != 0,
            self.sneak,
            pad & DIR_RIGHT != 0,
            pad & DIR_DOWN != 0,
            self.jump,
            self.place_block,
            self.attack,
        ]
    }

    /// Highlights `slot` without queueing a selection. Out-of-range slots are
    /// ignored. The app uses this to mirror the inventory's selection (which
    /// keyboard digits and the mouse wheel also change) into the touch HUD.
    pub fn set_selected(&mut self, slot: usize) {
        if slot < HOTBAR_SLOTS {
            self.selected = slot;
        }
    }

    /// The nine hotbar slots as normalized `(rect, selected)` pairs.
    ///
    /// Rects use the same top-left `0..=1` convention as
    /// [`TouchControls::hud_rects`]; the bar is centred horizontally and sits
    /// above the D-pad and the right-hand buttons.
    pub fn hotbar_rects(&self) -> [([f32; 4], bool); HOTBAR_SLOTS] {
        std::array::from_fn(|slot| {
            let rect = self.hotbar_slot_rect(slot);
            (
                self.normalize(rect[0], rect[1], rect[2], rect[3]),
                slot == self.selected,
            )
        })
    }

    /// The visible controls as `(rect, color)` pairs in normalized `0..=1`
    /// coordinates with a top-left origin.
    ///
    /// The order is fixed: D-pad up, left, centre (sneak), right, down, then
    /// jump, place, attack and the faint break-zone hint at the top of the
    /// look area.
    pub fn hud_rects(&self) -> Vec<([f32; 4], [f32; 4])> {
        let up = self.pad_cell_rect(1, 0);
        let left = self.pad_cell_rect(0, 1);
        let centre = self.sneak_rect();
        let right = self.pad_cell_rect(2, 1);
        let down = self.pad_cell_rect(1, 2);
        let jump = self.jump_rect();
        let place = self.place_rect();
        let attack = self.attack_rect();
        let sneak_color = if self.sneak {
            SNEAK_ACTIVE_COLOR
        } else {
            PAD_BUTTON_COLOR
        };
        vec![
            (self.rect(up), PAD_BUTTON_COLOR),
            (self.rect(left), PAD_BUTTON_COLOR),
            (self.rect(centre), sneak_color),
            (self.rect(right), PAD_BUTTON_COLOR),
            (self.rect(down), PAD_BUTTON_COLOR),
            (self.rect(jump), JUMP_BUTTON_COLOR),
            (self.rect(place), PLACE_BUTTON_COLOR),
            (self.rect(attack), ATTACK_BUTTON_COLOR),
            (
                // Already a screen fraction: keep the exact normalized edges
                // so the hint always abuts the right edge of the screen.
                [PAD_ZONE, 0.0, 1.0 - PAD_ZONE, BREAK_HINT_HEIGHT],
                BREAK_HINT_COLOR,
            ),
        ]
    }

    fn rect(&self, rect: [f32; 4]) -> [f32; 4] {
        self.normalize(rect[0], rect[1], rect[2], rect[3])
    }

    fn min_side(&self) -> f32 {
        self.width.min(self.height)
    }

    fn pad_cell(&self) -> f32 {
        self.min_side() * PAD_CELL
    }

    /// Top-left pixel origin of the 3x3 D-pad footprint.
    fn pad_origin(&self) -> [f32; 2] {
        let cell = self.pad_cell();
        let margin = self.min_side() * PAD_MARGIN;
        [margin, self.height - margin - cell * 3.0]
    }

    /// `(x, y, width, height)` of the D-pad cell at `column`/`row`.
    fn pad_cell_rect(&self, column: usize, row: usize) -> [f32; 4] {
        let cell = self.pad_cell();
        let origin = self.pad_origin();
        [
            origin[0] + cell * column as f32,
            origin[1] + cell * row as f32,
            cell,
            cell,
        ]
    }

    /// The sneak toggle cell, at the centre of the D-pad.
    fn sneak_rect(&self) -> [f32; 4] {
        self.pad_cell_rect(1, 1)
    }

    /// The four direction buttons with their bits, for hit-testing.
    fn pad_buttons(&self) -> [(u8, [f32; 4]); 4] {
        [
            (DIR_UP, self.pad_cell_rect(1, 0)),
            (DIR_RIGHT, self.pad_cell_rect(2, 1)),
            (DIR_DOWN, self.pad_cell_rect(1, 2)),
            (DIR_LEFT, self.pad_cell_rect(0, 1)),
        ]
    }

    /// Direction bits whose button contains `point`. Buttons share edges, so
    /// a touch on a corner reports both adjacent directions.
    fn directions_at(&self, point: [f32; 2]) -> u8 {
        let mut directions = 0;
        for (direction, rect) in self.pad_buttons() {
            if contains(rect, point) {
                directions |= direction;
            }
        }
        directions
    }

    /// `(x, y, width, height)` of the jump button in pixels.
    fn jump_rect(&self) -> [f32; 4] {
        let side = self.min_side() * JUMP_SIZE;
        let margin = self.min_side() * ACTION_MARGIN;
        [
            self.width - margin - side,
            self.height - margin - side,
            side,
            side,
        ]
    }

    /// `(x, y, width, height)` of the place button in pixels.
    ///
    /// Right-aligned with the jump button and sitting one gap above it.
    fn place_rect(&self) -> [f32; 4] {
        let side = self.min_side() * ACTION_SIZE;
        let margin = self.min_side() * ACTION_MARGIN;
        let gap = self.min_side() * ACTION_GAP;
        let jump = self.jump_rect();
        [self.width - margin - side, jump[1] - gap - side, side, side]
    }

    /// `(x, y, width, height)` of the attack button in pixels.
    ///
    /// It is the place button's size, shifted one button plus a gap to the
    /// left so the two never overlap.
    fn attack_rect(&self) -> [f32; 4] {
        let place = self.place_rect();
        let gap = self.min_side() * ACTION_GAP;
        [place[0] - gap - place[2], place[1], place[2], place[3]]
    }

    /// `(x, y, width, height)` of hotbar `slot` in pixels.
    ///
    /// The side is capped against the screen width so the bar still fits on
    /// unusually narrow surfaces.
    fn hotbar_slot_rect(&self, slot: usize) -> [f32; 4] {
        let side = (self.min_side() * HOTBAR_SLOT_SIZE).min(self.width * 0.11);
        let gap = side * (HOTBAR_GAP / HOTBAR_SLOT_SIZE);
        let total = side * HOTBAR_SLOTS as f32 + gap * (HOTBAR_SLOTS - 1) as f32;
        let left = (self.width - total) * 0.5;
        let y = self.height - self.min_side() * HOTBAR_BOTTOM - side;
        [left + slot as f32 * (side + gap), y, side, side]
    }

    /// Converts a pixel rectangle into normalized screen coordinates.
    fn normalize(&self, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
        if self.width <= 0.0 || self.height <= 0.0 {
            return [0.0; 4];
        }
        [
            (x / self.width).clamp(0.0, 1.0),
            (y / self.height).clamp(0.0, 1.0),
            (w / self.width).clamp(0.0, 1.0),
            (h / self.height).clamp(0.0, 1.0),
        ]
    }

    fn has_role(&self, role: Role) -> bool {
        self.fingers.values().any(|finger| finger.role == role)
    }

    fn touch_at(
        &mut self,
        id: u64,
        phase: winit::event::TouchPhase,
        point: [f32; 2],
        now: Instant,
    ) {
        match phase {
            winit::event::TouchPhase::Started => self.press(id, point, now),
            winit::event::TouchPhase::Moved => self.drag(id, point),
            winit::event::TouchPhase::Ended => self.release(id, point, now, true),
            winit::event::TouchPhase::Cancelled => self.release(id, point, now, false),
        }
    }

    /// Assigns a freshly started finger to a control, ignoring duplicates.
    fn press(&mut self, id: u64, point: [f32; 2], now: Instant) {
        if self.fingers.contains_key(&id) {
            return;
        }
        let finger = Finger {
            role: Role::Place,
            start: point,
            last: point,
            started: now,
            travel: 0.0,
        };

        // The D-pad's centre cell toggles sneak. It is tested before every
        // other control: the four direction cells meet at the middle, so a
        // corner touch there would otherwise read as a movement.
        if contains(self.sneak_rect(), point) && self.directions_at(point) == 0 {
            self.sneak = !self.sneak;
            self.fingers.insert(
                id,
                Finger {
                    role: Role::Sneak,
                    ..finger
                },
            );
            return;
        }

        // The hotbar is checked first: a slot tap must never become a look
        // drag or press a control, even though the bar overlays the tail of
        // the look zone. Selection takes effect immediately and is queued for
        // the next tick.
        let side = self.hotbar_slot_rect(0)[2];
        if side > 0.0 {
            for slot in 0..HOTBAR_SLOTS {
                if contains(self.hotbar_slot_rect(slot), point) {
                    self.selected = slot;
                    self.select = Some(slot);
                    self.fingers.insert(
                        id,
                        Finger {
                            role: Role::Hotbar,
                            ..finger
                        },
                    );
                    return;
                }
            }
        }

        // The jump button is the largest control in the look zone and is
        // checked before the buttons above it.
        if contains(self.jump_rect(), point) {
            if self.has_role(Role::Jump) {
                return;
            }
            self.jump = true;
            self.fingers.insert(
                id,
                Finger {
                    role: Role::Jump,
                    ..finger
                },
            );
            return;
        }

        // The attack button is checked next to the place button so a touch on
        // it can never become a look drag either.
        if contains(self.attack_rect(), point) {
            if self.has_role(Role::Attack) {
                return;
            }
            self.attack = true;
            self.fingers.insert(
                id,
                Finger {
                    role: Role::Attack,
                    ..finger
                },
            );
            return;
        }

        // The place button is checked next so a touch on it can never become a
        // look drag, even though it sits inside the look zone.
        if contains(self.place_rect(), point) {
            if self.has_role(Role::Place) {
                return;
            }
            self.place_block = true;
            self.fingers.insert(id, finger);
            return;
        }

        // Direction buttons: several may be held at once. A corner press
        // reports both adjacent directions.
        let directions = self.directions_at(point);
        if directions != 0 {
            self.fingers.insert(
                id,
                Finger {
                    role: Role::Pad(directions),
                    ..finger
                },
            );
            return;
        }

        // Dead space around the D-pad stays inert, but the left half itself
        // still belongs to the pad rather than the look zone.
        if point[0] < self.width * PAD_ZONE {
            return;
        }

        if self.has_role(Role::Look) {
            return;
        }
        self.fingers.insert(
            id,
            Finger {
                role: Role::Look,
                ..finger
            },
        );
    }

    /// Updates a tracked finger; touches that were ignored at press time stay
    /// ignored because they have no entry.
    fn drag(&mut self, id: u64, point: [f32; 2]) {
        let Some(finger) = self.fingers.get(&id).copied() else {
            return;
        };
        let delta = [point[0] - finger.last[0], point[1] - finger.last[1]];
        // A pad finger accumulates directions as it slides: adjacent buttons
        // OR their flags, while an opposite direction replaces the old one.
        let role = match finger.role {
            Role::Pad(held) => {
                let here = self.directions_at(point);
                // Sliding onto a corner between two buttons keeps both; the
                // centre cell alone keeps what was held.
                Role::Pad(if here == 0 {
                    held
                } else {
                    merge_directions(held, here)
                })
            }
            other => other,
        };
        if let Some(tracked) = self.fingers.get_mut(&id) {
            tracked.last = point;
            tracked.travel = tracked.travel.max(distance(finger.start, point));
            tracked.role = role;
        }

        match role {
            Role::Look => {
                // Desktop mouse convention: pixels add degrees directly.
                self.look[0] += delta[0] * LOOK_DEGREES_PER_PIXEL;
                self.look[1] += delta[1] * LOOK_DEGREES_PER_PIXEL;
            }
            // Movement is re-derived from the accumulated direction bits.
            Role::Pad(_) => {}
            // Press-only controls are inert while dragged.
            Role::Sneak | Role::Jump | Role::Place | Role::Attack | Role::Hotbar => {}
        }
    }

    /// Drops a finger. `ended` distinguishes a real release (which may tap)
    /// from a cancelled gesture (which never acts).
    fn release(&mut self, id: u64, point: [f32; 2], now: Instant, ended: bool) {
        let Some(finger) = self.fingers.remove(&id) else {
            return;
        };
        if !ended {
            return;
        }
        let travel = finger.travel.max(distance(finger.start, point));
        if now.saturating_duration_since(finger.started) >= TAP_TIME || travel >= TAP_SLOP {
            return;
        }
        match finger.role {
            Role::Look => self.break_block = true,
            // The direction buttons act while held; the sneak cell, jump,
            // place, attack and hotbar already acted on press.
            Role::Pad(_) | Role::Sneak | Role::Jump | Role::Place | Role::Attack | Role::Hotbar => {
            }
        }
    }
}

/// The movement flags for the held D-pad direction bits.
fn movement_from_pad(directions: u8) -> Input {
    Input {
        forward: directions & DIR_UP != 0,
        back: directions & DIR_DOWN != 0,
        left: directions & DIR_LEFT != 0,
        right: directions & DIR_RIGHT != 0,
        ..Input::default()
    }
}

/// The bits opposite to `directions`.
fn opposites(directions: u8) -> u8 {
    let mut opposite = 0;
    if directions & DIR_UP != 0 {
        opposite |= DIR_DOWN;
    }
    if directions & DIR_DOWN != 0 {
        opposite |= DIR_UP;
    }
    if directions & DIR_LEFT != 0 {
        opposite |= DIR_RIGHT;
    }
    if directions & DIR_RIGHT != 0 {
        opposite |= DIR_LEFT;
    }
    opposite
}

/// Merges a newly touched direction into the bits a pad finger already
/// holds: adjacent buttons OR together (a diagonal), while an opposite
/// direction replaces its counterpart so opposing flags never cancel out.
fn merge_directions(held: u8, touched: u8) -> u8 {
    if touched == 0 {
        return 0;
    }
    (held & !opposites(touched)) | touched
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    (dx * dx + dy * dy).sqrt()
}

fn contains(rect: [f32; 4], point: [f32; 2]) -> bool {
    point[0] >= rect[0]
        && point[0] <= rect[0] + rect[2]
        && point[1] >= rect[1]
        && point[1] <= rect[1] + rect[3]
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::TouchPhase;

    const W: f32 = 1080.0;
    const H: f32 = 1920.0;

    fn new_controls() -> TouchControls {
        let mut controls = TouchControls::new();
        controls.resize(W, H);
        controls
    }

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn rect_center(rect: [f32; 4]) -> [f32; 2] {
        [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0]
    }

    /// Float comparison for layout edges, tolerant of one-pixel rounding.
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// A normalized HUD rect in physical pixels.
    fn pixel(rect: [f32; 4]) -> [f32; 4] {
        [rect[0] * W, rect[1] * H, rect[2] * W, rect[3] * H]
    }

    /// Centre of HUD entry `index` in physical pixels.
    fn hud_center(controls: &TouchControls, index: usize) -> [f32; 2] {
        rect_center(pixel(controls.hud_rects()[index].0))
    }

    #[test]
    fn pad_directions_map_to_movement_and_clear_on_release() {
        let now = Instant::now();
        let cases: [(usize, fn(&Input) -> bool); 4] = [
            (0, |movement| movement.forward),
            (1, |movement| movement.left),
            (3, |movement| movement.right),
            (4, |movement| movement.back),
        ];
        for (index, held) in cases {
            let mut controls = new_controls();
            let point = hud_center(&controls, index);
            controls.touch_at(1, TouchPhase::Started, point, now);
            let frame = controls.frame();
            assert!(held(&frame.movement), "HUD index {index}");
            assert_eq!(frame.look, [0.0, 0.0]);
            assert!(!frame.jump);
            assert!(!frame.sneak);

            // Held movement keeps being reported until the finger lifts.
            assert!(held(&controls.frame().movement), "HUD index {index}");
            controls.touch_at(1, TouchPhase::Ended, point, at(now, 40));
            assert_eq!(controls.frame().movement, Input::default());
            assert!(!controls.frame().jump);
        }
    }

    #[test]
    fn several_direction_buttons_can_be_held_at_once() {
        let mut controls = new_controls();
        let now = Instant::now();
        let up = hud_center(&controls, 0);
        let left = hud_center(&controls, 1);

        controls.touch_at(1, TouchPhase::Started, up, now);
        controls.touch_at(2, TouchPhase::Started, left, at(now, 1));
        let movement = controls.frame().movement;
        assert!(movement.forward);
        assert!(movement.left);
        assert!(!movement.right);
        assert!(!movement.back);

        // Lifting one finger leaves the other direction held.
        controls.touch_at(1, TouchPhase::Ended, up, at(now, 30));
        let movement = controls.frame().movement;
        assert!(!movement.forward);
        assert!(movement.left);
        controls.touch_at(2, TouchPhase::Ended, left, at(now, 40));
        assert_eq!(controls.frame().movement, Input::default());
    }

    #[test]
    fn sliding_between_adjacent_buttons_ors_their_flags() {
        let mut controls = new_controls();
        let now = Instant::now();
        let up = hud_center(&controls, 0);
        let right = hud_center(&controls, 3);

        controls.touch_at(1, TouchPhase::Started, up, now);
        assert!(controls.frame().movement.forward);
        // Slide from up to right: the diagonal holds both directions.
        controls.touch_at(1, TouchPhase::Moved, right, at(now, 20));
        let movement = controls.frame().movement;
        assert!(movement.forward, "diagonal keeps the old direction");
        assert!(movement.right);
        assert!(!movement.left);
        assert!(!movement.back);

        // Sliding onto the centre cell keeps the held directions without
        // toggling sneak (only a press toggles it).
        let centre = hud_center(&controls, 2);
        controls.touch_at(1, TouchPhase::Moved, centre, at(now, 30));
        let held = controls.frame().movement;
        assert!(held.forward && held.right, "directions survive the slide");
        assert!(!controls.frame().sneak);
        controls.touch_at(1, TouchPhase::Ended, centre, at(now, 40));
        assert_eq!(controls.frame().movement, Input::default());

        // A touch exactly on the shared corner reports both buttons.
        let up_rect = pixel(controls.hud_rects()[0].0);
        let corner = [up_rect[0] + up_rect[2], up_rect[1] + up_rect[3]];
        controls.touch_at(2, TouchPhase::Started, corner, at(now, 100));
        let movement = controls.frame().movement;
        assert!(movement.forward);
        assert!(movement.right);
    }

    #[test]
    fn sneak_centre_toggles_and_is_sticky() {
        let mut controls = new_controls();
        let now = Instant::now();
        let point = hud_center(&controls, 2);

        controls.touch_at(1, TouchPhase::Started, point, now);
        let frame = controls.frame();
        assert!(frame.sneak);
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert_eq!(frame.look, [0.0, 0.0]);
        // Holding the cell toggles exactly once.
        assert!(controls.frame().sneak);
        controls.touch_at(1, TouchPhase::Ended, point, at(now, 80));
        assert!(controls.frame().sneak, "sticky after release");

        // A second finger on the cell toggles it once more: the toggle is
        // per press, not per finger, so two fingers flip it twice.
        controls.touch_at(2, TouchPhase::Started, point, at(now, 100));
        assert!(!controls.frame().sneak, "second finger flipped it off");
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 140));
        assert!(!controls.frame().sneak);
        // A third press turns it back on.
        controls.touch_at(3, TouchPhase::Started, point, at(now, 200));
        assert!(controls.frame().sneak);
        controls.touch_at(3, TouchPhase::Ended, point, at(now, 240));
        assert!(controls.frame().sneak);
        assert!(!controls.frame().jump);
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn sneak_cell_colour_marks_the_active_toggle() {
        let mut controls = new_controls();
        let now = Instant::now();
        let inactive = controls.hud_rects()[2].1;
        let point = hud_center(&controls, 2);

        controls.touch_at(1, TouchPhase::Started, point, now);
        controls.touch_at(1, TouchPhase::Ended, point, at(now, 40));
        let active = controls.hud_rects()[2].1;
        assert_ne!(active, inactive);
        assert_ne!(active, controls.hud_rects()[0].1, "distinct from the pad");

        controls.touch_at(2, TouchPhase::Started, point, at(now, 60));
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 100));
        assert_eq!(controls.hud_rects()[2].1, inactive);
    }

    #[test]
    fn jump_button_fires_once_on_press() {
        let mut controls = new_controls();
        let now = Instant::now();
        let point = hud_center(&controls, 5);

        controls.touch_at(1, TouchPhase::Started, point, now);
        let frame = controls.frame();
        assert!(frame.jump);
        assert!(!frame.place_block);
        assert!(!frame.attack);
        assert!(!frame.break_block);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert_eq!(frame.movement, Input::default());
        // One-shot even while the button stays held.
        assert!(!controls.frame().jump);

        let dragged = [point[0] - 60.0, point[1] - 80.0];
        controls.touch_at(1, TouchPhase::Moved, dragged, at(now, 40));
        assert_eq!(controls.frame().look, [0.0, 0.0]);
        controls.touch_at(1, TouchPhase::Ended, dragged, at(now, 80));
        let frame = controls.frame();
        assert!(!frame.jump);
        assert!(!frame.break_block);

        // A fresh press jumps again.
        controls.touch_at(2, TouchPhase::Started, point, at(now, 120));
        assert!(controls.frame().jump);
    }

    #[test]
    fn look_drag_reports_scaled_degrees_with_mouse_sign() {
        let mut controls = new_controls();
        let now = Instant::now();
        let start = [W * 0.80, H * 0.30];

        controls.touch_at(7, TouchPhase::Started, start, now);
        // Pressing without moving reports nothing yet.
        assert_eq!(controls.frame().look, [0.0, 0.0]);

        // Right 40 px, up 20 px.
        controls.touch_at(
            7,
            TouchPhase::Moved,
            [start[0] + 40.0, start[1] - 20.0],
            at(now, 16),
        );
        let frame = controls.frame();
        assert!((frame.look[0] - 6.0).abs() < 1e-4);
        assert!((frame.look[1] + 3.0).abs() < 1e-4);
        assert!(!frame.break_block);
        assert_eq!(frame.movement, Input::default());

        // The delta was drained by the first frame.
        assert_eq!(controls.frame().look, [0.0, 0.0]);
    }

    #[test]
    fn short_look_tap_breaks_exactly_once() {
        let mut controls = new_controls();
        let now = Instant::now();
        let point = [W * 0.72, H * 0.5];

        controls.touch_at(2, TouchPhase::Started, point, now);
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 120));
        let frame = controls.frame();
        assert!(frame.break_block);
        assert!(!frame.place_block);
        assert!(!frame.jump);
        assert!(!frame.sneak);
        assert_eq!(frame.look, [0.0, 0.0]);

        // One-shot: the next tick is clean.
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn long_look_press_and_drag_do_not_break() {
        let mut controls = new_controls();
        let now = Instant::now();
        let point = [W * 0.72, H * 0.5];

        // Held longer than the tap window.
        controls.touch_at(2, TouchPhase::Started, point, now);
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 400));
        assert!(!controls.frame().break_block);

        // Released quickly but dragged farther than the slop.
        controls.touch_at(3, TouchPhase::Started, point, at(now, 500));
        let dragged = [point[0] + 20.0, point[1]];
        controls.touch_at(3, TouchPhase::Moved, dragged, at(now, 510));
        controls.touch_at(3, TouchPhase::Ended, dragged, at(now, 520));
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn place_button_presses_without_starting_look() {
        let mut controls = new_controls();
        let now = Instant::now();
        let center = hud_center(&controls, 6);

        controls.touch_at(3, TouchPhase::Started, center, now);
        let frame = controls.frame();
        assert!(frame.place_block);
        assert!(!frame.break_block);
        assert!(!frame.jump);
        assert_eq!(frame.look, [0.0, 0.0]);

        // Dragging off the button still belongs to the button, not the look
        // area, and never breaks.
        let dragged = [center[0] - 80.0, center[1] - 120.0];
        controls.touch_at(3, TouchPhase::Moved, dragged, at(now, 50));
        assert_eq!(controls.frame().look, [0.0, 0.0]);
        controls.touch_at(3, TouchPhase::Ended, dragged, at(now, 90));
        assert!(!controls.frame().break_block);
        assert!(!controls.frame().place_block);
    }

    #[test]
    fn attack_button_triggers_attack_once_without_other_actions() {
        let mut controls = new_controls();
        let now = Instant::now();
        let center = hud_center(&controls, 7);

        controls.touch_at(5, TouchPhase::Started, center, now);
        let frame = controls.frame();
        assert!(frame.attack);
        assert!(!frame.break_block);
        assert!(!frame.place_block);
        assert!(!frame.jump);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert_eq!(frame.select, None);
        // One-shot: the next tick is clean even while the finger is held.
        assert!(!controls.frame().attack);

        // Dragging off the button never turns into look or break.
        let dragged = [center[0] - 90.0, center[1] - 140.0];
        controls.touch_at(5, TouchPhase::Moved, dragged, at(now, 40));
        assert_eq!(controls.frame().look, [0.0, 0.0]);
        controls.touch_at(5, TouchPhase::Ended, dragged, at(now, 90));
        let frame = controls.frame();
        assert!(!frame.attack);
        assert!(!frame.break_block);
        assert!(!frame.place_block);

        // A second press after the release fires again.
        controls.touch_at(6, TouchPhase::Started, center, at(now, 200));
        assert!(controls.frame().attack);
        controls.touch_at(6, TouchPhase::Ended, center, at(now, 240));
        let frame = controls.frame();
        assert!(!frame.attack);
        assert!(!frame.break_block);
    }

    #[test]
    fn attack_and_place_buttons_are_independent() {
        let mut controls = new_controls();
        let now = Instant::now();
        let place_center = hud_center(&controls, 6);
        let attack_center = hud_center(&controls, 7);

        // Both buttons held at once: both one-shots fire.
        controls.touch_at(7, TouchPhase::Started, attack_center, now);
        controls.touch_at(8, TouchPhase::Started, place_center, at(now, 1));
        let frame = controls.frame();
        assert!(frame.attack);
        assert!(frame.place_block);
        assert!(!frame.break_block);

        // A second finger on an already-held button is ignored.
        controls.touch_at(9, TouchPhase::Started, attack_center, at(now, 2));
        let frame = controls.frame();
        assert!(!frame.attack, "one attack per press");
        assert!(!frame.place_block);
    }

    #[test]
    fn multitouch_direction_look_and_jump_are_isolated() {
        let mut controls = new_controls();
        let now = Instant::now();
        let up = hud_center(&controls, 0);
        let jump = hud_center(&controls, 5);
        let look_start = [W * 0.80, H * 0.30];

        // Finger 0 holds forward, finger 1 looks around, finger 2 jumps.
        controls.touch_at(0, TouchPhase::Started, up, now);
        controls.touch_at(1, TouchPhase::Started, look_start, at(now, 1));
        controls.touch_at(
            1,
            TouchPhase::Moved,
            [look_start[0] + 40.0, look_start[1] - 20.0],
            at(now, 2),
        );
        controls.touch_at(2, TouchPhase::Started, jump, at(now, 3));

        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert!(!frame.movement.right);
        assert!((frame.look[0] - 6.0).abs() < 1e-4);
        assert!((frame.look[1] + 3.0).abs() < 1e-4);
        assert!(frame.jump);
        assert!(!frame.place_block);
        assert!(!frame.attack);
        assert!(!frame.break_block);
        assert!(!frame.sneak);

        // Releasing the look and jump fingers leaves the held direction.
        controls.touch_at(
            1,
            TouchPhase::Ended,
            [look_start[0] + 40.0, look_start[1] - 20.0],
            at(now, 40),
        );
        controls.touch_at(2, TouchPhase::Ended, jump, at(now, 50));
        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert!(!frame.jump);
        assert!(!frame.break_block);

        controls.touch_at(0, TouchPhase::Ended, up, at(now, 60));
        assert_eq!(controls.frame().movement, Input::default());
    }

    #[test]
    fn no_cross_zone_actions() {
        let mut controls = new_controls();
        let now = Instant::now();

        // A look drag never moves, jumps, places, attacks or sneaks.
        let look = [W * 0.80, H * 0.30];
        controls.touch_at(1, TouchPhase::Started, look, now);
        controls.touch_at(1, TouchPhase::Moved, [look[0] + 20.0, look[1]], at(now, 10));
        let frame = controls.frame();
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert!(!frame.place_block);
        assert!(!frame.attack);
        assert!(!frame.sneak);
        // Dragged past the tap slop: releasing does not break.
        controls.touch_at(1, TouchPhase::Ended, [look[0] + 20.0, look[1]], at(now, 20));
        assert!(!controls.frame().break_block);

        // A held direction never looks or presses anything.
        let up = hud_center(&controls, 0);
        controls.touch_at(2, TouchPhase::Started, up, at(now, 100));
        controls.touch_at(2, TouchPhase::Moved, [up[0] + 30.0, up[1]], at(now, 120));
        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert!(!frame.place_block);
        assert!(!frame.attack);
        assert!(!frame.jump);
        controls.touch_at(2, TouchPhase::Ended, [up[0] + 30.0, up[1]], at(now, 160));
        assert!(!controls.frame().break_block);

        // Dead space inside the pad footprint is inert, even when dragged
        // into the look zone.
        let origin = controls.pad_origin();
        let dead = [
            origin[0] + controls.pad_cell() * 0.5,
            origin[1] + controls.pad_cell() * 0.5,
        ];
        controls.touch_at(3, TouchPhase::Started, dead, at(now, 200));
        controls.touch_at(3, TouchPhase::Moved, look, at(now, 220));
        let frame = controls.frame();
        assert_eq!(frame.movement, Input::default());
        assert_eq!(frame.look, [0.0, 0.0]);
        controls.touch_at(3, TouchPhase::Ended, look, at(now, 240));
        assert!(!controls.frame().break_block);

        // A jump press never becomes a look drag or a block action.
        let jump = hud_center(&controls, 5);
        controls.touch_at(4, TouchPhase::Started, jump, at(now, 300));
        controls.touch_at(
            4,
            TouchPhase::Moved,
            [jump[0] - 40.0, jump[1] - 40.0],
            at(now, 310),
        );
        let frame = controls.frame();
        assert!(frame.jump);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert!(!frame.place_block);
        assert!(!frame.attack);
        assert!(!frame.sneak);
    }

    #[test]
    fn cancel_releases_without_actions() {
        let mut controls = new_controls();
        let now = Instant::now();
        let up = hud_center(&controls, 0);

        controls.touch_at(8, TouchPhase::Started, up, now);
        assert!(controls.frame().movement.forward);
        controls.touch_at(8, TouchPhase::Cancelled, up, at(now, 50));
        let frame = controls.frame();
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert!(!frame.place_block);

        // A cancelled short look press never breaks.
        let point = [W * 0.72, H * 0.5];
        controls.touch_at(9, TouchPhase::Started, point, at(now, 100));
        controls.touch_at(9, TouchPhase::Cancelled, point, at(now, 140));
        assert!(!controls.frame().break_block);

        // A cancelled jump press still acts on press, but releasing it never
        // fires a second action.
        let jump = hud_center(&controls, 5);
        controls.touch_at(10, TouchPhase::Started, jump, at(now, 200));
        assert!(controls.frame().jump);
        controls.touch_at(10, TouchPhase::Cancelled, jump, at(now, 240));
        assert!(!controls.frame().jump);
    }

    #[test]
    fn hud_rects_order_and_bounds() {
        let controls = new_controls();
        let rects = controls.hud_rects();
        assert_eq!(rects.len(), 9);
        assert_eq!(rects, controls.hud_rects());

        for (rect, color) in &rects {
            for value in rect.iter().chain(color.iter()) {
                assert!(value.is_finite(), "{value}");
                assert!((0.0..=1.0).contains(value), "{value}");
            }
            assert!(rect[2] > 0.0);
            assert!(rect[3] > 0.0);
            // The whole quad stays inside the screen, not just its origin.
            assert!(rect[0] + rect[2] <= 1.0 + 1e-4, "{rect:?}");
            assert!(rect[1] + rect[3] <= 1.0 + 1e-4, "{rect:?}");
        }

        let px: Vec<[f32; 4]> = rects.iter().map(|(rect, _)| pixel(*rect)).collect();
        let min_side = W.min(H);

        // The first five entries are the D-pad cross, up/left/centre/right/down.
        let cell = min_side * PAD_CELL;
        for index in 0..5 {
            assert!(
                near(px[index][2], cell),
                "cell {index} side {:?}",
                px[index]
            );
            assert!(near(px[index][2], px[index][3]));
        }
        assert!(near(px[0][0], px[2][0]), "up above the centre");
        assert!(near(px[0][1] + px[0][3], px[2][1]));
        assert!(near(px[2][1] + px[2][3], px[4][1]), "down below the centre");
        assert!(near(px[1][0] + px[1][2], px[2][0]), "left of the centre");
        assert!(near(px[2][0] + px[2][2], px[3][0]), "right of the centre");
        assert!(near(px[1][1], px[2][1]));
        assert!(near(px[3][1], px[2][1]));
        assert!(px[0][1] > H * 0.5 && px[0][0] < W * 0.5, "bottom-left pad");

        // A big jump button in the bottom-right corner.
        assert!(near(px[5][2], min_side * JUMP_SIZE));
        assert!(near(px[5][3], min_side * JUMP_SIZE));
        assert!(px[5][0] > W * 0.5 && px[5][1] > H * 0.5);
        assert!(near(px[5][0] + px[5][2], W - min_side * ACTION_MARGIN));
        assert!(near(px[5][1] + px[5][3], H - min_side * ACTION_MARGIN));

        // Place and attack share a row above the jump button, inside the look
        // zone.
        assert!(px[6][1] + px[6][3] < px[5][1], "place above jump");
        assert_eq!(px[6][1], px[7][1], "buttons share a row");
        assert_eq!([px[6][2], px[6][3]], [px[7][2], px[7][3]]);
        assert!(px[7][0] + px[7][2] < px[6][0], "attack left of place");
        assert!(px[6][0] > W * PAD_ZONE && px[7][0] > W * PAD_ZONE);

        // The look hint covers the top of the right half.
        assert_eq!(
            rects[8].0,
            [PAD_ZONE, 0.0, 1.0 - PAD_ZONE, BREAK_HINT_HEIGHT]
        );
    }

    #[test]
    fn hud_rects_match_hit_testing() {
        let now = Instant::now();
        let directions: [(usize, fn(&Input) -> bool); 4] = [
            (0, |movement| movement.forward),
            (1, |movement| movement.left),
            (3, |movement| movement.right),
            (4, |movement| movement.back),
        ];
        for (index, held) in directions {
            let mut controls = new_controls();
            let point = hud_center(&controls, index);
            controls.touch_at(1, TouchPhase::Started, point, now);
            let frame = controls.frame();
            assert!(held(&frame.movement), "HUD index {index}");
            assert!(!frame.sneak);
            assert_eq!(frame.look, [0.0, 0.0]);
        }

        // Centre: sneak toggle.
        let mut controls = new_controls();
        let point = hud_center(&controls, 2);
        controls.touch_at(2, TouchPhase::Started, point, now);
        assert!(controls.frame().sneak);
        assert_eq!(controls.frame().movement, Input::default());
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 40));
        assert!(controls.frame().sneak);

        // Jump.
        let mut controls = new_controls();
        let point = hud_center(&controls, 5);
        controls.touch_at(3, TouchPhase::Started, point, now);
        assert!(controls.frame().jump);

        // Place.
        let mut controls = new_controls();
        let point = hud_center(&controls, 6);
        controls.touch_at(4, TouchPhase::Started, point, now);
        assert!(controls.frame().place_block);

        // Attack.
        let mut controls = new_controls();
        let point = hud_center(&controls, 7);
        controls.touch_at(5, TouchPhase::Started, point, now);
        assert!(controls.frame().attack);

        // The hint is part of the look zone: a short tap breaks.
        let mut controls = new_controls();
        let point = hud_center(&controls, 8);
        controls.touch_at(6, TouchPhase::Started, point, now);
        assert_eq!(controls.frame().look, [0.0, 0.0]);
        controls.touch_at(6, TouchPhase::Ended, point, at(now, 60));
        assert!(controls.frame().break_block);
    }

    /// Whether two pixel rectangles share any area.
    fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    }

    #[test]
    fn lower_controls_do_not_overlap() {
        let controls = new_controls();
        let rects: Vec<[f32; 4]> = controls
            .hud_rects()
            .iter()
            .take(8)
            .map(|(rect, _)| pixel(*rect))
            .collect();

        for (index, rect) in rects.iter().enumerate() {
            assert!(rect[0] >= 0.0, "control {index}");
            assert!(rect[1] >= 0.0, "control {index}");
            assert!(rect[0] + rect[2] <= W + 1e-3, "control {index}");
            assert!(rect[1] + rect[3] <= H + 1e-3, "control {index}");
            for (other_index, other) in rects.iter().enumerate().skip(index + 1) {
                assert!(
                    !overlaps(*rect, *other),
                    "controls {index} and {other_index} overlap"
                );
            }
            for slot in 0..HOTBAR_SLOTS {
                assert!(
                    !overlaps(*rect, controls.hotbar_slot_rect(slot)),
                    "control {index} overlaps hotbar slot {slot}"
                );
            }
        }
    }

    #[test]
    fn resize_rebuilds_the_layout() {
        let mut controls = new_controls();
        let portrait = controls.hud_rects();
        controls.resize(1920.0, 1080.0);
        let landscape = controls.hud_rects();
        assert_ne!(portrait, landscape);
        for (rect, _) in &landscape {
            for value in rect {
                assert!((0.0..=1.0).contains(value), "{value}");
            }
        }
        // The jump button stays in the bottom-right corner after rotation.
        assert!(landscape[5].0[0] > 0.5 && landscape[5].0[1] > 0.5);
        assert!(landscape[0].0[0] < 0.5);
        assert!(landscape[0].0[1] + landscape[0].0[3] > 0.5);
    }

    #[test]
    fn public_touch_path_routes_to_the_new_controls() {
        let mut controls = new_controls();
        let point = hud_center(&controls, 6);
        // The public entry point stamps the event with the wall clock; the
        // place press is immediate, so this needs no timing window.
        controls.touch(42, TouchPhase::Started, point[0], point[1]);
        assert!(controls.frame().place_block);
        controls.touch(42, TouchPhase::Ended, point[0], point[1]);
        assert!(!controls.frame().break_block);
    }

    /// Centre of hotbar `slot` in physical pixels.
    fn slot_center(controls: &TouchControls, slot: usize) -> [f32; 2] {
        let rect = controls.hotbar_slot_rect(slot);
        [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0]
    }

    #[test]
    fn hotbar_tap_selects_without_any_other_action() {
        let mut controls = new_controls();
        let now = Instant::now();
        let point = slot_center(&controls, 0);
        // Slot 0 sits inside the pad zone; the hotbar must win there.
        assert!(point[0] < W * PAD_ZONE);

        controls.touch_at(11, TouchPhase::Started, point, now);
        let frame = controls.frame();
        assert_eq!(frame.select, Some(0));
        assert_eq!(controls.selected(), 0);
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert!(!frame.place_block);
        assert!(!frame.sneak);
        assert_eq!(frame.look, [0.0, 0.0]);

        // Releasing is inert: no jump, no break.
        controls.touch_at(11, TouchPhase::Ended, point, at(now, 80));
        let frame = controls.frame();
        assert_eq!(frame.select, None);
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert!(!frame.place_block);

        // The rightmost slot lives in the look zone and behaves the same.
        let point = slot_center(&controls, 8);
        assert!(point[0] > W * PAD_ZONE);
        controls.touch_at(12, TouchPhase::Started, point, at(now, 200));
        assert_eq!(controls.frame().select, Some(8));
        controls.touch_at(12, TouchPhase::Ended, point, at(now, 260));
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn hotbar_drag_never_looks_or_breaks() {
        let mut controls = new_controls();
        let now = Instant::now();
        let start = slot_center(&controls, 4);
        assert!(start[0] > W * PAD_ZONE);

        controls.touch_at(21, TouchPhase::Started, start, now);
        let dragged = [start[0] + 40.0, start[1] - 30.0];
        controls.touch_at(21, TouchPhase::Moved, dragged, at(now, 16));
        let frame = controls.frame();
        assert_eq!(frame.look, [0.0, 0.0]);
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.break_block);

        controls.touch_at(21, TouchPhase::Ended, dragged, at(now, 30));
        assert!(!controls.frame().break_block);

        // The same spot is a look tap again once no hotbar finger owns it.
        let look = [W * 0.72, H * 0.5];
        controls.touch_at(22, TouchPhase::Started, look, at(now, 100));
        controls.touch_at(22, TouchPhase::Ended, look, at(now, 180));
        assert!(controls.frame().break_block);
    }

    #[test]
    fn hotbar_works_alongside_a_held_direction() {
        let mut controls = new_controls();
        let now = Instant::now();
        let up = hud_center(&controls, 0);

        controls.touch_at(30, TouchPhase::Started, up, now);
        assert!(controls.frame().movement.forward);

        let point = slot_center(&controls, 3);
        controls.touch_at(31, TouchPhase::Started, point, at(now, 5));
        controls.touch_at(31, TouchPhase::Ended, point, at(now, 40));
        let frame = controls.frame();
        assert_eq!(frame.select, Some(3));
        // The direction keeps driving and the tap never jumps.
        assert!(frame.movement.forward);
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert_eq!(frame.look, [0.0, 0.0]);
    }

    #[test]
    fn hotbar_rects_are_centred_above_the_lower_new_controls() {
        let controls = new_controls();
        let rects = controls.hotbar_rects();
        assert_eq!(rects.iter().filter(|(_, selected)| *selected).count(), 1);
        assert!(rects[0].1, "slot 0 is the default selection");

        let first = rects[0].0;
        for (slot, (rect, _)) in rects.iter().enumerate() {
            assert_eq!(rect[1], first[1], "slot {slot} shares the row");
            assert_eq!(rect[2], first[2], "slot {slot} shares the size");
            assert_eq!(rect[3], first[3], "slot {slot} shares the size");
            if slot > 0 {
                assert!(rects[slot - 1].0[0] < rect[0], "slot {slot} moves right");
            }
            for value in rect {
                assert!(value.is_finite(), "{value}");
                assert!((0.0..=1.0).contains(value), "{value}");
            }
            assert!(rect[0] + rect[2] <= 1.0 + 1e-4);
            assert!(rect[1] + rect[3] <= 1.0 + 1e-4);
        }
        let last = rects[HOTBAR_SLOTS - 1].0;
        assert!(
            (first[0] - (1.0 - last[0] - last[2])).abs() < 1e-4,
            "the bar is centred"
        );
        assert!(first[1] + first[3] < 0.9, "above the bottom of the screen");

        // In pixels the bar clears the D-pad and the right-hand buttons.
        let rect = controls.hotbar_slot_rect(4);
        let pad_top = pixel(controls.hud_rects()[0].0)[1];
        let place_top = pixel(controls.hud_rects()[6].0)[1];
        let jump_top = pixel(controls.hud_rects()[5].0)[1];
        assert!(rect[1] + rect[3] < pad_top);
        assert!(rect[1] + rect[3] < place_top);
        assert!(rect[1] + rect[3] < jump_top);
    }

    #[test]
    fn hotbar_selection_highlight_follows_taps_and_syncs() {
        let mut controls = new_controls();
        let now = Instant::now();
        assert!(controls.hotbar_rects()[0].1);

        let point = slot_center(&controls, 6);
        controls.touch_at(40, TouchPhase::Started, point, now);
        controls.touch_at(40, TouchPhase::Ended, point, at(now, 60));
        assert_eq!(controls.selected(), 6);
        assert_eq!(controls.frame().select, Some(6));
        let rects = controls.hotbar_rects();
        for (slot, (_, selected)) in rects.iter().enumerate() {
            assert_eq!(*selected, slot == 6, "slot {slot}");
        }

        // Mirroring an external (keyboard) selection only moves the highlight
        // and never queues another selection for the server.
        controls.set_selected(8);
        assert_eq!(controls.selected(), 8);
        assert!(controls.hotbar_rects()[8].1);
        assert!(!controls.hotbar_rects()[6].1);
        assert_eq!(controls.frame().select, None);

        // Out-of-range slots are ignored.
        controls.set_selected(HOTBAR_SLOTS);
        assert_eq!(controls.selected(), 8);
    }
}
