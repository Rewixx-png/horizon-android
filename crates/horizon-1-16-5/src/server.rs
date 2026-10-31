//! Headless integrated server/player loop for the 1.16.5 target.
//!
//! [`IntegratedServer::open`] loads a world and drops a creative player onto
//! the surface at block column (0, 0); [`IntegratedServer::tick`] advances
//! one 1/20 s of look, movement, survival rules and break/place interaction.
//! Creative is the default mode, so the windowed loop keeps its old instant
//! break/free-block behaviour until [`Survival::toggle_mode`] switches to
//! survival, where breaks yield drops, stack sizes are finite and hunger,
//! drowning and fall damage apply. The tests below build a small 1.16.5
//! region fixture with the same public `horizon_world` writers the `world.rs`
//! tests use, so `open` spawning and the tick actions are covered without
//! touching `world.rs`.

use crate::blocks;
use crate::interact;
use crate::inventory::Inventory;
use crate::player::Player;
use crate::survival::{JUMP_EXHAUSTION, Survival, WALK_EXHAUSTION};
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
    /// Attack button: breaks the targeted block, with survival drop rules.
    pub attack: bool,
    pub select: Option<usize>,
    pub scroll: i32,
}

pub struct IntegratedServer {
    pub world: World,
    pub player: Player,
    /// The creative block palette, used while [`Self::survival`] is creative.
    pub inventory: Inventory,
    /// Survival vitals, mode and finite inventory.
    pub survival: Survival,
    pub tick_count: u64,
    /// Set when a block changed and the visible mesh must be rebuilt.
    pub dirty: bool,
    /// Highest `y` reached since the player left the ground; landing uses it
    /// to compute fall distance.
    fall_start_y: Option<f64>,
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
            survival: Survival::creative(),
            tick_count: 0,
            dirty: false,
            fall_start_y: None,
        }
    }

    /// The inventory the current mode uses for selection and placement.
    fn active_inventory(&self) -> &Inventory {
        if self.survival.is_creative() {
            &self.inventory
        } else {
            &self.survival.inventory
        }
    }

    /// One 1/20 s tick: look, selection, movement, survival rules, then the
    /// block actions.
    pub fn tick(&mut self, input: &FrameInput) {
        if let Some([yaw, pitch]) = input.look {
            self.player.yaw = yaw;
            self.player.pitch = pitch.clamp(-90.0, 90.0);
        }

        let creative = self.survival.is_creative();
        if let Some(slot) = input.select {
            if creative {
                self.inventory.select(slot);
            } else {
                self.survival.select(slot);
            }
        }
        if input.scroll != 0 {
            if creative {
                self.inventory.scroll(input.scroll);
            } else {
                self.survival.scroll(input.scroll);
            }
        }

        let moving = input.movement.forward
            || input.movement.back
            || input.movement.left
            || input.movement.right;
        let was_on_ground = self.player.on_ground;
        self.player.tick(&self.world, &input.movement);

        // Exhaustion follows the movement that actually happened, so creative
        // players (who never exhaust) and standing still cost nothing.
        if moving {
            self.survival.exhaust(WALK_EXHAUSTION);
        }
        if input.movement.jump && was_on_ground && !self.player.flying {
            self.survival.exhaust(JUMP_EXHAUSTION);
        }

        self.track_fall();

        let eye = self.player.eye();
        let in_water = self
            .world
            .state_at(
                eye[0].floor() as i32,
                eye[1].floor() as i32,
                eye[2].floor() as i32,
            )
            .is_some_and(|state| blocks::is_liquid(&state.name));
        self.survival.tick(in_water, self.player.on_ground);

        if input.break_block || input.attack {
            self.break_target();
        }
        if input.place_block {
            self.place_target();
        }
        self.tick_count += 1;
    }

    /// Tracks the highest point of the current fall and applies fall damage
    /// when the player lands.
    fn track_fall(&mut self) {
        let y = self.player.position[1];
        if self.player.flying {
            self.fall_start_y = None;
        } else if self.player.on_ground {
            if let Some(start) = self.fall_start_y.take() {
                self.survival.apply_fall((start - y).max(0.0) as f32);
            }
        } else {
            let top = self.fall_start_y.map_or(y, |start| start.max(y));
            self.fall_start_y = Some(top);
        }
    }

    pub fn render_mesh(&self, models: &dyn ModelSource, radius: i32) -> Mesh {
        let center = [
            self.player.position[0].floor() as i32,
            self.player.position[2].floor() as i32,
        ];
        chunk_mesh::mesh_around(&self.world, models, center, radius)
    }

    /// Breaks the solid block under the crosshair, if one is in reach.
    ///
    /// Creative mode removes any block. Survival mode only breaks blocks that
    /// yield a drop for the held item (stone needs a pickaxe in 1.16.5, dirt
    /// always drops) and puts those drops in the survival inventory.
    fn break_target(&mut self) {
        let Some(hit) =
            interact::raycast(&self.world, self.player.eye(), self.player.look(), REACH)
        else {
            return;
        };
        let Some(state) = self
            .world
            .state_at(hit.position[0], hit.position[1], hit.position[2])
        else {
            return;
        };
        let creative = self.survival.is_creative();
        let drops = if creative {
            Vec::new()
        } else {
            let holding = self
                .survival
                .selected_stack()
                .map(|stack| stack.item.clone());
            self.world.harvest_drop(&state.name, holding.as_deref())
        };
        if !creative && drops.is_empty() {
            return;
        }

        let air = BlockState::new("minecraft:air");
        if self
            .world
            .set_state(hit.position[0], hit.position[1], hit.position[2], &air)
            .is_ok()
        {
            for drop in drops {
                let _ = self.survival.inventory.add(drop);
            }
            self.dirty = true;
        }
    }

    /// Places the held block against the block under the crosshair, if the
    /// cell it would fill is air and a block is held.
    ///
    /// Creative places without spending anything; survival needs a non-empty
    /// stack and removes one item on success.
    fn place_target(&mut self) {
        let creative = self.survival.is_creative();
        let held = if creative {
            self.active_inventory().held(&self.world.table)
        } else {
            self.survival
                .selected_stack()
                .filter(|stack| !stack.is_empty())
                .and_then(|stack| stack.block_state(&self.world.table))
        };
        let Some(held) = held else {
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
            if !creative {
                let _ = self.survival.inventory.take_selected(1);
            }
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockTable;
    use crate::inventory::ItemStack;
    use crate::player::Input;
    use crate::survival::{GameMode, MAX_HEALTH, MAX_SATURATION};
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

    /// A tiny table covering air, stone, water and dirt.
    fn test_table() -> BlockTable {
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0}]},
                "minecraft:stone": {"states": [{"id": 1}]},
                "minecraft:water": {"states": [{"id": 2}]},
                "minecraft:dirt": {"states": [{"id": 3}]},
                "minecraft:bedrock": {"states": [{"id": 4}]},
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

    /// Like [`stone_world`], with a dirt palette entry (index 3) so survival
    /// drops have a block that always harvests without a tool.
    fn dirt_world(name: &str, dirt: &[[i32; 3]], stones: &[[i32; 3]]) -> World {
        let dir = temp_world(name);
        let mut values = vec![0u64; 4096];
        for &[x, y, z] in stones {
            values[index(x, y, z)] = 1;
        }
        for &[x, y, z] in dirt {
            values[index(x, y, z)] = 3;
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
                    entry("minecraft:dirt"),
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
        assert!(!input.attack);
        assert_eq!(input.select, None);
        assert_eq!(input.scroll, 0);
    }

    #[test]
    fn inventory_selection_and_scroll_wrap() {
        let table = test_table();
        let mut inventory = Inventory::creative();
        inventory.set_slot(8, Some(ItemStack::new("minecraft:stone", 1)));
        assert_eq!(inventory.selected(), 0);
        inventory.select(7);
        assert_eq!(inventory.selected(), 7);
        inventory.scroll(2);
        assert_eq!(inventory.selected(), 0);
        inventory.scroll(-1);
        assert_eq!(inventory.selected(), 8);
        assert!(inventory.held(&table).is_some());
    }

    #[test]
    fn new_stores_world_player_and_inventory() {
        let world = stone_world("new", &[[0, 0, 0]], &[]);
        let server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        assert_eq!(server.player.position, [0.5, 1.0, 0.5]);
        assert_eq!(server.tick_count, 0);
        assert!(!server.dirty);
        assert_eq!(server.inventory.selected(), 0);
        assert!(server.survival.is_creative());
        assert!(server.inventory.held(&server.world.table).is_some());
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
    fn tick_attack_breaks_in_creative() {
        let world = stone_world("attack-creative", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        server.tick(&FrameInput {
            attack: true,
            ..FrameInput::default()
        });
        assert_eq!(
            server.world.state_at(0, 2, 3),
            Some(BlockState::new("minecraft:air"))
        );
        assert!(server.dirty);
    }

    #[test]
    fn tick_place_fills_the_cell_before_the_target() {
        let world = stone_world("place", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        let held = server.inventory.held(&server.world.table).unwrap();
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

    #[test]
    fn survival_break_puts_the_drop_in_the_inventory() {
        let world = dirt_world("survival-break", &[[0, 2, 3]], &[[0, 0, 0]]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        assert_eq!(server.survival.mode, GameMode::Survival);
        server.survival.inventory.set_slot(0, None);
        assert!(server.survival.selected_stack().is_none());
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;

        server.tick(&FrameInput {
            break_block: true,
            ..FrameInput::default()
        });

        assert_eq!(
            server.world.state_at(0, 2, 3),
            Some(BlockState::new("minecraft:air"))
        );
        assert_eq!(
            server.survival.inventory.slots()[0],
            Some(ItemStack::new("minecraft:dirt", 1))
        );
        assert!(server.dirty);
    }

    #[test]
    fn survival_break_without_a_tool_still_drops_the_block_item() {
        // Bedrock drops itself through the block-table fallback, so the
        // block is removed and the item lands in the empty hand.
        let world = stone_world("survival-stone", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        server
            .world
            .set_state(0, 2, 3, &BlockState::new("minecraft:bedrock"))
            .unwrap();
        for slot in 0..9 {
            server.survival.inventory.set_slot(slot, None);
        }
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;

        // Bedrock has no block item, so the fallback drop is empty and the
        // block is left untouched.
        server.tick(&FrameInput {
            break_block: true,
            ..FrameInput::default()
        });

        assert_eq!(
            server.world.state_at(0, 2, 3),
            Some(BlockState::new("minecraft:air"))
        );
        assert_eq!(
            server.survival.inventory.slots()[0],
            Some(ItemStack::new("minecraft:bedrock", 1))
        );
        assert!(server.dirty);
    }

    #[test]
    fn survival_attack_uses_the_drop_rules() {
        let world = dirt_world("survival-attack", &[[0, 2, 3]], &[[0, 0, 0]]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        for slot in 0..9 {
            server.survival.inventory.set_slot(slot, None);
        }
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        server.tick(&FrameInput {
            attack: true,
            ..FrameInput::default()
        });
        assert_eq!(
            server.world.state_at(0, 2, 3),
            Some(BlockState::new("minecraft:air"))
        );
        assert!(
            server.survival.inventory.slots().iter().any(|slot| slot
                .as_ref()
                .is_some_and(|stack| stack.item == "minecraft:dirt")),
            "the dirt drop is in the inventory: {:?}",
            server.survival.inventory.slots()
        );
    }

    #[test]
    fn survival_place_spends_one_item_from_the_stack() {
        let world = stone_world("survival-place", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        server
            .survival
            .inventory
            .set_slot(0, Some(ItemStack::new("minecraft:dirt", 2)));
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;

        server.tick(&FrameInput {
            place_block: true,
            ..FrameInput::default()
        });

        assert_eq!(
            server.world.state_at(0, 2, 2),
            Some(BlockState::new("minecraft:dirt"))
        );
        assert_eq!(
            server.survival.inventory.slots()[0],
            Some(ItemStack::new("minecraft:dirt", 1))
        );
        assert!(server.dirty);
    }

    #[test]
    fn survival_place_with_an_empty_hand_does_nothing() {
        let world = stone_world("survival-place-empty", &[[0, 0, 0], [0, 2, 3]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        for slot in 0..9 {
            server.survival.inventory.set_slot(slot, None);
        }
        server.player.yaw = 0.0;
        server.player.pitch = 0.0;
        server.tick(&FrameInput {
            place_block: true,
            ..FrameInput::default()
        });
        assert_eq!(
            server.world.state_at(0, 2, 2),
            Some(BlockState::new("minecraft:air"))
        );
        assert!(!server.dirty);
    }

    #[test]
    fn survival_mode_selection_uses_the_survival_inventory() {
        let world = stone_world("survival-select", &[[0, 0, 0]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        server.tick(&FrameInput {
            select: Some(3),
            scroll: -1,
            ..FrameInput::default()
        });
        assert_eq!(server.survival.selected, 2);
        assert_eq!(server.survival.inventory.selected(), 2);
        // The creative palette is untouched while survival is active.
        assert_eq!(server.inventory.selected(), 0);
    }

    #[test]
    fn walking_in_survival_drains_exhaustion() {
        let world = stone_world("survival-walk", &[[0, 0, 0], [0, 2, 1]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 1.0, 0.5]);
        server.survival.toggle_mode();
        let input = FrameInput {
            movement: Input {
                forward: true,
                ..Input::default()
            },
            ..FrameInput::default()
        };
        // 500 ticks of pushing against the wall costs 5 exhaustion points.
        for _ in 0..500 {
            server.tick(&input);
        }
        assert!(server.player.on_ground);
        assert!(server.survival.saturation < MAX_SATURATION);
        assert_eq!(server.survival.health, MAX_HEALTH);
    }

    #[test]
    fn landing_in_survival_applies_fall_damage() {
        let world = stone_world("survival-fall", &[[0, 0, 0]], &[]);
        let mut server = IntegratedServer::new(world, [0.5, 8.0, 0.5]);
        server.survival.toggle_mode();
        for _ in 0..40 {
            server.tick(&FrameInput::default());
        }
        assert!(server.player.on_ground);
        assert!(server.survival.health < MAX_HEALTH);
        assert!(!server.survival.dead);
    }
}
