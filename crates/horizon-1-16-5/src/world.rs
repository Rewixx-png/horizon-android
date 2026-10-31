//! Direct 1.16.5 Anvil world loading and saving.
//!
//! [`World::open`] reads every `region/r.<x>.<z>.mca` of a world directory and
//! decodes its chunks into [`ChunkColumn`]s: up to 16 block sections (height
//! 256) plus the 1024-entry biome grid. A chunk column whose `Level` compound
//! carries no `Sections` (or an empty list) is empty and skipped, exactly like
//! a chunk the region header does not list at all.
//!
//! [`World::save`] writes only the chunks changed since opening (or the last
//! save) back in place. Each one is re-encoded as a fresh root with
//! `DataVersion` 2586, `Level.Status` `full`, a local palette per section and
//! the same packing and biome layout described below, so the crate's own
//! reader round-trips it.
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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use horizon_core::position::ChunkPos;
use horizon_world::compression::Compression;
use horizon_world::nbt::{JavaString, Limits, NamedTag, Tag};
use horizon_world::region::{ChunkRecord, RegionFile};

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
    /// Chunk positions changed since the last successful save.
    dirty: HashSet<(i32, i32)>,
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
        Ok(Self {
            table,
            air,
            chunks,
            dirty: HashSet::new(),
        })
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

    /// The `minecraft:air` state: the state the world was loaded with, or a
    /// bare `minecraft:air` if the table does not hold it.
    pub fn air_state(&self) -> BlockState {
        self.table
            .state(self.air)
            .cloned()
            .unwrap_or_else(air_state)
    }

    /// The block-table state id at world coordinates.
    ///
    /// A loaded chunk returns its id, or the air id where the y is outside
    /// 0..256 or the section was not saved. A position without a loaded
    /// chunk is `None`.
    pub fn state_id_at(&self, x: i32, y: i32, z: i32) -> Option<u16> {
        let column = self.chunks.get(&(x >> 4, z >> 4))?;
        if !(0..256).contains(&y) {
            return Some(self.air);
        }
        Some(match column.section((y >> 4) as usize) {
            Some(section) => section[(((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize],
            None => self.air,
        })
    }

    /// Writes a block-table state id at world coordinates.
    ///
    /// Errors when no chunk is loaded at the position or the y is outside
    /// 0..256. A missing section is created filled with air.
    pub fn set_state_id(&mut self, x: i32, y: i32, z: i32, id: u16) -> Result<(), String> {
        if !(0..256).contains(&y) {
            return Err(format!("block y {y} is outside 0..256"));
        }
        let air = self.air;
        let position = (x >> 4, z >> 4);
        let column = self
            .chunks
            .get_mut(&position)
            .ok_or_else(|| format!("no chunk loaded at ({}, {})", position.0, position.1))?;
        let section = column.sections[(y >> 4) as usize]
            .get_or_insert_with(|| Box::new([air; SECTION_BLOCKS]));
        let index = (((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize;
        let changed = section[index] != id;
        section[index] = id;
        if changed {
            self.dirty.insert(position);
        }
        Ok(())
    }

    /// Writes a block state at world coordinates, resolving it through the
    /// block table.
    ///
    /// Errors when the state is not in the table, no chunk is loaded at the
    /// position or the y is outside 0..256.
    pub fn set_state(&mut self, x: i32, y: i32, z: i32, state: &BlockState) -> Result<(), String> {
        let id = self
            .table
            .state_id(state)
            .ok_or_else(|| format!("{} is not in the 1.16.5 block table", describe(state)))?;
        self.set_state_id(x, y, z, id)
    }

    /// Whether any chunk changed since the world was opened or last saved.
    pub fn has_unsaved_changes(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Writes every dirty chunk in place under `root/region/`, creating the
    /// directory and region files as needed.
    ///
    /// Each chunk is rebuilt as a fresh 1.16.5 `Level` root (`DataVersion`
    /// 2586, `Status` `full`, local section palettes, packed `BlockStates`
    /// and the column's `Biomes`), so the result is exactly what this crate's
    /// [`World::open`] reads back. Clean and unloaded chunks are not touched.
    ///
    /// The dirty set loses only the chunks written successfully; a failure
    /// returns an error naming the chunk and its region file, and the chunks
    /// from that point on stay dirty for a later retry. Returns whether any
    /// chunk was written.
    pub fn save(&mut self, root: &Path) -> Result<bool, String> {
        if self.dirty.is_empty() {
            return Ok(false);
        }
        let region_dir = root.join("region");
        fs::create_dir_all(&region_dir)
            .map_err(|error| format!("creating {}: {error}", region_dir.display()))?;
        // Vanilla stores the chunk's last-modified time as a u32 epoch second.
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
            .min(u64::from(u32::MAX)) as u32;

        let mut positions: Vec<(i32, i32)> = self.dirty.iter().copied().collect();
        positions.sort_unstable();
        let mut wrote = false;
        for (cx, cz) in positions {
            // Only loaded chunks can be dirty through the public API; a
            // stale entry has nothing to encode and stays dirty.
            let Some(column) = self.chunks.get(&(cx, cz)) else {
                continue;
            };
            let region = ChunkPos::new(cx >> 5, cz >> 5);
            let path = region_dir.join(format!("r.{}.{}.mca", region.x, region.z));
            let nbt = self
                .column_nbt(column)
                .map_err(|error| format!("chunk ({cx}, {cz}) in {}: {error}", path.display()))?;
            let record = ChunkRecord::from_nbt(&nbt, Compression::Gzip, timestamp, Limits::saved())
                .map_err(|error| format!("chunk ({cx}, {cz}) in {}: {error}", path.display()))?;
            let mut file = RegionFile::open_writable(&path, true)
                .map_err(|error| format!("chunk ({cx}, {cz}) in {}: {error}", path.display()))?;
            file.write(ChunkPos::new(cx, cz), &record)
                .map_err(|error| format!("chunk ({cx}, {cz}) in {}: {error}", path.display()))?;
            // A later failure leaves only the chunks not yet written dirty.
            self.dirty.remove(&(cx, cz));
            wrote = true;
        }
        Ok(wrote)
    }

    /// Builds the 1.16.5 chunk NBT root for one loaded column.
    fn column_nbt(&self, column: &ChunkColumn) -> Result<NamedTag, String> {
        let at = format!("chunk ({}, {})", column.x, column.z);
        let mut sections = Vec::new();
        for (y, section) in column.sections.iter().enumerate() {
            let Some(blocks) = section else {
                continue;
            };
            sections.push(self.section_nbt(blocks, y, &at)?);
        }
        Ok(NamedTag {
            name: JavaString::default(),
            tag: Tag::compound([
                ("DataVersion", Tag::Int(2586)),
                (
                    "Level",
                    Tag::compound([
                        ("xPos", Tag::Int(column.x)),
                        ("zPos", Tag::Int(column.z)),
                        ("Status", Tag::String(JavaString::from("full"))),
                        ("Sections", Tag::List(sections)),
                        ("Biomes", Tag::IntArray(column.biomes.to_vec())),
                    ]),
                ),
            ]),
        })
    }

    /// Encodes one section with its own local palette and 1.16.5 packing.
    fn section_nbt(
        &self,
        blocks: &[u16; SECTION_BLOCKS],
        y: usize,
        at: &str,
    ) -> Result<Tag, String> {
        // 1.16.5 re-enumerates the distinct states of the section on save.
        let mut palette: Vec<u16> = Vec::new();
        let mut indices: HashMap<u16, u64> = HashMap::new();
        let mut values = Vec::with_capacity(SECTION_BLOCKS);
        for &id in blocks.iter() {
            let index = match indices.get(&id) {
                Some(&index) => index,
                None => {
                    let index = palette.len() as u64;
                    palette.push(id);
                    indices.insert(id, index);
                    index
                }
            };
            values.push(index);
        }
        let bits = bits_per_entry(palette.len());
        let values_per_long = 64usize / bits as usize;
        let mut longs = vec![0i64; SECTION_BLOCKS.div_ceil(values_per_long)];
        for (index, &value) in values.iter().enumerate() {
            longs[index / values_per_long] |=
                (value << ((index % values_per_long) * bits as usize)) as i64;
        }
        let entries = palette
            .iter()
            .map(|&id| {
                let state = self.table.state(id).ok_or_else(|| {
                    format!("{at}: section {y} holds block-state id {id}, which is not in the 1.16.5 block table")
                })?;
                Ok(palette_entry_nbt(state))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Tag::compound([
            ("Y", Tag::Byte(y as i8)),
            ("Palette", Tag::List(entries)),
            ("BlockStates", Tag::LongArray(longs)),
        ]))
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

    /// The block-table state id at block coordinates local to the column.
    ///
    /// `None` for a coordinate outside 0..16 or where the section was not
    /// saved (a missing section reads as air).
    pub fn state_id(&self, x: usize, y: usize, z: usize) -> Option<u16> {
        if x >= 16 || y >= 16 || z >= 16 {
            return None;
        }
        self.sections[y >> 4]
            .as_deref()
            .map(|section| section[(y << 8) | (z << 4) | x])
    }

    /// Writes a block-table state id at section-local block coordinates
    /// (each axis in 0..16, addressing the bottom section), creating that
    /// section filled with `air` where the column did not save one.
    ///
    /// Errors for a coordinate outside 0..16 on any axis.
    pub fn set_state_id(
        &mut self,
        x: usize,
        y: usize,
        z: usize,
        id: u16,
        air: u16,
    ) -> Result<(), String> {
        if x >= 16 || y >= 16 || z >= 16 {
            return Err(format!("block ({x}, {y}, {z}) is outside 0..16"));
        }
        let section = self.sections[y >> 4].get_or_insert_with(|| Box::new([air; SECTION_BLOCKS]));
        section[(y << 8) | (z << 4) | x] = id;
        Ok(())
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

/// One `Palette` entry from a block table state: `Name` plus an optional
/// `Properties` compound.
fn palette_entry_nbt(state: &BlockState) -> Tag {
    let name = Tag::String(JavaString::from(state.name.as_str()));
    if state.properties.is_empty() {
        return Tag::compound([("Name", name)]);
    }
    let properties = Tag::compound(
        state
            .properties
            .iter()
            .map(|(key, value)| (key.as_str(), Tag::String(JavaString::from(value.as_str())))),
    );
    Tag::compound([("Name", name), ("Properties", properties)])
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
    use horizon_world::region::{ChunkRecord, RegionFile, RegionWriter};

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

    /// A 1.16.5 table with air (id 0) and 17 distinct `test_block` states
    /// (ids 1..=17), so a section can exceed the 16-entry 4-bit palette.
    fn wide_table() -> BlockTable {
        let states: Vec<_> = (0..17u16)
            .map(|index| {
                serde_json::json!({
                    "id": index + 1,
                    "properties": {"variant": index.to_string()},
                })
            })
            .collect();
        let json = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0}]},
                "minecraft:test_block": {"states": states},
            }},
        });
        let path = std::env::temp_dir().join(format!(
            "horizon-1-16-5-world-wide-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("world")
        ));
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let table = BlockTable::load(&path).unwrap();
        fs::remove_file(&path).unwrap();
        table
    }

    /// Writes one chunk (Zlib, timestamp 0) into its region file.
    fn write_chunk(dir: &Path, root: NamedTag, x: i32, z: i32) {
        let path = dir
            .join("region")
            .join(format!("r.{}.{}.mca", x >> 5, z >> 5));
        let record = ChunkRecord::from_nbt(&root, Compression::Zlib, 0, Limits::saved()).unwrap();
        match RegionFile::open_writable(&path, true) {
            Ok(mut region) => region.write(ChunkPos::new(x, z), &record).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // No region directory yet: let RegionWriter create it.
                let mut writer = RegionWriter::create(&path).unwrap();
                writer.write(ChunkPos::new(x, z), &record).unwrap();
                writer.finish().unwrap();
            }
            Err(error) => panic!("opening {}: {error}", path.display()),
        }
    }

    /// The chunk NBT root read back from `dir`'s region file.
    fn read_chunk(dir: &Path, x: i32, z: i32) -> NamedTag {
        let path = dir
            .join("region")
            .join(format!("r.{}.{}.mca", x >> 5, z >> 5));
        let mut region = RegionFile::open(&path).unwrap();
        region
            .read(ChunkPos::new(x, z))
            .unwrap()
            .unwrap()
            .nbt(Limits::saved())
            .unwrap()
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
    fn chunk_column_writes_round_trip_air_stone_air() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        // One stone block at (3, 2, 1) in section 0; no section above.
        let palette = vec![entry("minecraft:air", None), entry("minecraft:stone", None)];
        let mut values = vec![0u64; SECTION_BLOCKS];
        values[531] = 1;
        let root = chunk_root(
            vec![section_tag(0, palette, Some(pack(&values, 4)))],
            None,
            0,
            0,
        );
        let mut column = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table)
            .unwrap()
            .unwrap();
        assert_eq!(column.state_id(3, 2, 1), Some(stone));
        assert_eq!(column.state_id(4, 2, 1), Some(air));
        assert!(column.section(1).is_none());

        // Air -> stone at a block that was air, then stone -> air again.
        column.set_state_id(4, 2, 1, stone, air).unwrap();
        assert_eq!(column.state_id(4, 2, 1), Some(stone));
        column.set_state_id(3, 2, 1, air, air).unwrap();
        assert_eq!(column.state_id(3, 2, 1), Some(air));
        assert_eq!(column.state_id(4, 2, 1), Some(stone));
    }

    #[test]
    fn chunk_column_allocates_a_missing_section() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        // A chunk whose only section is out of range (Y=-1) decodes to a
        // column with no sections at all.
        let root = chunk_root(vec![compound(&[("Y", Tag::Byte(-1))])], None, 0, 0);
        let mut column = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table)
            .unwrap()
            .unwrap();
        assert!(column.section(0).is_none());
        assert_eq!(column.state_id(5, 2, 5), None);

        // The write allocates the missing section filled with air.
        column.set_state_id(5, 2, 5, stone, air).unwrap();
        let section = column.section(0).unwrap();
        assert_eq!(section[(2 << 8) | (5 << 4) | 5], stone);
        assert_eq!(section.iter().filter(|&&id| id != air).count(), 1);
        assert_eq!(column.state_id(5, 2, 5), Some(stone));
        assert_eq!(column.state_id(5, 1, 5), Some(air));
    }

    #[test]
    fn chunk_column_rejects_out_of_range_coordinates() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        let root = chunk_root(
            vec![section_tag(0, vec![entry("minecraft:air", None)], None)],
            None,
            0,
            0,
        );
        let mut column = ChunkColumn::from_nbt(&root, ChunkPos::new(0, 0), &table)
            .unwrap()
            .unwrap();
        for (x, y, z) in [
            (16usize, 0usize, 0usize),
            (0, 16, 0),
            (0, 0, 16),
            (16, 16, 16),
        ] {
            let error = column.set_state_id(x, y, z, stone, air).unwrap_err();
            assert!(error.contains("outside 0..16"), "{error}");
            assert_eq!(column.state_id(x, y, z), None);
        }
    }

    #[test]
    fn world_mutates_states_and_reports_errors() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        let dir = temp_dir("mutation");
        let region_path = dir.join("region").join("r.0.0.mca");
        let mut writer = RegionWriter::create(&region_path).unwrap();
        // Chunk (0, 0), all air, section 0 only.
        let root = chunk_root(
            vec![section_tag(0, vec![entry("minecraft:air", None)], None)],
            None,
            0,
            0,
        );
        writer
            .write(
                ChunkPos::new(0, 0),
                &ChunkRecord::from_nbt(&root, Compression::Zlib, 0, Limits::saved()).unwrap(),
            )
            .unwrap();
        writer.finish().unwrap();

        let mut world = World::open(&dir, table).unwrap();
        assert_eq!(world.air_state(), BlockState::new("minecraft:air"));
        assert_eq!(world.state_id_at(0, 0, 0), Some(air));
        assert_eq!(world.state_id_at(0, 200, 0), Some(air));
        // Out-of-range y in a loaded chunk is air; no chunk at all is None.
        assert_eq!(world.state_id_at(0, -1, 0), Some(air));
        assert_eq!(world.state_id_at(0, 256, 0), Some(air));
        assert_eq!(world.state_id_at(64, 0, 0), None);

        // Air -> stone through the table-resolving write; the missing
        // section 12 is created, and `state_at` sees the new block.
        world
            .set_state(3, 2, 1, &BlockState::new("minecraft:stone"))
            .unwrap();
        assert_eq!(world.state_id_at(3, 2, 1), Some(stone));
        assert_eq!(
            world.state_at(3, 2, 1),
            Some(BlockState::new("minecraft:stone"))
        );
        world
            .set_state(3, 200, 1, &BlockState::new("minecraft:stone"))
            .unwrap();
        assert_eq!(world.state_id_at(3, 200, 1), Some(stone));
        // Stone -> air by raw id.
        world.set_state_id(3, 2, 1, air).unwrap();
        assert_eq!(world.state_id_at(3, 2, 1), Some(air));
        assert_eq!(
            world.state_at(3, 2, 1),
            Some(BlockState::new("minecraft:air"))
        );

        // Unknown states, unloaded chunks and out-of-range y are errors.
        let error = world
            .set_state(0, 0, 0, &BlockState::new("minecraft:not_a_block"))
            .unwrap_err();
        assert!(error.contains("not in the 1.16.5 block table"), "{error}");
        assert!(error.contains("minecraft:not_a_block"), "{error}");
        assert!(
            world
                .set_state(64, 0, 0, &BlockState::new("minecraft:stone"))
                .is_err()
        );
        assert!(
            world
                .set_state_id(64, 0, 0, stone)
                .unwrap_err()
                .contains("no chunk loaded")
        );
        for y in [-1, 256] {
            let error = world.set_state_id(0, y, 0, stone).unwrap_err();
            assert!(error.contains("outside 0..256"), "{error}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn first_and_default_state_resolve_bare_and_namespaced_names() {
        let table = test_table();
        // Bare and namespaced names resolve to the same state.
        assert_eq!(
            table.first_state("stone").unwrap(),
            BlockState::new("minecraft:stone")
        );
        assert_eq!(
            table.first_state("minecraft:stone").unwrap(),
            BlockState::new("minecraft:stone")
        );
        // The table does not store the `default` flag, so `default_state`
        // is the lowest-id state: oak_log{axis=x} (id 2), not the entry the
        // test JSON marks `default`.
        let first = table.first_state("oak_log").unwrap();
        assert_eq!(first.properties.get("axis").map(String::as_str), Some("x"));
        assert_eq!(table.default_state("oak_log").unwrap(), first);
        assert_eq!(
            table.default_state("stone").unwrap(),
            BlockState::new("minecraft:stone")
        );
        assert!(table.first_state("not_a_block").is_none());
        assert!(table.default_state("not_a_block").is_none());
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
    fn save_round_trips_changed_blocks_and_writes_a_1_16_5_root() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        let dir = temp_dir("save-roundtrip");
        // Chunk (0, 0), all air, section 0 only, with a recognisable biome.
        let root = chunk_root(
            vec![section_tag(0, vec![entry("minecraft:air", None)], None)],
            Some(vec![7; BIOMES]),
            0,
            0,
        );
        write_chunk(&dir, root, 0, 0);

        let mut world = World::open(&dir, table.clone()).unwrap();
        assert!(!world.has_unsaved_changes());
        assert_eq!(world.state_id_at(3, 2, 1), Some(air));
        world
            .set_state(3, 2, 1, &BlockState::new("minecraft:stone"))
            .unwrap();
        // Section 12 is created for the high-y block.
        world
            .set_state(3, 200, 1, &BlockState::new("minecraft:stone"))
            .unwrap();
        assert!(world.has_unsaved_changes());
        assert!(world.save(&dir).unwrap());
        assert!(!world.has_unsaved_changes());

        // The fresh root carries the 1.16.5 data version, a full status, a
        // local section palette and the column's biome array.
        let saved = read_chunk(&dir, 0, 0);
        assert_eq!(saved.tag.get("DataVersion"), Some(&Tag::Int(2586)));
        let level = saved.tag.get("Level").unwrap();
        assert_eq!(
            level.get("Status"),
            Some(&Tag::String(JavaString::from("full")))
        );
        let sections = match level.get("Sections") {
            Some(Tag::List(sections)) => sections,
            other => panic!("Level.Sections: {other:?}"),
        };
        assert_eq!(sections.len(), 2);
        assert!(
            sections
                .iter()
                .any(|section| section.get("Y") == Some(&Tag::Byte(12)))
        );
        let section0 = sections
            .iter()
            .find(|section| section.get("Y") == Some(&Tag::Byte(0)))
            .unwrap();
        match section0.get("Palette") {
            Some(Tag::List(palette)) => assert_eq!(palette.len(), 2),
            other => panic!("section 0 Palette: {other:?}"),
        }
        assert!(matches!(
            section0.get("BlockStates"),
            Some(Tag::LongArray(longs)) if longs.len() == 256
        ));
        assert!(matches!(
            level.get("Biomes"),
            Some(Tag::IntArray(biomes)) if biomes.len() == BIOMES && biomes.iter().all(|&b| b == 7)
        ));

        // The crate's own reader must see every changed state.
        let world = World::open(&dir, table).unwrap();
        assert_eq!(world.state_id_at(3, 2, 1), Some(stone));
        assert_eq!(world.state_id_at(3, 200, 1), Some(stone));
        assert_eq!(
            world.state_at(3, 200, 1),
            Some(BlockState::new("minecraft:stone"))
        );
        assert_eq!(world.state_id_at(4, 200, 1), Some(air));
        assert_eq!(world.chunk(0, 0).unwrap().biome(3, 2, 1), 7);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_tracks_dirty_chunks_and_skips_clean_ones() {
        let table = test_table();
        let air = table.state_id(&BlockState::new("minecraft:air")).unwrap();
        let stone = table.state_id(&BlockState::new("minecraft:stone")).unwrap();
        let dir = temp_dir("save-dirty");
        // Two chunks in the same region, both all air.
        for x in [0, 1] {
            write_chunk(
                &dir,
                chunk_root(
                    vec![section_tag(0, vec![entry("minecraft:air", None)], None)],
                    None,
                    x,
                    0,
                ),
                x,
                0,
            );
        }
        let mut world = World::open(&dir, table).unwrap();
        assert!(!world.has_unsaved_changes());
        // Writing the value already there is not a change.
        world.set_state_id(0, 0, 0, air).unwrap();
        assert!(!world.has_unsaved_changes());
        assert!(!world.save(&dir).unwrap());

        world.set_state_id(0, 0, 0, stone).unwrap();
        assert!(world.has_unsaved_changes());
        assert!(world.save(&dir).unwrap());
        assert!(!world.has_unsaved_changes());
        // Nothing left to write.
        assert!(!world.save(&dir).unwrap());

        // The dirty chunk was rewritten (fresh timestamp); the clean chunk
        // keeps the fixture's timestamp 0.
        let timestamps: Vec<_> = RegionFile::open(dir.join("region").join("r.0.0.mca"))
            .unwrap()
            .chunks()
            .collect();
        let (dirty, clean): (Vec<_>, Vec<_>) = timestamps
            .into_iter()
            .partition(|(position, _)| *position == ChunkPos::new(0, 0));
        assert_eq!(dirty.len(), 1);
        assert!(dirty[0].1 > 0);
        assert_eq!(clean, vec![(ChunkPos::new(1, 0), 0)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_round_trips_a_wide_palette_and_a_high_section() {
        let table = wide_table();
        let dir = temp_dir("save-wide");
        // Chunk (-1, 2) lives in r.-1.0.mca.
        write_chunk(
            &dir,
            chunk_root(
                vec![section_tag(0, vec![entry("minecraft:air", None)], None)],
                None,
                -1,
                2,
            ),
            -1,
            2,
        );
        let mut world = World::open(&dir, table.clone()).unwrap();
        // 17 distinct non-air states in section 0 (plus implicit air = an
        // 18-entry palette, past the 16-state 4-bit floor), at unique cells.
        for id in 1u16..=17 {
            world
                .set_state_id(-16 + i32::from(id % 16), 0, 32 + i32::from(id / 16), id)
                .unwrap();
        }
        // Section 15, i.e. y 240..256, still inside chunk (-1, 2).
        world.set_state_id(-11, 250, 39, 17).unwrap();
        assert!(world.has_unsaved_changes());
        assert!(world.save(&dir).unwrap());

        let saved = read_chunk(&dir, -1, 2);
        let sections = match saved
            .tag
            .get("Level")
            .and_then(|level| level.get("Sections"))
        {
            Some(Tag::List(sections)) => sections,
            other => panic!("Level.Sections: {other:?}"),
        };
        let section0 = sections
            .iter()
            .find(|section| section.get("Y") == Some(&Tag::Byte(0)))
            .unwrap();
        let palette = match section0.get("Palette") {
            Some(Tag::List(palette)) => palette.len(),
            other => panic!("section 0 Palette: {other:?}"),
        };
        assert_eq!(palette, 18);
        assert!(palette > 16, "palette {palette} must exceed 16 states");
        assert!(matches!(
            section0.get("BlockStates"),
            Some(Tag::LongArray(longs)) if longs.len() == 342
        ));
        assert!(
            sections
                .iter()
                .any(|section| section.get("Y") == Some(&Tag::Byte(15)))
        );

        // Everything survives a reopen through the crate's own reader.
        let world = World::open(&dir, table).unwrap();
        let air = world.state_id_at(-16, 0, 32).unwrap();
        for id in 1u16..=17 {
            assert_eq!(
                world.state_id_at(-16 + i32::from(id % 16), 0, 32 + i32::from(id / 16)),
                Some(id),
                "state {id}"
            );
        }
        assert_eq!(world.state_id_at(-11, 250, 39), Some(17));
        assert_eq!(world.state_id_at(-11, 251, 39), Some(air));
        assert_eq!(world.state_id_at(-12, 250, 39), Some(air));
        assert!(world.chunk(-1, 2).unwrap().section(15).is_some());
        fs::remove_dir_all(&dir).unwrap();
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
