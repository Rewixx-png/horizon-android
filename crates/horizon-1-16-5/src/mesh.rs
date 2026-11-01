//! CPU chunk meshing for the 1.16.5 target.
//!
//! It consumes a `BlockSource` and a `ModelSource` and produces the shared
//! `Mesh`: one pass over a box of world blocks, translating each baked
//! model-space quad (0..16) into block units (0..1) and turning it into two
//! triangles.

use crate::light::{BlockLightSource, LightSampler, ambient_occlusion};
use crate::{BlockSource, Face, Mesh, ModelSource, Vertex};

/// The three block states that count as transparent for face culling.
/// Opacity-aware culling (e.g. treating glass, leaves, water as
/// non-occluding) is future work; today every other neighbor hides the face.
const AIR_NAMES: [&str; 3] = ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"];

fn is_air(name: &str) -> bool {
    AIR_NAMES.contains(&name)
}

/// Mesh every block in the box `min..min+size` (block units, exclusive on the
/// top side of each axis).
///
/// Blocks are visited in `x`-major, then `y`, then `z` order and quads are
/// emitted in model order, so the output is deterministic for a given pair of
/// sources. A quad with a `cullface` is dropped when the neighbor in that
/// direction has a known state that is not air/cave_air/void_air; neighbors
/// outside the source never cull; a neighbor outside the box but present in
/// the source may still cull, so adjacent chunk meshes can share faces. Quads are assumed to
/// list their four vertices counter-clockwise as seen from the front, so the
/// fan `(0,1,2),(0,2,3)` preserves winding; UVs (model-space 0..16) and
/// normals are kept verbatim, positions are divided by 16 and offset by the
/// block origin.
pub fn mesh_box(
    source: &dyn BlockSource,
    models: &dyn ModelSource,
    min: [i32; 3],
    size: [u32; 3],
) -> Mesh {
    mesh_impl(source, models, None, min, size).0
}

/// Mesh the box like [`mesh_box`], and additionally sample one light value per
/// vertex.
///
/// The returned `Vec<f32>` is parallel to `Mesh::vertices`: `lights[i]` belongs
/// to `vertices[i]`. Each value is
/// [`LightSampler::vertex_light`](crate::light::LightSampler::vertex_light) for
/// that vertex's block, corner and normal multiplied by
/// [`ambient_occlusion`](crate::light::ambient_occlusion), so it is in
/// `0.05..=1.0`. `light` supplies the sky/block levels and `source` supplies the
/// block states the occlusion test needs. A shader is expected to multiply the
/// sampled texture by this value.
///
/// Faces and vertex order are identical to [`mesh_box`]; the two functions emit
/// the same mesh.
pub fn mesh_box_lit(
    source: &dyn BlockSource,
    models: &dyn ModelSource,
    light: &dyn BlockLightSource,
    min: [i32; 3],
    size: [u32; 3],
) -> (Mesh, Vec<f32>) {
    let (mesh, lights) = mesh_impl(source, models, Some(light), min, size);
    (mesh, lights.unwrap_or_default())
}

