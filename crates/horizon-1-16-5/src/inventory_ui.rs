//! The player inventory and 3x3 crafting screen: state, vanilla pick-up
//! rules and a resolution-independent layout.
//!
//! The 36 inventory rectangles are the inventory's own slots: the nine hotbar
//! slots first, the twenty-seven storage slots behind them. [`layout`] returns
//! its rectangles in panel order - three storage rows, the hotbar row, the 3x3
//! crafting grid and finally the result slot - so a caller can hit-test a
//! pointer and index the same rectangles while drawing.

use crate::crafting::Recipes;
use crate::inventory::{Inventory, ItemStack, MAX_STACK_SIZE};

/// Cells in the 3x3 crafting grid.
pub const CRAFT_SLOTS: usize = 9;
/// Hotbar slots; the first nine inventory slots.
pub const HOTBAR_SLOTS: usize = 9;
/// Index of the result rectangle in [`layout`]'s rectangle list.
pub const RESULT_SLOT: usize = Inventory::SLOTS + CRAFT_SLOTS;
/// Number of rectangles [`layout`] returns: inventory, grid and result.
pub const SLOT_RECTS: usize = RESULT_SLOT + 1;

/// The inventory screen's state.
///
/// `cursor` is the stack carried by the pointer (vanilla's "held" stack),
/// `craft` is the row-major 3x3 grid, and `hovered` is the rectangle the
/// pointer is over, or `None` when it is outside the panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InventoryScreen {
    pub open: bool,
    pub cursor: Option<ItemStack>,
    pub craft: [Option<ItemStack>; CRAFT_SLOTS],
    pub hovered: Option<usize>,
}

impl InventoryScreen {
    /// Opens a closed screen and closes an open one. The carried stack and the
    /// craft grid survive, so closing and reopening never loses items.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Closes the screen. Carried and grid stacks are kept for the caller;
    /// use [`Self::close_into`] to hand them back to an inventory.
    pub fn close(&mut self) {
        self.open = false;
        self.hovered = None;
    }

    /// Closes the screen, returning the carried stack and every craft-grid
    /// stack to `inventory`. Anything that does not fit stays in the screen
    /// state, so no item is destroyed.
    pub fn close_into(&mut self, inventory: &mut Inventory) {
        self.close();
        for cell in &mut self.craft {
            let Some(stack) = cell.take() else {
                continue;
            };
            let leftover = inventory.add_to_storage(stack.clone());
            if leftover > 0 {
                *cell = Some(ItemStack::new(stack.item, leftover as u32));
            }
        }
        if let Some(stack) = self.cursor.take() {
            let leftover = inventory.add_to_storage(stack.clone());
            if leftover > 0 {
                self.cursor = Some(ItemStack::new(stack.item, leftover as u32));
            }
        }
    }

    /// Handles a left click on inventory slot `index` with vanilla rules:
    /// an empty cursor takes the whole stack, the same item merges up to
    /// [`MAX_STACK_SIZE`], a different item is swapped in, and dropping a
    /// stack on an empty slot places it. `shift` moves the clicked stack
    /// between the hotbar and storage instead of picking it up.
    pub fn click(&mut self, inventory: &mut Inventory, index: usize, shift: bool) {
        if index >= Inventory::SLOTS {
            return;
        }
        if shift {
            // Shift-click moves only the clicked stack; a carried stack stays
            // on the pointer, exactly like vanilla.
            if self.cursor.is_some() {
                return;
            }
            let Some(stack) = inventory.slot(index).cloned() else {
                return;
            };
            inventory.set(index, None);
            let range = if index < HOTBAR_SLOTS {
                HOTBAR_SLOTS..Inventory::SLOTS
            } else {
                0..HOTBAR_SLOTS
            };
            if let Some(rest) = merge_into(inventory, range, stack) {
                // No room in the other half: put the stack back.
                inventory.set(index, Some(rest));
            }
            return;
        }
        match (self.cursor.take(), inventory.slot(index).cloned()) {
            (None, Some(stack)) => {
                inventory.set(index, None);
                self.cursor = Some(stack);
            }
            (None, None) => {}
            (Some(cursor), None) => inventory.set(index, Some(cursor)),
            (Some(mut cursor), Some(slot)) if cursor.item == slot.item => {
                let moved = (MAX_STACK_SIZE - slot.count).min(cursor.count);
                if moved > 0 {
                    let mut merged = slot;
                    merged.count += moved;
                    inventory.set(index, Some(merged));
                    cursor.count -= moved;
                }
                if cursor.count > 0 {
                    self.cursor = Some(cursor);
                }
            }
            (Some(cursor), Some(slot)) => {
                inventory.set(index, Some(cursor));
                self.cursor = Some(slot);
            }
        }
    }

