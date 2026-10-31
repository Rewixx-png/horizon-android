//! 1.16.5 block loot tables, read straight from `server.jar`.
//!
//! Every `data/minecraft/loot_tables/blocks/*.json` is loaded together with
//! `data/minecraft/tags/items/*.json` so `minecraft:tag` entries can expand to
//! concrete items. Only block tables are loaded: nested `minecraft:loot_table`
//! entries resolve when they point at another loaded block table, otherwise the
//! enclosing roll simply drops nothing.
//!
//! # Determinism
//!
//! Rolling uses `Lcg`, a 64-bit linear congruential generator. It is **not**
//! Java's `java.util.Random`, so a roll will not match vanilla's sequence for a
//! given seed, but it is fully deterministic here: the same table, seed and tool
//! always produce the same drops, and the seed advances once per draw.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use zip::ZipArchive;

use crate::inventory::ItemStack;

const TABLE_PREFIX: &str = "data/minecraft/loot_tables/blocks/";
const ITEM_TAG_PREFIX: &str = "data/minecraft/tags/items/";

/// All block loot tables of a 1.16.5 `server.jar`.
pub struct LootTables {
    tables: HashMap<String, Value>,
    item_tags: HashMap<String, Vec<String>>,
}

impl LootTables {
    /// Loads every `blocks/*.json` loot table (and the item tags they may
    /// reference) from a 1.16.5 `server.jar`.
    pub fn load(server_jar: &Path) -> Result<Self, String> {
        let file =
            File::open(server_jar).map_err(|error| format!("{}: {error}", server_jar.display()))?;
        let mut archive =
            ZipArchive::new(file).map_err(|error| format!("{}: {error}", server_jar.display()))?;
        let mut tables = HashMap::new();
        let mut item_tags = HashMap::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| format!("{}: {error}", server_jar.display()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            let table_key = name
                .strip_prefix(TABLE_PREFIX)
                .and_then(|path| path.strip_suffix(".json"));
            let tag_key = name
                .strip_prefix(ITEM_TAG_PREFIX)
                .and_then(|path| path.strip_suffix(".json"));
            if table_key.is_none() && tag_key.is_none() {
                continue;
            }
            let mut json = String::new();
            entry
                .read_to_string(&mut json)
                .map_err(|error| format!("{name}: {error}"))?;
            let value: Value =
                serde_json::from_str(&json).map_err(|error| format!("{name}: {error}"))?;
            if let Some(key) = table_key {
                tables.insert(key.to_owned(), value);
            } else if let Some(key) = tag_key {
                item_tags.insert(key.to_owned(), tag_values(&value));
            }
        }
        Ok(Self { tables, item_tags })
    }

    /// Rolls the drops for `block` with `seed` and an optional held tool.
    ///
    /// `tool` is the item name of the held tool (`"minecraft:iron_pickaxe"`);
    /// enchantments are not modelled, so predicates that require a specific
    /// enchantment (such as silk touch) never match. A block without a loaded
    /// table falls back to its own item name, which is the usual case in 1.16.5
    /// where most blocks drop themselves.
    pub fn roll(&self, block: &str, seed: u64, tool: Option<&str>) -> Vec<ItemStack> {
        let key = path_of(block);
        let Some(table) = self.tables.get(key) else {
            return vec![ItemStack::new(item_name(block), 1)];
        };
        let mut context = RollContext {
            tables: self,
            tool: tool.map(path_of),
            rng: Lcg::new(seed),
            depth: 0,
        };
        let mut out = Vec::new();
        context.roll_table(table, &mut out);
        out
    }

    /// The number of loaded block loot tables; used by diagnostics and tests.
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Whether no block loot table was loaded.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    fn expand_item_tag(&self, tag: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        self.expand_tag(path_of(tag), &mut seen, &mut out);
        out
    }

    fn expand_tag(&self, key: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        if !seen.insert(key.to_owned()) {
            return;
        }
        let Some(values) = self.item_tags.get(key) else {
            return;
        };
        for value in values {
            if let Some(nested) = value.strip_prefix('#') {
                self.expand_tag(path_of(nested), seen, out);
            } else {
                out.push(item_name(value));
            }
        }
    }
}

struct RollContext<'a> {
    tables: &'a LootTables,
    tool: Option<&'a str>,
    rng: Lcg,
    depth: u8,
}

