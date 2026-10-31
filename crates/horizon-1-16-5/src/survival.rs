//! Survival player state for the 1.16.5 playable slice.
//!
//! [`Survival`] owns the vitals a non-creative player needs: health, hunger,
//! saturation, air, fall damage and the inventory drops go into. The
//! integrated server ticks it once per 1/20 s and feeds it the movement,
//! water and landing events the vanilla rules are built on. Creative mode is
//! the neutral state: no damage, no hunger and a free block palette, so the
//! windowed loop keeps its old behaviour until the mode is toggled.

use crate::inventory::{Inventory, ItemStack};

/// Full health, in half-hearts.
pub const MAX_HEALTH: f32 = 20.0;
/// Full hunger, in half-drumsticks.
pub const MAX_FOOD: i32 = 20;
/// The saturation cap vanilla applies to a fresh player.
pub const MAX_SATURATION: f32 = 5.0;
/// Air, in ticks, before drowning damage starts (15 s like vanilla).
pub const MAX_AIR: i32 = 300;

/// Ticks between natural regeneration points.
pub const REGEN_INTERVAL: u32 = 80;
/// Ticks between starvation damage points.
pub const STARVE_INTERVAL: u32 = 80;
/// Ticks between drowning damage points once air is gone.
pub const DROWN_INTERVAL: u32 = 20;

/// Exhaustion needed to spend one saturation or hunger point.
pub const EXHAUSTION_PER_FOOD: f32 = 4.0;
/// Exhaustion added per tick of walking.
pub const WALK_EXHAUSTION: f32 = 0.01;
/// Exhaustion added by one jump.
pub const JUMP_EXHAUSTION: f32 = 0.1;

/// Blocks of fall distance taken for free before damage starts.
const FALL_FREE_BLOCKS: f32 = 3.0;
/// Air regained per tick while out of water but still airborne.
const AIR_REFILL: i32 = 4;

/// The two modes the playable slice supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GameMode {
    /// Damage, hunger, drops and finite block stacks.
    #[default]
    Survival,
    /// The original free-flying block palette.
    Creative,
}

/// A player's survival stats, inventory and selected hotbar slot.
///
/// The private counters are the vanilla tick timers; they are not part of the
/// public state a caller needs to read.
#[derive(Clone, Debug, PartialEq)]
pub struct Survival {
    pub mode: GameMode,
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub inventory: Inventory,
    pub selected: usize,
    pub dead: bool,
    /// Exhaustion accumulated since the last saturation/hunger point.
    exhaustion: f32,
    /// Remaining air, in ticks; drowning damage starts at zero.
    air: i32,
    regen_ticks: u32,
    starve_ticks: u32,
    drown_ticks: u32,
}

impl Default for Survival {
    fn default() -> Self {
        Self::new()
    }
}

impl Survival {
    /// A fresh survival player with an empty hotbar.
    pub fn new() -> Self {
        Self {
            mode: GameMode::Survival,
            health: MAX_HEALTH,
            food: MAX_FOOD,
            saturation: MAX_SATURATION,
            inventory: Inventory::empty(),
            selected: 0,
            dead: false,
            exhaustion: 0.0,
            air: MAX_AIR,
            regen_ticks: 0,
            starve_ticks: 0,
            drown_ticks: 0,
        }
    }

    /// A creative player with the nine-slot block palette.
    pub fn creative() -> Self {
        Self {
            mode: GameMode::Creative,
            inventory: Inventory::creative(),
            ..Self::new()
        }
    }

    pub fn is_creative(&self) -> bool {
        self.mode == GameMode::Creative
    }

