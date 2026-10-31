//! Multi-chunk mesh building for the 1.16.5 target.

use crate::assets::ModelPack;
use crate::world::World;
use crate::{BlockSource, BlockState, BlockTable, Mesh, ModelSource};
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

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
    let chunks = chunks_around(center, radius);
    let mut meshes = Vec::with_capacity(chunks.len());
    for [x, z] in chunks {
        meshes.push(mesh_chunk(world, models, x, z));
    }
    merge(meshes)
}

/// The chunks [`mesh_around`] visits for a block-column `center`, in the
/// order it visits them.
pub fn chunks_around(center: [i32; 2], radius: i32) -> Vec<[i32; 2]> {
    if radius < 0 {
        return Vec::new();
    }
    let chunk_x = center[0].div_euclid(16);
    let chunk_z = center[1].div_euclid(16);
    let width = radius as usize * 2 + 1;
    let mut chunks = Vec::with_capacity(width * width);
    for x in chunk_x - radius..=chunk_x + radius {
        for z in chunk_z - radius..=chunk_z + radius {
            chunks.push([x, z]);
        }
    }
    chunks
}

/// Blocks in one 16 x 256 x 16 chunk column.
const CHUNK_VOLUME: usize = 16 * 256 * 16;
/// Sentinel for a block the captured source did not know. Vanilla block
/// tables stay well below 65535 states, so this id is never a real state.
const MISSING: u16 = u16::MAX;

/// Index of a block in a snapshot column: Y outermost, then Z, then X.
fn chunk_index(x: usize, y: usize, z: usize) -> usize {
    (y << 8) | (z << 4) | x
}

/// How a [`WorldSnapshot`] turns ids back into block states.
enum SnapshotStates {
    /// States interned while capturing from an arbitrary [`BlockSource`].
    Interned(Vec<BlockState>),
    /// The world's own block-table ids, exactly as [`World`] stores them.
    Table(Arc<BlockTable>),
}

/// An immutable, `Send + Sync` copy of the blocks one mesh pass needs.
///
/// Capturing queries the source once per block; the mesher can then run on a
/// worker thread without touching the live world. A position outside every
/// captured chunk is `None`, and a hole the source did not know inside a
/// captured chunk stays `None` as well.
pub struct WorldSnapshot {
    chunks: HashMap<[i32; 2], Vec<u16>>,
    states: SnapshotStates,
}

impl WorldSnapshot {
    /// Captures `chunks` from any [`BlockSource`], interning each distinct
    /// state it returns.
    ///
    /// This is the general form used by tests and by sources that only expose
    /// [`BlockSource`]. A [`World`] should use [`Self::capture_from_world`],
    /// which copies its existing block-table ids instead of cloning a
    /// [`BlockState`] for every block.
    pub fn capture(world: &dyn BlockSource, chunks: &[[i32; 2]]) -> Self {
        let mut states = vec![BlockState::new("minecraft:air")];
        let mut ids: HashMap<BlockState, u16> = HashMap::new();
        ids.insert(states[0].clone(), 0);
        let mut captured = HashMap::with_capacity(chunks.len());
        for &chunk in chunks {
            let mut data = vec![MISSING; CHUNK_VOLUME];
            for y in 0..256i32 {
                for z in 0..16i32 {
                    for x in 0..16i32 {
                        let world_x = chunk[0] * 16 + x;
                        let world_z = chunk[1] * 16 + z;
                        let Some(state) = world.state_at(world_x, y, world_z) else {
                            continue;
                        };
                        let id = match ids.get(&state) {
                            Some(&id) => id,
                            None => {
                                let Ok(id) = u16::try_from(states.len()) else {
                                    continue;
                                };
                                if id == MISSING {
                                    continue;
                                }
                                ids.insert(state.clone(), id);
                                states.push(state);
                                id
                            }
                        };
                        data[chunk_index(x as usize, y as usize, z as usize)] = id;
                    }
                }
            }
            // An all-unknown chunk is indistinguishable from a chunk the
            // source never loaded, which `BlockSource` reports as `None`.
            if data.iter().all(|&id| id == MISSING) {
                continue;
            }
            captured.insert(chunk, data);
        }
        Self {
            chunks: captured,
            states: SnapshotStates::Interned(states),
        }
    }