fn mesh_impl(
    source: &dyn BlockSource,
    models: &dyn ModelSource,
    light: Option<&dyn BlockLightSource>,
    min: [i32; 3],
    size: [u32; 3],
) -> (Mesh, Option<Vec<f32>>) {
    let mut mesh = Mesh::default();
    let mut lights = light.map(|_| Vec::new());
    for x in 0..size[0] {
        for y in 0..size[1] {
            for z in 0..size[2] {
                let pos = [min[0] + x as i32, min[1] + y as i32, min[2] + z as i32];
                let Some(state) = source.state_at(pos[0], pos[1], pos[2]) else {
                    continue;
                };
                let Some(model) = models.model(&state) else {
                    continue;
                };
                for quad in model.quads {
                    if let Some(face) = quad.cullface {
                        let offset = face.offset();
                        if let Some(neighbor) = source.state_at(
                            pos[0] + offset[0],
                            pos[1] + offset[1],
                            pos[2] + offset[2],
                        ) {
                            if !is_air(&neighbor.name) {
                                continue;
                            }
                        }
                    }
                    let base = mesh.vertices.len() as u32;
                    mesh.textures.push(quad.texture.clone());
                    for vertex in quad.vertices {
                        mesh.vertices.push(Vertex {
                            position: [
                                pos[0] as f32 + vertex.position[0] / 16.0,
                                pos[1] as f32 + vertex.position[1] / 16.0,
                                pos[2] as f32 + vertex.position[2] / 16.0,
                            ],
                            uv: vertex.uv,
                            normal: vertex.normal,
                        });
                        if let (Some(lights), Some(light)) = (&mut lights, light) {
                            let corner = corner_signs(vertex.position);
                            let normal = normal_signs(vertex.normal);
                            let occlusion = ambient_occlusion(source, pos, face_of(normal), corner);
                            let value = LightSampler::vertex_light(light, pos, corner, normal);
                            lights.push(value * occlusion);
                        }
                    }
                    mesh.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                }
            }
        }
    }
    (mesh, lights)
}

/// The corner a model-space vertex sits at, as `-1`/`1` per axis.
fn corner_signs(position: [f32; 3]) -> [i32; 3] {
    position.map(|value| if value < 8.0 { -1 } else { 1 })
}

/// A face normal as `-1`/`0`/`1` per axis.
fn normal_signs(normal: [f32; 3]) -> [i32; 3] {
    normal.map(|value| {
        if value > 0.5 {
            1
        } else if value < -0.5 {
            -1
        } else {
            0
        }
    })
}

