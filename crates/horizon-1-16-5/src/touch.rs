//! On-screen touch controls for the 1.16.5 Android target.
//!
//! The phone layout is deliberately simple and fixed so that every gesture is
//! deterministic and testable without an Android device:
//!
//! - the left 45% of the screen is a virtual joystick whose base sits in the
//!   bottom-left corner. Direction and magnitude (relative to the smaller
//!   screen side) drive [`Input::forward`], [`Input::back`], [`Input::left`]
//!   and [`Input::right`]; a short tap with no drag jumps;
//! - the rest of the screen is the look zone: dragging reports degree deltas
//!   in the desktop mouse convention (`yaw -= look[0]`, `pitch += look[1]`),
//!   and a short tap breaks the targeted block;
//! - a square place button in the bottom-right corner places the held block on
//!   press and never starts a look drag;
//! - a nine-slot hotbar sits centred just above the joystick and place button.
//!   Tapping a slot selects it; because the hotbar is hit-tested first, such a
//!   touch never starts a look drag, breaks a block or jumps.
//!
//! [`TouchControls::frame`] drains the one-shot actions and the accumulated
//! look delta for one tick while held movement keeps being reported, so it can
//! be called unconditionally every frame. [`TouchControls::hud_rects`] and
//! [`TouchControls::hotbar_rects`] expose the same layout as normalized
//! rectangles for the HUD renderer.
//!
//! Coordinates are physical pixels from winit with a top-left origin.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::player::Input;

/// Fraction of the width that belongs to the movement joystick.
const STICK_ZONE: f32 = 0.45;
/// Joystick radius as a fraction of the smaller screen side.
const STICK_RADIUS: f32 = 0.12;
/// Knob radius as a fraction of the smaller screen side (about half the base).
const KNOB_RADIUS: f32 = 0.055;
/// Distance between the joystick and the screen edges. Wide enough that the
/// knob stays on screen when the stick is pushed fully into the corner.
const STICK_MARGIN: f32 = 0.06;
/// Deadzone as a fraction of the joystick radius: smaller offsets read neutral.
const STICK_DEADZONE: f32 = 0.25;
/// Side of the square place button as a fraction of the smaller screen side.
const PLACE_SIZE: f32 = 0.18;
/// Distance between the place button and the bottom-right corner.
const PLACE_MARGIN: f32 = 0.03;
/// Height of the faint break-zone hint at the top of the look zone.
const BREAK_HINT_HEIGHT: f32 = 0.07;
/// Number of hotbar slots.
pub const HOTBAR_SLOTS: usize = 9;
/// Side of one hotbar slot as a fraction of the smaller screen side.
const HOTBAR_SLOT_SIZE: f32 = 0.085;
/// Gap between two hotbar slots, as a fraction of the smaller screen side.
const HOTBAR_GAP: f32 = 0.007;
/// Distance from the bottom edge to the hotbar, as a fraction of the smaller
/// screen side. Larger than the joystick's `STICK_MARGIN + 2 * STICK_RADIUS`
/// so the bar always clears the joystick and the place button.
const HOTBAR_BOTTOM: f32 = 0.335;

/// Longest press that still counts as a tap instead of a hold.
const TAP_TIME: Duration = Duration::from_millis(250);
/// Largest finger travel, in pixels, that still counts as a tap.
const TAP_SLOP: f32 = 10.0;
/// Look sensitivity in degrees per physical pixel.
const LOOK_DEGREES_PER_PIXEL: f32 = 0.15;

/// `sin(22.5deg)`: a normalized direction component at least this large
/// selects that axis, giving eight evenly sized movement sectors.
const SECTOR: f32 = 0.382_683_43;

// Subtle translucent white controls, drawn over the world.
const STICK_BASE_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
const STICK_KNOB_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.30];
const PLACE_BUTTON_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.22];
const BREAK_HINT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.05];

/// One tick of touch input.
///
/// `movement` is the held joystick state and is therefore still set on the
/// next call; everything else is a one-shot (or, for `look`, an accumulated
/// delta) that [`TouchControls::frame`] drains.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TouchFrame {
    /// Held movement direction from the virtual joystick.
    pub movement: Input,
    /// Look delta in degrees, desktop mouse convention: the caller applies
    /// `yaw -= look[0]` and `pitch += look[1]`.
    pub look: [f32; 2],
    /// Set once for a short look-zone tap.
    pub break_block: bool,
    /// Set once when the place button is pressed.
    pub place_block: bool,
    /// Set once for a short joystick-zone tap.
    pub jump: bool,
    /// Hotbar slot selected by a touch on the hotbar, if any.
    pub select: Option<usize>,
}

