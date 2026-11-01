//! The offline (standalone) world generator for the 1.16.5 target.
//!
//! It is deliberately not Minecraft's generator: it exists so the client has
//! something to walk on when no world directory and no server are given (the
//! default Android launch). The result is an ordinary [`World`], so break and
//! place edit the same sections the mesher and the saver read.
//!
//! Terrain comes from two octaves of value noise: a base plateau at
//! [`BASE_Y`], hills up to [`MAX_SURFACE_Y`], water filling everything at or
//! below [`SEA_LEVEL`], sand where the ground meets the water, and
//! procedurally placed oaks. Every choice is a pure function of the seed and
//! the block coordinates, so two runs produce the same world.

use crate::BlockSource;
use crate::BlockTable;
use crate::types::BlockState;
use crate::world::{ChunkColumn, World};

/// The flat plateau every column starts from.
pub const BASE_Y: i32 = 64;
/// The highest hill a column can reach.
pub const MAX_SURFACE_Y: i32 = 82;
/// Water level: every air block at or below it becomes water.
pub const SEA_LEVEL: i32 = 62;
/// The highest ground that still reads as a beach.
const BEACH_TOP_Y: i32 = 63;
/// Dirt thickness under the surface block.
const DIRT_DEPTH: i32 = 4;
/// Trees sit on a 6x6 cell grid, at most one per cell.
const TREE_CELL: i32 = 6;
/// Chance in 0..100 that a cell holds a tree.
const TREE_CHANCE: u32 = 34;
/// The lowest ground a tree can grow on (keeps trees out of the water).
const TREE_MIN_GROUND: i32 = SEA_LEVEL + 2;

/// The seed the standalone world uses; `--seed` picks another one.
pub const DEFAULT_SEED: u64 = 0x0031_1650_C0FF_EE11;

/// Per-column terrain: the y of the top solid block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terrain {
    /// The y of the column's top solid block.
    pub surface: i32,
}