    /// Captures directly from a loaded world using the block-table ids in its
    /// chunk sections. `table` must be the world's own table; the app shares
    /// one `Arc` with every snapshot instead of cloning it per remesh.
    pub fn capture_from_world(world: &World, table: &Arc<BlockTable>, chunks: &[[i32; 2]]) -> Self {
        let air = table
            .state_id(&BlockState::new("minecraft:air"))
            .unwrap_or(0);
        let mut captured = HashMap::with_capacity(chunks.len());
        for &chunk in chunks {
            let Some(column) = world.chunk(chunk[0], chunk[1]) else {
                // A chunk the world never loaded reads as `None` through
                // `BlockSource`, so leave it out of the snapshot too.
                continue;
            };
            let mut data = vec![air; CHUNK_VOLUME];
            for section in 0..16 {
                if let Some(blocks) = column.section(section) {
                    let start = section * 4096;
                    data[start..start + 4096].copy_from_slice(blocks);
                }
            }
            captured.insert(chunk, data);
        }
        Self {
            chunks: captured,
            states: SnapshotStates::Table(table.clone()),
        }
    }

    fn state(&self, id: u16) -> Option<BlockState> {
        match &self.states {
            SnapshotStates::Interned(states) => states.get(usize::from(id)).cloned(),
            SnapshotStates::Table(table) => table.state(id).cloned(),
        }
    }
}

impl BlockSource for WorldSnapshot {
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let column = self.chunks.get(&[x.div_euclid(16), z.div_euclid(16)])?;
        if !(0..256).contains(&y) {
            // `World` reports the canonical air state outside the column,
            // regardless of how the table spells its own air entry.
            return Some(BlockState::new("minecraft:air"));
        }
        let index = chunk_index(
            x.rem_euclid(16) as usize,
            y as usize,
            z.rem_euclid(16) as usize,
        );
        let id = column[index];
        if id == MISSING {
            return None;
        }
        Some(
            self.state(id)
                .unwrap_or_else(|| BlockState::new("minecraft:air")),
        )
    }
}

/// The worker's pure meshing entry point. Kept as a separate name so the
/// background path cannot accidentally diverge from [`mesh_around`].
pub fn mesh_snapshot(
    snapshot: &dyn BlockSource,
    models: &dyn ModelSource,
    center: [i32; 2],
    radius: i32,
) -> Mesh {
    mesh_around(snapshot, models, center, radius)
}

/// One chunk remesh request handed to the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshJob {
    /// Block position the mesh is centred on.
    pub center: [i32; 2],
    /// Chunk radius around `center`.
    pub radius: i32,
    /// Monotonic generation; installation keeps the newest one.
    pub generation: u64,
}

/// A finished background mesh and the metadata the render thread installs.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshResult {
    pub mesh: Mesh,
    pub textures: Vec<String>,
    pub generation: u64,
    /// Wall-clock time the mesher spent on this job.
    pub millis: u64,
}

struct WorkerJob {
    snapshot: Arc<WorldSnapshot>,
    job: MeshJob,
}

/// A `ModelPack` moved into the single meshing thread.
///
/// `ModelPack` memoizes bakes in a `RefCell`, which is `!Sync`. The worker is
/// the only owner of the pack after construction: [`MeshWorker::new`] moves
/// the caller's `Arc` into the thread, and `app` never bakes on the render
/// thread again after the initial synchronous mesh. Under that invariant no
/// two threads touch the cache at once, which is what moving it requires.
struct WorkerModels(Arc<ModelPack>);

// SAFETY: the wrapper is only constructed by `MeshWorker::new`, which moves
// the sole `Arc` into the worker thread. Until that thread finishes, the pack
// is not used from any other thread (see the invariant above).
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for WorkerModels {}

impl ModelSource for WorkerModels {
    fn model(&self, state: &BlockState) -> Option<crate::BakedModel> {
        self.0.model(state)
    }
}

/// Background chunk mesher: a single FIFO thread with a result channel.
///
/// The render thread sends snapshots and keeps the previous mesh visible;
/// [`Self::try_recv`] only moves finished meshes into the renderer. At most
/// one job needs to be in flight for correctness, though the worker itself
/// processes every request it is given.
pub struct MeshWorker {
    jobs: mpsc::Sender<WorkerJob>,
    results: mpsc::Receiver<MeshResult>,
}

