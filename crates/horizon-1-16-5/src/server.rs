//! Headless integrated server/player loop for the 1.16.5 target.
//!
//! [`IntegratedServer::open`] loads a world and drops a creative player onto
//! the surface at block column (0, 0); [`IntegratedServer::tick`] advances
//! one 1/20 s of look, movement and break/place interaction. The tests below
//! build a small 1.16.5 region fixture with the same public `horizon_world`
//! writers the `world.rs` tests use, so `open` spawning and the tick actions
//! are covered without touching `world.rs`.

use crate::blocks;
use crate::interact;
use crate::inventory::Inventory;
use crate::player::Player;
use crate::world::World;
use crate::{BlockSource, BlockState, Mesh, ModelSource, chunk_mesh};

/// Block reach of break/place interactions.
const REACH: f64 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameInput {
    pub movement: crate::player::Input,
    /// Absolute yaw/pitch in degrees; `None` keeps the current look.
    pub look: Option<[f32; 2]>,
    pub break_block: bool,
    pub place_block: bool,
    pub select: Option<usize>,
    pub scroll: i32,
}

pub struct IntegratedServer {
    pub world: World,
    pub player: Player,
    pub inventory: Inventory,
    pub tick_count: u64,
    /// Set when a block changed and the visible mesh must be rebuilt.
    pub dirty: bool,
}

impl IntegratedServer {
    /// Spawns at the first free block above the surface at `(0, 0)`.
    pub fn open(world: World) -> Self {
        // Bottom-up scan for the highest solid block in the spawn column.
        // A missing chunk (or an all-air column) falls back to y 64.
        let mut highest: Option<i32> = None;
        for y in 0..256 {
            let Some(state) = world.state_at(0, y, 0) else {
                break;
            };
            if blocks::is_solid(&state.name) {
                highest = Some(y);
            }
        }
        let spawn_y = highest.map_or(64.0, |y| f64::from(y) + 1.001);
        Self::new(world, [0.5, spawn_y, 0.5])
    }

    pub fn new(world: World, spawn: [f64; 3]) -> Self {
        Self {
            world,
            player: Player::new(spawn),
            inventory: Inventory::creative(),
            tick_count: 0,
            dirty: false,
        }
    }

    /// One 1/20 s tick: look, movement, then block actions.
    pub fn tick(&mut self, input: &FrameInput) {
        if let Some([yaw, pitch]) = input.look {
            self.player.yaw = yaw;
            self.player.pitch = pitch.clamp(-90.0, 90.0);
        }
        if let Some(slot) = input.select {
            self.inventory.select(slot);
        }
        if input.scroll != 0 {
            self.inventory.scroll(input.scroll);
        }
        self.player.tick(&self.world, &input.movement);

        if input.break_block {
            self.break_target();
        }
        if input.place_block {
            self.place_target();
        }
        self.tick_count += 1;
    }

    pub fn render_mesh(&self, models: &dyn ModelSource, radius: i32) -> Mesh {
        let center = [
            self.player.position[0].floor() as i32,
            self.player.position[2].floor() as i32,
        ];
        chunk_mesh::mesh_around(&self.world, models, center, radius)
    }

    /// Breaks the solid block under the crosshair, if one is in reach.
    fn break_target(&mut self) {
        if let Some(hit) =
            interact::raycast(&self.world, self.player.eye(), self.player.look(), REACH)
        {
            let air = BlockState::new("minecraft:air");
            if self
                .world
                .set_state(hit.position[0], hit.position[1], hit.position[2], &air)
                .is_ok()
            {
                self.dirty = true;
            }
        }
    }