/// Integer hash of a coordinate pair and a seed (xorshift* finaliser).
fn hash2(x: i32, z: i32, seed: u64) -> u32 {
    let mut h = seed
        ^ (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (z as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    (h ^ (h >> 31)) as u32
}

/// A smooth `0..=1` interpolation weight.
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Value noise in `0..=1` at fractional coordinates.
fn value_noise(x: f32, z: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let z0 = z.floor() as i32;
    let tx = smooth(x - x0 as f32);
    let tz = smooth(z - z0 as f32);
    let sample = |dx: i32, dz: i32| hash2(x0 + dx, z0 + dz, seed) as f32 / u32::MAX as f32;
    let (a, b) = (sample(0, 0), sample(1, 0));
    let (c, d) = (sample(0, 1), sample(1, 1));
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * tz
}

/// Two octaves: broad hills with a finer ripple on top.
fn height_noise(x: i32, z: i32, seed: u64) -> f32 {
    let broad = value_noise(x as f32 / 96.0, z as f32 / 96.0, seed);
    let detail = value_noise(x as f32 / 28.0, z as f32 / 28.0, seed ^ 0x5DEE_CE66);
    broad * 0.8 + detail * 0.2
}

/// The terrain of the column at world coordinates `(x, z)`.
pub fn column_at(x: i32, z: i32, seed: u64) -> Terrain {
    let n = height_noise(x, z, seed);
    // The plateau holds most of the map; only the noise crests lift hills.
    let lift = (n.powi(2) * (MAX_SURFACE_Y - BASE_Y) as f32).round() as i32;
    Terrain {
        surface: BASE_Y + lift.clamp(0, MAX_SURFACE_Y - BASE_Y),
    }
}

/// The trunk base of the tree growing at `(x, z)`, when one does.
///
/// A tree belongs to the first column of its 6x6 cell, so its position never
/// depends on generation order or on how far the radius happens to reach.
pub fn tree_at(x: i32, z: i32, seed: u64) -> Option<(i32, i32)> {
    let cell = (x.div_euclid(TREE_CELL), z.div_euclid(TREE_CELL));
    if hash2(cell.0, cell.1, seed ^ 0x7F4A_7C15) % 100 >= TREE_CHANCE {
        return None;
    }
    let origin = (cell.0 * TREE_CELL, cell.1 * TREE_CELL);
    (origin == (x, z)).then_some(origin)
}

/// The block ids one generator run resolves once from the table.
struct Palette {
    air: u32,
    bedrock: u32,
    stone: u32,
    dirt: u32,
    grass: u32,
    sand: u32,
    water: u32,
    log: u32,
    leaves: u32,
}

impl Palette {
    fn new(table: &BlockTable) -> Result<Self, String> {
        let id = |name: &str| -> Result<u32, String> {
            let state = table
                .default_state(name)
                .ok_or_else(|| format!("{name} is not in the 1.16.5 block table"))?;
            table
                .state_id(&state)
                .ok_or_else(|| format!("{name} is not in the 1.16.5 block table"))
        };
        Ok(Self {
            air: id("minecraft:air")?,
            bedrock: id("minecraft:bedrock")?,
            stone: id("minecraft:stone")?,
            dirt: id("minecraft:dirt")?,
            grass: id("minecraft:grass_block")?,
            sand: id("minecraft:sand")?,
            water: id("minecraft:water")?,
            log: id("minecraft:oak_log")?,
            leaves: id("minecraft:oak_leaves")?,
        })
    }
}

/// Whether the ground at `surface` is low enough to read as a beach.
fn is_beach(surface: i32) -> bool {
    surface <= BEACH_TOP_Y
}

/// Writes the ground, sub-surface and water of one world column into
/// `column`; `x, z` are world coordinates.
fn write_terrain(palette: &Palette, column: &mut ChunkColumn, x: i32, z: i32, surface: i32) {
    let surface = surface.clamp(1, 254);
    let sandy = is_beach(surface);
    let set = |column: &mut ChunkColumn, y: i32, id: u32| {
        if !(0..256).contains(&y) {
            return;
        }
        if let Some(blocks) = column.section_mut((y as usize) >> 4) {
            let index = ((y as usize & 15) << 8) | ((z as usize & 15) << 4) | (x as usize & 15);
            blocks[index] = id;
        }
    };
    set(column, 0, palette.bedrock);
    for y in 1..=surface {
        let id = if y == surface {
            if sandy { palette.sand } else { palette.grass }
        } else if y > surface - DIRT_DEPTH {
            if sandy { palette.sand } else { palette.dirt }
        } else {
            palette.stone
        };
        set(column, y, id);
    }
    for y in (surface + 1)..=SEA_LEVEL {
        set(column, y, palette.water);
    }
}

/// Grows one oak with its trunk base at `(x, z)` and its ground at `ground`.
///
/// The trunk is 4 or 5 `oak_log` blocks; the canopy is 5x5, 5x5 and 3x3
/// layers of `oak_leaves` with the 5x5 corners rounded off, like vanilla's
/// oak, plus the top leaf above the trunk.
fn grow_tree(world: &mut World, palette: &Palette, x: i32, z: i32, ground: i32, seed: u64) {
    let height = 4 + (hash2(x, z, seed ^ 0xBEEF) % 2) as i32;
    let mut put = |x: i32, y: i32, z: i32, id: u32| {
        if let Err(error) = world.set_state_id(x, y, z, id) {
            eprintln!("worldgen: ({x},{y},{z}): {error}");
        }
    };
    let crown = ground + height;
    // Leaves first, trunk last: the canopy layers wrap the trunk's upper part,
    // so the logs have to be written after them or they would be overwritten.
    for (layer_y, radius) in [(crown - 2, 2i32), (crown - 1, 2), (crown, 1)] {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if radius == 2 && dx.abs() == 2 && dz.abs() == 2 {
                    continue;
                }
                put(x + dx, layer_y, z + dz, palette.leaves);
            }
        }
    }
    put(x, crown + 1, z, palette.leaves);
    for y in 1..=height {
        put(x, ground + y, z, palette.log);
    }
}

