//! 1.16.5 light sampling for CPU chunk meshing.
//!
//! Vanilla stores two light levels per block: sky light (`0..=15`, attenuated
//! by the height map) and block light (`0..=15`, from torches, lava and other
//! emitters). The mesher does not read the world's light store directly;
//! callers plug in a [`BlockLightSource`], so this module stays independent of
//! any particular world layout.
//!
//! Two values are derived per block-face vertex:
//!
//! * a brightness in `0..=1` from the vanilla combination of sky and block
//!   light, `sky * (15 - block) / 15 + block`, divided by 15 and averaged over
//!   the four blocks touching the vertex on the outside of the face, with the
//!   vanilla `0.05` floor ([`LightSampler::vertex_light`]);
//! * an ambient-occlusion factor of `1.0`, `0.8`, `0.6` or `0.4` from the
//!   solid blocks around that same corner ([`ambient_occlusion`]).
//!
//! Their product is the value a shader multiplies the block texture by; that
//! is what [`crate::mesh::mesh_box_lit`] emits alongside the mesh.

use crate::blocks;
use crate::{BlockSource, Face};

/// Sky/block light levels of a block, each `0..=15`.
pub trait BlockLightSource {
    /// The `(sky, block)` light at a block position.
    ///
    /// `None` means the position is unknown (for example an unloaded chunk) and
    /// [`LightSampler::sample`] treats it as open sky.
    fn light_at(&self, x: i32, y: i32, z: i32) -> Option<[u8; 2]>;
}

/// Reads 1.16.5-style sky/block light and turns it into vertex brightness.
pub struct LightSampler;

impl LightSampler {
    /// The `(sky, block)` light at `pos`, each clamped to `0..=15`.
    ///
    /// A position the source does not know is open sky, `[15, 0]`, so holes in
    /// the loaded area do not darken seams between chunks.
    pub fn sample(world: &dyn BlockLightSource, pos: [i32; 3]) -> [u8; 2] {
        let [sky, block] = world.light_at(pos[0], pos[1], pos[2]).unwrap_or([15, 0]);
        [sky.min(15), block.min(15)]
    }

    /// Brightness in `0..=1` for one vertex of the `cell` block's face.
    ///
    /// `normal` is the outward face normal (only its dominant axis matters) and
    /// `corner_offset` points at the vertex: the components perpendicular to
    /// `normal` are `-1`/`1` and select which side neighbours are sampled. The
    /// light is averaged over the block in front of the face plus its two side
    /// neighbours and the diagonal one, as vanilla's smooth lighting does, and
    /// the result is floored at the vanilla `0.05`.
    ///
    /// The diagonal only feeds the average; ambient occlusion is a separate
    /// multiplier, see [`ambient_occlusion`].
    pub fn vertex_light(
        world: &dyn BlockLightSource,
        cell: [i32; 3],
        corner_offset: [i32; 3],
        normal: [i32; 3],
    ) -> f32 {
        let normal = dominant_axis(normal);
        let front = add(cell, normal);
        let (axis_a, axis_b) = tangent_axes(normal);
        let side_a = add(front, scale(axis_a, sign(dot(axis_a, corner_offset))));
        let side_b = add(front, scale(axis_b, sign(dot(axis_b, corner_offset))));
        let diagonal = add(side_b, scale(axis_a, sign(dot(axis_a, corner_offset))));
        let average = (light_value(Self::sample(world, front))
            + light_value(Self::sample(world, side_a))
            + light_value(Self::sample(world, side_b))
            + light_value(Self::sample(world, diagonal)))
            / 4.0;
        average.max(0.05)
    }
}

