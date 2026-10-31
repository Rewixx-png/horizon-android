//! Multi-chunk mesh building for the 1.16.5 target.

use crate::{BlockSource, Mesh, ModelSource};

/// Meshes one full 16x256x16 column at chunk coordinates `(x, z)`.
pub fn mesh_chunk(world: &dyn BlockSource, models: &dyn ModelSource, x: i32, z: i32) -> Mesh {
    crate::mesh::mesh_box(world, models, [x * 16, 0, z * 16], [16, 256, 16])
}

/// Merges meshes whose vertices already use world coordinates.
///
/// Vertices and textures are concatenated in input order (one texture per
/// quad, so `textures[i]` always belongs to quad `i`), and indices are
/// concatenated with a `u32` vertex offset. Fully empty meshes are skipped.
///
/// Each non-empty input must be internally consistent: `vertices.len() % 4 ==
/// 0` and `textures.len() == vertices.len() / 4`. Malformed input panics
/// rather than being filtered out: every internal producer (`mesh_box`,
/// `mesh_chunk`) upholds the contract, so a violation signals a programmer
/// error that silently dropping geometry would only mask.
pub fn merge(meshes: impl IntoIterator<Item = Mesh>) -> Mesh {
    let mut merged = Mesh::default();
    for mesh in meshes {
        if mesh.vertices.is_empty() && mesh.textures.is_empty() {
            continue;
        }
        assert!(
            mesh.vertices.len() % 4 == 0,
            "mesh has {} vertices, not a whole number of quads",
            mesh.vertices.len()
        );
        assert_eq!(
            mesh.textures.len(),
            mesh.vertices.len() / 4,
            "mesh has {} textures for {} quads",
            mesh.textures.len(),
            mesh.vertices.len() / 4
        );
        let base = merged.vertices.len() as u32;
        merged.vertices.extend(mesh.vertices);
        merged.textures.extend(mesh.textures);
        merged
            .indices
            .extend(mesh.indices.iter().map(|index| base + index));
    }
    merged
}

