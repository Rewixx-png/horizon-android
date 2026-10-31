//! Direct 1.16.5 Anvil world loading.
//!
//! [`World::open`] reads every `region/r.<x>.<z>.mca` of a world directory and
//! decodes its chunks into [`ChunkColumn`]s: up to 16 block sections (height
//! 256) plus the 1024-entry biome grid. A chunk column whose `Level` compound
//! carries no `Sections` (or an empty list) is empty and skipped, exactly like
//! a chunk the region header does not list at all.
//!
//! # 1.16.5 section storage
//!
//! Each entry of `Level.Sections` is a compound with `Y` (byte, 0..15), a
//! `Palette` list of `{Name, Properties?}` compounds and a `BlockStates` long
//! array of packed palette indices. The packing is the 1.16.x `BitStorage`
//! layout (verified against the 1.16.5 server binary):
//!
//! - `bits = max(4, ceillog2(palette.len()))`; `ceillog2(1) == 0` like
//!   vanilla's `Mth.ceillog2`, so a single entry still uses the 4-bit floor.
//! - Each long holds `values_per_long = 64 / bits` whole indices. An index
//!   never spans two longs; the unused high bits of each long stay zero.
//!   Index `i` lives in long `i / values_per_long` at bit offset
//!   `(i % values_per_long) * bits`, least significant bits first.
//! - The long array therefore has `ceil(4096 / values_per_long)` entries:
//!   256 at 4 bits, 342 at 5, 410 at 6, 456 at 7, 512 at 8, 586 at 9,
//!   683 at 10, 820 at 11-12 and 1024 at 13-16 bits. That is the
//!   `(size + valuesPerLong - 1) / valuesPerLong` length the 1.16.5
//!   `BitStorage` constructor enforces; the pre-1.16 continuous
//!   `ceil(4096 * bits / 64)` layout is rejected with a clear error.
//! - Block order inside a section is YZX: `index = y << 8 | z << 4 | x`.
//!
//! On disk every palette is the section's own local palette: 1.16.5
//! re-enumerates the distinct states on save, so indices never exceed the
//! palette length even for sections the in-memory global palette served.
//! A single-entry palette fills the whole section; vanilla itself writes the
//! full zero array for it (256 longs), but a missing or empty `BlockStates`
//! is accepted for it as well.
//!
//! Biomes are the `Level.Biomes` int array of 1024 ids: 4×4×4-block cells
//! ordered Y, then Z, then X (`index = (y_cell << 4) | (z_cell << 2) |
//! x_cell`), as in 1.16.5's `ChunkBiomeContainer`. Missing or short arrays
//! zero-fill the rest, like vanilla ignoring them; a longer array keeps the
//! first 1024 ids.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use horizon_core::position::ChunkPos;
use horizon_world::nbt::{Limits, NamedTag, Tag};
use horizon_world::region::RegionFile;

use crate::{BlockSource, BlockState, BlockTable, Properties};

/// Sections per chunk column: 16 sections of 16 blocks cover y 0..256.
const SECTIONS: usize = 16;
/// Blocks per section (16×16×16).
const SECTION_BLOCKS: usize = 4096;
/// Biome ids per column: 4×4 horizontal × 64 vertical cells of 4×4×4 blocks.
const BIOMES: usize = 1024;
/// One decoded section: 1.16.5 block-table state ids in YZX order.
type Section = Box<[u16; SECTION_BLOCKS]>;

/// A whole 1.16.5 world directory, loaded eagerly into memory.
pub struct World {
    /// The 1.16.5 block-state table the world's states were resolved against.
    pub table: BlockTable,
    /// The table id of `minecraft:air`, used where a chunk is loaded but a
    /// section (or the whole y range) is absent.
    air: u16,
    chunks: HashMap<(i32, i32), ChunkColumn>,
}