/// Ambient occlusion for one vertex of `cell`'s `face`.
///
/// `corner` is the same corner direction [`LightSampler::vertex_light`] takes:
/// its components along the two axes perpendicular to the face are `-1`/`1`.
/// The three blocks around the vertex on the outside of the face are tested
/// with [`blocks::is_solid`] and the count of occluders maps to vanilla's
/// `1.0` / `0.8` / `0.6` / `0.4`. When both side neighbours are solid the
/// corner is fully occluded regardless of the diagonal, as in vanilla.
pub fn ambient_occlusion(
    world: &dyn BlockSource,
    cell: [i32; 3],
    face: Face,
    corner: [i32; 3],
) -> f32 {
    let normal = face.offset();
    let front = add(cell, normal);
    let (axis_a, axis_b) = tangent_axes(normal);
    let side_a = add(front, scale(axis_a, sign(dot(axis_a, corner))));
    let side_b = add(front, scale(axis_b, sign(dot(axis_b, corner))));
    let diagonal = add(side_b, scale(axis_a, sign(dot(axis_a, corner))));
    let solid_a = solid_at(world, side_a);
    let solid_b = solid_at(world, side_b);
    let solid_diagonal = solid_at(world, diagonal);
    let occluders = if solid_a && solid_b {
        3
    } else {
        u8::from(solid_a) + u8::from(solid_b) + u8::from(solid_diagonal)
    };
    match occluders {
        0 => 1.0,
        1 => 0.8,
        2 => 0.6,
        _ => 0.4,
    }
}

/// `(sky * (15 - block) / 15 + block) / 15`, i.e. the vanilla light strength
/// with block light filling in where sky light is missing.
fn light_value(light: [u8; 2]) -> f32 {
    let sky = f32::from(light[0].min(15));
    let block = f32::from(light[1].min(15));
    (sky * (15.0 - block) / 15.0 + block) / 15.0
}

/// The dominant axis of a face normal, as a unit vector.
fn dominant_axis(normal: [i32; 3]) -> [i32; 3] {
    if normal[0] != 0 {
        [normal[0].signum(), 0, 0]
    } else if normal[1] != 0 {
        [0, normal[1].signum(), 0]
    } else if normal[2] != 0 {
        [0, 0, normal[2].signum()]
    } else {
        [0, 1, 0]
    }
}

/// The two unit axes perpendicular to a face normal.
fn tangent_axes(normal: [i32; 3]) -> ([i32; 3], [i32; 3]) {
    if normal[0] != 0 {
        ([0, 1, 0], [0, 0, 1])
    } else if normal[1] != 0 {
        ([1, 0, 0], [0, 0, 1])
    } else {
        ([1, 0, 0], [0, 1, 0])
    }
}

fn solid_at(world: &dyn BlockSource, pos: [i32; 3]) -> bool {
    world
        .state_at(pos[0], pos[1], pos[2])
        .is_some_and(|state| blocks::is_solid(&state.name))
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [i32; 3], factor: i32) -> [i32; 3] {
    [a[0] * factor, a[1] * factor, a[2] * factor]
}