/// Meshes every chunk within `radius` chunks of `center`, where `center` is a
/// block coordinate. Chunks outside the loaded world produce no geometry.
pub fn mesh_around(
    world: &dyn BlockSource,
    models: &dyn ModelSource,
    center: [i32; 2],
    radius: i32,
) -> Mesh {
    // `div_euclid` floors, so negative block coordinates land in the right
    // chunk: chunk -1 spans blocks -16..-1.
    let chunk_x = center[0].div_euclid(16);
    let chunk_z = center[1].div_euclid(16);
    let width = if radius >= 0 {
        radius as usize * 2 + 1
    } else {
        0
    };
    let mut meshes = Vec::with_capacity(width * width);
    for x in chunk_x - radius..=chunk_x + radius {
        for z in chunk_z - radius..=chunk_z + radius {
            meshes.push(mesh_chunk(world, models, x, z));
        }
    }
    merge(meshes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BakedModel, BlockState, Face, Quad, Vertex};
    use std::collections::HashMap;

    fn stone() -> BlockState {
        BlockState::new("minecraft:stone")
    }

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

    /// Bakes the unit cube model for stone and nothing for anything else.
    struct StoneModels;

    impl ModelSource for StoneModels {
        fn model(&self, state: &BlockState) -> Option<BakedModel> {
            if state.name == "minecraft:stone" {
                Some(cube_model())
            } else {
                None
            }
        }
    }

    /// The full 0..16 cube in the 1.16.5 model vertex order: four corners per
    /// face, counter-clockwise as seen from outside, each with the matching
    /// cullface. UVs are sloppily taken from the x/y position; the mesher
    /// only has to preserve them.
    fn cube_model() -> BakedModel {
        let faces: [(Face, [[f32; 3]; 4]); 6] = [
            (
                Face::Down,
                [
                    [0.0, 0.0, 16.0],
                    [0.0, 0.0, 0.0],
                    [16.0, 0.0, 0.0],
                    [16.0, 0.0, 16.0],
                ],
            ),
            (
                Face::Up,
                [
                    [16.0, 16.0, 0.0],
                    [0.0, 16.0, 0.0],
                    [0.0, 16.0, 16.0],
                    [16.0, 16.0, 16.0],
                ],
            ),
            (
                Face::North,
                [
                    [16.0, 0.0, 0.0],
                    [0.0, 0.0, 0.0],
                    [0.0, 16.0, 0.0],
                    [16.0, 16.0, 0.0],
                ],
            ),
            (
                Face::South,
                [
                    [0.0, 0.0, 16.0],
                    [16.0, 0.0, 16.0],
                    [16.0, 16.0, 16.0],
                    [0.0, 16.0, 16.0],
                ],
            ),
            (
                Face::West,
                [
                    [0.0, 0.0, 0.0],
                    [0.0, 0.0, 16.0],
                    [0.0, 16.0, 16.0],
                    [0.0, 16.0, 0.0],
                ],
            ),
            (
                Face::East,
                [
                    [16.0, 0.0, 16.0],
                    [16.0, 0.0, 0.0],
                    [16.0, 16.0, 0.0],
                    [16.0, 16.0, 16.0],
                ],
            ),
        ];
        let texture = "minecraft:block/stone".to_string();
        BakedModel {
            quads: faces
                .into_iter()
                .map(|(face, corners)| Quad {
                    vertices: corners.map(|position| Vertex {
                        position,
                        uv: [position[0], position[1]],
                        normal: face.normal(),
                    }),
                    texture: texture.clone(),
                    cullface: Some(face),
                })
                .collect(),
        }
    }

    /// One hand-built quad at `origin`: four vertices, six indices, one
    /// texture, structured exactly like a `mesh_box` output quad.
    fn quad_mesh(origin: [f32; 3]) -> Mesh {
        Mesh {
            vertices: (0..4)
                .map(|i| Vertex {
                    position: [origin[0] + i as f32, origin[1], origin[2]],
                    uv: [i as f32, 0.0],
                    normal: [0.0, 1.0, 0.0],
                })
                .collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
            textures: vec!["minecraft:block/stone".to_string()],
        }
    }

    #[test]
    fn merge_offsets_indices_and_concatenates_textures() {
        let merged = merge([quad_mesh([0.0, 0.0, 0.0]), quad_mesh([1.0, 0.0, 0.0])]);
        assert_eq!(merged.vertices.len(), 8);
        assert_eq!(merged.textures.len(), 2);
        assert_eq!(merged.indices, vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        // The second quad keeps its own texture and world position.
        assert_eq!(merged.textures[1], "minecraft:block/stone");
        assert!(merged.vertices[4..].iter().all(|v| v.position[0] >= 1.0));
    }

    #[test]
    fn merge_skips_empty_meshes() {
        let quad = quad_mesh([0.0, 0.0, 0.0]);
        let merged = merge([Mesh::default(), quad.clone(), Mesh::default()]);
        assert_eq!(merged, quad);
    }

    #[test]
    #[should_panic(expected = "not a whole number of quads")]
    fn merge_rejects_partial_quads() {
        let mut mesh = quad_mesh([0.0, 0.0, 0.0]);
        mesh.vertices.pop();
        merge([mesh]);
    }

    #[test]
    #[should_panic(expected = "textures for")]
    fn merge_rejects_mismatched_texture_count() {
        let mut mesh = quad_mesh([0.0, 0.0, 0.0]);
        mesh.textures.push("minecraft:block/dirt".to_string());
        merge([mesh]);
    }

    #[test]
    fn mesh_around_floors_negative_centers() {
        let mut world = MapSource::default();
        world.states.insert([-1, 0, 0], stone());
        // Both -1 and -16 floor to chunk -1 and yield the same geometry.
        let from_minus_one = mesh_around(&world, &StoneModels, [-1, 0], 0);
        let from_minus_sixteen = mesh_around(&world, &StoneModels, [-16, 0], 0);
        assert_eq!(from_minus_one, from_minus_sixteen);
        // An isolated cube: six faces, no culling, all inside chunk -1.
        assert_eq!(from_minus_one.vertices.len(), 24);
        assert_eq!(from_minus_one.indices.len(), 36);
        assert_eq!(from_minus_one.textures.len(), 6);
        assert!(
            from_minus_one
                .vertices
                .iter()
                .all(|v| (-1.0..=0.0).contains(&v.position[0]))
        );
        // A center flooring to chunk -2 sees no geometry.
        let empty = mesh_around(&world, &StoneModels, [-17, 0], 0);
        assert_eq!(empty, Mesh::default());
    }

    #[test]
    fn mesh_around_merges_chunks_across_the_origin() {
        let mut world = MapSource::default();
        world.states.insert([0, 0, 0], stone());
        world.states.insert([-1, 0, 0], stone());
        let mesh = mesh_around(&world, &StoneModels, [0, 0], 1);
        // Chunks -1..=1 on both axes. Both cubes appear; the shared east/west
        // faces cull across the chunk border, leaving 10 quads.
        assert_eq!(mesh.vertices.len(), 40);
        assert_eq!(mesh.indices.len(), 60);
        assert_eq!(mesh.textures.len(), 10);
        // Quad i owns vertices 4i..4i+4 and indices 6i..6i+6, so textures
        // stay lined up with their quads after the offset merge.
        for i in 0..mesh.textures.len() {
            assert_eq!(
                &mesh.indices[i * 6..i * 6 + 6],
                &[
                    (4 * i) as u32,
                    (4 * i + 1) as u32,
                    (4 * i + 2) as u32,
                    (4 * i) as u32,
                    (4 * i + 2) as u32,
                    (4 * i + 3) as u32,
                ]
            );
        }
        // World-space positions span both sides of the chunk border.
        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0]).collect();
        let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(min_x, -1.0);
        assert_eq!(max_x, 1.0);
        assert!(xs.iter().any(|&x| x < 0.0) && xs.iter().any(|&x| x >= 0.0));
    }

    #[test]
    fn mesh_around_is_deterministic() {
        let mut world = MapSource::default();
        world.states.insert([0, 0, 0], stone());
        world.states.insert([3, 4, 5], stone());
        let first = mesh_around(&world, &StoneModels, [1, 2], 2);
        let second = mesh_around(&world, &StoneModels, [1, 2], 2);
        assert_eq!(first, second);
        assert!(!first.vertices.is_empty());
    }

    #[test]
    fn empty_world_yields_empty_mesh() {
        let world = MapSource::default();
        let mesh = mesh_around(&world, &StoneModels, [0, 0], 3);
        assert_eq!(mesh, Mesh::default());
    }
}
