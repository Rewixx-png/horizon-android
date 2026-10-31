//! Player movement for the 1.16.5 target. See the play contract in the crate
//! docs: 20 ticks per second, block coordinates, y is the feet position.
//!
//! The constants follow 1.16.5 closely: gravity 0.08 per tick with 0.98 air
//! drag, ground friction `0.6 * 0.91`, walk acceleration 0.1 (sprint x1.3,
//! sneak x0.3), air acceleration 0.02 and a jump velocity of 0.42. Collision
//! is axis-separated AABB overlap against [`crate::blocks::is_solid`] with a
//! grounded 0.6 step-up, so a full block still stops the player while
//! half-block ledges can be walked onto.

use crate::BlockSource;
use crate::blocks::is_solid;

// --- physics constants (blocks per tick, one tick = 1/20 s) ---

/// Downward velocity added each tick while airborne (or grounded) and not
/// flying.
const GRAVITY: f64 = 0.08;
/// Horizontal velocity factor per tick in the air.
const AIR_DRAG: f64 = 0.98;
/// Block slipperiness; ground horizontal friction is `0.6 * 0.91` per tick.
const SLIPPERINESS: f64 = 0.6;
const GROUND_FRICTION: f64 = 0.91;
/// Horizontal acceleration while on the ground.
const WALK_ACCELERATION: f64 = 0.1;
/// Horizontal acceleration while airborne and not flying.
const AIR_ACCELERATION: f64 = 0.02;
const SPRINT_FACTOR: f64 = 1.3;
const SNEAK_FACTOR: f64 = 0.3;
/// Initial upward velocity on jump.
const JUMP_VELOCITY: f64 = 0.42;
/// Highest ledge the grounded player can step onto without jumping.
const STEP_HEIGHT: f64 = 0.6;
/// Safety cap on vertical velocity in either direction.
const MAX_VERTICAL_SPEED: f64 = 4.0;
/// Horizontal acceleration while flying (a bit faster than air).
const FLY_ACCELERATION: f64 = 0.05;
/// Vertical velocity change per tick while holding jump/sneak in flight.
const FLY_VERTICAL_ACCELERATION: f64 = 0.12;
/// Vertical velocity decay while flying with neither jump nor sneak held.
const FLY_VERTICAL_DRAG: f64 = 0.9;
/// Horizontal drag while flying (stiffer than air drag so flight speed
/// settles in a playable range instead of drifting up).
const FLY_DRAG: f64 = 0.9;
/// Flying vertical speed cap.
const FLY_MAX_VERTICAL: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    /// Feet position in block coordinates.
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    /// Degrees, 0 looking toward +Z, +90 toward +X.
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub flying: bool,
    pub sneaking: bool,
}

impl Player {
    pub const WIDTH: f64 = 0.6;
    pub const HEIGHT: f64 = 1.8;
    pub const EYE_HEIGHT: f64 = 1.62;

    /// Half of [`Self::WIDTH`], the horizontal AABB radius.
    const HALF_WIDTH: f64 = Self::WIDTH / 2.0;

    pub fn new(position: [f64; 3]) -> Self {
        Self {
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            flying: false,
            sneaking: false,
        }
    }

    pub fn eye(&self) -> [f64; 3] {
        [
            self.position[0],
            self.position[1] + Self::EYE_HEIGHT,
            self.position[2],
        ]
    }

    /// Unit look direction from yaw/pitch: yaw 0 looks toward +Z and +90
    /// toward +X; positive pitch looks down, like vanilla.
    pub fn look(&self) -> [f32; 3] {
        let (sy, cy) = (self.yaw as f64).to_radians().sin_cos();
        let (sp, cp) = (self.pitch as f64).to_radians().sin_cos();
        [(sy * cp) as f32, (-sp) as f32, (cy * cp) as f32]
    }

    /// Sets the absolute look angles. Pitch is clamped to -90..90 and yaw is
    /// wrapped into -180..180.
    pub fn set_look(&mut self, yaw: f32, pitch: f32) {
        self.yaw = ((yaw as f64 + 180.0).rem_euclid(360.0) - 180.0) as f32;
        self.pitch = pitch.clamp(-90.0, 90.0);
    }