    /// Handles a click on craft-grid cell `index`: a carried stack puts one
    /// item into the cell, and an empty pointer picks the whole cell up.
    /// A cell holding a different item is emptied back into `inventory` first,
    /// so nothing is lost when the grid is rearranged.
    pub fn click_craft(&mut self, inventory: &mut Inventory, index: usize) {
        if index >= CRAFT_SLOTS {
            return;
        }
        let Some(cursor) = self.cursor.take() else {
            self.cursor = self.craft[index].take();
            return;
        };
        let different = self.craft[index]
            .as_ref()
            .is_some_and(|cell| cell.item != cursor.item);
        if different {
            // Return the old contents; if they do not fit the grid keeps
            // them and the carried stack is left untouched.
            let old = self.craft[index].take();
            if let Some(old) = old {
                let leftover = inventory.add_to_storage(old.clone());
                if leftover > 0 {
                    self.craft[index] = Some(ItemStack::new(old.item, leftover as u32));
                    self.cursor = Some(cursor);
                    return;
                }
            }
        }
        let room = self.craft[index]
            .as_ref()
            .map_or(MAX_STACK_SIZE, |cell| MAX_STACK_SIZE - cell.count);
        if cursor.is_empty() || room == 0 {
            // Nothing to place (or the cell is full): keep the cursor as it is.
            self.cursor = Some(cursor);
            return;
        }
        let moved = 1.min(room);
        match self.craft[index].as_mut() {
            Some(cell) => cell.count += moved,
            None => self.craft[index] = Some(ItemStack::new(cursor.item.clone(), moved)),
        }
        if cursor.count > moved {
            self.cursor = Some(ItemStack::new(cursor.item, cursor.count - moved));
        }
    }

    /// The stack the current grid would craft, when a recipe book is loaded
    /// and a recipe matches.
    pub fn result(&self, recipes: Option<&Recipes>) -> Option<ItemStack> {
        recipes?.craft(&self.craft)
    }

    /// The panel rectangle and every slot rectangle for a `width` x `height`
    /// surface, in normalized top-left coordinates.
    ///
    /// The rectangles are ordered: 27 storage slots (row-major), the nine
    /// hotbar slots, the nine craft cells and the result slot. The result is a
    /// pure function of its arguments.
    pub fn layout(width: f32, height: f32) -> ([f32; 4], Vec<[f32; 4]>) {
        let w = width.max(1.0);
        let h = height.max(1.0);
        let min = w.min(h);
        let slot = (min * 0.075).max(1.0);
        let gap = slot * 0.12;
        let pad = gap * 2.0;

        let columns = 9.0;
        let grid_w = slot * columns + gap * (columns - 1.0);
        let craft_w = slot * 4.0 + gap * 4.0;
        let inner_w = grid_w.max(craft_w);
        let panel_w = inner_w + pad * 2.0;

        let rows_h = slot * 3.0 + gap * 2.0;
        let craft_h = rows_h;
        let panel_h = pad * 2.0 + craft_h + gap * 2.0 + rows_h + gap * 2.0 + slot;

        let left = (w - panel_w) * 0.5;
        let top = (h - panel_h) * 0.5;
        let panel = [left / w, top / h, panel_w / w, panel_h / h];

        let rect = |x: f32, y: f32| [x / w, y / h, slot / w, slot / h];
        let mut rects = Vec::with_capacity(SLOT_RECTS);

        // Crafting block, centered above the storage rows.
        let craft_left = (w - craft_w) * 0.5;
        let craft_top = top + pad;
        let mut craft = Vec::with_capacity(CRAFT_SLOTS + 1);
        for row in 0..3 {
            for col in 0..3 {
                let x = craft_left + col as f32 * (slot + gap);
                let y = craft_top + row as f32 * (slot + gap);
                craft.push(rect(x, y));
            }
        }
        // Result slot, right of the grid and vertically centered on it.
        craft.push(rect(
            craft_left + 3.0 * (slot + gap) + gap,
            craft_top + slot + gap,
        ));

        // Storage rows use inventory slots 9..36.
        let storage_top = craft_top + craft_h + gap * 2.0;
        let grid_left = (w - grid_w) * 0.5;
        for row in 0..3 {
            for col in 0..9 {
                let x = grid_left + col as f32 * (slot + gap);
                let y = storage_top + row as f32 * (slot + gap);
                rects.push(rect(x, y));
            }
        }
        // The hotbar row is inventory slots 0..9.
        let hotbar_top = storage_top + rows_h + gap * 2.0;
        for col in 0..9 {
            rects.push(rect(grid_left + col as f32 * (slot + gap), hotbar_top));
        }
        rects.extend(craft);
        (panel, rects)
    }
}