impl World {
    /// Loads every `region/r.<x>.<z>.mca` under `root`.
    ///
    /// Chunk records whose `Level` has no `Sections` are skipped as empty.
    /// A present chunk that fails to parse aborts with the region path and
    /// chunk position in the error; a region file that cannot be read does
    /// the same.
    pub fn open(root: &Path, table: BlockTable) -> Result<Self, String> {
        let air = table.state_id(&air_state()).unwrap_or(0);
        let region_dir = root.join("region");
        let mut chunks = HashMap::new();
        for path in region_files(&region_dir)? {
            let mut region = RegionFile::open(&path)
                .map_err(|error| format!("opening {}: {error}", path.display()))?;
            // `chunks()` borrows immutably while `read()` needs `&mut`.
            let positions: Vec<ChunkPos> = region.chunks().map(|(position, _)| position).collect();
            for position in positions {
                let record = match region.read(position) {
                    Ok(Some(record)) => record,
                    Ok(None) => continue,
                    Err(error) => {
                        return Err(format!(
                            "reading chunk ({}, {}) in {}: {error}",
                            position.x,
                            position.z,
                            path.display()
                        ));
                    }
                };
                let root = record
                    .nbt(Limits::saved())
                    .map_err(|error| format!("chunk ({}, {}): {error}", position.x, position.z))?;
                if let Some(column) = ChunkColumn::from_nbt(&root, position, &table)? {
                    chunks.insert((column.x, column.z), column);
                }
            }
        }
        Ok(Self { table, air, chunks })
    }

    /// The loaded chunk column at chunk coordinates (x, z), if any.
    pub fn chunk(&self, x: i32, z: i32) -> Option<&ChunkColumn> {
        self.chunks.get(&(x, z))
    }

    /// All loaded chunk columns.
    pub fn chunks(&self) -> impl Iterator<Item = &ChunkColumn> {
        self.chunks.values()
    }