/// Generates terrain over `radius` chunks around the origin and plants trees.
///
/// Every column is written before any tree, so a canopy crossing a chunk
/// border still lands on solid ground. Returns the number of chunks written.
pub fn generate(world: &mut World, radius: i32, seed: u64) -> Result<usize, String> {
    let radius = radius.max(1);
    let palette = Palette::new(&world.table)?;
    let mut written = 0;
    let mut trees: Vec<(i32, i32, i32)> = Vec::new();
    for chunk_x in -radius..=radius {
        for chunk_z in -radius..=radius {
            let mut column = ChunkColumn::empty(chunk_x, chunk_z);
            for section in 0..=((MAX_SURFACE_Y + 1) as usize >> 4) {
                if let Some(blocks) = column.section_mut(section) {
                    blocks.fill(palette.air);
                }
            }
            for z in 0..16i32 {
                for x in 0..16i32 {
                    let world_x = chunk_x * 16 + x;
                    let world_z = chunk_z * 16 + z;
                    let terrain = column_at(world_x, world_z, seed);
                    write_terrain(&palette, &mut column, world_x, world_z, terrain.surface);
                    if terrain.surface > TREE_MIN_GROUND
                        && let Some((tree_x, tree_z)) = tree_at(world_x, world_z, seed)
                    {
                        trees.push((tree_x, tree_z, terrain.surface));
                    }
                }
            }
            world.insert_column(column)?;
            written += 1;
        }
    }
    for (x, z, ground) in trees {
        // A canopy reaches two blocks out; a tree on the square's edge would
        // otherwise lose half its leaves in unloaded chunks.
        if !in_loaded_square(world, x - 2, z - 2) || !in_loaded_square(world, x + 2, z + 2) {
            continue;
        }
        grow_tree(world, &palette, x, z, ground, seed);
    }
    Ok(written)
}

/// Whether the chunk holding `(x, z)` was generated.
fn in_loaded_square(world: &World, x: i32, z: i32) -> bool {
    world.state_at(x, 0, z).is_some()
}

/// The y a player at `(x, z)` should spawn on: one above the top solid block.
pub fn spawn_height(world: &World, x: i32, z: i32) -> i32 {
    let mut highest = BASE_Y;
    for y in 0..256 {
        let Some(state) = world.state_at(x, y, z) else {
            break;
        };
        if crate::blocks::is_solid(&state.name) {
            highest = y;
        }
    }
    highest + 1
}

/// The default standalone world: a generated square with `radius` chunks.
pub fn fill_flat(world: &mut World, radius: i32) -> Result<usize, String> {
    generate(world, radius, DEFAULT_SEED)
}