    /// Places the held block against the block under the crosshair, if the
    /// cell it would fill is air and a block is held.
    fn place_target(&mut self) {
        let Some(held) = self.inventory.held().cloned() else {
            return;
        };
        let Some(hit) =
            interact::raycast(&self.world, self.player.eye(), self.player.look(), REACH)
        else {
            return;
        };
        let target = interact::placement(&hit);
        let is_air = self
            .world
            .state_at(target[0], target[1], target[2])
            .is_some_and(|state| blocks::is_air(&state.name));
        // A hotbar state without the block's optional properties (grass's
        // `snowy`, for example) is not an exact table key; place the table's
        // first state for that block instead.
        let held = if self.world.table.state_id(&held).is_some() {
            held
        } else {
            self.world.table.default_state(&held.name).unwrap_or(held)
        };
        if is_air
            && self
                .world
                .set_state(target[0], target[1], target[2], &held)
                .is_ok()
        {
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockTable;
    use crate::player::Input;
    use horizon_core::position::ChunkPos;
    use horizon_world::compression::Compression;
    use horizon_world::nbt::{JavaString, Limits, NamedTag, Tag};
    use horizon_world::region::{ChunkRecord, RegionWriter};
    use std::path::{Path, PathBuf};

    // --- NBT/region helpers, mirroring the `world.rs` test fixtures ---

    fn tag(value: &str) -> Tag {
        Tag::String(JavaString::from(value))
    }

    fn compound(entries: &[(&str, Tag)]) -> Tag {
        Tag::Compound(
            entries
                .iter()
                .map(|(key, value)| (JavaString::from(*key), value.clone()))
                .collect(),
        )
    }

    fn section_tag(y: i8, palette: Vec<Tag>, states: Option<Vec<i64>>) -> Tag {
        let mut entries = vec![("Y", Tag::Byte(y)), ("Palette", Tag::List(palette))];
        if let Some(states) = states {
            entries.push(("BlockStates", Tag::LongArray(states)));
        }
        compound(&entries)
    }

    fn entry(name: &str) -> Tag {
        compound(&[("Name", tag(name))])
    }

    /// Packs palette indices with the 1.16.5 per-long layout (LSB first,
    /// indices never span two longs).
    fn pack(values: &[u64], bits: u32) -> Vec<i64> {
        let per_long = 64usize / bits as usize;
        let mut longs = vec![0i64; values.len().div_ceil(per_long)];
        for (index, &value) in values.iter().enumerate() {
            longs[index / per_long] |= (value as i64) << ((index % per_long) * bits as usize);
        }
        longs
    }

    /// A 1.16.5 chunk root at chunk coordinates (x, z) with one section 0.
    fn chunk_root(x: i32, z: i32, section: Tag) -> NamedTag {
        NamedTag {
            name: JavaString::from(""),
            tag: compound(&[(
                "Level",
                compound(&[
                    ("Sections", Tag::List(vec![section])),
                    ("xPos", Tag::Int(x)),
                    ("zPos", Tag::Int(z)),
                ]),
            )]),
        }
    }

    /// A tiny table covering air, stone and water.
    fn test_table() -> BlockTable {
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0}]},
                "minecraft:stone": {"states": [{"id": 1}]},
                "minecraft:water": {"states": [{"id": 2}]},
            }},
        });
        let path = std::env::temp_dir().join(format!(
            "horizon-1-16-5-server-table-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("server")
        ));
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let table = BlockTable::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        table
    }

    fn temp_world(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-1-16-5-server-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("region")).unwrap();
        dir
    }

    /// Writes one chunk into `region/r.0.0.mca`.
    fn write_chunk(dir: &Path, root: NamedTag, x: i32, z: i32) {
        let region_path = dir.join("region").join("r.0.0.mca");
        let mut writer = RegionWriter::create(&region_path).unwrap();
        let record = ChunkRecord::from_nbt(&root, Compression::Zlib, 0, Limits::saved()).unwrap();
        writer.write(ChunkPos::new(x, z), &record).unwrap();
        writer.finish().unwrap();
    }

    /// The section-0 index of (x, y, z) in YZX order.
    fn index(x: i32, y: i32, z: i32) -> usize {
        ((y << 8) | (z << 4) | x) as usize
    }

    /// A world whose chunk (0, 0) has stone at `stones` and water at `water`
    /// (y < 16), air everywhere else in the section.
    fn stone_world(name: &str, stones: &[[i32; 3]], water: &[[i32; 3]]) -> World {
        let dir = temp_world(name);
        let mut values = vec![0u64; 4096];
        for &[x, y, z] in stones {
            values[index(x, y, z)] = 1;
        }
        for &[x, y, z] in water {
            values[index(x, y, z)] = 2;
        }
        let root = chunk_root(
            0,
            0,
            section_tag(
                0,
                vec![
                    entry("minecraft:air"),
                    entry("minecraft:stone"),
                    entry("minecraft:water"),
                ],
                Some(pack(&values, 4)),
            ),
        );
        write_chunk(&dir, root, 0, 0);
        World::open(&dir, test_table()).unwrap()
    }

    #[test]
    fn frame_input_defaults_are_neutral() {
        let input = FrameInput::default();
        assert_eq!(input.movement, Input::default());
        assert_eq!(input.look, None);
        assert!(!input.break_block);
        assert!(!input.place_block);
        assert_eq!(input.select, None);
        assert_eq!(input.scroll, 0);
    }

    #[test]
    fn inventory_selection_and_scroll_wrap() {
        let mut inventory = Inventory::creative();
        assert_eq!(inventory.selected(), 0);
        inventory.select(7);
        assert_eq!(inventory.selected(), 7);
        inventory.scroll(2);
        assert_eq!(inventory.selected(), 0);
        inventory.scroll(-1);
        assert_eq!(inventory.selected(), 8);
        assert!(inventory.held().is_some());
    }

    #[test]
    fn new_stores_world_player_and_inventory() {
        let world = stone_world("new", &[[0, 0, 0]], &[]);
        let server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        assert_eq!(server.player.position, [0.5, 1.0, 0.5]);
        assert_eq!(server.tick_count, 0);
        assert!(!server.dirty);
        assert_eq!(server.inventory.selected(), 0);
        assert!(server.inventory.held().is_some());
    }

    #[test]
    fn open_spawns_above_the_surface() {
        let world = stone_world("open", &[[0, 0, 0], [0, 7, 0]], &[]);
        let server = IntegratedServer::open(world);
        assert_eq!(server.player.position, [0.5, 8.001, 0.5]);
    }

    #[test]
    fn open_without_a_spawn_chunk_falls_back_to_y_64() {
        let dir = temp_world("empty");
        let world = World::open(&dir, test_table()).unwrap();
        let server = IntegratedServer::open(world);
        assert_eq!(server.player.position, [0.5, 64.0, 0.5]);
    }

    #[test]
    fn tick_applies_look_select_scroll_and_counts() {
        let world = stone_world("tick", &[[0, 0, 0]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.tick(&FrameInput {
            look: Some([180.0, 120.0]),
            select: Some(3),
            scroll: -1,
            ..FrameInput::default()
        });
        assert_eq!(server.player.yaw, 180.0);
        assert_eq!(server.player.pitch, 90.0);
        assert_eq!(server.inventory.selected(), 2);
        assert_eq!(server.tick_count, 1);
        assert!(!server.dirty);
    }

    #[test]
    fn tick_break_replaces_the_target_with_air() {
        let world = stone_world("break", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0; // horizontal, toward the wall at +Z
        server.tick(&FrameInput {
            break_block: true,
            ..FrameInput::default()
        });
        assert_eq!(
            server.world.state_at(0, 2, 3),
            Some(BlockState::new("minecraft:air"))
        );
        assert!(server.dirty);
    }

    #[test]
    fn tick_break_does_not_mark_dirty_without_a_target() {
        let world = stone_world("break-miss", &[[0, 0, 0]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0; // horizontal; nothing solid at eye level
        server.tick(&FrameInput {
            break_block: true,
            ..FrameInput::default()
        });
        assert!(!server.dirty);
    }

    #[test]
    fn tick_place_fills_the_cell_before_the_target() {
        let world = stone_world("place", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        let held = server.inventory.held().unwrap().clone();
        server.tick(&FrameInput {
            place_block: true,
            ..FrameInput::default()
        });
        assert_eq!(server.world.state_at(0, 2, 2), Some(held));
        assert!(server.dirty);
    }

    #[test]
    fn tick_place_skips_non_air_targets() {
        let world = stone_world("place-water", &[[0, 0, 0], [0, 2, 3]], &[[0, 2, 2]]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        server.tick(&FrameInput {
            place_block: true,
            ..FrameInput::default()
        });
        assert_eq!(
            server.world.state_at(0, 2, 2),
            Some(BlockState::new("minecraft:water"))
        );
        assert!(!server.dirty);
    }
}