/// Free-function form of [`InventoryScreen::layout`].
pub fn layout(width: f32, height: f32) -> ([f32; 4], Vec<[f32; 4]>) {
    InventoryScreen::layout(width, height)
}

/// The rectangle of the touch inventory button in the top-right corner, in
/// normalized coordinates.
pub fn button_rect(width: f32, height: f32) -> [f32; 4] {
    let w = width.max(1.0);
    let h = height.max(1.0);
    let min = w.min(h);
    let side = min * 0.09;
    let margin = min * 0.03;
    [(w - side - margin) / w, margin / h, side / w, side / h]
}

/// Whether `(x, y)` lies in `rect`. Coordinates are normalized like the
/// rectangles [`layout`] returns.
pub fn inside(rect: [f32; 4], x: f32, y: f32) -> bool {
    x >= rect[0] && x <= rect[0] + rect[2] && y >= rect[1] && y <= rect[1] + rect[3]
}

/// The index of the rectangle containing `(x, y)`, if any.
pub fn hit(rects: &[[f32; 4]], x: f32, y: f32) -> Option<usize> {
    rects.iter().position(|rect| inside(*rect, x, y))
}

/// Moves `stack` into the first free or partially filled slots of `range`,
/// returning what did not fit.
fn merge_into(
    inventory: &mut Inventory,
    range: std::ops::Range<usize>,
    mut stack: ItemStack,
) -> Option<ItemStack> {
    for index in range.clone() {
        if stack.count == 0 {
            break;
        }
        let Some(existing) = inventory.slot(index) else {
            continue;
        };
        if existing.item != stack.item || existing.count >= MAX_STACK_SIZE {
            continue;
        }
        let moved = (MAX_STACK_SIZE - existing.count).min(stack.count);
        let mut merged = existing.clone();
        merged.count += moved;
        inventory.set(index, Some(merged));
        stack.count -= moved;
    }
    for index in range {
        if stack.count == 0 {
            break;
        }
        if inventory.slot(index).is_none() {
            inventory.set(index, Some(stack.clone()));
            stack.count = 0;
        }
    }
    if stack.count == 0 { None } else { Some(stack) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item, count)
    }

    /// The item and count in `index`, if any.
    fn held(inventory: &Inventory, index: usize) -> Option<(String, u32)> {
        inventory
            .slot(index)
            .map(|stack| (stack.item.clone(), stack.count))
    }

    fn item(name: &str, count: u32) -> Option<(String, u32)> {
        Some((name.to_owned(), count))
    }

    fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    }

    #[test]
    fn layout_is_deterministic_and_stays_on_screen() {
        let (panel, rects) = InventoryScreen::layout(800.0, 600.0);
        assert_eq!(rects.len(), SLOT_RECTS);
        assert_eq!(rects, InventoryScreen::layout(800.0, 600.0).1);
        for rect in std::iter::once(panel).chain(rects.iter().copied()) {
            assert!(rect[0] >= 0.0 && rect[1] >= 0.0, "{rect:?}");
            assert!(rect[0] + rect[2] <= 1.0 + 1e-6, "{rect:?}");
            assert!(rect[1] + rect[3] <= 1.0 + 1e-6, "{rect:?}");
            assert!(rect[2] > 0.0 && rect[3] > 0.0, "{rect:?}");
        }
        // Every slot stays inside the panel.
        for rect in &rects {
            assert!(rect[0] >= panel[0] - 1e-6, "{rect:?} in {panel:?}");
            assert!(rect[1] >= panel[1] - 1e-6, "{rect:?} in {panel:?}");
            assert!(rect[0] + rect[2] <= panel[0] + panel[2] + 1e-6);
            assert!(rect[1] + rect[3] <= panel[1] + panel[3] + 1e-6);
        }
        // Slots never overlap each other.
        for first in 0..rects.len() {
            for second in first + 1..rects.len() {
                assert!(!overlaps(rects[first], rects[second]), "{first}/{second}");
            }
        }
        // The result sits right of the grid row it belongs to.
        assert!(rects[RESULT_SLOT][0] > rects[RESULT_SLOT - 1][0]);
        let middle_cell = rects[RESULT_SLOT - 5];
        assert!((rects[RESULT_SLOT][1] - middle_cell[1]).abs() < 1e-6);
    }

    #[test]
    fn layout_scales_with_the_surface() {
        let (small, small_rects) = InventoryScreen::layout(320.0, 240.0);
        let (large, large_rects) = InventoryScreen::layout(1920.0, 1080.0);
        // Normalized slot sizes differ with the aspect ratio, but both stay
        // ordered and on screen.
        assert!(small[2] > large[2]);
        assert!(small_rects[0][2] > large_rects[0][2]);
        assert!(large_rects[0][2] > 0.0);
    }

    #[test]
    fn hit_and_inside_find_the_slot_under_the_pointer() {
        let (panel, rects) = InventoryScreen::layout(800.0, 600.0);
        let center = |rect: [f32; 4]| [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
        for (index, rect) in rects.iter().enumerate() {
            let point = center(*rect);
            assert_eq!(hit(&rects, point[0], point[1]), Some(index));
        }
        let middle = center(panel);
        assert!(inside(panel, middle[0], middle[1]));
        assert_eq!(hit(&rects, -1.0, -1.0), None);
        let button = button_rect(800.0, 600.0);
        let corner = center(button);
        assert!(inside(button, corner[0], corner[1]));
    }

    #[test]
    fn click_takes_merges_and_swaps() {
        let mut screen = InventoryScreen::default();
        let mut inventory = Inventory::empty();

        // Empty cursor takes the whole stack.
        inventory.set(0, Some(stack("minecraft:stone", 10)));
        screen.click(&mut inventory, 0, false);
        assert_eq!(screen.cursor, Some(stack("minecraft:stone", 10)));
        assert_eq!(inventory.slot(0), None);

        // Dropping it on an empty slot places the whole stack.
        screen.click(&mut inventory, 1, false);
        assert_eq!(held(&inventory, 1), item("minecraft:stone", 10));
        assert_eq!(screen.cursor, None);

        // Same item merges up to 64 and leaves the remainder carried.
        inventory.set(2, Some(stack("minecraft:stone", 10)));
        screen.cursor = Some(stack("minecraft:stone", 64));
        screen.click(&mut inventory, 2, false);
        assert_eq!(held(&inventory, 2), item("minecraft:stone", 64));
        assert_eq!(screen.cursor, Some(stack("minecraft:stone", 10)));

        // A different item swaps.
        inventory.set(2, Some(stack("minecraft:dirt", 3)));
        screen.cursor = Some(stack("minecraft:stone", 10));
        screen.click(&mut inventory, 2, false);
        assert_eq!(held(&inventory, 2), item("minecraft:stone", 10));
        assert_eq!(screen.cursor, Some(stack("minecraft:dirt", 3)));
    }

    #[test]
    fn shift_click_moves_between_hotbar_and_storage() {
        let mut screen = InventoryScreen::default();
        let mut inventory = Inventory::empty();
        inventory.set(0, Some(stack("minecraft:stone", 5)));

        screen.click(&mut inventory, 0, true);
        assert_eq!(inventory.slot(0), None);
        assert_eq!(held(&inventory, 9), item("minecraft:stone", 5));

        screen.click(&mut inventory, 9, true);
        assert_eq!(inventory.slot(9), None);
        assert_eq!(held(&inventory, 0), item("minecraft:stone", 5));

        // A carried stack ignores shift-click, like vanilla.
        screen.cursor = Some(stack("minecraft:dirt", 2));
        inventory.set(1, Some(stack("minecraft:stone", 1)));
        screen.click(&mut inventory, 1, true);
        assert_eq!(held(&inventory, 1), item("minecraft:stone", 1));
        assert_eq!(screen.cursor, Some(stack("minecraft:dirt", 2)));
    }

    #[test]
    fn shift_click_stacks_merge_with_existing_storage() {
        let mut screen = InventoryScreen::default();
        let mut inventory = Inventory::empty();
        inventory.set(9, Some(stack("minecraft:stone", 60)));
        inventory.set(0, Some(stack("minecraft:stone", 10)));

        screen.click(&mut inventory, 0, true);
        assert_eq!(held(&inventory, 9), item("minecraft:stone", 64));
        assert_eq!(held(&inventory, 10), item("minecraft:stone", 6));
        assert_eq!(inventory.slot(0), None);
    }

    #[test]
    fn click_craft_moves_one_item_and_takes_the_cell_back() {
        let mut screen = InventoryScreen::default();
        let mut inventory = Inventory::empty();
        screen.cursor = Some(stack("minecraft:oak_planks", 3));

        screen.click_craft(&mut inventory, 0);
        assert_eq!(screen.craft[0], Some(stack("minecraft:oak_planks", 1)));
        assert_eq!(screen.cursor, Some(stack("minecraft:oak_planks", 2)));

        screen.click_craft(&mut inventory, 3);
        assert_eq!(screen.craft[3], Some(stack("minecraft:oak_planks", 1)));
        assert_eq!(screen.cursor, Some(stack("minecraft:oak_planks", 1)));

        // An empty pointer takes the whole cell.
        screen.cursor = None;
        screen.click_craft(&mut inventory, 0);
        assert_eq!(screen.craft[0], None);
        assert_eq!(screen.cursor, Some(stack("minecraft:oak_planks", 1)));

        // A different item in the cell is returned to the inventory first.
        screen.cursor = Some(stack("minecraft:dirt", 2));
        screen.click_craft(&mut inventory, 3);
        assert_eq!(screen.craft[3], Some(stack("minecraft:dirt", 1)));
        assert_eq!(held(&inventory, 0), item("minecraft:oak_planks", 1));
        assert_eq!(screen.cursor, Some(stack("minecraft:dirt", 1)));
    }

    #[test]
    fn craft_click_ignores_out_of_range_cells() {
        let mut screen = InventoryScreen::default();
        let mut inventory = Inventory::empty();
        screen.cursor = Some(stack("minecraft:stone", 1));
        screen.click_craft(&mut inventory, CRAFT_SLOTS);
        assert_eq!(screen.cursor, Some(stack("minecraft:stone", 1)));
        assert!(screen.craft.iter().all(Option::is_none));
    }

    #[test]
    fn toggle_and_close_never_lose_items() {
        let mut screen = InventoryScreen::default();
        assert!(!screen.open);
        screen.toggle();
        assert!(screen.open);
        screen.cursor = Some(stack("minecraft:stone", 4));
        screen.craft[0] = Some(stack("minecraft:oak_planks", 2));

        let mut inventory = Inventory::empty();
        screen.close_into(&mut inventory);
        assert!(!screen.open);
        assert_eq!(screen.cursor, None);
        assert!(screen.craft.iter().all(Option::is_none));
        assert_eq!(held(&inventory, 0), item("minecraft:oak_planks", 2));
        assert_eq!(held(&inventory, 1), item("minecraft:stone", 4));

        // `close` alone keeps the state for a later reopen.
        screen.open = true;
        screen.close();
        assert!(!screen.open);
        assert_eq!(screen.hovered, None);
    }

    #[test]
    fn result_is_none_without_a_recipe_book() {
        let screen = InventoryScreen::default();
        assert_eq!(screen.result(None), None);
    }
}