/// The face a (sign-only) normal points along.
fn face_of(normal: [i32; 3]) -> Face {
    let [x, y, z] = normal;
    if x != 0 {
        if x < 0 { Face::West } else { Face::East }
    } else if y != 0 {
        if y < 0 { Face::Down } else { Face::Up }
    } else if z < 0 {
        Face::North
    } else {
        Face::South
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BakedModel, BlockState, Face, Quad};
    use std::collections::HashMap;

    fn stone() -> BlockState {
        BlockState::new("minecraft:stone")
    }

    fn air() -> BlockState {
        BlockState::new("minecraft:air")
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

    /// Every triangle's geometric normal (edge cross product) must agree with
    /// its vertex normals, i.e. the fan preserves the quad's winding.
    fn assert_consistent_winding(mesh: &Mesh) {
        assert_eq!(mesh.indices.len() % 3, 0);
        for triangle in mesh.indices.chunks_exact(3) {
            let a = mesh.vertices[triangle[0] as usize].position;
            let b = mesh.vertices[triangle[1] as usize].position;
            let c = mesh.vertices[triangle[2] as usize].position;
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let normal = mesh.vertices[triangle[0] as usize].normal;
            let dot = cross[0] * normal[0] + cross[1] * normal[1] + cross[2] * normal[2];
            assert!(dot > 0.0, "triangle winds opposite to its vertex normal");
        }
    }

    #[test]
    fn lone_cube_yields_six_quads_in_the_unit_box() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        let mesh = mesh_box(&source, &StoneModels, [0, 0, 0], [1, 1, 1]);
        // 6 faces x (4 vertices, 6 indices).
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(mesh.textures.len(), 6);
        assert!(
            mesh.textures
                .iter()
                .all(|texture| texture == "minecraft:block/stone")
        );
        for vertex in &mesh.vertices {
            for component in vertex.position {
                assert!((0.0..=1.0).contains(&component));
            }
            for component in vertex.uv {
                assert!((0.0..=16.0).contains(&component));
            }
        }
        assert_consistent_winding(&mesh);
    }

    #[test]
    fn opaque_neighbor_culls_exactly_the_shared_face() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        source.states.insert([0, 1, 0], stone());
        let mesh = mesh_box(&source, &StoneModels, [0, 0, 0], [1, 1, 1]);
        assert_eq!(mesh.vertices.len(), 20);
        assert_eq!(mesh.indices.len(), 30);
        // Five quads remain: the lower block's up face (all four corners at
        // y = 1.0) is gone while its down face is untouched; side quads only
        // touch y = 1.0 with two of their corners.
        let quads: Vec<&[Vertex]> = mesh.vertices.chunks_exact(4).collect();
        assert_eq!(quads.len(), 5);
        assert!(quads.iter().any(|q| q.iter().all(|v| v.position[1] == 0.0)));
        assert!(!quads.iter().any(|q| q.iter().all(|v| v.position[1] == 1.0)));
        assert_consistent_winding(&mesh);
    }

    #[test]
    fn air_neighbors_never_cull() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        source.states.insert([0, 1, 0], air());
        source
            .states
            .insert([0, 0, -1], BlockState::new("minecraft:cave_air"));
        source
            .states
            .insert([-1, 0, 0], BlockState::new("minecraft:void_air"));
        let mesh = mesh_box(&source, &StoneModels, [0, 0, 0], [1, 1, 1]);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(mesh.textures.len(), 6);
        assert!(
            mesh.textures
                .iter()
                .all(|texture| texture == "minecraft:block/stone")
        );
    }

    #[test]
    fn two_block_column_culls_the_shared_pair() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        source.states.insert([0, 1, 0], stone());
        let mesh = mesh_box(&source, &StoneModels, [0, 0, 0], [1, 2, 1]);
        // 2 x 6 faces minus the up face of the lower block and the down face
        // of the upper block.
        assert_eq!(mesh.vertices.len(), 40);
        assert_eq!(mesh.indices.len(), 60);
        assert_consistent_winding(&mesh);
    }

    #[test]
    fn adjacent_cubes_are_deterministic_and_cull_the_shared_face() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        source.states.insert([1, 0, 0], stone());
        let first = mesh_box(&source, &StoneModels, [0, 0, 0], [2, 1, 1]);
        let second = mesh_box(&source, &StoneModels, [0, 0, 0], [2, 1, 1]);
        assert_eq!(first, second);
        assert_eq!(first.vertices.len(), 40); // 2 cubes, one shared face culled
        assert_eq!(first.indices.len(), 60);
    }

    /// Open sky everywhere, no block light.
    struct OpenSky;

    impl BlockLightSource for OpenSky {
        fn light_at(&self, _x: i32, _y: i32, _z: i32) -> Option<[u8; 2]> {
            Some([15, 0])
        }
    }

    #[test]
    fn lit_mesh_matches_mesh_box_vertex_for_vertex() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        source.states.insert([0, 1, 0], stone());
        let plain = mesh_box(&source, &StoneModels, [0, 0, 0], [1, 2, 1]);
        let (lit, lights) = mesh_box_lit(&source, &StoneModels, &OpenSky, [0, 0, 0], [1, 2, 1]);
        assert_eq!(lit, plain);
        assert_eq!(lights.len(), lit.vertices.len());
        // Open sky with no obstructing blocks is unshadowed.
        assert!(lights.iter().all(|value| (value - 1.0).abs() < 1e-6));
    }

    #[test]
    fn lit_mesh_applies_ambient_occlusion() {
        let mut source = MapSource::default();
        source.states.insert([0, 0, 0], stone());
        // Touches the up face's (+x, +z) corners without sharing a face.
        source.states.insert([1, 1, 0], stone());
        let (mesh, lights) = mesh_box_lit(&source, &StoneModels, &OpenSky, [0, 0, 0], [1, 1, 1]);
        assert_eq!(lights.len(), mesh.vertices.len());
        assert!(
            lights.iter().any(|value| (value - 0.8).abs() < 1e-6),
            "expected a shaded corner: {lights:?}"
        );
        assert!(lights.iter().all(|value| *value >= 0.05 && *value <= 1.0));
    }
}