/// Which gesture a finger owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Stick,
    Look,
    Place,
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

/// Virtual joystick, look area, place button and hotbar for one touch screen.
#[derive(Clone, Debug, Default)]
pub struct TouchControls {
    width: f32,
    height: f32,
    /// Finger id currently driving the joystick, if any.
    stick: Option<u64>,
    /// Normalized joystick offset: `[-1, 1]` on both axes, length at most 1,
    /// `+y` pointing down the screen.
    stick_axis: [f32; 2],
    fingers: BTreeMap<u64, Finger>,
    look: [f32; 2],
    jump: bool,
    break_block: bool,
    place_block: bool,
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
    /// A finger already holding the joystick keeps its role; its direction is
    /// recomputed against the new base so a resize never strands a held stick.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.width = width.max(0.0);
        self.height = height.max(0.0);
        if let Some(id) = self.stick {
            if let Some(finger) = self.fingers.get(&id) {
                let last = finger.last;
                self.stick_axis = self.axis_at(last);
            }
        }
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
    /// Held movement is not cleared: it is derived from the still-touching
    /// fingers. Safe to call every tick, whether or not anything happened.
    pub fn frame(&mut self) -> TouchFrame {
        let frame = TouchFrame {
            movement: movement_from_axis(self.stick_axis),
            look: self.look,
            break_block: self.break_block,
            place_block: self.place_block,
            jump: self.jump,
            select: self.select.take(),
        };
        self.look = [0.0; 2];
        self.break_block = false;
        self.place_block = false;
        self.jump = false;
        frame
    }

