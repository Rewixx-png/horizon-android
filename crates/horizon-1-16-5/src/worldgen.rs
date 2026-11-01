//! A small deterministic flat world for the offline (standalone) mode.
//!
//! This is deliberately not Minecraft's world generator: it exists so the
//! client has something to stand on when no world directory and no server are
//! given (the default Android launch). The layers follow 1.16.5's flat preset —
//! bedrock, stone, dirt, grass — and the result is an ordinary [`World`], so
//! break and place edit the same sections the mesher and the saver read.

use crate::world::{ChunkColumn, World};
use crate::{BlockState, BlockTable};

/// The world's layers from the bottom: (block, thickness).
const LAYERS: &[(&str, i32)] = &[
    ("minecraft:bedrock", 1),
    ("minecraft:stone", 2),
    ("minecraft:dirt", 2),
    ("minecraft:grass_block", 1),
];

/// The y of the top block of the flat world (the grass layer's surface).
pub fn surface_y() -> i32 {
    LAYERS.iter().map(|(_, height)| height).sum::<i32>() - 1
}

/// The state used for a column's layer at `y`, or `None` above the surface.
fn layer_state(table: &BlockTable, y: i32) -> Option<BlockState> {
    let mut top = 0;
    for (name, height) in LAYERS {
        top += height;
        if y < top {
            // The default state (lowest id) is what the layer names.
            return table.default_state(name);
        }
    }
    None
}

/// Fills `world` with a flat area of `radius` chunks around the origin.
///
/// Every column in `-radius..=radius` (inclusive) on both axes is inserted, so
/// the player spawns on solid ground and the mesher has neighbours to cull
/// against. Returns the number of chunks written.
pub fn fill_flat(world: &mut World, radius: i32) -> Result<usize, String> {
    let radius = radius.max(1);
    let mut written = 0;
    for chunk_x in -radius..=radius {
        for chunk_z in -radius..=radius {
            let mut column = ChunkColumn::empty(chunk_x, chunk_z);
            for y in 0..=surface_y() {
                let Some(state) = layer_state(&world.table, y) else {
                    continue;
                };
                let id = world
                    .table
                    .state_id(&state)
                    .ok_or_else(|| format!("{} is not in the 1.16.5 block table", state.name))?;
                let blocks = column
                    .section_mut(y as usize >> 4)
                    .ok_or_else(|| "flat generator left a section missing".to_owned())?;
                for z in 0..16usize {
                    for x in 0..16usize {
                        blocks[(y as usize & 15) << 8 | z << 4 | x] = id;
                    }
                }
            }
            world.insert_column(column)?;
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockSource;

    /// A tiny table with the flat layers, written to a temporary file.
    fn table() -> BlockTable {
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0}]},
                "minecraft:bedrock": {"states": [{"id": 1}]},
                "minecraft:stone": {"states": [{"id": 2}]},
                "minecraft:dirt": {"states": [{"id": 3}]},
                "minecraft:grass_block": {"states": [{"id": 4}]},
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

    #[test]
    fn flat_world_has_a_walkable_surface() {
        let mut world = World::empty(table());
        let written = fill_flat(&mut world, 1).unwrap();
        assert_eq!(written, 9, "3x3 chunks");
        let surface = surface_y();
        let top = world.state_at(0, surface, 0).expect("loaded column");
        assert_eq!(top.name, "minecraft:grass_block");
        assert_eq!(
            world.state_at(0, surface + 1, 0).unwrap().name,
            "minecraft:air"
        );
        // Layers: bedrock at 0, stone 1..2, dirt 3..4, grass at the surface.
        assert_eq!(
            world.state_at(0, surface - 1, 0).unwrap().name,
            "minecraft:dirt"
        );
        assert_eq!(
            world.state_at(0, surface - 3, 0).unwrap().name,
            "minecraft:stone"
        );
        assert_eq!(world.state_at(0, 0, 0).unwrap().name, "minecraft:bedrock");
    }

    #[test]
    fn flat_world_edges_are_solid_and_editable() {
        let mut world = World::empty(table());
        fill_flat(&mut world, 2).unwrap();
        // Chunk 2 spans x 32..47 and chunk -2 spans z -32..-17; both are
        // inside a radius-2 square, so their far corner is solid.
        let edge = 2 * 16 + 15;
        let near_edge = -(2 * 16);
        let corner = world.state_at(edge, surface_y(), near_edge);
        assert_eq!(corner, Some(BlockState::new("minecraft:grass_block")));
        assert!(
            world.state_at(3 * 16, surface_y(), 0).is_none(),
            "outside the radius"
        );
        // Breaking a block keeps the rest of the column intact.
        let air = BlockState::new("minecraft:air");
        world.set_state(0, surface_y(), 0, &air).unwrap();
        assert_eq!(
            world.state_at(0, surface_y(), 0).unwrap().name,
            "minecraft:air"
        );
        assert!(world.has_unsaved_changes());
    }
}
