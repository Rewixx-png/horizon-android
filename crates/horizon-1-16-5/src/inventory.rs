//! A nine-slot creative hotbar for the 1.16.5 playable slice.

use crate::{BlockState, BlockTable};

/// The most items one slot holds, matching vanilla's default stack size.
pub const MAX_STACK_SIZE: u32 = 64;

/// A stack of items in a hotbar slot.
///
/// Items are named like blocks in 1.16.5 (`minecraft:stone`), so an item that
/// names a placeable block resolves to that block's default state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemStack {
    pub item: String,
    pub count: u32,
}

impl ItemStack {
    pub fn new(item: impl Into<String>, count: u32) -> Self {
        Self {
            item: item.into(),
            count,
        }
    }

    /// Whether this stack holds nothing.
    pub fn is_empty(&self) -> bool {
        self.count == 0 || self.item.is_empty()
    }

    /// The block state this item places, when the item names a block.
    ///
    /// Items are named after their block in 1.16.5, so `minecraft:stone`
    /// resolves to the block table's default stone state. Items without a
    /// matching block (`minecraft:stone_sword`) return `None`.
    pub fn block_state(&self, table: &BlockTable) -> Option<BlockState> {
        if self.is_empty() {
            return None;
        }
        table.default_state(&self.item)
    }
}

/// Total inventory slots: a nine-slot hotbar followed by twenty-seven
/// storage slots, like vanilla's player inventory.
pub const SLOTS: usize = 36;
/// Hotbar slots, the only ones the hotbar API selects or writes.
const HOTBAR_SLOTS: usize = 9;

/// A nine-slot hotbar backed by twenty-seven storage slots.
#[derive(Clone, Debug, PartialEq)]
pub struct Inventory {
    slots: [Option<ItemStack>; SLOTS],
    selected: usize,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::empty()
    }
}

impl Inventory {
    /// Total slots, hotbar and storage together.
    pub const SLOTS: usize = crate::inventory::SLOTS;

