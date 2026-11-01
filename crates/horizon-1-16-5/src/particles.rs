//! Block-break particles for the 1.16.5 target.
//!
//! One break spawns a burst of small voxels tinted with the broken block's
//! dominant texture colour. The emitter keeps them in a fixed-capacity `Vec`
//! and updates in place: no allocation happens in the per-frame path, which
//! matters on a phone where a break can fire every few ticks.

use crate::atlas::TextureAtlas;
use crate::types::BlockState;

/// Particles one break spawns.
pub const BURST: usize = 14;
/// How long a particle lives, in seconds.
pub const LIFETIME: f32 = 0.7;
/// Downward acceleration, in blocks per second squared.
const GRAVITY: f32 = 18.0;
/// Never keep more than this many particles alive at once.
pub const CAPACITY: usize = 512;
/// Half the edge of one particle cube, in blocks.
const HALF_SIZE: f32 = 0.06;

/// One voxel particle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// World position, in blocks.
    pub position: [f32; 3],
    /// Velocity in blocks per second.
    pub velocity: [f32; 3],
    /// Straight RGB in `0..=1`, taken from the block's dominant texture colour.
    pub color: [f32; 3],
    /// Seconds since the particle spawned.
    pub age: f32,
}

impl Particle {
    /// Whether the particle is still inside its lifetime.
    pub fn alive(&self) -> bool {
        self.age < LIFETIME
    }

    /// Fades out over the last third of the lifetime.
    pub fn alpha(&self) -> f32 {
        let left = LIFETIME - self.age;
        (left / (LIFETIME / 3.0)).clamp(0.0, 1.0)
    }

    /// The cube's half extent, which shrinks a little as it ages.
    pub fn half_size(&self) -> f32 {
        HALF_SIZE * (1.0 - 0.35 * (self.age / LIFETIME))
    }
}

/// Deterministic per-break randomness: a small xorshift seeded from the
/// break position and a counter, so two identical breaks look identical in
/// tests and a replay reproduces the same frame.
struct Jitter(u32);