    /// The hotbar slot the touch HUD highlights.
    pub fn selected(&self) -> usize {
        self.selected
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
    /// above the joystick and place button.
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
    /// The order is fixed: joystick base, joystick knob, place button and the
    /// faint break-zone hint at the top of the look area.
    pub fn hud_rects(&self) -> Vec<([f32; 4], [f32; 4])> {
        let min_side = self.min_side();
        let radius = min_side * STICK_RADIUS;
        let center = self.stick_center();
        let knob_center = [
            center[0] + self.stick_axis[0] * radius,
            center[1] + self.stick_axis[1] * radius,
        ];
        let knob_radius = min_side * KNOB_RADIUS;
        let place = self.place_rect();
        vec![
            (
                self.normalize(
                    center[0] - radius,
                    center[1] - radius,
                    radius * 2.0,
                    radius * 2.0,
                ),
                STICK_BASE_COLOR,
            ),
            (
                self.normalize(
                    knob_center[0] - knob_radius,
                    knob_center[1] - knob_radius,
                    knob_radius * 2.0,
                    knob_radius * 2.0,
                ),
                STICK_KNOB_COLOR,
            ),
            (
                self.normalize(place[0], place[1], place[2], place[3]),
                PLACE_BUTTON_COLOR,
            ),
            (
                // Already a screen fraction: keep the exact normalized edges
                // so the hint always abuts the right edge of the screen.
                [STICK_ZONE, 0.0, 1.0 - STICK_ZONE, BREAK_HINT_HEIGHT],
                BREAK_HINT_COLOR,
            ),
        ]
    }

    /// Position of the joystick base in pixels.
    fn stick_center(&self) -> [f32; 2] {
        let radius = self.stick_radius();
        let margin = self.min_side() * STICK_MARGIN;
        [margin + radius, self.height - margin - radius]
    }

    fn stick_radius(&self) -> f32 {
        self.min_side() * STICK_RADIUS
    }

    fn min_side(&self) -> f32 {
        self.width.min(self.height)
    }

    /// `(x, y, width, height)` of the place button in pixels.
    fn place_rect(&self) -> [f32; 4] {
        let side = self.min_side() * PLACE_SIZE;
        let margin = self.min_side() * PLACE_MARGIN;
        [
            self.width - margin - side,
            self.height - margin - side,
            side,
            side,
        ]
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

    /// Normalized joystick offset for a finger position.
    fn axis_at(&self, point: [f32; 2]) -> [f32; 2] {
        let radius = self.stick_radius();
        if radius <= 0.0 {
            return [0.0; 2];
        }
        let center = self.stick_center();
        let mut axis = [
            (point[0] - center[0]) / radius,
            (point[1] - center[1]) / radius,
        ];
        let length = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
        if length > 1.0 {
            axis[0] /= length;
            axis[1] /= length;
        }
        axis
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

        // The hotbar is checked first: a slot tap must never become a look
        // drag, a block break or a jump, even though the bar overlays the
        // tail of the look zone. Selection takes effect immediately and is
        // queued for the next tick.
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

        // The button is checked next so a touch on it can never become a
        // look drag, even though it sits inside the look zone.
        if contains(self.place_rect(), point) {
            if self.has_role(Role::Place) {
                return;
            }
            self.place_block = true;
            self.fingers.insert(id, finger);
            return;
        }

        if point[0] < self.width * STICK_ZONE {
            if self.stick.is_some() {
                return;
            }
            self.stick = Some(id);
            self.stick_axis = self.axis_at(point);
            self.fingers.insert(
                id,
                Finger {
                    role: Role::Stick,
                    ..finger
                },
            );
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
        if let Some(tracked) = self.fingers.get_mut(&id) {
            tracked.last = point;
            tracked.travel = tracked.travel.max(distance(finger.start, point));
        }

        match finger.role {
            Role::Stick => self.stick_axis = self.axis_at(point),
            Role::Look => {
                // Desktop mouse convention: pixels add degrees directly.
                self.look[0] += delta[0] * LOOK_DEGREES_PER_PIXEL;
                self.look[1] += delta[1] * LOOK_DEGREES_PER_PIXEL;
            }
            Role::Place => {}
            // A hotbar finger owns the slot; dragging must not look or act.
            Role::Hotbar => {}
        }
    }

    /// Drops a finger. `ended` distinguishes a real release (which may tap)
    /// from a cancelled gesture (which never acts).
    fn release(&mut self, id: u64, point: [f32; 2], now: Instant, ended: bool) {
        let Some(finger) = self.fingers.remove(&id) else {
            return;
        };
        if finger.role == Role::Stick && self.stick == Some(id) {
            self.stick = None;
            self.stick_axis = [0.0; 2];
        }
        if !ended {
            return;
        }
        let travel = finger.travel.max(distance(finger.start, point));
        if now.saturating_duration_since(finger.started) >= TAP_TIME || travel >= TAP_SLOP {
            return;
        }
        match finger.role {
            Role::Stick => self.jump = true,
            Role::Look => self.break_block = true,
            Role::Place => {}
            // Selection already happened on press; releasing never acts.
            Role::Hotbar => {}
        }
    }
}

/// Maps a normalized joystick offset to the four movement flags.
///
/// The offset is split into eight 45 degree sectors so a diagonal reads as
/// two directions instead of silently falling between axis thresholds.
fn movement_from_axis(axis: [f32; 2]) -> Input {
    let magnitude = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
    if magnitude < STICK_DEADZONE {
        return Input::default();
    }
    let x = axis[0] / magnitude;
    // Screen space has +y pointing down, so up the screen is -y.
    let y = -axis[1] / magnitude;
    Input {
        forward: y >= SECTOR,
        back: -y >= SECTOR,
        left: -x >= SECTOR,
        right: x >= SECTOR,
        ..Input::default()
    }
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

    fn controls() -> TouchControls {
        let mut controls = TouchControls::new();
        controls.resize(W, H);
        controls
    }

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn joystick_sets_movement_and_clears_on_release() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();
        let radius = controls.stick_radius();

        // Push 80% of the radius toward the top of the screen.
        let push = [center[0], center[1] - radius * 0.8];
        controls.touch_at(1, TouchPhase::Started, push, now);
        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert!(!frame.movement.back);
        assert!(!frame.movement.left);
        assert!(!frame.movement.right);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert!(!frame.jump);

        // Held movement is still reported on the next tick.
        assert!(controls.frame().movement.forward);

        // A real drag (> slop) then release moves and does not jump.
        let dragged = [push[0] + 30.0, push[1]];
        controls.touch_at(1, TouchPhase::Moved, dragged, at(now, 20));
        controls.touch_at(1, TouchPhase::Ended, dragged, at(now, 40));
        let frame = controls.frame();
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
    }

    #[test]
    fn joystick_deadzone_and_sectors() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();
        let radius = controls.stick_radius();

        // Inside the deadzone: neutral.
        controls.touch_at(
            1,
            TouchPhase::Started,
            [center[0] + radius * 0.2, center[1]],
            now,
        );
        assert_eq!(controls.frame().movement, Input::default());

        // Straight up from the deadzone edge: only forward.
        controls.touch_at(
            1,
            TouchPhase::Moved,
            [center[0], center[1] - radius * 0.5],
            at(now, 1),
        );
        let movement = controls.frame().movement;
        assert!(movement.forward);
        assert!(!movement.back);
        assert!(!movement.left);
        assert!(!movement.right);

        // Top-right diagonal: forward and right.
        controls.touch_at(
            1,
            TouchPhase::Moved,
            [center[0] + radius * 0.5, center[1] - radius * 0.5],
            at(now, 2),
        );
        let movement = controls.frame().movement;
        assert!(movement.forward);
        assert!(movement.right);
        assert!(!movement.left);
        assert!(!movement.back);
    }

    #[test]
    fn look_drag_reports_scaled_degrees_with_mouse_sign() {
        let mut controls = controls();
        let now = Instant::now();
        let start = [W * 0.72, H * 0.4];

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

        // The delta was drained by the first frame.
        assert_eq!(controls.frame().look, [0.0, 0.0]);
    }

    #[test]
    fn short_look_tap_breaks_exactly_once() {
        let mut controls = controls();
        let now = Instant::now();
        let point = [W * 0.72, H * 0.5];

        controls.touch_at(2, TouchPhase::Started, point, now);
        controls.touch_at(2, TouchPhase::Ended, point, at(now, 120));
        let frame = controls.frame();
        assert!(frame.break_block);
        assert!(!frame.place_block);
        assert!(!frame.jump);
        assert_eq!(frame.look, [0.0, 0.0]);

        // One-shot: the next tick is clean.
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn long_look_press_and_drag_do_not_break() {
        let mut controls = controls();
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
        let mut controls = controls();
        let now = Instant::now();
        let rect = controls.place_rect();
        let center = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];

        controls.touch_at(3, TouchPhase::Started, center, now);
        let frame = controls.frame();
        assert!(frame.place_block);
        assert!(!frame.break_block);
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
    fn short_stick_tap_jumps_once() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();

        controls.touch_at(4, TouchPhase::Started, center, now);
        assert_eq!(controls.frame().movement, Input::default());
        controls.touch_at(4, TouchPhase::Ended, center, at(now, 100));
        assert!(controls.frame().jump);
        assert!(!controls.frame().jump);
    }

    #[test]
    fn multitouch_isolates_the_zones() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();
        let radius = controls.stick_radius();
        let look_start = [W * 0.75, H * 0.5];

        // Finger 0 drives the joystick, finger 1 looks around.
        controls.touch_at(
            0,
            TouchPhase::Started,
            [center[0], center[1] - radius * 0.7],
            now,
        );
        controls.touch_at(1, TouchPhase::Started, look_start, at(now, 1));
        controls.touch_at(
            1,
            TouchPhase::Moved,
            [look_start[0] + 30.0, look_start[1]],
            at(now, 2),
        );

        // Extra fingers in the occupied look and stick zones are ignored.
        controls.touch_at(2, TouchPhase::Started, [W * 0.8, H * 0.6], at(now, 3));
        controls.touch_at(2, TouchPhase::Moved, [W * 0.9, H * 0.9], at(now, 4));
        controls.touch_at(
            3,
            TouchPhase::Started,
            [center[0] + radius, center[1]],
            at(now, 5),
        );
        controls.touch_at(
            3,
            TouchPhase::Moved,
            [center[0] + radius, center[1] - radius],
            at(now, 6),
        );

        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert!(!frame.movement.right);
        assert!((frame.look[0] - 4.5).abs() < 1e-4);
        assert_eq!(frame.look[1], 0.0);

        // Releasing the ignored fingers leaves the held gesture untouched.
        controls.touch_at(2, TouchPhase::Ended, [W * 0.9, H * 0.9], at(now, 7));
        controls.touch_at(
            3,
            TouchPhase::Ended,
            [center[0] + radius, center[1] - radius],
            at(now, 8),
        );
        let frame = controls.frame();
        assert!(frame.movement.forward);
        assert_eq!(frame.look, [0.0, 0.0]);
        assert!(!frame.break_block);
        assert!(!frame.jump);
    }