impl<'a> RollContext<'a> {
    fn table(&self, key: &str) -> Option<&'a Value> {
        self.tables.tables.get(key)
    }

    fn roll_table(&mut self, table: &Value, out: &mut Vec<ItemStack>) {
        let Some(pools) = table.get("pools").and_then(Value::as_array) else {
            return;
        };
        for pool in pools {
            if !self.conditions(pool.get("conditions")) {
                continue;
            }
            let rolls = self.roll_count(pool.get("rolls"));
            let Some(entries) = pool.get("entries").and_then(Value::as_array) else {
                continue;
            };
            for _ in 0..rolls {
                if let Some(index) = self.pick(entries) {
                    self.entry(&entries[index], out);
                }
            }
        }
    }

    fn entry(&mut self, entry: &Value, out: &mut Vec<ItemStack>) {
        if !self.conditions(entry.get("conditions")) {
            return;
        }
        if kind(entry) == "minecraft:alternatives" {
            // `alternatives` takes the first child whose conditions pass.
            let Some(children) = entry.get("children").and_then(Value::as_array) else {
                return;
            };
            for child in children {
                if self.conditions(child.get("conditions")) {
                    self.entry_body(child, out);
                    return;
                }
            }
            return;
        }
        self.entry_body(entry, out);
    }

    fn entry_body(&mut self, entry: &Value, out: &mut Vec<ItemStack>) {
        match kind(entry) {
            "minecraft:item" => {
                let Some(name) = entry.get("name").and_then(Value::as_str) else {
                    return;
                };
                let count = self.functions_count(entry.get("functions"));
                if count > 0 {
                    out.push(ItemStack::new(name.to_owned(), count));
                }
            }
            "minecraft:tag" => {
                let Some(name) = entry.get("name").and_then(Value::as_str) else {
                    return;
                };
                let values = self.tables.expand_item_tag(name);
                if values.is_empty() {
                    return;
                }
                let index = self.rng.below(values.len() as u64) as usize;
                let count = self.functions_count(entry.get("functions"));
                if count > 0 {
                    out.push(ItemStack::new(values[index].clone(), count));
                }
            }
            "minecraft:loot_table" => {
                if self.depth >= 8 {
                    return;
                }
                let Some(name) = entry.get("name").and_then(Value::as_str) else {
                    return;
                };
                if let Some(table) = self.table(path_of(name)) {
                    self.depth += 1;
                    self.roll_table(table, out);
                    self.depth -= 1;
                }
            }
            "minecraft:group" => {
                if let Some(children) = entry.get("children").and_then(Value::as_array) {
                    for child in children {
                        self.entry(child, out);
                    }
                }
            }
            "minecraft:alternatives" => {
                // Nested inside another entry body, e.g. gravel's flint/gravel
                // pair under an outer silk-touch alternative.
                if let Some(children) = entry.get("children").and_then(Value::as_array) {
                    for child in children {
                        if self.conditions(child.get("conditions")) {
                            self.entry_body(child, out);
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Picks an entry index by its `weight` (default 1).
    fn pick(&mut self, entries: &[Value]) -> Option<usize> {
        let weights: Vec<u64> = entries
            .iter()
            .map(|entry| entry.get("weight").and_then(Value::as_u64).unwrap_or(1))
            .collect();
        let total: u64 = weights.iter().sum();
        if total == 0 {
            return None;
        }
        let mut target = self.rng.below(total);
        for (index, weight) in weights.iter().enumerate() {
            if target < *weight {
                return Some(index);
            }
            target -= *weight;
        }
        None
    }

    fn conditions(&mut self, conditions: Option<&Value>) -> bool {
        let Some(conditions) = conditions.and_then(Value::as_array) else {
            return true;
        };
        conditions.iter().all(|condition| self.condition(condition))
    }

    fn condition(&mut self, condition: &Value) -> bool {
        match condition_kind(condition) {
            "minecraft:survives_explosion" => condition
                .get("survives_explosion")
                .or_else(|| condition.get("value"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            "minecraft:match_tool" => {
                // Enchantments are not tracked, so a predicate that requires
                // one (silk touch) can never be satisfied.
                let Some(predicate) = condition.get("predicate") else {
                    return true;
                };
                if !predicate
                    .get("enchantments")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
                {
                    return false;
                }
                match predicate.get("item").and_then(Value::as_str) {
                    Some(item) => self.tool == Some(path_of(item)),
                    None => true,
                }
            }
            "minecraft:random_chance" => condition
                .get("chance")
                .and_then(Value::as_f64)
                .is_none_or(|chance| self.rng.next_f64() < chance),
            // Fortune-enchantment dependent; without an enchantment level use
            // the first (no-fortune) chance.
            "minecraft:table_bonus" => condition
                .get("chances")
                .and_then(Value::as_array)
                .and_then(|chances| chances.first())
                .and_then(Value::as_f64)
                .is_none_or(|chance| self.rng.next_f64() < chance),
            "minecraft:inverted" => {
                if let Some(term) = condition.get("term") {
                    return !self.condition(term);
                }
                let terms = condition
                    .get("terms")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                !terms.iter().any(|term| self.condition(term))
            }
            "minecraft:alternative" => {
                let terms = condition
                    .get("terms")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                terms.iter().any(|term| self.condition(term))
            }
            // Block state, entity and location predicates are not modelled;
            // they pass so a drop is not silently lost.
            _ => true,
        }
    }

    fn functions_count(&mut self, functions: Option<&Value>) -> u32 {
        let mut count = 1;
        let Some(functions) = functions.and_then(Value::as_array) else {
            return count;
        };
        for function in functions {
            if function_kind(function) == "minecraft:set_count" {
                if let Some(value) = function.get("count") {
                    count = self.count_value(value);
                }
            }
        }
        count
    }

    fn roll_count(&mut self, rolls: Option<&Value>) -> u32 {
        match rolls {
            Some(value) => self.count_value(value),
            None => 1,
        }
    }

    fn count_value(&mut self, value: &Value) -> u32 {
        match value {
            Value::Number(number) => number
                .as_f64()
                .unwrap_or(1.0)
                .round()
                .clamp(0.0, u32::MAX as f64) as u32,
            Value::Object(fields) => match fields.get("type").and_then(Value::as_str) {
                Some("minecraft:uniform") => {
                    let min = fields.get("min").and_then(Value::as_f64).unwrap_or(0.0);
                    let max = fields.get("max").and_then(Value::as_f64).unwrap_or(min);
                    self.uniform_int(min, max)
                }
                Some("minecraft:binomial") => {
                    let n = fields
                        .get("n")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                        .max(0.0) as u32;
                    let p = fields.get("p").and_then(Value::as_f64).unwrap_or(0.0);
                    (0..n).filter(|_| self.rng.next_f64() < p).count() as u32
                }
                _ => fields
                    .get("value")
                    .and_then(Value::as_f64)
                    .map(|value| value.round().clamp(0.0, u32::MAX as f64) as u32)
                    .unwrap_or(1),
            },
            _ => 1,
        }
    }

    fn uniform_int(&mut self, min: f64, max: f64) -> u32 {
        let low = min.round() as i64;
        let high = max.round() as i64;
        if high <= low {
            return low.max(0) as u32;
        }
        let span = (high - low + 1) as u64;
        (low + self.rng.below(span) as i64).clamp(0, i64::from(u32::MAX)) as u32
    }
}

/// A small deterministic 64-bit LCG; not Java's `Random`.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `0..bound`; does not advance for a bound of 0 or 1.
    fn below(&mut self, bound: u64) -> u64 {
        if bound <= 1 {
            0
        } else {
            self.next_u64() % bound
        }
    }
}

fn kind(value: &Value) -> &str {
    value.get("type").and_then(Value::as_str).unwrap_or("")
}

/// Condition objects label their kind with `condition`, not `type`.
fn condition_kind(value: &Value) -> &str {
    value.get("condition").and_then(Value::as_str).unwrap_or("")
}

/// Function objects label their kind with `function`, not `type`.
fn function_kind(value: &Value) -> &str {
    value.get("function").and_then(Value::as_str).unwrap_or("")
}

/// The path part of a namespaced name: `minecraft:stone` becomes `stone`.
fn path_of(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn item_name(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

fn tag_values(value: &Value) -> Vec<String> {
    value
        .get("values")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| match value {
                    Value::String(name) => Some(name.clone()),
                    Value::Object(fields) => {
                        fields.get("id").and_then(Value::as_str).map(str::to_owned)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_JAR: AtomicU64 = AtomicU64::new(0);

    /// Writes a throwaway `server.jar` containing `blocks/<name>.json` tables.
    fn jar_with(tables: &[(&str, Value)]) -> PathBuf {
        let unique = NEXT_JAR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "horizon-1-16-5-loot-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, json) in tables {
            zip.start_file(
                format!("{TABLE_PREFIX}{name}.json"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(json.to_string().as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn remove(jar: &Path) {
        if let Some(dir) = jar.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn survives_explosion_false_drops_nothing() {
        let jar = jar_with(&[(
            "glass",
            serde_json::json!({
                "pools": [{
                    "rolls": 1,
                    "entries": [{
                        "type": "minecraft:item",
                        "name": "minecraft:glass",
                        "conditions": [{
                            "condition": "minecraft:survives_explosion",
                            "survives_explosion": false
                        }]
                    }]
                }]
            }),
        )]);
        let tables = LootTables::load(&jar).unwrap();
        assert_eq!(tables.len(), 1);
        assert!(tables.roll("minecraft:glass", 3, None).is_empty());
        remove(&jar);
    }

    #[test]
    fn set_count_is_deterministic_and_uniform() {
        let jar = jar_with(&[(
            "redstone_ore",
            serde_json::json!({
                "pools": [{
                    "rolls": 1,
                    "entries": [{
                        "type": "minecraft:item",
                        "name": "minecraft:redstone",
                        "functions": [{
                            "function": "minecraft:set_count",
                            "count": {"type": "minecraft:uniform", "min": 1, "max": 4}
                        }]
                    }]
                }]
            }),
        )]);
        let tables = LootTables::load(&jar).unwrap();
        for seed in 0..8 {
            let first = tables.roll("minecraft:redstone_ore", seed, None);
            let second = tables.roll("minecraft:redstone_ore", seed, None);
            assert_eq!(first, second);
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].item, "minecraft:redstone");
            assert!((1..=4).contains(&first[0].count));
        }
        let counts: HashSet<u32> = (0..8)
            .map(|seed| tables.roll("minecraft:redstone_ore", seed, None)[0].count)
            .collect();
        assert_eq!(counts, HashSet::from([1, 2, 3, 4]));
        remove(&jar);
    }

    #[test]
    fn alternatives_prefers_the_matching_conditional_entry() {
        let jar = jar_with(&[
            (
                "stone",
                serde_json::json!({
                    "pools": [{
                        "rolls": 1,
                        "entries": [{
                            "type": "minecraft:alternatives",
                            "children": [
                                {
                                    "type": "minecraft:item",
                                    "name": "minecraft:stone",
                                    "conditions": [{
                                        "condition": "minecraft:match_tool",
                                        "predicate": {"item": "minecraft:shears"}
                                    }]
                                },
                                {
                                    "type": "minecraft:item",
                                    "name": "minecraft:cobblestone",
                                    "conditions": [{"condition": "minecraft:survives_explosion"}]
                                }
                            ]
                        }]
                    }]
                }),
            ),
            (
                "glass",
                serde_json::json!({
                    "pools": [{
                        "rolls": 1,
                        "entries": [{
                            "type": "minecraft:item",
                            "name": "minecraft:glass",
                            "conditions": [{
                                "condition": "minecraft:match_tool",
                                "predicate": {
                                    "enchantments": [{"enchantment": "minecraft:silk_touch"}]
                                }
                            }]
                        }]
                    }]
                }),
            ),
        ]);
        let tables = LootTables::load(&jar).unwrap();
        let shears = tables.roll("minecraft:stone", 5, Some("minecraft:shears"));
        assert_eq!(shears, vec![ItemStack::new("minecraft:stone", 1)]);
        let hand = tables.roll("minecraft:stone", 5, None);
        assert_eq!(hand, vec![ItemStack::new("minecraft:cobblestone", 1)]);
        // A condition requiring an enchantment never matches, so no silk touch.
        assert!(
            tables
                .roll("minecraft:glass", 5, Some("minecraft:shears"))
                .is_empty()
        );
        remove(&jar);
    }

    #[test]
    fn unknown_block_falls_back_to_its_own_item() {
        let jar = jar_with(&[]);
        let tables = LootTables::load(&jar).unwrap();
        assert!(tables.is_empty());
        assert_eq!(
            tables.roll("minecraft:bedrock", 11, None),
            vec![ItemStack::new("minecraft:bedrock", 1)]
        );
        assert_eq!(
            tables.roll("bedrock", 11, None),
            vec![ItemStack::new("minecraft:bedrock", 1)],
            "unqualified names are given the minecraft namespace"
        );
        remove(&jar);
    }
}
