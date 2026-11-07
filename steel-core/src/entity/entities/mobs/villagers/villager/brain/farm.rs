//! Farming (vanilla `HarvestFarmland` and `UseBonemeal`).

use std::sync::Arc;

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    level_events, sound_events, vanilla_blocks, vanilla_game_events, vanilla_items,
    vanilla_villager_professions,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId};

use super::super::VillagerEntity;
use super::poi::closer_to_center_than;
use crate::behavior::items::BoneMealItem;
use crate::behavior::{BLOCK_BEHAVIORS, ITEM_BEHAVIORS};
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    LOOK_TARGET, PositionTracker, SECONDARY_JOB_SITE, WALK_TARGET, WalkTarget,
};
use crate::entity::{Entity as _, Mob as _};
use crate::inventory::container::Container as _;
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `HarvestFarmland.HARVEST_DURATION`.
const HARVEST_DURATION: i32 = 200;
/// Vanilla `HarvestFarmland.SPEED_MODIFIER`.
const HARVEST_SPEED_MODIFIER: f32 = 0.5;
/// Vanilla `UseBonemeal.BONEMEALING_DURATION`.
const BONEMEALING_DURATION: i32 = 80;
/// Vanilla `UseBonemeal`: ticks between bone meal sessions.
const BONEMEALING_COOLDOWN: i32 = 160;

/// Vanilla `block instanceof CropBlock` with its `isMaxAge`.
fn crop_is_max_age(state: BlockStateId) -> Option<bool> {
    BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .crop_is_max_age(state)
}

fn set_look_and_walk_target(body: &VillagerEntity, pos: BlockPos) {
    let mut memories = body.brain.memories().lock();
    memories.set(&LOOK_TARGET, Some(PositionTracker::block(pos)));
    memories.set(
        &WALK_TARGET,
        Some(WalkTarget::new(
            PositionTracker::block(pos),
            HARVEST_SPEED_MODIFIER,
            1,
        )),
    );
}

const HARVEST_FARMLAND_ENTRY: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&SECONDARY_JOB_SITE, MemoryStatus::ValuePresent),
];

/// Vanilla `HarvestFarmland`: farmers break grown crops next to them and
/// plant seeds on the empty farmland.
pub(super) struct HarvestFarmland {
    above_farmland_pos: Option<BlockPos>,
    next_ok_start_time: i64,
    time_worked_so_far: i32,
    valid_farmland_around_villager: Vec<BlockPos>,
}

impl HarvestFarmland {
    pub(super) const fn new() -> Self {
        Self {
            above_farmland_pos: None,
            next_ok_start_time: 0,
            time_worked_so_far: 0,
            valid_farmland_around_villager: Vec::new(),
        }
    }

    /// Vanilla `getValidFarmland`.
    fn valid_farmland(&self) -> Option<BlockPos> {
        if self.valid_farmland_around_villager.is_empty() {
            return None;
        }
        let index = rand::random_range(0..self.valid_farmland_around_villager.len());
        Some(self.valid_farmland_around_villager[index])
    }

    /// Vanilla `validPos`: a grown crop, or air above farmland.
    fn valid_pos(world: &World, pos: BlockPos) -> bool {
        let state = world.get_block_state(pos);
        crop_is_max_age(state) == Some(true)
            || state.is_air()
                && world.get_block_state(pos.below()).get_block() == &vanilla_blocks::FARMLAND
    }

    /// Vanilla `tick`'s planting: the first plantable seed in the inventory.
    fn plant(world: &Arc<World>, body: &VillagerEntity, pos: BlockPos) {
        let inventory = body.inventory().lock();
        for slot in 0..inventory.get_container_size() {
            let stack = inventory.get_item(slot);
            if stack.is_empty() || !stack.item().has_tag(&ItemTag::VILLAGER_PLANTABLE_SEEDS) {
                continue;
            }
            let Some(block_item) = ITEM_BEHAVIORS.get_behavior(stack.item()).as_block_item() else {
                continue;
            };
            let place = block_item.block.default_state();
            // Unlocked while placing the block changes the world.
            drop(inventory);
            world.set_block(pos, place, UpdateFlags::UPDATE_ALL);
            world.game_event(
                &vanilla_game_events::BLOCK_PLACE,
                pos,
                &GameEventContext::new(Some(body), Some(place)),
            );
            // Vanilla plays it at the block's corner, not its center.
            world.play_sound_at(
                &sound_events::ITEM_CROP_PLANT,
                SoundSource::Blocks,
                DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z())),
                1.0,
                1.0,
                None,
            );
            body.inventory().lock().get_item_mut(slot).shrink(1);
            return;
        }
    }
}