    #[test]
    fn cancel_releases_without_actions() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();
        let radius = controls.stick_radius();

        controls.touch_at(
            8,
            TouchPhase::Started,
            [center[0], center[1] - radius * 0.6],
            now,
        );
        assert!(controls.frame().movement.forward);
        controls.touch_at(
            8,
            TouchPhase::Cancelled,
            [center[0], center[1] - radius * 0.6],
            at(now, 50),
        );
        let frame = controls.frame();
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert!(!frame.place_block);

        // A cancelled short look press never breaks.
        let point = [W * 0.72, H * 0.5];
        controls.touch_at(9, TouchPhase::Started, point, at(now, 100));
        controls.touch_at(9, TouchPhase::Cancelled, point, at(now, 140));
        let frame = controls.frame();
        assert!(!frame.break_block);
        assert!(!frame.jump);
    }

    #[test]
    fn hud_rects_are_normalized_and_deterministic() {
        let mut controls = controls();
        let first = controls.hud_rects();
        assert_eq!(first, controls.hud_rects());
        assert_eq!(first.len(), 4);

        for (rect, color) in &first {
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

        // Joystick bottom-left, place button bottom-right.
        assert!(first[0].0[0] < 0.5 && first[0].0[1] > 0.5);
        assert!(first[2].0[0] > 0.5 && first[2].0[1] > 0.5);

        // Deflecting the stick moves the knob but not the base.
        let center = controls.stick_center();
        let radius = controls.stick_radius();
        controls.touch_at(
            0,
            TouchPhase::Started,
            [center[0] + radius * 0.9, center[1]],
            Instant::now(),
        );
        let moved = controls.hud_rects();
        assert_eq!(moved[0], first[0]);
        assert_ne!(moved[1], first[1]);
        assert!(moved[1].0[0] > first[1].0[0]);
    }

    #[test]
    fn resize_rebuilds_the_layout() {
        let mut controls = controls();
        let portrait = controls.hud_rects();
        controls.resize(1920.0, 1080.0);
        let landscape = controls.hud_rects();
        assert_ne!(portrait, landscape);
        for (rect, _) in &landscape {
            for value in rect {
                assert!((0.0..=1.0).contains(value), "{value}");
            }
        }
        assert!(landscape[2].0[0] > 0.5 && landscape[2].0[1] > 0.5);
    }

    #[test]
    fn public_touch_path_routes_to_the_controls() {
        let mut controls = controls();
        let rect = controls.place_rect();
        let point = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
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
        let mut controls = controls();
        let now = Instant::now();
        let point = slot_center(&controls, 0);
        // Slot 0 sits inside the joystick zone; the hotbar must win there.
        assert!(point[0] < W * STICK_ZONE);

        controls.touch_at(11, TouchPhase::Started, point, now);
        let frame = controls.frame();
        assert_eq!(frame.select, Some(0));
        assert_eq!(controls.selected(), 0);
        assert_eq!(frame.movement, Input::default());
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert!(!frame.place_block);
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
        assert!(point[0] > W * STICK_ZONE);
        controls.touch_at(12, TouchPhase::Started, point, at(now, 200));
        assert_eq!(controls.frame().select, Some(8));
        controls.touch_at(12, TouchPhase::Ended, point, at(now, 260));
        assert!(!controls.frame().break_block);
    }

    #[test]
    fn hotbar_drag_never_looks_or_breaks() {
        let mut controls = controls();
        let now = Instant::now();
        let start = slot_center(&controls, 4);
        assert!(start[0] > W * STICK_ZONE);

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
    fn hotbar_works_alongside_a_held_joystick() {
        let mut controls = controls();
        let now = Instant::now();
        let center = controls.stick_center();
        let radius = controls.stick_radius();

        controls.touch_at(
            30,
            TouchPhase::Started,
            [center[0], center[1] - radius * 0.8],
            now,
        );
        assert!(controls.frame().movement.forward);

        let point = slot_center(&controls, 3);
        controls.touch_at(31, TouchPhase::Started, point, at(now, 5));
        controls.touch_at(31, TouchPhase::Ended, point, at(now, 40));
        let frame = controls.frame();
        assert_eq!(frame.select, Some(3));
        // The joystick keeps driving and the tap never jumps.
        assert!(frame.movement.forward);
        assert!(!frame.jump);
        assert!(!frame.break_block);
        assert_eq!(frame.look, [0.0, 0.0]);
    }

    #[test]
    fn hotbar_rects_are_centred_above_the_lower_controls() {
        let controls = controls();
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

        // In pixels the bar clears the joystick base and the place button.
        let rect = controls.hotbar_slot_rect(4);
        let center = controls.stick_center();
        let radius = controls.stick_radius();
        assert!(rect[1] + rect[3] < center[1] - radius);
        assert!(rect[1] + rect[3] < controls.place_rect()[1]);
    }

    #[test]
    fn hotbar_selection_highlight_follows_taps_and_syncs() {
        let mut controls = controls();
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