fn dot(a: [i32; 3], b: [i32; 3]) -> i32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sign(value: i32) -> i32 {
    value.signum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockState;
    use std::collections::{HashMap, HashSet};

    /// The same `(sky, block)` level everywhere.
    struct FlatLight([u8; 2]);

    impl BlockLightSource for FlatLight {
        fn light_at(&self, _x: i32, _y: i32, _z: i32) -> Option<[u8; 2]> {
            Some(self.0)
        }
    }

    /// Explicit light levels; unknown positions are unloaded.
    #[derive(Default)]
    struct MapLight(HashMap<[i32; 3], [u8; 2]>);

    impl BlockLightSource for MapLight {
        fn light_at(&self, x: i32, y: i32, z: i32) -> Option<[u8; 2]> {
            self.0.get(&[x, y, z]).copied()
        }
    }

    /// Stone at the listed positions, nothing elsewhere.
    #[derive(Default)]
    struct Solids(HashSet<[i32; 3]>);

    impl BlockSource for Solids {
        fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
            if self.0.contains(&[x, y, z]) {
                Some(BlockState::new("minecraft:stone"))
            } else {
                None
            }
        }
    }

    #[test]
    fn open_sky_reaches_full_brightness() {
        let light = FlatLight([15, 0]);
        assert_eq!(LightSampler::sample(&light, [0, 64, 0]), [15, 0]);
        let value = LightSampler::vertex_light(&light, [0, 0, 0], [1, 0, 1], [0, 1, 0]);
        assert!((value - 1.0).abs() < 1e-6, "{value}");
    }

    #[test]
    fn a_roof_lowers_the_vertex_light() {
        let mut light = MapLight::default();
        let open = [[0, 1, 0], [1, 1, 0], [0, 1, 1], [1, 1, 1]];
        for pos in open {
            light.0.insert(pos, [15, 0]);
        }
        let sunny = LightSampler::vertex_light(&light, [0, 0, 0], [1, 0, 1], [0, 1, 0]);
        assert!((sunny - 1.0).abs() < 1e-6, "{sunny}");

        let mut roofed = MapLight::default();
        for pos in open {
            roofed.0.insert(pos, [0, 0]);
        }
        let dark = LightSampler::vertex_light(&roofed, [0, 0, 0], [1, 0, 1], [0, 1, 0]);
        assert!(dark < sunny, "{dark} !< {sunny}");
        assert!((dark - 0.05).abs() < 1e-6, "{dark}");
    }

    #[test]
    fn block_light_fills_in_without_sky() {
        let light = FlatLight([0, 14]);
        let value = LightSampler::vertex_light(&light, [0, 0, 0], [0, 0, 1], [0, 1, 0]);
        assert!((value - 14.0 / 15.0).abs() < 1e-5, "{value}");
        assert!((value - 0.93).abs() < 0.01, "{value}");
    }

    #[test]
    fn sample_clamps_and_defaults_unknown_positions_to_open_sky() {
        let light = FlatLight([200, 200]);
        assert_eq!(LightSampler::sample(&light, [0, 0, 0]), [15, 15]);
        let unknown = MapLight::default();
        assert_eq!(LightSampler::sample(&unknown, [0, 0, 0]), [15, 0]);
    }

    #[test]
    fn ambient_occlusion_counts_side_neighbours() {
        // Face::Up, corner (+x, +z): side neighbours [1,1,0] and [0,1,1],
        // diagonal [1,1,1].
        let side_a = [1, 1, 0];
        let side_b = [0, 1, 1];
        let diagonal = [1, 1, 1];
        let cases: [(usize, &[[i32; 3]], f32); 4] = [
            (0, &[], 1.0),
            (1, &[side_a], 0.8),
            (2, &[side_a, diagonal], 0.6),
            (3, &[side_a, side_b, diagonal], 0.4),
        ];
        for (label, occluders, expected) in cases {
            let mut world = Solids::default();
            for pos in occluders {
                world.0.insert(*pos);
            }
            let ao = ambient_occlusion(&world, [0, 0, 0], Face::Up, [1, 0, 1]);
            assert!((ao - expected).abs() < 1e-6, "case {label}: {ao}");
        }
    }

    #[test]
    fn ambient_occlusion_is_vanilla_when_both_sides_are_solid() {
        // Both sides solid hides the diagonal, so the corner is fully occluded.
        let mut world = Solids::default();
        world.0.insert([1, 1, 0]);
        world.0.insert([0, 1, 1]);
        let ao = ambient_occlusion(&world, [0, 0, 0], Face::Up, [1, 0, 1]);
        assert!((ao - 0.4).abs() < 1e-6, "{ao}");
    }

    #[test]
    fn ambient_occlusion_follows_the_requested_corner() {
        // Only the -x side is solid: the (-x, +z) corner is shaded, the
        // (+x, +z) corner is not.
        let mut world = Solids::default();
        world.0.insert([-1, 1, 0]);
        assert!((ambient_occlusion(&world, [0, 0, 0], Face::Up, [-1, 0, 1]) - 0.8).abs() < 1e-6);
        assert!((ambient_occlusion(&world, [0, 0, 0], Face::Up, [1, 0, 1]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn non_solid_blocks_never_occlude() {
        struct Plants;
        impl BlockSource for Plants {
            fn state_at(&self, _x: i32, _y: i32, _z: i32) -> Option<BlockState> {
                Some(BlockState::new("minecraft:poppy"))
            }
        }
        let ao = ambient_occlusion(&Plants, [0, 0, 0], Face::Up, [1, 0, 1]);
        assert!((ao - 1.0).abs() < 1e-6, "{ao}");
    }
}
