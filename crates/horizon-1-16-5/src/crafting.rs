//! 1.16.5 crafting recipes for the playable survival slice.
//!
//! [`Recipes`] reads the shaped and shapeless crafting recipes out of a
//! 1.16.5 `server.jar` (`data/minecraft/recipes/*.json`) together with the
//! item tags those recipes point at (`data/minecraft/tags/items/*.json`,
//! nested tags included). Matching follows vanilla 1.16.5
//! `InventoryCrafting` semantics: shaped patterns are trimmed to their
//! bounding box and tried at every 3x3 offset (mirrored variants included),
//! while shapeless recipes consume one grid slot per ingredient in any order.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use zip::ZipArchive;

use crate::inventory::ItemStack;

/// Upper bound for nested `#tag` references resolved while loading.
const MAX_TAG_DEPTH: usize = 16;

/// One slot of a shaped pattern must accept the item in that grid cell.
///
/// Recipes spell these as `{ "item": ... }`, `{ "tag": ... }` or a JSON array
/// of either; tag entries are expanded to their items at load time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ingredient {
    /// A single, fully namespaced item id.
    Item(String),
    /// An item tag (`name` kept for diagnostics) with its expanded items.
    Tag { name: String, items: Vec<String> },
    /// Alternatives, any of which matches.
    Any(Vec<Ingredient>),
}

impl Ingredient {
    /// The tag id this ingredient resolves through, when it is a tag.
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Tag { name, .. } => Some(name),
            _ => None,
        }
    }

    fn matches(&self, item: &str) -> bool {
        match self {
            Self::Item(expected) => expected == item,
            Self::Tag { items, .. } => items.iter().any(|candidate| candidate == item),
            Self::Any(entries) => entries.iter().any(|entry| entry.matches(item)),
        }
    }
}

/// A `minecraft:crafting_shaped` recipe with its pattern trimmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shaped {
    id: String,
    /// Horizontal mirroring is part of vanilla shaped crafting.
    mirror: bool,
    width: usize,
    height: usize,
    /// `width * height` cells, row-major; `None` is an empty pattern cell.
    ingredients: Vec<Option<Ingredient>>,
    result: ItemStack,
}