    /// Every slot empty, with the first hotbar slot selected.
    pub fn empty() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            selected: 0,
        }
    }

    /// Stone, dirt, grass, planks, log, glass, bricks, stone bricks and wool
    /// in the hotbar; storage starts empty.
    pub fn creative() -> Self {
        let hotbar = [
            Some(ItemStack::new("minecraft:stone", 1)),
            Some(ItemStack::new("minecraft:dirt", 1)),
            Some(ItemStack::new("minecraft:grass_block", 1)),
            Some(ItemStack::new("minecraft:oak_planks", 1)),
            Some(ItemStack::new("minecraft:oak_log", 1)),
            Some(ItemStack::new("minecraft:glass", 1)),
            Some(ItemStack::new("minecraft:bricks", 1)),
            Some(ItemStack::new("minecraft:stone_bricks", 1)),
            Some(ItemStack::new("minecraft:white_wool", 1)),
        ];
        let mut slots: [Option<ItemStack>; SLOTS] = std::array::from_fn(|_| None);
        for (slot, stack) in slots.iter_mut().zip(hotbar) {
            *slot = stack;
        }
        Self { slots, selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selects `slot` (0..9); out-of-range values are ignored.
    pub fn select(&mut self, slot: usize) {
        if slot < HOTBAR_SLOTS {
            self.selected = slot;
        }
    }

    /// Moves the selection by `delta`, wrapping through the nine slots.
    pub fn scroll(&mut self, delta: i32) {
        self.selected =
            (self.selected as i64 + delta as i64).rem_euclid(HOTBAR_SLOTS as i64) as usize;
    }

    /// The block state the selected stack places, when it is a block item.
    pub fn held(&self, table: &BlockTable) -> Option<BlockState> {
        self.slots[self.selected].as_ref()?.block_state(table)
    }

    /// The nine hotbar slots: the view every existing hotbar caller uses.
    pub fn slots(&self) -> &[Option<ItemStack>; 9] {
        self.hotbar()
    }

    /// The nine hotbar slots.
    pub fn hotbar(&self) -> &[Option<ItemStack>; 9] {
        let (hotbar, _) = self.slots.split_at(HOTBAR_SLOTS);
        std::convert::TryFrom::try_from(hotbar).expect("the hotbar is the first nine slots")
    }

    /// The twenty-seven storage slots behind the hotbar.
    pub fn storage(&self) -> &[Option<ItemStack>; 27] {
        let (_, storage) = self.slots.split_at(HOTBAR_SLOTS);
        std::convert::TryFrom::try_from(storage).expect("storage is the last twenty-seven slots")
    }

    /// The stack in `slot` (0..36, hotbar first); empty and out-of-range
    /// slots return `None`.
    /// Every slot, hotbar first, then storage.
    pub fn all_slots(&self) -> &[Option<ItemStack>; SLOTS] {
        &self.slots
    }

    pub fn slot(&self, index: usize) -> Option<&ItemStack> {
        self.slots.get(index)?.as_ref()
    }

    /// Stores `stack` in `slot` (0..36); out-of-range values are ignored.
    pub fn set(&mut self, index: usize, stack: Option<ItemStack>) {
        if index < SLOTS {
            self.slots[index] = stack;
        }
    }

    /// The vanilla block-model texture used as the hotbar icon for `slot`,
    /// e.g. `minecraft:block/stone`. Empty slots return `None`; other items
    /// fall back to their `minecraft:item/<name>` sprite.
    pub fn icon_texture(&self, slot: usize) -> Option<String> {
        let stack = self.slots.get(slot)?.as_ref()?;
        icon_texture_for_item(&stack.item)
    }

    /// The icon texture's sprite-local UV rectangle, `[0, 0, 1, 1]`, or
    /// `None` when [`Self::icon_texture`] has no icon for `slot`.
    ///
    /// The app resolves the atlas sprite for [`Self::icon_texture`] itself
    /// and samples it with these local coordinates.
    pub fn icon(&self, slot: usize) -> Option<[f32; 4]> {
        let stack = self.slots.get(slot)?.as_ref()?;
        item_path(&stack.item).map(|_| [0.0, 0.0, 1.0, 1.0])
    }

    /// Stores `stack` in a hotbar `slot` (0..9); other values are ignored.
    pub fn set_slot(&mut self, slot: usize, stack: Option<ItemStack>) {
        if slot < HOTBAR_SLOTS {
            self.slots[slot] = stack;
        }
    }

    /// Inserts `stack`, merging into the selected slot first, then into the
    /// first matching or empty hotbar slot, then into storage. Returns the
    /// number of items that did not fit.
    pub fn add(&mut self, stack: ItemStack) -> usize {
        if stack.is_empty() {
            return 0;
        }
        let item = stack.item;
        let mut remaining = stack.count;

        remaining = merge_slot(&mut self.slots[self.selected], &item, remaining);
        for slot in self.slots.iter_mut() {
            if remaining == 0 {
                break;
            }
            remaining = merge_slot(slot, &item, remaining);
        }
        remaining as usize
    }

    /// Removes up to `amount` items from the selected slot, returning them as
    /// a stack when anything was taken.
    pub fn take_selected(&mut self, amount: u32) -> Option<ItemStack> {
        if amount == 0 {
            return None;
        }
        let slot = &mut self.slots[self.selected];
        let stack = slot.as_mut()?;
        let taken = stack.count.min(amount);
        if taken == 0 {
            return None;
        }
        stack.count -= taken;
        let item = stack.item.clone();
        if stack.count == 0 {
            *slot = None;
        }
        Some(ItemStack { item, count: taken })
    }

    /// Stores `stack` in the hotbar first, then in storage, ignoring the
    /// selection. Returns the number of items that did not fit.
    pub fn add_to_storage(&mut self, stack: ItemStack) -> usize {
        if stack.is_empty() {
            return 0;
        }
        let item = stack.item;
        let mut remaining = stack.count;
        // Hotbar slots first, then the twenty-seven storage slots.
        for slot in self.slots.iter_mut() {
            if remaining == 0 {
                break;
            }
            remaining = merge_slot(slot, &item, remaining);
        }
        remaining as usize
    }

    /// Removes one `item` from the first slot holding it, hotbar first, and
    /// returns whether anything was removed.
    pub fn remove_one(&mut self, item: &str) -> bool {
        for slot in self.slots.iter_mut() {
            let matches = match slot {
                Some(stack) => stack.item == item,
                None => false,
            };
            if !matches {
                continue;
            }
            let emptied = match slot {
                Some(stack) => {
                    stack.count -= 1;
                    stack.count == 0
                }
                None => false,
            };
            if emptied {
                *slot = None;
            }
            return true;
        }
        false
    }
}

/// Fills `slot` with `item` up to [`MAX_STACK_SIZE`], returning what remains.
fn merge_slot(slot: &mut Option<ItemStack>, item: &str, mut remaining: u32) -> u32 {
    match slot {
        Some(existing) if existing.item == item => {
            let room = MAX_STACK_SIZE.saturating_sub(existing.count);
            let moved = room.min(remaining);
            existing.count += moved;
            remaining -= moved;
        }
        None => {
            let moved = remaining.min(MAX_STACK_SIZE);
            *slot = Some(ItemStack {
                item: item.to_owned(),
                count: moved,
            });
            remaining -= moved;
        }
        Some(_) => {}
    }
    remaining
}

/// The vanilla block-model texture that renders a creative block as its
/// hotbar icon. Bare names are treated as `minecraft:` names.
fn block_icon_texture(path: &str) -> Option<&'static str> {
    Some(match path {
        "stone" => "minecraft:block/stone",
        "dirt" => "minecraft:block/dirt",
        "grass_block" => "minecraft:block/grass_block_side",
        "oak_planks" => "minecraft:block/oak_planks",
        "oak_log" => "minecraft:block/oak_log",
        "glass" => "minecraft:block/glass",
        "bricks" => "minecraft:block/bricks",
        "stone_bricks" => "minecraft:block/stone_bricks",
        "white_wool" => "minecraft:block/white_wool",
        _ => return None,
    })
}

