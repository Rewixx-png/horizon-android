//! Block predicates shared by the 1.16.5 player, raycast and placement code.
//!
//! 1.16.5 has no single "solid" flag in the block-state table: collision
//! shapes live in code. This module keeps the small set the playable slice
//! needs. Unknown blocks are solid, which is the safe default for walking and
//! ray tracing; the listed exceptions are the blocks a player must walk
//! through.

/// Blocks that never stop movement and are not a raycast target.
const NON_SOLID: &[&str] = &[
    "minecraft:air",
    "minecraft:cave_air",
    "minecraft:void_air",
    "minecraft:water",
    "minecraft:lava",
    "minecraft:fire",
    "minecraft:soul_fire",
    "minecraft:torch",
    "minecraft:wall_torch",
    "minecraft:soul_torch",
    "minecraft:soul_wall_torch",
    "minecraft:redstone_torch",
    "minecraft:redstone_wall_torch",
    "minecraft:redstone_wire",
    "minecraft:tripwire",
    "minecraft:tripwire_hook",
    "minecraft:rail",
    "minecraft:powered_rail",
    "minecraft:detector_rail",
    "minecraft:activator_rail",
    "minecraft:grass",
    "minecraft:fern",
    "minecraft:dead_bush",
    "minecraft:tall_grass",
    "minecraft:large_fern",
    "minecraft:sapling",
    "minecraft:dandelion",
    "minecraft:poppy",
    "minecraft:blue_orchid",
    "minecraft:allium",
    "minecraft:azure_bluet",
    "minecraft:red_tulip",
    "minecraft:orange_tulip",
    "minecraft:white_tulip",
    "minecraft:pink_tulip",
    "minecraft:oxeye_daisy",
    "minecraft:cornflower",
    "minecraft:lily_of_the_valley",
    "minecraft:wither_rose",
    "minecraft:sugar_cane",
    "minecraft:wheat",
    "minecraft:carrots",
    "minecraft:potatoes",
    "minecraft:beetroots",
    "minecraft:cobweb",
    "minecraft:snow",
    "minecraft:vine",
    "minecraft:lily_pad",
];

fn name_only(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// Whether a block state lets the player pass through it.
pub fn is_air(name: &str) -> bool {
    matches!(name_only(name), "air" | "cave_air" | "void_air")
}

pub fn is_liquid(name: &str) -> bool {
    matches!(name_only(name), "water" | "lava")
}

/// Whether the block has a collision box for movement and block picking.
///
/// This is a playable-slice approximation, not a port of every 1.16.5 shape:
/// slabs, stairs, fences, doors and carpet are treated as full cubes.
pub fn is_solid(name: &str) -> bool {
    if is_air(name) || is_liquid(name) {
        return false;
    }
    let plain = name_only(name);
    // Flowers, crops and saplings are individual ids in 1.16.5, but a mod or
    // a newer state may append a suffix; the explicit list stays authoritative.
    !NON_SOLID.iter().any(|block| {
        let block = block.strip_prefix("minecraft:").unwrap_or(block);
        block == plain
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_liquid_and_plants_do_not_block() {
        for name in [
            "minecraft:air",
            "minecraft:cave_air",
            "minecraft:water",
            "minecraft:lava",
            "minecraft:grass",
            "minecraft:poppy",
            "minecraft:torch",
            "minecraft:rail",
        ] {
            assert!(!is_solid(name), "{name}");
        }
    }

    #[test]
    fn unknown_and_cube_blocks_block() {
        for name in [
            "minecraft:stone",
            "minecraft:oak_planks",
            "minecraft:glass",
            "modded:strange_block",
        ] {
            assert!(is_solid(name), "{name}");
        }
    }

    #[test]
    fn namespaces_are_optional() {
        assert!(!is_solid("air"));
        assert!(is_solid("stone"));
    }
}