/// The state type of the block table, kept in scope for the fixtures below.
#[allow(dead_code)]
fn _state(name: &str) -> BlockState {
    BlockState::new(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny table with the blocks the generator writes.
    fn table() -> BlockTable {
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0}]},
                "minecraft:bedrock": {"states": [{"id": 1}]},
                "minecraft:stone": {"states": [{"id": 2}]},
                "minecraft:dirt": {"states": [{"id": 3}]},
                "minecraft:grass_block": {"states": [{"id": 4}]},
                "minecraft:sand": {"states": [{"id": 5}]},
                "minecraft:water": {"states": [{"id": 6}]},
                "minecraft:oak_log": {"states": [{"id": 7}]},
                "minecraft:oak_leaves": {"states": [{"id": 8}]},
            }},
        });
        let path = std::env::temp_dir().join(format!(
            "horizon-1165-worldgen-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let table = BlockTable::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        table
    }

    fn world(radius: i32) -> World {
        let mut world = World::empty(table());
        generate(&mut world, radius, DEFAULT_SEED).unwrap();
        world
    }

    #[test]
    fn terrain_stays_between_the_plateau_and_the_hill_top() {
        for (x, z) in [(0, 0), (7, -3), (500, 500), (-913, 44)] {
            let surface = column_at(x, z, DEFAULT_SEED).surface;
            assert!((BASE_Y..=MAX_SURFACE_Y).contains(&surface), "{surface}");
        }
    }

    #[test]
    fn terrain_is_deterministic_and_seeded() {
        assert_eq!(
            column_at(12, -34, DEFAULT_SEED),
            column_at(12, -34, DEFAULT_SEED)
        );
        let a: i32 = (-20..20)
            .map(|x| column_at(x, 5, DEFAULT_SEED).surface)
            .sum();
        let b: i32 = (-20..20).map(|x| column_at(x, 5, 99).surface).sum();
        assert_ne!(a, b, "a different seed reshapes the hills");
    }

    #[test]
    fn bedrock_stone_dirt_and_grass_layers_stack() {
        let world = world(1);
        let surface = column_at(0, 0, DEFAULT_SEED).surface;
        let at = |y: i32| {
            world
                .state_at(0, y, 0)
                .map(|state| state.name)
                .unwrap_or_default()
        };
        assert_eq!(at(0), "minecraft:bedrock");
        if surface > BEACH_TOP_Y {
            assert_eq!(at(surface - DIRT_DEPTH), "minecraft:stone");
            assert_eq!(at(surface - 1), "minecraft:dirt");
            assert_eq!(at(surface), "minecraft:grass_block");
        }
    }

    #[test]
    fn water_fills_every_air_block_up_to_the_sea_level() {
        let world = world(1);
        let surface = column_at(0, 0, DEFAULT_SEED).surface;
        if surface < SEA_LEVEL {
            for y in (surface + 1)..=SEA_LEVEL {
                assert_eq!(
                    world.state_at(0, y, 0).unwrap().name,
                    "minecraft:water",
                    "y {y}"
                );
            }
            assert_eq!(
                world.state_at(0, SEA_LEVEL + 1, 0).unwrap().name,
                "minecraft:air"
            );
        }
        // A hill above the sea stays dry.
        assert_eq!(
            world
                .state_at(0, MAX_SURFACE_Y, 0)
                .map(|s| s.name)
                .unwrap_or_default(),
            "minecraft:air"
        );
    }

    #[test]
    fn beaches_are_sand_at_the_waterline() {
        let world = world(1);
        for x in -16..16 {
            for z in -16..16 {
                let surface = column_at(x, z, DEFAULT_SEED).surface;
                let top = world.state_at(x, surface, z).unwrap().name;
                if surface <= BEACH_TOP_Y && surface <= SEA_LEVEL + 1 {
                    assert_eq!(top, "minecraft:sand", "({x},{z}) surface {surface}");
                }
            }
        }
    }

    #[test]
    fn oaks_have_a_trunk_and_a_three_layer_canopy() {
        let mut world = World::empty(table());
        generate(&mut world, 2, DEFAULT_SEED).unwrap();
        let mut checked = 0;
        for x in -32..32 {
            for z in -32..32 {
                let Some((tx, tz)) = tree_at(x, z, DEFAULT_SEED) else {
                    continue;
                };
                let ground = column_at(tx, tz, DEFAULT_SEED).surface;
                if ground <= TREE_MIN_GROUND {
                    continue;
                }
                let at = |x: i32, y: i32, z: i32| {
                    world.state_at(x, y, z).map(|s| s.name).unwrap_or_default()
                };
                // Trunk: 4 or 5 logs, then the canopy above.
                let mut trunk = 0;
                while at(tx, ground + 1 + trunk, tz) == "minecraft:oak_log" {
                    trunk += 1;
                }
                assert!((4..=5).contains(&trunk), "trunk {trunk} at ({tx},{tz})");
                let crown = ground + trunk;
                assert_eq!(at(tx, crown + 1, tz), "minecraft:oak_leaves");
                // Layer corners are rounded off.
                assert_eq!(at(tx - 2, crown - 2, tz - 2), "minecraft:air");
                assert_eq!(at(tx - 1, crown - 2, tz), "minecraft:oak_leaves");
                assert_eq!(at(tx - 1, crown, tz - 1), "minecraft:oak_leaves");
                checked += 1;
            }
        }
        assert!(checked > 0, "the test area has trees");
    }

    #[test]
    fn spawn_height_lands_on_the_surface() {
        let world = world(1);
        let y = spawn_height(&world, 0, 0);
        let below = world.state_at(0, y - 1, 0).unwrap();
        assert!(crate::blocks::is_solid(&below.name));
        let standing = world.state_at(0, y, 0).unwrap();
        assert!(crate::blocks::is_air(&standing.name));
        assert_eq!(y, column_at(0, 0, DEFAULT_SEED).surface.max(SEA_LEVEL) + 1);
    }

    #[test]
    fn generated_chunks_are_editable_and_dirty() {
        let mut world = world(1);
        let air = BlockState::new("minecraft:air");
        let surface = column_at(0, 0, DEFAULT_SEED).surface;
        world.set_state(0, surface, 0, &air).unwrap();
        assert!(world.has_unsaved_changes());
    }
}
