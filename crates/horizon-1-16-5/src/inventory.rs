//! A nine-slot creative hotbar for the 1.16.5 playable slice.

use crate::BlockState;

#[derive(Clone, Debug, PartialEq)]
pub struct Inventory {
    slots: [Option<BlockState>; 9],
    selected: usize,
}

impl Inventory {
    /// Stone, dirt, grass, planks, log, glass, bricks, stone bricks and wool.
    pub fn creative() -> Self {
        let mut log = BlockState::new("minecraft:oak_log");
        log.properties.insert("axis".into(), "y".into());
        Self {
            slots: [
                Some(BlockState::new("minecraft:stone")),
                Some(BlockState::new("minecraft:dirt")),
                {
                    let mut grass = BlockState::new("minecraft:grass_block");
                    grass.properties.insert("snowy".into(), "false".into());
                    Some(grass)
                },
                Some(BlockState::new("minecraft:oak_planks")),
                Some(log),
                Some(BlockState::new("minecraft:glass")),
                Some(BlockState::new("minecraft:bricks")),
                Some(BlockState::new("minecraft:stone_bricks")),
                Some(BlockState::new("minecraft:white_wool")),
            ],
            selected: 0,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selects `slot`; out-of-range values are ignored.
    pub fn select(&mut self, slot: usize) {
        if slot < self.slots.len() {
            self.selected = slot;
        }
    }

    /// Moves the selection by `delta`, wrapping through the nine slots.
    pub fn scroll(&mut self, delta: i32) {
        self.selected = (self.selected as i64 + delta as i64).rem_euclid(9) as usize;
    }

    pub fn held(&self) -> Option<&BlockState> {
        self.slots[self.selected].as_ref()
    }

    pub fn slots(&self) -> &[Option<BlockState>; 9] {
        &self.slots
    }

    /// Stores `state` in `slot`; out-of-range values are ignored.
    pub fn set_slot(&mut self, slot: usize, state: Option<BlockState>) {
        if slot < self.slots.len() {
            self.slots[slot] = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis_of(state: &BlockState) -> Option<&str> {
        state.properties.get("axis").map(String::as_str)
    }

    #[test]
    fn creative_preset_contents() {
        let inventory = Inventory::creative();
        let names: Vec<Option<&str>> = inventory
            .slots()
            .iter()
            .map(|slot| slot.as_ref().map(|state| state.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                Some("minecraft:stone"),
                Some("minecraft:dirt"),
                Some("minecraft:grass_block"),
                Some("minecraft:oak_planks"),
                Some("minecraft:oak_log"),
                Some("minecraft:glass"),
                Some("minecraft:bricks"),
                Some("minecraft:stone_bricks"),
                Some("minecraft:white_wool"),
            ]
        );
        // The log is preset to the y axis.
        assert_eq!(axis_of(inventory.slots()[4].as_ref().unwrap()), Some("y"));
    }

    #[test]
    fn select_and_held() {
        let mut inventory = Inventory::creative();
        assert_eq!(inventory.selected(), 0);
        assert_eq!(inventory.held().unwrap().name, "minecraft:stone");

        inventory.select(4);
        assert_eq!(inventory.selected(), 4);
        assert_eq!(inventory.held().unwrap().name, "minecraft:oak_log");

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
        let mut inventory = Inventory::creative();
        let wool = inventory.slots()[8].clone();

        inventory.set_slot(0, None);
        assert_eq!(inventory.held(), None);

        // Out-of-range stores are ignored, not a panic.
        inventory.set_slot(9, Some(BlockState::new("minecraft:dirt")));
        inventory.set_slot(usize::MAX, Some(BlockState::new("minecraft:dirt")));
        assert_eq!(inventory.slots()[8], wool);
    }
}