/// The atlas texture used for `item`'s hotbar icon.
///
/// Block items keep their vanilla block-model texture; everything else
/// resolves to `minecraft:item/<name>`. The caller checks the name against
/// the client jar's atlas, so this returns the conventional name even for
/// items the current jar does not ship.
pub fn icon_texture_for_item(item: &str) -> Option<String> {
    let path = item_path(item)?;
    Some(match block_icon_texture(path) {
        Some(texture) => texture.to_owned(),
        None => format!("minecraft:item/{path}"),
    })
}

/// The path part of an item name, with the `minecraft:` namespace stripped.
fn item_path(item: &str) -> Option<&str> {
    let path = item.strip_prefix("minecraft:").unwrap_or(item);
    (!path.is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BLOCKS: [&str; 9] = [
        "stone",
        "dirt",
        "grass_block",
        "oak_planks",
        "oak_log",
        "glass",
        "bricks",
        "stone_bricks",
        "white_wool",
    ];

    fn table() -> BlockTable {
        let mut entries = serde_json::Map::new();
        for (index, name) in BLOCKS.iter().enumerate() {
            entries.insert(
                format!("minecraft:{name}"),
                json!({"states": [{"id": index + 1}]}),
            );
        }
        let json = json!({"version": "1.16.5", "blocks": {"entries": entries}});
        let path = std::env::temp_dir().join(format!(
            "horizon-1-16-5-inventory-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let table = BlockTable::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        table
    }

    #[test]
    fn creative_preset_contents() {
        let inventory = Inventory::creative();
        let contents: Vec<Option<(&str, u32)>> = inventory
            .slots()
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|stack| (stack.item.as_str(), stack.count))
            })
            .collect();
        assert_eq!(
            contents,
            vec![
                Some(("minecraft:stone", 1)),
                Some(("minecraft:dirt", 1)),
                Some(("minecraft:grass_block", 1)),
                Some(("minecraft:oak_planks", 1)),
                Some(("minecraft:oak_log", 1)),
                Some(("minecraft:glass", 1)),
                Some(("minecraft:bricks", 1)),
                Some(("minecraft:stone_bricks", 1)),
                Some(("minecraft:white_wool", 1)),
            ]
        );
    }

    #[test]
    fn select_and_held() {
        let table = table();
        let mut inventory = Inventory::creative();
        assert_eq!(inventory.selected(), 0);
        assert_eq!(inventory.held(&table).unwrap().name, "minecraft:stone");

        inventory.select(4);
        assert_eq!(inventory.selected(), 4);
        assert_eq!(inventory.held(&table).unwrap().name, "minecraft:oak_log");

        // Out-of-range selects are ignored, not a panic.
        inventory.select(9);
        inventory.select(usize::MAX);
        assert_eq!(inventory.selected(), 4);
    }

    #[test]
    fn scroll_wraps_both_ways() {
        let mut inventory = Inventory::creative();

        inventory.scroll(1);
        assert_eq!(inventory.selected(), 1);
        inventory.scroll(-1);
        assert_eq!(inventory.selected(), 0);

        // Negative and large deltas wrap modulo nine.
        inventory.scroll(-1);
        assert_eq!(inventory.selected(), 8);
        inventory.scroll(10);
        assert_eq!(inventory.selected(), 0);
        inventory.scroll(-17);
        assert_eq!(inventory.selected(), 1);
    }

    #[test]
    fn set_slot_ignores_out_of_range() {
        let table = table();
        let mut inventory = Inventory::creative();
        let wool = inventory.slots()[8].clone();

        inventory.set_slot(0, None);
        assert_eq!(inventory.held(&table), None);

        // Out-of-range stores are ignored, not a panic.
        inventory.set_slot(9, Some(ItemStack::new("minecraft:dirt", 1)));
        inventory.set_slot(usize::MAX, Some(ItemStack::new("minecraft:dirt", 1)));
        assert_eq!(inventory.slots()[8], wool);
    }

    #[test]
    fn item_stacks_resolve_blocks_only() {
        let table = table();

        assert_eq!(
            ItemStack::new("minecraft:stone", 1).block_state(&table),
            Some(BlockState::new("minecraft:stone"))
        );
        assert_eq!(
            ItemStack::new("minecraft:stone_sword", 1).block_state(&table),
            None
        );
        assert_eq!(
            ItemStack::new("minecraft:stone", 0).block_state(&table),
            None
        );
        assert_eq!(ItemStack::new("", 1).block_state(&table), None);

        let mut inventory = Inventory::creative();
        inventory.set_slot(0, Some(ItemStack::new("minecraft:stone_sword", 1)));
        assert_eq!(inventory.held(&table), None);

        inventory.select(1);
        assert_eq!(inventory.held(&table).unwrap().name, "minecraft:dirt");
    }

    #[test]
    fn add_prefers_selected_then_matching_then_empty() {
        let mut inventory = Inventory::empty();
        inventory.select(5);

        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 10)), 0);
        assert_eq!(inventory.slots()[5].as_ref().unwrap().count, 10);

        // The selected slot takes the rest of the matching stack first, and
        // the overflow fills the first empty slot.
        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 60)), 0);
        assert_eq!(inventory.slots()[5].as_ref().unwrap().count, 64);
        assert_eq!(inventory.slots()[0].as_ref().unwrap().count, 6);
        assert!(inventory.slots()[1].is_none());

        // With an unavailable selected slot, a matching stack is filled
        // before an empty slot is used.
        inventory.select(3);
        inventory.set_slot(3, Some(ItemStack::new("minecraft:dirt", 64)));
        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 10)), 0);
        assert_eq!(inventory.slots()[0].as_ref().unwrap().count, 16);
        assert!(inventory.slots()[1].is_none());
    }

    #[test]
    fn add_returns_overflow_when_all_slots_fill() {
        let mut inventory = Inventory::empty();
        let capacity = Inventory::SLOTS as u32 * MAX_STACK_SIZE;

        assert_eq!(
            inventory.add(ItemStack::new("minecraft:stone", capacity)),
            0
        );
        for index in 0..Inventory::SLOTS {
            assert_eq!(inventory.slot(index).unwrap().count, MAX_STACK_SIZE);
        }

        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 10)), 10);
        assert_eq!(inventory.add(ItemStack::new("minecraft:dirt", 1)), 1);
        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 0)), 0);
    }

    #[test]
    fn take_selected_splits_and_empties() {
        let mut inventory = Inventory::empty();
        inventory.select(4);
        inventory.set_slot(4, Some(ItemStack::new("minecraft:stone", 10)));

        assert_eq!(
            inventory.take_selected(3),
            Some(ItemStack::new("minecraft:stone", 3))
        );
        assert_eq!(inventory.slots()[4].as_ref().unwrap().count, 7);

        assert_eq!(
            inventory.take_selected(20),
            Some(ItemStack::new("minecraft:stone", 7))
        );
        assert_eq!(inventory.slots()[4], None);
        assert_eq!(inventory.take_selected(1), None);
        assert_eq!(inventory.take_selected(0), None);
    }

    #[test]
    fn icon_textures_for_all_creative_slots() {
        let inventory = Inventory::creative();
        let expected = [
            "minecraft:block/stone",
            "minecraft:block/dirt",
            "minecraft:block/grass_block_side",
            "minecraft:block/oak_planks",
            "minecraft:block/oak_log",
            "minecraft:block/glass",
            "minecraft:block/bricks",
            "minecraft:block/stone_bricks",
            "minecraft:block/white_wool",
        ];
        for (slot, texture) in expected.iter().enumerate() {
            assert_eq!(inventory.icon_texture(slot).as_deref(), Some(*texture));
            assert_eq!(inventory.icon(slot), Some([0.0, 0.0, 1.0, 1.0]));
        }
    }

    #[test]
    fn icon_texture_falls_back_to_item_sprites() {
        assert_eq!(
            icon_texture_for_item("minecraft:stone").as_deref(),
            Some("minecraft:block/stone")
        );
        assert_eq!(
            icon_texture_for_item("minecraft:stone_sword").as_deref(),
            Some("minecraft:item/stone_sword")
        );
        assert_eq!(
            icon_texture_for_item("stone_sword").as_deref(),
            Some("minecraft:item/stone_sword")
        );
        assert_eq!(icon_texture_for_item(""), None);
    }

    #[test]
    fn icon_is_none_for_empty_slots() {
        let mut inventory = Inventory::creative();
        inventory.set_slot(0, Some(ItemStack::new("minecraft:command_block", 1)));
        inventory.set_slot(1, None);
        assert_eq!(
            inventory.icon_texture(0).as_deref(),
            Some("minecraft:item/command_block")
        );
        assert_eq!(inventory.icon_texture(1), None);
        assert_eq!(inventory.icon(1), None);
        assert_eq!(inventory.icon_texture(9), None);
        assert_eq!(inventory.icon(usize::MAX), None);
    }

    #[test]
    fn hotbar_view_stays_nine_slots() {
        let inventory = Inventory::creative();
        assert_eq!(Inventory::SLOTS, 36);
        assert_eq!(inventory.slots().len(), 9);
        assert_eq!(inventory.hotbar().len(), 9);
        assert_eq!(inventory.storage().len(), 27);

        for (index, stack) in inventory.slots().iter().enumerate() {
            assert_eq!(inventory.slot(index), stack.as_ref());
        }
        assert!(inventory.storage().iter().all(Option::is_none));
        assert_eq!(inventory.slot(Inventory::SLOTS), None);
        assert_eq!(inventory.slot(usize::MAX), None);

        // `set` reaches storage; out-of-range stores are ignored.
        let mut inventory = Inventory::empty();
        inventory.set(35, Some(ItemStack::new("minecraft:dirt", 2)));
        assert_eq!(
            inventory.storage()[26],
            Some(ItemStack::new("minecraft:dirt", 2))
        );
        inventory.set(Inventory::SLOTS, Some(ItemStack::new("minecraft:dirt", 1)));
        assert_eq!(inventory.slot(Inventory::SLOTS), None);
        assert_eq!(inventory.storage()[26].as_ref().unwrap().count, 2);
    }

    #[test]
    fn hotbar_spills_into_storage() {
        let mut inventory = Inventory::empty();
        // Nine hotbar stacks of 64 hold 576 items; the rest spills into
        // storage.
        assert_eq!(inventory.add(ItemStack::new("minecraft:stone", 600)), 0);
        for slot in inventory.slots() {
            assert_eq!(slot.as_ref().unwrap().count, MAX_STACK_SIZE);
        }
        assert_eq!(inventory.storage()[0].as_ref().unwrap().count, 24);
        assert!(inventory.storage()[1..].iter().all(Option::is_none));
    }

    #[test]
    fn storage_add_fills_the_hotbar_first() {
        let mut inventory = Inventory::empty();
        inventory.set(9, Some(ItemStack::new("minecraft:stick", 63)));
        assert_eq!(
            inventory.add_to_storage(ItemStack::new("minecraft:stick", 3)),
            0
        );
        assert_eq!(inventory.slot(0).as_ref().unwrap().count, 3);
        assert_eq!(inventory.slot(9).as_ref().unwrap().count, 63);

        // With the hotbar full, a stack lands in the first free storage
        // slot; later adds merge into it.
        let mut inventory = Inventory::empty();
        for index in 0..9 {
            inventory.set(
                index,
                Some(ItemStack::new("minecraft:stone", MAX_STACK_SIZE)),
            );
        }
        assert_eq!(
            inventory.add_to_storage(ItemStack::new("minecraft:stick", 4)),
            0
        );
        assert_eq!(inventory.storage()[0].as_ref().unwrap().count, 4);
        assert_eq!(
            inventory.add_to_storage(ItemStack::new("minecraft:stick", MAX_STACK_SIZE)),
            0
        );
        assert_eq!(
            inventory.storage()[0].as_ref().unwrap().count,
            MAX_STACK_SIZE
        );
        assert_eq!(inventory.storage()[1].as_ref().unwrap().count, 4);
    }

    #[test]
    fn remove_one_takes_the_first_match() {
        let mut inventory = Inventory::empty();
        inventory.add_to_storage(ItemStack::new("minecraft:stick", 2));
        inventory.set(9, Some(ItemStack::new("minecraft:stick", 1)));

        assert!(inventory.remove_one("minecraft:stick"));
        assert_eq!(inventory.slot(0).as_ref().unwrap().count, 1);
        assert!(inventory.remove_one("minecraft:stick"));
        assert_eq!(inventory.slot(0), None);
        // The storage stack is the next match.
        assert!(inventory.remove_one("minecraft:stick"));
        assert_eq!(inventory.slot(9), None);
        assert!(!inventory.remove_one("minecraft:stick"));
        assert!(!inventory.remove_one("minecraft:dirt"));
    }
}