    pub fn toggle_flying(&mut self) {
        self.flying = !self.flying;
        if self.flying {
            // Drop accumulated fall speed so enabling flight never slams
            // into the ground.
            self.velocity[1] = 0.0;
        }
    }

    /// `(min, max)` of the collision box; feet are at `position`.
    pub fn aabb(&self) -> ([f64; 3], [f64; 3]) {
        let h = Self::HALF_WIDTH;
        (
            [self.position[0] - h, self.position[1], self.position[2] - h],
            [
                self.position[0] + h,
                self.position[1] + Self::HEIGHT,
                self.position[2] + h,
            ],
        )
    }

    /// Advances one 1/20 s tick against the block source.
    pub fn tick(&mut self, world: &dyn BlockSource, input: &Input) {
        self.sneaking = input.sneak;

        // Horizontal acceleration along the unit movement direction.
        let [dx, dz] = self.movement_direction(input);
        let mut acceleration = if self.flying {
            FLY_ACCELERATION
        } else if self.on_ground {
            WALK_ACCELERATION
        } else {
            AIR_ACCELERATION
        };
        if input.sprint {
            acceleration *= SPRINT_FACTOR;
        }
        if input.sneak && !self.flying {
            acceleration *= SNEAK_FACTOR;
        }
        self.velocity[0] += dx * acceleration;
        self.velocity[2] += dz * acceleration;

        // Vertical velocity.
        if self.flying {
            if input.jump {
                self.velocity[1] += FLY_VERTICAL_ACCELERATION;
            }
            if input.sneak {
                self.velocity[1] -= FLY_VERTICAL_ACCELERATION;
            }
            if !input.jump && !input.sneak {
                self.velocity[1] *= FLY_VERTICAL_DRAG;
            }
            self.velocity[1] = self.velocity[1].clamp(-FLY_MAX_VERTICAL, FLY_MAX_VERTICAL);
        } else if self.on_ground && input.jump {
            // The jump tick applies no gravity, so the initial 0.42 is kept
            // whole and the apex stays at vanilla height (~1.25 blocks).
            self.velocity[1] = JUMP_VELOCITY;
        } else {
            self.velocity[1] -= GRAVITY;
            self.velocity[1] *= AIR_DRAG;
            self.velocity[1] = self.velocity[1].clamp(-MAX_VERTICAL_SPEED, MAX_VERTICAL_SPEED);
        }

        // Axis-separated movement, X then Y then Z like the vanilla
        // collision code.
        for axis in 0..3 {
            let delta = self.velocity[axis];
            if delta != 0.0 {
                self.move_axis(world, axis, delta);
            }
        }

        // Friction after the move, using the fresh on-ground state.
        let horizontal_drag = if self.flying {
            FLY_DRAG
        } else if self.on_ground {
            SLIPPERINESS * GROUND_FRICTION
        } else {
            AIR_DRAG
        };
        self.velocity[0] *= horizontal_drag;
        self.velocity[2] *= horizontal_drag;
    }

    pub fn teleport(&mut self, position: [f64; 3]) {
        self.position = position;
        self.velocity = [0.0; 3];
        self.on_ground = false;
    }
}

// Collision internals.
impl Player {
    /// Unit horizontal movement direction from the held keys and the yaw.
    fn movement_direction(&self, input: &Input) -> [f64; 2] {
        let forward = f64::from(input.forward) - f64::from(input.back);
        let strafe = f64::from(input.right) - f64::from(input.left);
        if forward == 0.0 && strafe == 0.0 {
            return [0.0, 0.0];
        }
        let (sy, cy) = (self.yaw as f64).to_radians().sin_cos();
        // Forward (yaw 0 -> +Z, 90 -> +X) and right, the forward vector
        // turned -90 degrees about +Y.
        let (fx, fz) = (sy, cy);
        let (rx, rz) = (-cy, sy);
        let dx = fx * forward + rx * strafe;
        let dz = fz * forward + rz * strafe;
        let length = (dx * dx + dz * dz).sqrt();
        [dx / length, dz / length]
    }