impl Behavior<VillagerEntity> for HarvestFarmland {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &HARVEST_FARMLAND_ENTRY
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        if !world.get_game_rule(&MOB_GRIEFING)
            || body.profession().key != vanilla_villager_professions::FARMER.key
        {
            return false;
        }
        let position = body.position();
        self.valid_farmland_around_villager.clear();
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = BlockPos::new(
                        (position.x + f64::from(x)).floor() as i32,
                        (position.y + f64::from(y)).floor() as i32,
                        (position.z + f64::from(z)).floor() as i32,
                    );
                    if Self::valid_pos(world, pos) {
                        self.valid_farmland_around_villager.push(pos);
                    }
                }
            }
        }
        self.above_farmland_pos = self.valid_farmland();
        self.above_farmland_pos.is_some()
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        if time > self.next_ok_start_time
            && let Some(pos) = self.above_farmland_pos
        {
            set_look_and_walk_target(body, pos);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        {
            let mut memories = body.brain.memories().lock();
            memories.erase(&LOOK_TARGET);
            memories.erase(&WALK_TARGET);
        }
        self.time_worked_so_far = 0;
        self.next_ok_start_time = time + 40;
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        if self
            .above_farmland_pos
            .is_some_and(|pos| !closer_to_center_than(pos, body, 1.0))
        {
            return;
        }
        if let Some(pos) = self.above_farmland_pos
            && time > self.next_ok_start_time
        {
            let state = world.get_block_state(pos);
            let below = world.get_block_state(pos.below()).get_block();
            let crop = crop_is_max_age(state);
            if crop == Some(true) {
                let _destroyed = world.destroy_block_by_entity(pos, true, body);
            }
            if state.is_air() && below == &vanilla_blocks::FARMLAND && body.has_farm_seeds() {
                Self::plant(world, body, pos);
            }
            if crop == Some(false) {
                self.valid_farmland_around_villager
                    .retain(|farmland| *farmland != pos);
                self.above_farmland_pos = self.valid_farmland();
                if let Some(next) = self.above_farmland_pos {
                    self.next_ok_start_time = time + 20;
                    set_look_and_walk_target(body, next);
                }
            }
        }
        self.time_worked_so_far += 1;
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &VillagerEntity, _time: i64) -> bool {
        self.time_worked_so_far < HARVEST_DURATION
    }
}

const USE_BONEMEAL_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
];

/// Vanilla `UseBonemeal`: villagers carrying bone meal use it on the
/// growing crops around them.
pub(super) struct UseBonemeal {
    next_work_cycle_time: i64,
    last_bonemealing_session: i32,
    time_worked_so_far: i32,
    crop_pos: Option<BlockPos>,
}

impl UseBonemeal {
    pub(super) const fn new() -> Self {
        Self {
            next_work_cycle_time: 0,
            last_bonemealing_session: 0,
            time_worked_so_far: 0,
            crop_pos: None,
        }
    }

    /// Vanilla `pickNextTarget`: a random growing crop around the villager.
    fn pick_next_target(world: &World, body: &VillagerEntity) -> Option<BlockPos> {
        let center = body.block_position();
        let mut result = None;
        let mut count = 0;
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = center.offset(x, y, z);
                    if crop_is_max_age(world.get_block_state(pos)) == Some(false) {
                        count += 1;
                        if rand::random_range(0..count) == 0 {
                            result = Some(pos);
                        }
                    }
                }
            }
        }
        result
    }

    /// Vanilla `setCurrentCropAsTarget`.
    fn set_current_crop_as_target(&self, body: &VillagerEntity) {
        if let Some(pos) = self.crop_pos {
            set_look_and_walk_target(body, pos);
        }
    }
}

impl Behavior<VillagerEntity> for UseBonemeal {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &USE_BONEMEAL_ENTRY
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        let tick_count = body.tick_count();
        if tick_count % 10 != 0
            || self.last_bonemealing_session != 0
                && self.last_bonemealing_session + BONEMEALING_COOLDOWN > tick_count
        {
            return false;
        }
        if body
            .inventory()
            .lock()
            .count_item(&vanilla_items::BONE_MEAL)
            <= 0
        {
            return false;
        }
        self.crop_pos = Self::pick_next_target(world, body);
        self.crop_pos.is_some()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &VillagerEntity, _time: i64) -> bool {
        self.time_worked_so_far < BONEMEALING_DURATION && self.crop_pos.is_some()
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, time: i64) {
        self.set_current_crop_as_target(body);
        body.set_item_slot(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::BONE_MEAL),
        );
        self.next_work_cycle_time = time;
        self.time_worked_so_far = 0;
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        body.set_item_slot(EquipmentSlot::MainHand, ItemStack::empty());
        self.last_bonemealing_session = body.tick_count();
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        let Some(target) = self.crop_pos else {
            return;
        };
        if time < self.next_work_cycle_time || !closer_to_center_than(target, body, 1.0) {
            return;
        }
        let bone_meal_slot = {
            let inventory = body.inventory().lock();
            (0..inventory.get_container_size())
                .find(|slot| inventory.get_item(*slot).is(&vanilla_items::BONE_MEAL))
        };
        // Vanilla `BoneMealItem.growCrop`: the stack shrinks when the crop
        // could take bone meal, whether or not it grew.
        if let Some(slot) = bone_meal_slot
            && BoneMealItem::grow(world, target)
        {
            body.inventory().lock().get_item_mut(slot).shrink(1);
            world.level_event(
                level_events::PARTICLES_AND_SOUND_PLANT_GROWTH,
                target,
                15,
                None,
            );
            self.crop_pos = Self::pick_next_target(world, body);
            self.set_current_crop_as_target(body);
            self.next_work_cycle_time = time + 40;
        }
        self.time_worked_so_far += 1;
    }
}