    /// Switches between survival and creative.
    ///
    /// Entering creative revives the player and restores the nine-block
    /// creative palette; leaving creative keeps that palette as the survival
    /// inventory, so the mode toggle never leaves the player unable to place
    /// anything.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            GameMode::Survival => GameMode::Creative,
            GameMode::Creative => GameMode::Survival,
        };
        if self.is_creative() {
            self.dead = false;
            self.health = MAX_HEALTH;
            self.food = MAX_FOOD;
            self.saturation = MAX_SATURATION;
            self.exhaustion = 0.0;
            self.air = MAX_AIR;
            self.regen_ticks = 0;
            self.starve_ticks = 0;
            self.drown_ticks = 0;
            self.inventory = Inventory::creative();
            self.selected = self.inventory.selected();
        }
    }

    /// Selects a hotbar slot, keeping [`Self::selected`] in sync.
    pub fn select(&mut self, slot: usize) {
        self.inventory.select(slot);
        self.selected = self.inventory.selected();
    }

    /// Moves the hotbar selection, keeping [`Self::selected`] in sync.
    pub fn scroll(&mut self, delta: i32) {
        self.inventory.scroll(delta);
        self.selected = self.inventory.selected();
    }

    /// The stack in the selected slot, for harvest checks and placement.
    pub fn selected_stack(&self) -> Option<&ItemStack> {
        self.inventory.slots().get(self.selected)?.as_ref()
    }

    /// Adds exhaustion from walking/jumping; every four points spends one
    /// saturation point and then one hunger point.
    pub fn exhaust(&mut self, amount: f32) {
        if self.is_creative() || self.dead || amount <= 0.0 {
            return;
        }
        self.exhaustion += amount;
        while self.exhaustion >= EXHAUSTION_PER_FOOD {
            self.exhaustion -= EXHAUSTION_PER_FOOD;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if self.food > 0 {
                self.food -= 1;
            } else {
                break;
            }
        }
    }

    /// Applies `amount` damage, ignoring creative players and the already dead.
    pub fn damage(&mut self, amount: f32) {
        if self.is_creative() || self.dead || amount <= 0.0 {
            return;
        }
        self.health -= amount;
        if self.health <= 0.0 {
            self.health = 0.0;
            self.dead = true;
        }
    }

    /// Applies vanilla fall damage: one point per block fallen past three.
    pub fn apply_fall(&mut self, distance_blocks: f32) {
        if self.is_creative() || self.dead {
            return;
        }
        let excess = distance_blocks - FALL_FREE_BLOCKS;
        if excess > 0.0 {
            self.damage(excess.floor());
        }
    }

    /// One 1/20 s of survival rules.
    ///
    /// `in_water` means the player's head is submerged, `on_ground` means the
    /// collision box rests on a block. Hunger-driven regeneration and
    /// starvation are independent of movement; the server feeds walking and
    /// jumping exhaustion via [`Self::exhaust`].
    pub fn tick(&mut self, in_water: bool, on_ground: bool) {
        if self.is_creative() || self.dead {
            return;
        }

        // Air only drains underwater; leaving the water refills it, and
        // standing on ground restores the full bar at once.
        if in_water {
            self.air = (self.air - 1).max(0);
            if self.air == 0 {
                self.drown_ticks += 1;
                if self.drown_ticks >= DROWN_INTERVAL {
                    self.drown_ticks = 0;
                    self.damage(1.0);
                }
            } else {
                self.drown_ticks = 0;
            }
        } else {
            self.air = if on_ground {
                MAX_AIR
            } else {
                (self.air + AIR_REFILL).min(MAX_AIR)
            };
            self.drown_ticks = 0;
        }

        if self.dead {
            return;
        }

        // Natural regeneration: one half-heart every 80 ticks at 18+ food.
        if self.food >= 18 && self.health < MAX_HEALTH {
            self.regen_ticks += 1;
            if self.regen_ticks >= REGEN_INTERVAL {
                self.regen_ticks = 0;
                self.health = (self.health + 1.0).min(MAX_HEALTH);
            }
        } else {
            self.regen_ticks = 0;
        }

        // Starvation: one half-heart every 80 ticks with an empty hunger bar.
        if self.food == 0 {
            self.starve_ticks += 1;
            if self.starve_ticks >= STARVE_INTERVAL {
                self.starve_ticks = 0;
                self.damage(1.0);
            }
        } else {
            self.starve_ticks = 0;
        }
    }

    /// Eats one food item's worth: eight hunger points and four saturation.
    pub fn eat(&mut self) {
        if self.dead {
            return;
        }
        self.food = (self.food + 8).min(MAX_FOOD);
        self.saturation = (self.saturation + 4.0)
            .min(MAX_SATURATION)
            .min(self.food as f32);
    }

    /// Revives the player at full vitals; the inventory is kept.
    pub fn respawn(&mut self) {
        self.health = MAX_HEALTH;
        self.food = MAX_FOOD;
        self.saturation = MAX_SATURATION;
        self.dead = false;
        self.exhaustion = 0.0;
        self.air = MAX_AIR;
        self.regen_ticks = 0;
        self.starve_ticks = 0;
        self.drown_ticks = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_full_in_survival() {
        let survival = Survival::new();
        assert_eq!(survival.mode, GameMode::Survival);
        assert_eq!(survival.health, MAX_HEALTH);
        assert_eq!(survival.food, MAX_FOOD);
        assert_eq!(survival.saturation, MAX_SATURATION);
        assert!(!survival.dead);
        assert_eq!(survival.selected, 0);
        assert!(survival.inventory.slots().iter().all(Option::is_none));
    }

    #[test]
    fn creative_is_full_and_immune() {
        let mut survival = Survival::creative();
        assert!(survival.is_creative());
        assert_eq!(
            survival.inventory.slots()[0],
            Some(ItemStack::new("minecraft:stone", 1))
        );
        survival.damage(20.0);
        survival.apply_fall(100.0);
        assert_eq!(survival.health, MAX_HEALTH);
        assert!(!survival.dead);
        // Creative hunger never drains, even with heavy exhaustion.
        survival.exhaust(100.0);
        for _ in 0..1000 {
            survival.tick(true, false);
        }
        assert_eq!(survival.health, MAX_HEALTH);
        assert_eq!(survival.food, MAX_FOOD);
    }

    #[test]
    fn damage_kills_and_respawn_resets() {
        let mut survival = Survival::new();
        survival.damage(7.5);
        assert_eq!(survival.health, 12.5);
        assert!(!survival.dead);
        survival.damage(12.5);
        assert_eq!(survival.health, 0.0);
        assert!(survival.dead);
        // The dead take no further damage.
        survival.damage(1.0);
        assert_eq!(survival.health, 0.0);

        survival.respawn();
        assert_eq!(survival.health, MAX_HEALTH);
        assert_eq!(survival.food, MAX_FOOD);
        assert_eq!(survival.saturation, MAX_SATURATION);
        assert!(!survival.dead);
    }

    #[test]
    fn fall_damage_starts_after_three_blocks() {
        let mut survival = Survival::new();
        survival.apply_fall(3.0);
        assert_eq!(survival.health, MAX_HEALTH);
        survival.apply_fall(4.9);
        assert_eq!(survival.health, MAX_HEALTH - 1.0);
        survival.apply_fall(6.0);
        assert_eq!(survival.health, MAX_HEALTH - 4.0);
    }

    #[test]
    fn exhaustion_spends_saturation_before_hunger() {
        let mut survival = Survival::new();
        survival.exhaust(20.0);
        assert_eq!(survival.saturation, 0.0);
        assert_eq!(survival.food, MAX_FOOD);

        survival.exhaust(8.0);
        assert_eq!(survival.food, 18);
        assert_eq!(survival.health, MAX_HEALTH);
    }

    #[test]
    fn moving_exhaustion_matches_the_walk_and_jump_rates() {
        let mut survival = Survival::new();
        survival.exhaust(WALK_EXHAUSTION);
        survival.exhaust(JUMP_EXHAUSTION);
        assert_eq!(survival.health, MAX_HEALTH);
        assert_eq!(survival.saturation, MAX_SATURATION);
        // 0.01 + 0.1 is far below the four-point threshold, so a full four
        // points only spends one saturation point.
        survival.exhaust(EXHAUSTION_PER_FOOD);
        assert_eq!(survival.saturation, MAX_SATURATION - 1.0);
    }

    #[test]
    fn natural_regeneration_takes_eighty_ticks() {
        let mut survival = Survival::new();
        survival.health = 10.0;
        for _ in 0..REGEN_INTERVAL - 1 {
            survival.tick(false, true);
        }
        assert_eq!(survival.health, 10.0);
        survival.tick(false, true);
        assert_eq!(survival.health, 11.0);
    }

    #[test]
    fn low_food_stops_regeneration() {
        let mut survival = Survival::new();
        survival.health = 10.0;
        survival.food = 17;
        for _ in 0..REGEN_INTERVAL * 2 {
            survival.tick(false, true);
        }
        assert_eq!(survival.health, 10.0);
    }

    #[test]
    fn starvation_damages_with_empty_hunger() {
        let mut survival = Survival::new();
        survival.food = 0;
        for _ in 0..STARVE_INTERVAL {
            survival.tick(false, true);
        }
        assert_eq!(survival.health, 19.0);
    }

    #[test]
    fn drowning_starts_after_air_runs_out() {
        let mut survival = Survival::new();
        for _ in 0..MAX_AIR {
            survival.tick(true, false);
        }
        // Air reached zero exactly when the last tick ran out; damage needs
        // a full 20 further ticks.
        assert_eq!(survival.health, MAX_HEALTH);
        for _ in 0..DROWN_INTERVAL {
            survival.tick(true, false);
        }
        assert_eq!(survival.health, MAX_HEALTH - 1.0);

        // Leaving the water refills the bar and stops the damage.
        for _ in 0..DROWN_INTERVAL * 3 {
            survival.tick(false, true);
        }
        assert_eq!(survival.health, MAX_HEALTH - 1.0);
    }

    #[test]
    fn eat_restores_hunger_and_saturation() {
        let mut survival = Survival::new();
        survival.food = 6;
        survival.saturation = 0.0;
        survival.eat();
        assert_eq!(survival.food, 14);
        assert_eq!(survival.saturation, 4.0);
        survival.eat();
        assert_eq!(survival.food, MAX_FOOD);
        assert_eq!(survival.saturation, MAX_SATURATION);
    }

    #[test]
    fn mode_toggle_switches_and_creative_revives() {
        let mut survival = Survival::new();
        survival.damage(20.0);
        assert!(survival.dead);
        survival.toggle_mode();
        assert_eq!(survival.mode, GameMode::Creative);
        assert!(!survival.dead);
        assert_eq!(survival.health, MAX_HEALTH);
        assert!(survival.inventory.slots()[0].is_some());

        survival.toggle_mode();
        assert_eq!(survival.mode, GameMode::Survival);
        assert_eq!(survival.health, MAX_HEALTH);
    }

    #[test]
    fn select_and_scroll_keep_selected_in_sync() {
        let mut survival = Survival::new();
        survival.select(4);
        assert_eq!(survival.selected, 4);
        assert_eq!(survival.inventory.selected(), 4);
        survival.scroll(6);
        assert_eq!(survival.selected, 1);
        assert_eq!(survival.inventory.selected(), 1);
    }
}