impl Shaped {
    fn parse(id: &str, value: &Value, tags: &HashMap<String, Vec<String>>) -> Option<Self> {
        let pattern = value
            .get("pattern")?
            .as_array()?
            .iter()
            .map(|row| row.as_str().unwrap_or(""))
            .collect::<Vec<_>>();
        if pattern.is_empty() {
            return None;
        }
        let key = value.get("key")?.as_object()?;
        let result = parse_result(value.get("result")?)?;
        let mirror = value.get("mirror").and_then(Value::as_bool).unwrap_or(true);
        let width = pattern.iter().map(|row| row.chars().count()).max()?;
        if width == 0 {
            return None;
        }
        let rows = pattern
            .iter()
            .map(|row| {
                let mut chars = row.chars().collect::<Vec<_>>();
                chars.resize(width, ' ');
                chars
            })
            .collect::<Vec<_>>();

        // Vanilla shrinks the pattern to the bounding box of its non-empty
        // cells, so a small pattern can be placed anywhere in the 3x3 grid.
        let mut min_row = usize::MAX;
        let mut max_row = 0;
        let mut min_col = usize::MAX;
        let mut max_col = 0;
        for (row, chars) in rows.iter().enumerate() {
            for (col, ch) in chars.iter().enumerate() {
                if *ch == ' ' {
                    continue;
                }
                min_row = min_row.min(row);
                max_row = max_row.max(row);
                min_col = min_col.min(col);
                max_col = max_col.max(col);
            }
        }
        if min_row == usize::MAX {
            return None;
        }

        let mut ingredients = Vec::with_capacity((max_row - min_row + 1) * (max_col - min_col + 1));
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                let ch = rows[row][col];
                if ch == ' ' {
                    ingredients.push(None);
                    continue;
                }
                let entry = key.get(&ch.to_string())?;
                ingredients.push(Some(parse_ingredient(entry, tags)));
            }
        }

        Some(Self {
            id: id.to_owned(),
            mirror,
            width: max_col - min_col + 1,
            height: max_row - min_row + 1,
            ingredients,
            result,
        })
    }

    fn matches(&self, grid: &[Option<ItemStack>; 9]) -> bool {
        if self.width > 3 || self.height > 3 {
            return false;
        }
        for offset_x in 0..=(3 - self.width) {
            for offset_y in 0..=(3 - self.height) {
                if self.matches_at(grid, offset_x, offset_y, false) {
                    return true;
                }
                if self.mirror && self.width > 1 && self.matches_at(grid, offset_x, offset_y, true)
                {
                    return true;
                }
            }
        }
        false
    }

    fn matches_at(
        &self,
        grid: &[Option<ItemStack>; 9],
        offset_x: usize,
        offset_y: usize,
        mirror: bool,
    ) -> bool {
        for row in 0..3 {
            for col in 0..3 {
                let relative_x = col as isize - offset_x as isize;
                let relative_y = row as isize - offset_y as isize;
                let ingredient = if relative_x >= 0
                    && relative_y >= 0
                    && (relative_x as usize) < self.width
                    && (relative_y as usize) < self.height
                {
                    let pattern_col = if mirror {
                        self.width - 1 - relative_x as usize
                    } else {
                        relative_x as usize
                    };
                    self.ingredients[relative_y as usize * self.width + pattern_col].as_ref()
                } else {
                    None
                };
                let stack = grid[row * 3 + col]
                    .as_ref()
                    .filter(|stack| !stack.is_empty());
                match (ingredient, stack) {
                    (Some(ingredient), Some(stack)) => {
                        if !ingredient.matches(&stack.item) {
                            return false;
                        }
                    }
                    (None, None) => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

/// A `minecraft:crafting_shapeless` recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shapeless {
    id: String,
    ingredients: Vec<Ingredient>,
    result: ItemStack,
}

impl Shapeless {
    fn parse(id: &str, value: &Value, tags: &HashMap<String, Vec<String>>) -> Option<Self> {
        let ingredients = value.get("ingredients")?.as_array()?;
        if ingredients.is_empty() {
            return None;
        }
        let result = parse_result(value.get("result")?)?;
        Some(Self {
            id: id.to_owned(),
            ingredients: ingredients
                .iter()
                .map(|entry| parse_ingredient(entry, tags))
                .collect(),
            result,
        })
    }

    /// Every filled grid slot must consume one distinct ingredient; the
    /// order of the slots does not matter, exactly like vanilla.
    fn matches(&self, grid: &[Option<ItemStack>; 9]) -> bool {
        let mut remaining = self.ingredients.iter().collect::<Vec<_>>();
        let mut filled = 0;
        for stack in grid.iter().flatten().filter(|stack| !stack.is_empty()) {
            filled += 1;
            let Some(position) = remaining
                .iter()
                .position(|ingredient| ingredient.matches(&stack.item))
            else {
                return false;
            };
            remaining.remove(position);
        }
        filled == self.ingredients.len() && remaining.is_empty()
    }
}

/// Every 1.16.5 crafting recipe of a `server.jar`, keyed for a 3x3 grid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recipes {
    shaped: Vec<Shaped>,
    shapeless: Vec<Shapeless>,
}

impl Recipes {
    /// Reads `data/*/recipes/*.json` and `data/*/tags/items/*.json` from a
    /// 1.16.5 `server.jar`; non-crafting recipe types are skipped.
    pub fn load(server_jar: &Path) -> Result<Self, String> {
        let file =
            File::open(server_jar).map_err(|error| format!("{}: {error}", server_jar.display()))?;
        let mut archive =
            ZipArchive::new(file).map_err(|error| format!("{}: {error}", server_jar.display()))?;
        let mut raw_tags: HashMap<String, Vec<String>> = HashMap::new();
        let mut raw_recipes: Vec<(String, Value)> = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| format!("{}: {error}", server_jar.display()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            let Some((namespace, kind, tail)) = data_path(&name) else {
                continue;
            };
            let target = if kind == "recipes" {
                tail.strip_suffix(".json")
                    .map(|path| format!("{namespace}:{path}"))
            } else if kind == "tags" {
                tag_name(namespace, tail)
            } else {
                None
            };
            let Some(target) = target else {
                continue;
            };
            let mut json = String::new();
            entry
                .read_to_string(&mut json)
                .map_err(|error| format!("{name}: {error}"))?;
            let value: Value =
                serde_json::from_str(&json).map_err(|error| format!("{name}: {error}"))?;
            if kind == "recipes" {
                raw_recipes.push((target, value));
            } else {
                raw_tags.insert(target, tag_values(&value));
            }
        }

        let tags = expand_tags(&raw_tags);
        // File-system order is not stable across archives; a deterministic
        // order keeps `craft` reproducible when several recipes fit.
        raw_recipes.sort_by(|left, right| left.0.cmp(&right.0));
        let mut shaped = Vec::new();
        let mut shapeless = Vec::new();
        for (id, value) in raw_recipes {
            let Some(kind) = value.get("type").and_then(Value::as_str) else {
                continue;
            };
            match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                "crafting_shaped" => {
                    if let Some(recipe) = Shaped::parse(&id, &value, &tags) {
                        shaped.push(recipe);
                    }
                }
                "crafting_shapeless" => {
                    if let Some(recipe) = Shapeless::parse(&id, &value, &tags) {
                        shapeless.push(recipe);
                    }
                }
                _ => {}
            }
        }
        Ok(Self { shaped, shapeless })
    }

    /// Crafts the first matching recipe, returning one fresh result stack.
    ///
    /// The grid is row-major: slot 0 is the top-left cell and slot 8 the
    /// bottom-right one. Counts in the grid are irrelevant, like vanilla.
    pub fn craft(&self, grid: &[Option<ItemStack>; 9]) -> Option<ItemStack> {
        for recipe in &self.shaped {
            if recipe.matches(grid) {
                return Some(recipe.result.clone());
            }
        }
        for recipe in &self.shapeless {
            if recipe.matches(grid) {
                return Some(recipe.result.clone());
            }
        }
        None
    }

    /// Whether the recipe with `recipe_id` (namespaced or not) fits `grid`.
    pub fn matches(&self, recipe_id: &str, grid: &[Option<ItemStack>; 9]) -> bool {
        let id = normalize_namespaced(recipe_id);
        self.shaped
            .iter()
            .any(|recipe| recipe.id == id && recipe.matches(grid))
            || self
                .shapeless
                .iter()
                .any(|recipe| recipe.id == id && recipe.matches(grid))
    }

    /// Number of crafting recipes that were loaded.
    pub fn len(&self) -> usize {
        self.shaped.len() + self.shapeless.len()
    }

    /// Whether the jar contained no crafting recipes at all.
    pub fn is_empty(&self) -> bool {
        self.shaped.is_empty() && self.shapeless.is_empty()
    }
}

/// Splits `data/<namespace>/<kind>/<tail>` archive member names.
fn data_path(name: &str) -> Option<(&str, &str, &str)> {
    let rest = name.strip_prefix("data/")?;
    let mut parts = rest.splitn(3, '/');
    let namespace = parts.next()?;
    let kind = parts.next()?;
    let tail = parts.next()?;
    Some((namespace, kind, tail))
}

/// Tag id from a `tags/items/<path>.json` member; other tag folders are not
/// item tags and are ignored.
fn tag_name(namespace: &str, tail: &str) -> Option<String> {
    let path = tail.strip_suffix(".json")?;
    let (directory, rest) = path.split_once('/')?;
    if directory != "items" {
        return None;
    }
    Some(format!("{namespace}:{rest}"))
}

/// Raw `values` of one tag file, keeping nested `#tag` references marked.
fn tag_values(value: &Value) -> Vec<String> {
    let mut values = Vec::new();
    let Some(entries) = value.get("values").and_then(Value::as_array) else {
        return values;
    };
    for entry in entries {
        let id = match entry {
            Value::String(id) => id.as_str(),
            Value::Object(object) => object.get("id").and_then(Value::as_str).unwrap_or(""),
            _ => "",
        };
        if id.is_empty() {
            continue;
        }
        match id.strip_prefix('#') {
            Some(nested) => values.push(format!("#{}", normalize_namespaced(nested))),
            None => values.push(normalize_namespaced(id)),
        }
    }
    values
}

/// Flattens every raw tag into its item ids (nested tags, cycles capped).
fn expand_tags(raw: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let mut expanded = HashMap::with_capacity(raw.len());
    for name in raw.keys() {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        expand_tag(name, raw, 0, &mut seen, &mut items);
        expanded.insert(name.clone(), items);
    }
    expanded
}

fn expand_tag(
    name: &str,
    raw: &HashMap<String, Vec<String>>,
    depth: usize,
    seen: &mut HashSet<String>,
    items: &mut Vec<String>,
) {
    if depth > MAX_TAG_DEPTH || !seen.insert(name.to_owned()) {
        return;
    }
    let Some(values) = raw.get(name) else {
        return;
    };
    for value in values {
        if let Some(nested) = value.strip_prefix('#') {
            expand_tag(nested, raw, depth + 1, seen, items);
        } else if !items.iter().any(|existing| existing == value) {
            items.push(value.clone());
        }
    }
}

fn parse_ingredient(value: &Value, tags: &HashMap<String, Vec<String>>) -> Ingredient {
    match value {
        Value::String(item) => Ingredient::Item(normalize_namespaced(item)),
        Value::Array(entries) => Ingredient::Any(
            entries
                .iter()
                .map(|entry| parse_ingredient(entry, tags))
                .collect(),
        ),
        Value::Object(object) => {
            if let Some(item) = object.get("item").and_then(Value::as_str) {
                Ingredient::Item(normalize_namespaced(item))
            } else if let Some(tag) = object.get("tag").and_then(Value::as_str) {
                let name = normalize_namespaced(tag.strip_prefix('#').unwrap_or(tag));
                let items = tags.get(&name).cloned().unwrap_or_default();
                Ingredient::Tag { name, items }
            } else {
                // Unknown ingredient shapes never match, like a missing tag.
                Ingredient::Any(Vec::new())
            }
        }
        _ => Ingredient::Any(Vec::new()),
    }
}

fn parse_result(value: &Value) -> Option<ItemStack> {
    let value = match value {
        Value::Array(results) => results.first()?,
        other => other,
    };
    let item = value.get("item")?.as_str()?;
    let count = value
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .min(u32::MAX as u64) as u32;
    Some(ItemStack::new(normalize_namespaced(item), count))
}

fn normalize_namespaced(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    /// Shaped: stick (1x2), crafting table (2x2), mirror test (1x2), item-OR
    /// (1x1). Shapeless: planks from a nested log tag, and a two-item recipe
    /// whose order must not matter. `furnace` is not a crafting recipe.
    const ENTRIES: &[(&str, &str)] = &[
        (
            "data/minecraft/recipes/oak_planks.json",
            r####"{"type":"minecraft:crafting_shapeless","ingredients":[{"tag":"minecraft:logs"}],"result":{"item":"minecraft:oak_planks","count":4}}"####,
        ),
        (
            "data/minecraft/recipes/stick.json",
            r####"{"type":"minecraft:crafting_shaped","pattern":["#","#"],"key":{"#":{"item":"minecraft:oak_planks"}},"result":{"item":"minecraft:stick","count":4}}"####,
        ),
        (
            "data/minecraft/recipes/crafting_table.json",
            r####"{"type":"minecraft:crafting_shaped","pattern":["##","##"],"key":{"#":{"tag":"minecraft:planks"}},"result":{"item":"minecraft:crafting_table"}}"####,
        ),
        (
            "data/minecraft/recipes/mirror_test.json",
            r####"{"type":"minecraft:crafting_shaped","pattern":["#X"],"key":{"#":{"item":"minecraft:oak_planks"},"X":{"item":"minecraft:stick"}},"result":{"item":"minecraft:mirrored"}}"####,
        ),
        (
            "data/minecraft/recipes/or_test.json",
            r####"{"type":"minecraft:crafting_shaped","pattern":["#"],"key":{"#":[{"item":"minecraft:stone"},{"tag":"minecraft:planks"}]},"result":{"item":"minecraft:or_result"}}"####,
        ),
        (
            "data/minecraft/recipes/shapeless_order.json",
            r####"{"type":"minecraft:crafting_shapeless","ingredients":[{"item":"minecraft:oak_planks"},{"item":"minecraft:stick"}],"result":{"item":"minecraft:order_result"}}"####,
        ),
        (
            "data/minecraft/recipes/furnace.json",
            r####"{"type":"minecraft:smelting","ingredient":{"item":"minecraft:oak_log"},"result":{"item":"minecraft:charcoal"}}"####,
        ),
        (
            "data/minecraft/tags/items/logs.json",
            r####"{"values":["#minecraft:logs_that_burn"]}"####,
        ),
        (
            "data/minecraft/tags/items/logs_that_burn.json",
            r####"{"values":["minecraft:oak_log","minecraft:birch_log"]}"####,
        ),
        (
            "data/minecraft/tags/items/planks.json",
            r####"{"values":["minecraft:oak_planks","minecraft:birch_planks"]}"####,
        ),
    ];

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "horizon-crafting-{}-{name}.zip",
            std::process::id()
        ));
        let file = File::create(&path).expect("create fixture zip");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (entry, body) in ENTRIES {
            writer.start_file(*entry, options).expect("zip entry");
            writer.write_all(body.as_bytes()).expect("zip body");
        }
        writer.finish().expect("finish zip");
        path
    }

    fn load(name: &str) -> Recipes {
        let path = fixture(name);
        let recipes = Recipes::load(&path).expect("load recipes");
        let _ = std::fs::remove_file(&path);
        recipes
    }

    fn grid(cells: &[(usize, &str, u32)]) -> [Option<ItemStack>; 9] {
        let mut grid: [Option<ItemStack>; 9] = std::array::from_fn(|_| None);
        for (slot, item, count) in cells {
            grid[*slot] = Some(ItemStack::new(*item, *count));
        }
        grid
    }

    #[test]
    fn loads_only_crafting_recipes() {
        let recipes = load("loads");
        assert_eq!(recipes.shaped.len(), 4);
        assert_eq!(recipes.shapeless.len(), 2);
        assert_eq!(recipes.len(), 6);
        assert!(!recipes.is_empty());
    }

    #[test]
    fn shapeless_tag_planks_from_logs() {
        let recipes = load("planks");
        let result = recipes
            .craft(&grid(&[(4, "minecraft:oak_log", 7)]))
            .expect("planks");
        assert_eq!(result, ItemStack::new("minecraft:oak_planks", 4));
        // `logs` only reaches `birch_log` through the nested `logs_that_burn`.
        let birch = recipes
            .craft(&grid(&[(8, "minecraft:birch_log", 1)]))
            .expect("birch planks");
        assert_eq!(birch, ItemStack::new("minecraft:oak_planks", 4));
    }

    #[test]
    fn shaped_stick_matches_at_every_offset() {
        let recipes = load("stick");
        for slots in [[0usize, 3], [1, 4], [2, 5]] {
            let result = recipes.craft(&grid(&[
                (slots[0], "minecraft:oak_planks", 1),
                (slots[1], "minecraft:oak_planks", 1),
            ]));
            assert_eq!(
                result,
                Some(ItemStack::new("minecraft:stick", 4)),
                "{slots:?}"
            );
        }
    }

    #[test]
    fn crafting_table_matches_two_by_two_at_offset() {
        let recipes = load("table");
        let result = recipes.craft(&grid(&[
            (4, "minecraft:oak_planks", 1),
            (5, "minecraft:birch_planks", 1),
            (7, "minecraft:oak_planks", 1),
            (8, "minecraft:birch_planks", 1),
        ]));
        assert_eq!(result, Some(ItemStack::new("minecraft:crafting_table", 1)));
    }

    #[test]
    fn shaped_recipe_mirrors() {
        let recipes = load("mirror");
        let direct = recipes.craft(&grid(&[
            (0, "minecraft:oak_planks", 1),
            (1, "minecraft:stick", 1),
        ]));
        let mirrored = recipes.craft(&grid(&[
            (0, "minecraft:stick", 1),
            (1, "minecraft:oak_planks", 1),
        ]));
        assert_eq!(direct, Some(ItemStack::new("minecraft:mirrored", 1)));
        assert_eq!(mirrored, Some(ItemStack::new("minecraft:mirrored", 1)));
    }

    #[test]
    fn shapeless_recipes_match_any_order() {
        let recipes = load("shapeless");
        let first = recipes.craft(&grid(&[
            (0, "minecraft:stick", 1),
            (8, "minecraft:oak_planks", 1),
        ]));
        let second = recipes.craft(&grid(&[
            (0, "minecraft:oak_planks", 1),
            (8, "minecraft:stick", 1),
        ]));
        assert_eq!(first, Some(ItemStack::new("minecraft:order_result", 1)));
        assert_eq!(second, Some(ItemStack::new("minecraft:order_result", 1)));
    }

    #[test]
    fn ingredient_array_matches_either_item() {
        let recipes = load("or");
        assert_eq!(
            recipes.craft(&grid(&[(0, "minecraft:stone", 1)])),
            Some(ItemStack::new("minecraft:or_result", 1))
        );
        assert_eq!(
            recipes.craft(&grid(&[(8, "minecraft:birch_planks", 1)])),
            Some(ItemStack::new("minecraft:or_result", 1))
        );
    }

    #[test]
    fn non_matching_grids_return_none() {
        let recipes = load("none");
        assert!(recipes.craft(&grid(&[])).is_none());
        // Two horizontal planks are neither the vertical stick pattern nor a
        // single-cell recipe.
        assert!(
            recipes
                .craft(&grid(&[
                    (0, "minecraft:oak_planks", 1),
                    (1, "minecraft:oak_planks", 1),
                ]))
                .is_none()
        );
        assert!(
            recipes
                .craft(&grid(&[
                    (0, "minecraft:stone", 1),
                    (4, "minecraft:dirt", 1),
                ]))
                .is_none()
        );
    }

    #[test]
    fn matches_recipe_by_id() {
        let recipes = load("ids");
        let grid = grid(&[
            (0, "minecraft:oak_planks", 1),
            (3, "minecraft:oak_planks", 1),
        ]);
        assert!(recipes.matches("minecraft:stick", &grid));
        assert!(recipes.matches("stick", &grid));
        assert!(!recipes.matches("minecraft:crafting_table", &grid));
        assert!(!recipes.matches("minecraft:missing", &grid));
    }
}