    /// Moves along one axis by `delta`, resolving collisions against solid
    /// blocks. Grounded horizontal moves attempt the 0.6 step-up.
    fn move_axis(&mut self, world: &dyn BlockSource, axis: usize, delta: f64) {
        self.position[axis] += delta;
        let Some(face) = self.face_hit(world, axis, delta) else {
            if axis == 1 {
                // Moved vertically without touching anything: airborne.
                self.on_ground = false;
            }
            return;
        };
        if axis == 1 {
            // Vertical: rest exactly on the top face when falling, stop
            // flush under the bottom face when rising.
            self.position[1] = if delta > 0.0 {
                face - Self::HEIGHT
            } else {
                face
            };
            self.velocity[1] = 0.0;
            self.on_ground = delta < 0.0;
        } else if !self.step_up(world, axis, delta) {
            // Blocked: push back flush against the blocking face.
            self.position[axis] = if delta > 0.0 {
                face - Self::HALF_WIDTH
            } else {
                face + Self::HALF_WIDTH
            };
            self.velocity[axis] = 0.0;
        }
    }

    /// The coordinate of the nearest solid face intersecting the current
    /// AABB along `axis` in the direction of `delta`, or `None` when clear.
    ///
    /// The box is stretched back along `axis` so the whole swept path of the
    /// move is tested, not just the final position: a fall at terminal
    /// velocity (3.92 blocks per tick) must still land on a one-block floor.
    /// Only faces on the far side of the box-before-the-move are considered,
    /// so a block behind the player (or inside float noise of a flush face)
    /// can never drag the player backwards.
    fn face_hit(&self, world: &dyn BlockSource, axis: usize, delta: f64) -> Option<f64> {
        // Hairline: a box touching a face, or within float noise of it,
        // does not count as penetrating the block.
        const EPSILON: f64 = 1e-9;
        let (mut min, mut max) = self.aabb();
        // Bounds of the box before this move, for the direction filter.
        let old_min = if delta > 0.0 {
            min[axis] - delta
        } else {
            min[axis]
        };
        let old_max = if delta > 0.0 {
            max[axis]
        } else {
            max[axis] - delta
        };
        if delta > 0.0 {
            min[axis] -= delta;
        } else {
            max[axis] -= delta;
        }
        let x0 = min[0].floor() as i32;
        let y0 = min[1].floor() as i32;
        let z0 = min[2].floor() as i32;
        let x1 = max[0].floor() as i32;
        let y1 = max[1].floor() as i32;
        let z1 = max[2].floor() as i32;
        let mut hit: Option<f64> = None;
        for y in y0..=y1 {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let bx = x as f64;
                    let by = y as f64;
                    let bz = z as f64;
                    if min[0] >= bx + 1.0 - EPSILON
                        || max[0] <= bx + EPSILON
                        || min[1] >= by + 1.0 - EPSILON
                        || max[1] <= by + EPSILON
                        || min[2] >= bz + 1.0 - EPSILON
                        || max[2] <= bz + EPSILON
                    {
                        continue;
                    }
                    let solid = world
                        .state_at(x, y, z)
                        .map(|state| is_solid(&state.name))
                        .unwrap_or(false);
                    if !solid {
                        continue;
                    }
                    let face = match axis {
                        0 => {
                            if delta > 0.0 {
                                bx
                            } else {
                                bx + 1.0
                            }
                        }
                        1 => {
                            if delta > 0.0 {
                                by
                            } else {
                                by + 1.0
                            }
                        }
                        _ => {
                            if delta > 0.0 {
                                bz
                            } else {
                                bz + 1.0
                            }
                        }
                    };
                    // Ignore faces behind the old box: they cannot be hit
                    // by a move in this direction.
                    let reachable = if delta > 0.0 {
                        face >= old_min
                    } else {
                        face <= old_max
                    };
                    if !reachable {
                        continue;
                    }
                    hit = Some(match hit {
                        None => face,
                        Some(previous) if delta > 0.0 => previous.min(face),
                        Some(previous) => previous.max(face),
                    });
                }
            }
        }
        hit
    }

    /// Attempts the grounded 0.6 step-up after a blocked horizontal move.
    ///
    /// Returns true when the step succeeded (the player now stands on the
    /// ledge). The move is undone, the player is raised onto the block under
    /// its feet, and the horizontal move is retried; any remaining overlap
    /// (a taller wall or a low ceiling) reverts the whole step so the player
    /// can never be teleported through geometry.
    fn step_up(&mut self, world: &dyn BlockSource, axis: usize, delta: f64) -> bool {
        if !self.on_ground {
            return false;
        }
        self.position[axis] -= delta;
        let rise = self.position[1].floor() + 1.0 - self.position[1];
        if rise <= 0.0 || rise > STEP_HEIGHT {
            return false;
        }
        self.position[1] += rise;
        self.position[axis] += delta;
        if self.face_hit(world, axis, delta).is_some() {
            self.position[axis] -= delta;
            self.position[1] -= rise;
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockState;
    use std::collections::HashMap;

    /// Block states placed at explicit coordinates; everything else is empty.
    #[derive(Default)]
    struct MapSource {
        states: HashMap<[i32; 3], BlockState>,
    }

    impl BlockSource for MapSource {
        fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
            self.states.get(&[x, y, z]).cloned()
        }
    }

    fn stone() -> BlockState {
        BlockState::new("minecraft:stone")
    }

    /// A stone floor with its top surface at y = 0.
    fn floor_map() -> MapSource {
        let mut map = MapSource::default();
        for x in -4..=4 {
            for z in -4..=4 {
                map.states.insert([x, -1, z], stone());
            }
        }
        map
    }

    /// Ticks until the player is grounded, or `limit` ticks.
    fn settle(player: &mut Player, world: &MapSource, limit: usize) {
        for _ in 0..limit {
            if player.on_ground {
                return;
            }
            player.tick(world, &Input::default());
        }
    }

    #[test]
    fn falling_lands_exactly_on_floor() {
        let world = floor_map();
        let mut player = Player::new([0.0, 10.0, 0.0]);
        for _ in 0..300 {
            player.tick(&world, &Input::default());
        }
        assert!(player.on_ground);
        assert_eq!(player.position[1], 0.0);
        assert_eq!(player.velocity[1], 0.0);
    }

    #[test]
    fn walking_into_wall_stops_before_it() {
        let mut world = floor_map();
        // A single wall cube in the path at feet level (y 0..1).
        world.states.insert([1, 0, 0], stone());
        let mut player = Player::new([0.5, 0.0, 0.0]);
        player.yaw = 90.0; // face +X
        player.on_ground = true;
        let input = Input {
            forward: true,
            ..Input::default()
        };
        for _ in 0..30 {
            player.tick(&world, &input);
        }
        // Flush against the wall face at x = 1, never inside it, stopped.
        assert!(player.position[0] + Player::HALF_WIDTH <= 1.0 + 1e-12);
        assert!(player.position[0] < 1.0);
        assert_eq!(player.velocity[0], 0.0);
        assert!(player.on_ground);
    }

    #[test]
    fn full_wall_is_never_climbed() {
        // A two-cube wall is well beyond STEP_HEIGHT, so walking into it
        // for a long time must never push the player through or onto it.
        let mut world = floor_map();
        for y in 0..=1 {
            for z in -1..=1 {
                world.states.insert([1, y, z], stone());
            }
        }
        let mut player = Player::new([0.5, 0.0, 0.0]);
        player.yaw = 90.0;
        player.on_ground = true;
        let input = Input {
            forward: true,
            sprint: true,
            ..Input::default()
        };
        for _ in 0..60 {
            player.tick(&world, &input);
        }
        assert!(player.position[0] + Player::HALF_WIDTH <= 1.0 + 1e-12);
        assert!(player.position[0] < 1.0);
        assert_eq!(player.position[1], 0.0);
    }

    #[test]
    fn step_up_climbs_a_ledge_within_range() {
        // `step_up` is called after the horizontal move has already been
        // applied: feet at 0.5, blocked at x = 0.8 by the cube ahead, whose
        // top is 0.5 above the feet, inside STEP_HEIGHT.
        let mut world = MapSource::default();
        world.states.insert([1, 0, 0], stone());
        let mut player = Player::new([0.8, 0.5, 0.0]);
        player.on_ground = true;
        assert!(player.step_up(&world, 0, 0.3));
        assert!((player.position[0] - 0.8).abs() < 1e-12);
        assert_eq!(player.position[1], 1.0);
    }

    #[test]
    fn step_up_is_refused_when_it_would_clip() {
        // The same ledge but with a second cube on top: the step is
        // reverted wholesale, so the player is never lifted through a wall.
        let mut world = MapSource::default();
        world.states.insert([1, 0, 0], stone());
        world.states.insert([1, 1, 0], stone());
        let mut player = Player::new([0.8, 0.5, 0.0]);
        player.on_ground = true;
        assert!(!player.step_up(&world, 0, 0.3));
        assert!((player.position[0] - 0.5).abs() < 1e-12);
        assert_eq!(player.position[1], 0.5);
    }

    #[test]
    fn jump_leaves_and_returns_to_ground() {
        let world = floor_map();
        let mut player = Player::new([0.0, 0.0, 0.0]);
        player.on_ground = true;
        let mut input = Input {
            jump: true,
            ..Input::default()
        };
        player.tick(&world, &input);
        assert!(!player.on_ground);
        assert!(player.position[1] > 0.0);

        input.jump = false;
        let mut apex = player.position[1];
        for _ in 0..200 {
            if player.on_ground {
                break;
            }
            player.tick(&world, &input);
            apex = apex.max(player.position[1]);
        }
        assert!(player.on_ground);
        assert_eq!(player.position[1], 0.0);
        // A real jump, not a hop.
        assert!(apex > 0.4);
    }

    #[test]
    fn sprint_moves_farther_than_walk() {
        let world = floor_map();
        let mut walker = Player::new([0.0, 0.0, 0.0]);
        let mut sprinter = Player::new([0.0, 0.0, 0.0]);
        walker.on_ground = true;
        sprinter.on_ground = true;
        let walk = Input {
            forward: true,
            ..Input::default()
        };
        let sprint = Input {
            forward: true,
            sprint: true,
            ..Input::default()
        };
        for _ in 0..40 {
            walker.tick(&world, &walk);
            sprinter.tick(&world, &sprint);
        }
        assert!(sprinter.position[2] > walker.position[2] + 0.5);
    }

    #[test]
    fn flying_does_not_fall() {
        let world = floor_map();
        let mut player = Player::new([0.0, 10.0, 0.0]);
        player.flying = true;
        for _ in 0..100 {
            player.tick(&world, &Input::default());
        }
        assert_eq!(player.position[1], 10.0);
        assert_eq!(player.velocity[1], 0.0);
    }

    #[test]
    fn unknown_blocks_are_solid_and_empty_world_is_safe() {
        // An unknown block name must collide (safe default).
        let mut solid_world = MapSource::default();
        solid_world
            .states
            .insert([0, -1, 0], BlockState::new("minecraft:custom_unknown"));
        let mut player = Player::new([0.0, 5.0, 0.0]);
        for _ in 0..100 {
            player.tick(&solid_world, &Input::default());
        }
        assert!(player.on_ground);
        assert_eq!(player.position[1], 0.0);

        // A completely empty world: the player falls forever but stays
        // finite (terminal velocity clamp, no NaN).
        let empty = MapSource::default();
        let mut player = Player::new([0.0, 100.0, 0.0]);
        for _ in 0..500 {
            player.tick(&empty, &Input::default());
        }
        assert!(player.position[1] < 100.0);
        assert!(player.velocity.iter().all(|v| v.is_finite()));
        assert!(player.position.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn settling_on_floor_sets_on_ground() {
        let world = floor_map();
        let mut player = Player::new([0.0, 0.5, 0.0]);
        settle(&mut player, &world, 60);
        assert!(player.on_ground);
        assert_eq!(player.position[1], 0.0);
    }
}