    /// Number of loaded chunk columns.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether no chunk column loaded.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

impl BlockSource for World {
    /// The block state at world coordinates.
    ///
    /// A loaded chunk returns its block, or `minecraft:air` where the y is
    /// outside 0..256 or the section was not saved (missing sections are
    /// air). A position without a loaded chunk is `None`.
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let column = self.chunks.get(&(x >> 4, z >> 4))?;
        if !(0..256).contains(&y) {
            return Some(air_state());
        }
        let id = match column.section((y >> 4) as usize) {
            Some(section) => section[(((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize],
            None => self.air,
        };
        Some(self.table.state(id).cloned().unwrap_or_else(air_state))
    }
}

/// One loaded chunk column: x/z position, up to 16 sections of table state
/// ids and the 1024-entry biome grid.
#[derive(Debug)]
pub struct ChunkColumn {
    pub x: i32,
    pub z: i32,
    sections: [Option<Section>; SECTIONS],
    biomes: [i32; BIOMES],
}

impl ChunkColumn {
    /// Decodes a 1.16.5 chunk NBT root; `None` for an empty chunk (no
    /// `Level.Sections`). `position` is the region slot, the fallback when
    /// `Level.xPos`/`zPos` are absent.
    fn from_nbt(
        root: &NamedTag,
        position: ChunkPos,
        table: &BlockTable,
    ) -> Result<Option<Self>, String> {
        let Some(level) = root.tag.get("Level") else {
            return Err(format!(
                "chunk ({}, {}) has no Level compound",
                position.x, position.z
            ));
        };
        if !matches!(level, Tag::Compound(_)) {
            return Err(format!(
                "chunk ({}, {}) Level is not a compound",
                position.x, position.z
            ));
        }
        let x = match level.get("xPos") {
            Some(Tag::Int(x)) => *x,
            _ => position.x,
        };
        let z = match level.get("zPos") {
            Some(Tag::Int(z)) => *z,
            _ => position.z,
        };
        let at = format!("chunk ({x}, {z})");
        let Some(sections) = level.get("Sections") else {
            return Ok(None);
        };
        let Tag::List(sections) = sections else {
            return Err(format!("{at}: Level.Sections is not a list"));
        };
        if sections.is_empty() {
            return Ok(None);
        }
        let mut decoded: [Option<Section>; SECTIONS] = std::array::from_fn(|_| None);
        for section in sections {
            let Some((y, blocks)) = decode_section(section, &at, table)? else {
                // 1.16.5 saves empty sections outside the block range (a
                // flat world has an empty Y=-1 section). They carry no
                // blocks and are ignored; a nonempty out-of-range section
                // is not part of this 256-high target.
                continue;
            };
            if decoded[y].is_some() {
                return Err(format!("{at}: duplicate section Y {y}"));
            }
            decoded[y] = Some(blocks);
        }
        let mut biomes = [0i32; BIOMES];
        match level.get("Biomes") {
            // Like vanilla, tolerate a wrong-size array instead of failing
            // the chunk: missing ids stay 0, extras are ignored.
            Some(Tag::IntArray(values)) => {
                let count = values.len().min(BIOMES);
                biomes[..count].copy_from_slice(&values[..count]);
            }
            Some(_) => return Err(format!("{at}: Level.Biomes is not an int array")),
            None => {}
        }
        Ok(Some(Self {
            x,
            z,
            sections: decoded,
            biomes,
        }))
    }

    /// The section at index 0..16: its 4096 block-table state ids in YZX
    /// order, or `None` where the chunk did not save one (air).
    pub fn section(&self, index: usize) -> Option<&[u16; SECTION_BLOCKS]> {
        self.sections.get(index).and_then(Option::as_deref)
    }

    /// The raw 1024-entry biome grid, ordered Y (outermost), then Z, then X:
    /// `index = (y_cell << 4) | (z_cell << 2) | x_cell` in 4×4×4-block cells.
    pub fn biomes(&self) -> &[i32; BIOMES] {
        &self.biomes
    }

    /// The biome id of the 4×4×4-block cell holding (x, y, z); block
    /// coordinates, with y clamped to the column like 1.16.5's
    /// `ChunkBiomeContainer.getBiome`.
    pub fn biome(&self, x: i32, y: i32, z: i32) -> i32 {
        self.biomes[biome_index(x, y, z)]
    }
}

/// Decodes one `Level.Sections` entry into its `Y` index and packed blocks.
fn decode_section(
    tag: &Tag,
    at: &str,
    table: &BlockTable,
) -> Result<Option<(usize, Section)>, String> {
    let Tag::Compound(_) = tag else {
        return Err(format!("{at}: a Sections entry is not a compound"));
    };
    let y = match tag.get("Y") {
        Some(Tag::Byte(y)) => i32::from(*y),
        Some(_) => return Err(format!("{at}: section Y is not a byte")),
        None => return Err(format!("{at}: section has no Y")),
    };
    let Some(y) = usize::try_from(y).ok().filter(|&y| y < SECTIONS) else {
        return Ok(None);
    };
    let Some(palette) = tag.get("Palette") else {
        // 1.16.5 writes empty sections with just Y (and light); they are
        // absent from the decoded column and therefore read as air.
        return Ok(None);
    };
    let Tag::List(palette) = palette else {
        return Err(format!("{at}: section {y} Palette is not a list"));
    };
    if palette.is_empty() {
        return Ok(None);
    }
    let mut ids = Vec::with_capacity(palette.len());
    for entry in palette {
        ids.push(palette_entry(entry, at, y, table)?);
    }
    let bits = bits_per_entry(ids.len());
    let values_per_long = 64usize / bits as usize;
    let expected = SECTION_BLOCKS.div_ceil(values_per_long);
    let mut blocks: Section = Box::new([0; SECTION_BLOCKS]);
    let states = tag.get("BlockStates");
    if states.is_none() || matches!(states, Some(Tag::LongArray(data)) if data.is_empty()) {
        // A single-entry palette may omit BlockStates entirely; every block
        // is then palette[0]. (Vanilla 1.16.5 itself writes the full zero
        // array, which the packed path below decodes identically.)
        if ids.len() != 1 {
            return Err(format!(
                "{at}: section {y} has no BlockStates for a {len}-entry Palette",
                len = ids.len()
            ));
        }
        blocks.fill(ids[0]);
    } else if let Some(Tag::LongArray(data)) = states {
        if data.len() != expected {
            return Err(format!(
                "{at}: section {y} BlockStates has {} longs, expected {expected} \
                 (4096 indices at {bits} bits, {values_per_long} per long)",
                data.len()
            ));
        }
        let mask = (1u64 << bits) - 1;
        for (index, block) in blocks.iter_mut().enumerate() {
            let value = (data[index / values_per_long] as u64
                >> ((index % values_per_long) * bits as usize))
                & mask;
            let palette_index = usize::try_from(value)
                .ok()
                .filter(|&index| index < ids.len())
                .ok_or_else(|| {
                    format!(
                        "{at}: section {y} BlockStates index {value} is outside \
                         its {len}-entry Palette",
                        len = ids.len()
                    )
                })?;
            *block = ids[palette_index];
        }
    } else {
        return Err(format!("{at}: section {y} BlockStates is not a long array"));
    }
    Ok(Some((y, blocks)))
}

/// `max(4, ceillog2(palette_len))`, the 1.16.5 per-index bit width.
///
/// `ceillog2` mirrors `Mth.ceillog2`: 0 for a single entry, otherwise
/// `32 - leading_zeros(len - 1)`. Palette lengths are bounded by memory, so
/// the result never exceeds 32 bits.
fn bits_per_entry(palette_len: usize) -> u32 {
    let bits = if palette_len <= 1 {
        0
    } else {
        usize::BITS - (palette_len - 1).leading_zeros()
    };
    bits.max(4)
}

/// One `Palette` entry: a `{Name, Properties?}` compound mapped through the
/// 1.16.5 block-state table.
fn palette_entry(tag: &Tag, at: &str, y: usize, table: &BlockTable) -> Result<u16, String> {
    let Tag::Compound(_) = tag else {
        return Err(format!(
            "{at}: section {y} has a Palette entry that is not a compound"
        ));
    };
    let name = match tag.get("Name") {
        Some(Tag::String(name)) => name.to_string_lossy(),
        Some(_) => return Err(format!("{at}: section {y} Palette Name is not a string")),
        None => {
            return Err(format!(
                "{at}: section {y} has a Palette entry without a Name"
            ));
        }
    };
    let mut properties = Properties::new();
    if let Some(values) = tag.get("Properties") {
        let Tag::Compound(values) = values else {
            return Err(format!(
                "{at}: section {y} Palette Properties of {name} is not a compound"
            ));
        };
        for (key, value) in values {
            let Tag::String(value) = value else {
                return Err(format!(
                    "{at}: section {y} Palette property {key} of {name} is not a string",
                    key = key.to_string_lossy()
                ));
            };
            properties.insert(key.to_string_lossy(), value.to_string_lossy());
        }
    }
    let state = BlockState { name, properties };
    table.state_id(&state).ok_or_else(|| {
        format!(
            "{at}: section {y} Palette entry {} is unknown: not in the 1.16.5 block table",
            describe(&state)
        )
    })
}

/// A compact `minecraft:name{key=value,..}` form for error messages.
fn describe(state: &BlockState) -> String {
    if state.properties.is_empty() {
        return state.name.clone();
    }
    let mut out = String::with_capacity(state.name.len() + 32);
    out.push_str(&state.name);
    out.push('{');
    for (index, (key, value)) in state.properties.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(key);
        out.push('=');
        out.push_str(value);
    }
    out.push('}');
    out
}

/// The 1.16.5 `ChunkBiomeContainer` cell index for block coordinates:
/// 4×4×4-block cells ordered Y (64 cells), then Z (4), then X (4), with y
/// clamped to 0..63 like vanilla.
fn biome_index(x: i32, y: i32, z: i32) -> usize {
    let cell_y = (y >> 2).clamp(0, 63);
    let cell_z = (z >> 2) & 3;
    let cell_x = (x >> 2) & 3;
    ((cell_y << 4) | (cell_z << 2) | cell_x) as usize
}

/// `minecraft:air` without properties.
fn air_state() -> BlockState {
    BlockState::new("minecraft:air")
}

/// The `region/r.<x>.<z>.mca` files of a region directory, sorted.
///
/// External `c.<x>.<z>.mcc` streams and every other file are skipped.
fn region_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        fs::read_dir(dir).map_err(|error| format!("reading {}: {error}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {}: {error}", dir.display()))?;
        if !entry.path().is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let mut parts = name.split('.');
        match (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            (Some("r"), Some(x), Some(z), Some("mca"), None)
                if x.parse::<i32>().is_ok() && z.parse::<i32>().is_ok() =>
            {
                files.push(entry.path());
            }
            _ => {}
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_world::compression::Compression;
    use horizon_world::nbt::JavaString;
    use horizon_world::region::{ChunkRecord, RegionWriter};

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

    fn list(entries: Vec<Tag>) -> Tag {
        Tag::List(entries)
    }

    /// A 1.16.5 chunk root: named compound with a `Level` inside.
    fn chunk_root(sections: Vec<Tag>, biomes: Option<Vec<i32>>, x: i32, z: i32) -> NamedTag {
        let mut level = vec![
            ("Sections", list(sections)),
            ("xPos", Tag::Int(x)),
            ("zPos", Tag::Int(z)),
        ];
        if let Some(biomes) = biomes {
            level.push(("Biomes", Tag::IntArray(biomes)));
        }
        NamedTag {
            name: JavaString::from(""),
            tag: compound(&[("Level", compound(&level))]),
        }
    }

    /// A section compound; `None` block states omits the `BlockStates` tag.
    fn section_tag(y: i8, palette: Vec<Tag>, states: Option<Vec<i64>>) -> Tag {
        let mut entries = vec![("Y", Tag::Byte(y)), ("Palette", list(palette))];
        if let Some(states) = states {
            entries.push(("BlockStates", Tag::LongArray(states)));
        }
        compound(&entries)
    }

    /// A palette entry with an optional property map.
    fn entry(name: &str, properties: Option<&[(&str, &str)]>) -> Tag {
        let mut entries = vec![("Name", tag(name))];
        if let Some(properties) = properties {
            let values: Vec<(&str, Tag)> = properties
                .iter()
                .map(|(key, value)| (*key, tag(value)))
                .collect();
            entries.push(("Properties", compound(&values)));
        }
        compound(&entries)
    }

    /// Packs values with the 1.16.5 per-long layout (LSB first, no spans).
    fn pack(values: &[u64], bits: u32) -> Vec<i64> {
        let values_per_long = 64usize / bits as usize;
        let mut longs = vec![0i64; values.len().div_ceil(values_per_long)];
        for (index, &value) in values.iter().enumerate() {
            longs[index / values_per_long] |=
                (value as i64) << ((index % values_per_long) * bits as usize);
        }
        longs
    }

    /// A small 1.16.5 table: air, stone and three oak_log axes.
    fn test_table() -> BlockTable {
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0, "default": true}]},
                "minecraft:stone": {"states": [{"id": 1, "default": true}]},
                "minecraft:oak_log": {"states": [
                    {"id": 2, "properties": {"axis": "x"}},
                    {"id": 3, "properties": {"axis": "y"}, "default": true},
                ]},
            }},
        });
        let path = std::env::temp_dir().join(format!(
            "horizon-1-16-5-world-table-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("world")
        ));
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let table = BlockTable::load(&path).unwrap();
        fs::remove_file(&path).unwrap();
        table
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-1-16-5-world-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn block_states_packed_length_formula() {
        // bits = max(4, ceillog2(palette)); longs = ceil(4096 / (64 / bits)).
        for (palette_len, bits, longs) in [
            (1usize, 4u32, 256usize),
            (2, 4, 256),
            (16, 4, 256),
            (17, 5, 342),
            (32, 5, 342),
            (33, 6, 410),
            (64, 6, 410),
            (65, 7, 456),
            (128, 7, 456),
            (129, 8, 512),
            (256, 8, 512),
            (257, 9, 586),
            (512, 9, 586),
            (513, 10, 683),
            (1024, 10, 683),
            (1025, 11, 820),
            (2048, 11, 820),
            (2049, 12, 820),
            (4096, 12, 820),
        ] {
            assert_eq!(bits_per_entry(palette_len), bits, "palette {palette_len}");
            let values_per_long = 64usize / bits as usize;
            assert_eq!(
                SECTION_BLOCKS.div_ceil(values_per_long),
                longs,
                "palette {palette_len}"
            );
        }
    }

    #[test]
    fn two_entry_palette_unpacks_and_resolves() {
        let table = test_table();
        let palette = vec![entry("minecraft:air", None), entry("minecraft:stone", None)];
        let mut values = vec![0u64; SECTION_BLOCKS];
        // Stone at (3, 2, 1), (5, 0, 5) and the very last index (15, 15, 15),
        // which sits at the top of the final long.
        for index in [531usize, 85, 4095] {
            values[index] = 1;
        }
        let root = chunk_root(
            vec![section_tag(0, palette, Some(pack(&values, 4)))],
            Some((0..BIOMES as i32).collect()),
            1,
            -2,
        );
        let column = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table)
            .unwrap()
            .unwrap();
        assert_eq!((column.x, column.z), (1, -2));
        let section = column.section(0).unwrap();
        assert_eq!(section[531], 1);
        assert_eq!(section[85], 1);
        assert_eq!(section[4095], 1);
        assert_eq!(section[0], 0);
        assert_eq!(section[547], 0);
        assert!(column.section(1).is_none());
        // Biome cells: (19, 2, -31) is cell (0, 0, 0) of chunk (1, -2)...
        assert_eq!(column.biome(19, 2, -31), 0);
        // ...and (31, 255, -17) is cell (3, 63, 3), the last entry.
        assert_eq!(column.biome(31, 255, -17), 1023);
    }

    #[test]
    fn world_open_resolves_states_and_skips_empty_chunks() {
        let table = test_table();
        let dir = temp_dir("states");
        let region_path = dir.join("region").join("r.0.0.mca");
        let mut writer = RegionWriter::create(&region_path).unwrap();
        // A real chunk (1, 2) with one stone block in section 0.
        let palette = vec![entry("minecraft:air", None), entry("minecraft:stone", None)];
        let mut values = vec![0u64; SECTION_BLOCKS];
        values[531] = 1;
        let root = chunk_root(
            vec![section_tag(0, palette, Some(pack(&values, 4)))],
            Some((0..BIOMES as i32).collect()),
            1,
            2,
        );
        writer
            .write(
                ChunkPos::new(1, 2),
                &ChunkRecord::from_nbt(&root, Compression::Zlib, 0, Limits::saved()).unwrap(),
            )
            .unwrap();
        // An empty chunk (3, 4): a Level without Sections must be skipped.
        let empty = NamedTag {
            name: JavaString::from(""),
            tag: compound(&[(
                "Level",
                compound(&[("xPos", Tag::Int(3)), ("zPos", Tag::Int(4))]),
            )]),
        };
        writer
            .write(
                ChunkPos::new(3, 4),
                &ChunkRecord::from_nbt(&empty, Compression::Zlib, 0, Limits::saved()).unwrap(),
            )
            .unwrap();
        writer.finish().unwrap();

        let world = World::open(&dir, table).unwrap();
        assert_eq!(world.len(), 1);
        assert!(world.chunk(1, 2).is_some());
        assert!(world.chunk(3, 4).is_none());
        assert!(world.chunk(0, 0).is_none());
        assert_eq!(
            world.state_at(19, 2, 33),
            Some(BlockState::new("minecraft:stone"))
        );
        assert_eq!(
            world.state_at(18, 2, 33),
            Some(BlockState::new("minecraft:air"))
        );
        // Loaded chunk, out-of-range y and missing section are air.
        assert_eq!(
            world.state_at(16, -1, 32),
            Some(BlockState::new("minecraft:air"))
        );
        assert_eq!(
            world.state_at(16, 256, 32),
            Some(BlockState::new("minecraft:air"))
        );
        assert_eq!(
            world.state_at(16, 200, 32),
            Some(BlockState::new("minecraft:air"))
        );
        // No chunk at all is None.
        assert_eq!(world.state_at(0, 0, 0), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_sections_without_a_palette_are_air_and_ignored() {
        let table = test_table();
        let palette = vec![entry("minecraft:air", None), entry("minecraft:stone", None)];
        let mut values = vec![0u64; SECTION_BLOCKS];
        values[0] = 1;
        let root = chunk_root(
            vec![
                compound(&[("Y", Tag::Byte(-1))]),
                section_tag(0, palette, Some(pack(&values, 4))),
                compound(&[("Y", Tag::Byte(1))]),
            ],
            Some((0..BIOMES as i32).collect()),
            0,
            0,
        );
        let column = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table)
            .unwrap()
            .unwrap();
        assert!(column.section(0).is_some());
        assert!(column.section(1).is_none());
        assert_eq!(
            column.section(0).unwrap()[0],
            table.state_id(&BlockState::new("minecraft:stone")).unwrap()
        );
    }

    #[test]
    fn single_value_palette_fills_the_section() {
        let table = test_table();
        let position = ChunkPos::new(0, 0);
        // Missing BlockStates, an empty array and vanilla's 256 zero longs
        // must all decode to the single palette entry.
        let stone = entry("minecraft:stone", None);
        for states in [None, Some(Vec::new()), Some(vec![0i64; 256])] {
            let root = chunk_root(
                vec![section_tag(0, vec![stone.clone()], states)],
                None,
                0,
                0,
            );
            let column = ChunkColumn::from_nbt(&root, position, &table)
                .unwrap()
                .unwrap();
            let section = column.section(0).unwrap();
            assert!(section.iter().all(|&id| id == 1));
        }
    }

    #[test]
    fn block_states_length_is_validated() {
        let table = test_table();
        let palette = vec![entry("minecraft:air", None), entry("minecraft:stone", None)];
        // 255 longs instead of the 256 the 4-bit layout needs.
        let root = chunk_root(
            vec![section_tag(0, palette, Some(vec![0i64; 255]))],
            None,
            0,
            0,
        );
        let error = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table).unwrap_err();
        assert!(error.contains("has 255 longs, expected 256"), "{error}");
    }

    #[test]
    fn unknown_palette_states_are_errors() {
        let table = test_table();
        let position = ChunkPos::new(0, 0);
        // Unknown block id.
        let root = chunk_root(
            vec![section_tag(
                0,
                vec![entry("minecraft:not_a_block", None)],
                None,
            )],
            None,
            0,
            0,
        );
        let error = ChunkColumn::from_nbt(&root, position, &table).unwrap_err();
        assert!(
            error.contains("unknown: not in the 1.16.5 block table"),
            "{error}"
        );
        assert!(error.contains("minecraft:not_a_block"), "{error}");
        // Known block, unknown property combination.
        let root = chunk_root(
            vec![section_tag(
                0,
                vec![entry("minecraft:oak_log", Some(&[("axis", "z")]))],
                None,
            )],
            None,
            0,
            0,
        );
        let error = ChunkColumn::from_nbt(&root, position, &table).unwrap_err();
        assert!(error.contains("minecraft:oak_log{axis=z}"), "{error}");
    }

    #[test]
    #[ignore = "needs a real 1.16.5 world: set HORIZON_1_16_5_WORLD to its directory (HORIZON_1_16_5_TABLE to its tables.json)"]
    fn real_world_loads_chunks() {
        let (Ok(world), Ok(table)) = (
            std::env::var("HORIZON_1_16_5_WORLD"),
            std::env::var("HORIZON_1_16_5_TABLE"),
        ) else {
            return;
        };
        let world = World::open(
            Path::new(&world),
            BlockTable::load(Path::new(&table)).unwrap(),
        )
        .unwrap();
        assert!(!world.is_empty());
        for column in world.chunks().take(16) {
            assert!((0..SECTIONS).any(|index| column.section(index).is_some()));
        }
    }
}
