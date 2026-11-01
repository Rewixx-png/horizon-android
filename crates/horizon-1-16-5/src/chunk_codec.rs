//! One place where a chunk becomes a [`ChunkColumn`], whatever its source.
//!
//! Two very different formats describe the same 1.16.5 chunk:
//!
//! * an Anvil region record (`Level.Sections` with a local palette and 1.16.x
//!   `BitStorage`), read by [`crate::world`];
//! * a network `Chunk Data` packet (protocol 754), which packs each section as
//!   `block count, bits per entry, palette, long array`, where ids above the
//!   palette threshold are 1.16.5 **global** block-state ids.
//!
//! Both end up in the same [`ChunkColumn`]: 16 sections of 4096 global ids and
//! a 1024-cell biome grid. Keeping the block-id space global (not
//! section-local) is what lets a network chunk and a saved chunk be compared,
//! meshed and overlaid by the same renderer.

use crate::world::ChunkColumn;
use horizon_protocol::v1_16_5::{ChunkData, ChunkSection};

/// Decoders shared by the Anvil reader and the network client.
pub struct ChunkCodec;

impl ChunkCodec {
    /// Decodes one network section into the crate's dense layout.
    ///
    /// Network sections are YZX packed (`index = y << 8 | z << 4 | x`) and
    /// either indirect (palette index) or direct (global id). A section whose
    /// palette says "single value, no data" (`bits == 0`) is that value
    /// everywhere. Light is not part of `Chunk Data`; it arrives in the
    /// separate Light Update packet, so it stays `None` for now and the light
    /// sampler falls back to the height map.
    pub fn section_from_network(
        section: &ChunkSection,
    ) -> Result<(Box<[u32; 4096]>, Option<Vec<u8>>), String> {
        let mut blocks: Box<[u32; 4096]> = Box::new([0; 4096]);
        for y in 0..16u8 {
            for z in 0..16u8 {
                for x in 0..16u8 {
                    let id = section
                        .block_state(x, y, z)
                        .map_err(|error| format!("section block ({x}, {y}, {z}): {error}"))?;
                    if id < 0 {
                        return Err(format!("section block ({x}, {y}, {z}) has id {id}"));
                    }
                    let index = usize::from(y) * 256 + usize::from(z) * 16 + usize::from(x);
                    blocks[index] = id as u32;
                }
            }
        }
        Ok((blocks, None))
    }

    /// Builds a full column from a `full = true` Chunk Data packet.
    ///
    /// Sections absent from the bitmask stay `None` (air). The biome grid is
    /// 1024 4×4×4 cells for a full chunk; a shorter array zero-fills the rest
    /// like the Anvil reader does.
    pub fn column_from_network(data: &ChunkData) -> Result<ChunkColumn, String> {
        let mut column = ChunkColumn::empty(data.x, data.z);
        Self::apply(&mut column, data)?;
        Ok(column)
    }

    /// Applies a Chunk Data packet to an existing column.
    ///
    /// A full chunk replaces every section and the biomes; a partial update
    /// (`full == false`, sent when the server changes blocks) only touches the
    /// sections named in its bitmask and leaves the rest, including biomes.
    pub fn apply(column: &mut ChunkColumn, data: &ChunkData) -> Result<(), String> {
        for (index, section) in &data.sections {
            let (blocks, lights) = Self::section_from_network(section)
                .map_err(|error| format!("section {index}: {error}"))?;
            column.replace_section(*index, Some(blocks), lights)?;
        }
        if data.full {
            column.replace_sections_absent_from(data.bitmask)?;
            if !data.biomes.is_empty() {
                let count = data.biomes.len().min(1024);
                let mut biomes = [0i32; 1024];
                for (cell, id) in data.biomes.iter().take(count).enumerate() {
                    biomes[cell] = *id;
                }
                column.set_biomes(biomes)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(bits: u8, palette: Vec<i32>, data: Vec<i64>) -> ChunkSection {
        ChunkSection {
            block_count: 0,
            bits,
            palette,
            data,
        }
    }

    /// A 4-bit section with a two-entry palette: index 1 for x = 0..7, 0
    /// elsewhere, so id 7 lands in the low half of every row.
    fn palette_section() -> ChunkSection {
        // 4096 entries at 4 bits = 16 per long = 256 longs; entry i at bits
        // (i % 16) * 4 of long i / 16.
        let mut longs = vec![0i64; 256];
        for index in 0..4096 {
            let value = if index % 16 < 8 { 1i64 } else { 0 };
            longs[index / 16] |= value << ((index % 16) * 4);
        }
        section(4, vec![3, 7], longs)
    }

    #[test]
    fn palette_section_decodes_low_half_eights() {
        let (blocks, _) = ChunkCodec::section_from_network(&palette_section()).unwrap();
        // YZX: index = y << 8 | z << 4 | x.
        assert_eq!(blocks[0], 7);
        assert_eq!(blocks[7], 7);
        assert_eq!(blocks[8], 3);
        assert_eq!(blocks[15], 3);
        assert_eq!(blocks[256], 7);
        assert_eq!(blocks[4095], 3);
    }

    #[test]
    fn single_value_section_fills_the_whole_section() {
        let (blocks, _) =
            ChunkCodec::section_from_network(&section(0, vec![11], Vec::new())).unwrap();
        assert!(blocks.iter().all(|&id| id == 11));
    }

    #[test]
    fn column_from_a_full_packet_carries_sections_and_biomes() {
        let data = ChunkData {
            x: 3,
            z: -2,
            full: true,
            bitmask: 0b101,
            heightmaps: horizon_world::nbt::Tag::Compound(Default::default()),
            biomes: vec![4; 1024],
            sections: vec![(0, palette_section()), (2, section(0, vec![9], Vec::new()))],
            block_entities: Vec::new(),
        };
        let column = ChunkCodec::column_from_network(&data).unwrap();
        assert_eq!((column.x, column.z), (3, -2));
        assert_eq!(column.section(0).unwrap()[0], 7);
        assert_eq!(column.section(2).unwrap()[4095], 9);
        assert!(column.section(1).is_none(), "masked-out section is air");
        assert_eq!(column.biome(0, 0, 0), 4);
    }

    #[test]
    fn partial_update_keeps_untouched_sections_and_biomes() {
        let mut column = ChunkCodec::column_from_network(&ChunkData {
            x: 0,
            z: 0,
            full: true,
            bitmask: 0b1,
            heightmaps: horizon_world::nbt::Tag::Compound(Default::default()),
            biomes: vec![2; 1024],
            sections: vec![(0, palette_section())],
            block_entities: Vec::new(),
        })
        .unwrap();
        let before = *column.section(0).unwrap();
        let updated = ChunkData {
            x: 0,
            z: 0,
            full: false,
            bitmask: 0b10,
            heightmaps: horizon_world::nbt::Tag::Compound(Default::default()),
            biomes: Vec::new(),
            sections: vec![(1, section(0, vec![5], Vec::new()))],
            block_entities: Vec::new(),
        };
        ChunkCodec::apply(&mut column, &updated).unwrap();
        assert_eq!(*column.section(0).unwrap(), before, "section 0 untouched");
        assert_eq!(column.section(1).unwrap()[0], 5);
        assert_eq!(column.biome(0, 0, 0), 2, "biomes kept on a partial update");
    }
}