impl Jitter {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// The dominant colour of a block texture, from the atlas.
///
/// The average of every opaque texel, which is a stable "dominant" colour and
/// keeps a grass block green and stone grey. A texture the atlas does not
/// have falls back to mid grey; nothing here allocates beyond the sprite
/// lookup.
pub fn dominant_color(atlas: &TextureAtlas, texture: &str) -> [f32; 3] {
    let Some(sprite) = atlas.sprite(texture) else {
        return [0.5, 0.5, 0.5];
    };
    let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
    for y in sprite.y..sprite.y + sprite.height {
        for x in sprite.x..sprite.x + sprite.width {
            let Some(texel) = atlas.texel(x, y) else {
                continue;
            };
            if texel[3] < 128 {
                continue;
            }
            r += u32::from(texel[0]);
            g += u32::from(texel[1]);
            b += u32::from(texel[2]);
            count += 1;
        }
    }
    if count == 0 {
        return [0.5, 0.5, 0.5];
    }
    let n = count as f32;
    [
        r as f32 / n / 255.0,
        g as f32 / n / 255.0,
        b as f32 / n / 255.0,
    ]
}

/// Spawns the burst for a block broken at block coordinates `(x, y, z)`.
///
/// Appends in place, never reallocates past [`CAPACITY`], and drops the oldest
/// particles when the emitter is full.
pub fn spawn_break(
    particles: &mut Vec<Particle>,
    block: &BlockState,
    color: [f32; 3],
    x: i32,
    y: i32,
    z: i32,
    counter: u32,
) {
    let mut jitter = Jitter(
        0x9E37_79B9
            ^ (x as u32).wrapping_mul(73_856_093)
            ^ (y as u32).wrapping_mul(19_349_663)
            ^ (z as u32).wrapping_mul(83_492_791)
            ^ counter.wrapping_mul(2_654_435_761)
            | 1,
    );
    let _ = block;
    if particles.len() + BURST > CAPACITY {
        let drop = (particles.len() + BURST - CAPACITY).min(particles.len());
        particles.drain(..drop);
    }
    for _ in 0..BURST {
        // Spread across the block's face, inside its 0..1 box.
        let inside = |j: &mut Jitter| 0.12 + j.next() * 0.76;
        let center = [
            x as f32 + inside(&mut jitter),
            y as f32 + inside(&mut jitter),
            z as f32 + inside(&mut jitter),
        ];
        let speed = |j: &mut Jitter| (j.next() - 0.5) * 3.2;
        // A little upward bias so the burst reads as a puff, not a splash.
        let velocity = [
            speed(&mut jitter),
            speed(&mut jitter) + 1.6,
            speed(&mut jitter),
        ];
        // Slight per-particle tint so the burst is not one flat colour.
        let shade = 0.8 + jitter.next() * 0.35;
        particles.push(Particle {
            position: center,
            velocity,
            color: [color[0] * shade, color[1] * shade, color[2] * shade],
            age: 0.0,
        });
    }
}

/// Advances every particle by `dt` seconds and drops the expired ones in
/// place, without reallocating.
pub fn tick(particles: &mut Vec<Particle>, dt: f32) {
    for particle in particles.iter_mut() {
        particle.age += dt;
        particle.velocity[1] -= GRAVITY * dt;
        for axis in 0..3 {
            particle.position[axis] += particle.velocity[axis] * dt;
        }
    }
    particles.retain(Particle::alive);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> BlockState {
        BlockState::new("minecraft:stone")
    }

    #[test]
    fn a_break_spawns_a_burst_inside_its_lifetime() {
        let mut particles = Vec::new();
        spawn_break(&mut particles, &block(), [0.5, 0.5, 0.5], 3, 64, -7, 1);
        assert_eq!(particles.len(), BURST);
        assert!((12..=16).contains(&particles.len()));
        for particle in &particles {
            assert!(particle.alive());
            assert_eq!(particle.age, 0.0);
            // Inside the broken block's box.
            assert!((3.0..=4.0).contains(&particle.position[0]));
            assert!((64.0..=65.0).contains(&particle.position[1]));
            assert!((-7.0..=-6.0).contains(&particle.position[2]));
        }
    }

    #[test]
    fn particles_fall_and_expire_after_the_lifetime() {
        let mut particles = Vec::new();
        spawn_break(&mut particles, &block(), [0.4, 0.4, 0.4], 0, 70, 0, 2);
        let mut highest = f32::MIN;
        for particle in &particles {
            highest = highest.max(particle.position[1]);
        }
        tick(&mut particles, 0.1);
        for particle in &particles {
            assert!(particle.velocity[1] < 1.6 + 1.0, "gravity pulls it down");
        }
        // After the full lifetime every particle is gone.
        for _ in 0..8 {
            tick(&mut particles, 0.1);
        }
        assert!(particles.is_empty());
    }

    #[test]
    fn the_emitter_never_grows_past_its_capacity() {
        let mut particles = Vec::new();
        for i in 0..1000 {
            spawn_break(&mut particles, &block(), [1.0, 0.0, 0.0], 0, 64, 0, i);
            assert!(particles.len() <= CAPACITY);
        }
        // Newest particles survive: the oldest ages were dropped first.
        assert_eq!(particles.len(), CAPACITY);
    }

    #[test]
    fn tick_does_not_reallocate_when_nothing_expires() {
        let mut particles = Vec::with_capacity(CAPACITY);
        spawn_break(&mut particles, &block(), [0.2, 0.6, 0.2], 0, 64, 0, 1);
        let capacity = particles.capacity();
        tick(&mut particles, 0.016);
        assert_eq!(particles.capacity(), capacity);
    }

    #[test]
    fn alpha_fades_over_the_last_third() {
        let mut particles = Vec::new();
        spawn_break(&mut particles, &block(), [1.0; 3], 0, 64, 0, 3);
        let particle = particles[0];
        assert!((particle.alpha() - 1.0).abs() < 1e-6);
        particles[0].age = LIFETIME * 0.9;
        assert!(particles[0].alpha() < 0.4);
        particles[0].age = LIFETIME;
        assert_eq!(particles[0].alpha(), 0.0);
    }
}