impl MeshWorker {
    /// Starts the worker. The pack is moved into the thread and must not be
    /// used from the render thread afterwards.
    pub fn new(models: Arc<ModelPack>) -> Self {
        let (jobs, job_rx) = mpsc::channel::<WorkerJob>();
        let (result_tx, results) = mpsc::channel::<MeshResult>();
        let models = WorkerModels(models);
        let spawned = thread::Builder::new()
            .name(String::from("horizon-1165-mesh"))
            .spawn(move || {
                while let Ok(WorkerJob { snapshot, job }) = job_rx.recv() {
                    let started = Instant::now();
                    let mesh = mesh_snapshot(&*snapshot, &models, job.center, job.radius);
                    let textures = mesh.textures.clone();
                    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let result = MeshResult {
                        mesh,
                        textures,
                        generation: job.generation,
                        millis,
                    };
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            // `new` cannot fail up the stack without a thread. Keep the
            // handles so the app still runs on its initial synchronous mesh.
            eprintln!("starting the chunk mesh worker: {error}");
        }
        Self { jobs, results }
    }

    /// Queues one remesh. The snapshot is captured by the caller so the
    /// render thread never reads the live world from here.
    pub fn request(
        &self,
        snapshot: Arc<WorldSnapshot>,
        center: [i32; 2],
        radius: i32,
        generation: u64,
    ) {
        let job = MeshJob {
            center,
            radius,
            generation,
        };
        let _ = self.jobs.send(WorkerJob { snapshot, job });
    }

    /// A finished mesh, if the worker produced one since the last call.
    pub fn try_recv(&self) -> Option<MeshResult> {
        self.results.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::ModelPack;
    use crate::{BakedModel, BlockState, Face, Quad, Vertex};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

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

    #[test]
    fn chunks_around_matches_mesh_around_order() {
        assert_eq!(
            chunks_around([0, 0], 1),
            vec![
                [-1, -1],
                [-1, 0],
                [-1, 1],
                [0, -1],
                [0, 0],
                [0, 1],
                [1, -1],
                [1, 0],
                [1, 1]
            ]
        );
        // Negative block columns floor into the matching chunk.
        assert_eq!(chunks_around([-1, 16], 0), vec![[-1, 1]]);
        assert!(chunks_around([0, 0], -1).is_empty());
    }

    #[test]
    fn world_snapshot_returns_captured_states_and_none_outside() {
        let mut world = MapSource::default();
        world.states.insert([1, 2, 3], stone());
        world.states.insert([-1, 0, 2], stone());
        let snapshot = WorldSnapshot::capture(&world, &[[0, 0], [-1, 0]]);

        assert_eq!(snapshot.state_at(1, 2, 3), Some(stone()));
        assert_eq!(snapshot.state_at(-1, 0, 2), Some(stone()));
        // A hole the source did not know inside a captured chunk stays None.
        assert_eq!(snapshot.state_at(1, 2, 4), None);
        // Chunks that were not captured are outside the snapshot.
        assert_eq!(snapshot.state_at(16, 2, 3), None);
        assert_eq!(snapshot.state_at(-17, 0, 2), None);
        // Outside the 0..256 column a captured chunk reads as air, like World.
        assert_eq!(
            snapshot.state_at(1, 256, 3),
            Some(BlockState::new("minecraft:air"))
        );
    }

    #[test]
    fn world_snapshot_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WorldSnapshot>();
    }

    #[test]
    fn mesh_snapshot_matches_mesh_around() {
        let mut world = MapSource::default();
        world.states.insert([1, 2, 3], stone());
        world.states.insert([-1, 0, 2], stone());
        world.states.insert([0, 0, 0], stone());
        let chunks = chunks_around([0, 0], 1);
        let snapshot = WorldSnapshot::capture(&world, &chunks);

        let direct = mesh_around(&world, &StoneModels, [0, 0], 1);
        let through_snapshot = mesh_snapshot(&snapshot, &StoneModels, [0, 0], 1);
        assert_eq!(through_snapshot, direct);
        assert!(!through_snapshot.vertices.is_empty());
    }

    /// An empty but valid model pack; the worker test only cares about the
    /// order results arrive in, not about baked geometry.
    fn empty_model_pack() -> ModelPack {
        let root = std::env::temp_dir().join(format!(
            "horizon-1165-mesh-pack-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(root.join("blockstates")).unwrap();
        std::fs::create_dir_all(root.join("models")).unwrap();
        let pack = ModelPack::load(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        pack
    }

    fn next_result(worker: &MeshWorker) -> MeshResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = worker.try_recv() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "the mesh worker produced no result"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn mesh_worker_returns_generations_in_order() {
        #[allow(clippy::arc_with_non_send_sync)]
        let worker = MeshWorker::new(Arc::new(empty_model_pack()));
        let snapshot = Arc::new(WorldSnapshot::capture(&MapSource::default(), &[[0, 0]]));
        worker.request(snapshot.clone(), [0, 0], 0, 1);
        worker.request(snapshot, [0, 0], 0, 2);

        let first = next_result(&worker);
        let second = next_result(&worker);
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert_eq!(first.textures, first.mesh.textures);
        assert_eq!(second.textures, second.mesh.textures);
    }
}
