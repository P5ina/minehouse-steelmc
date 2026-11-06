//! What a dispenser does with the item it fires (Vanilla `DispenseItemBehavior`).

use std::mem;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{Axis, BlockStateProperties, Direction};
use steel_registry::fluid::FluidRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_registry::{
    level_events, vanilla_blocks, vanilla_fluids, vanilla_game_events, vanilla_items,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, WorldAabb};

use crate::behavior::blocks::{CarvedPumpkinBlock, FireBlock, TntBlock};
use crate::behavior::context::BlockPlaceContext;
use crate::behavior::items::{BlockItem, BoneMealItem, can_light, empty_contents};
use crate::behavior::{BLOCK_BEHAVIORS, ITEM_BEHAVIORS, pickup_waterlogged_block};
use crate::block_entity::entities::DispenserBlockEntity;
use crate::inventory::prelude::EquipmentSlot;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Item speed factor of a dispenser (Vanilla passes 6 to `spawnItem`).
const DISPENSER_SPEED: f64 = 6.0;

/// The dispenser firing an item (Vanilla `BlockSource`).
pub(crate) struct BlockSource<'a> {
    pub world: &'a Arc<World>,
    pub pos: BlockPos,
    pub state: BlockStateId,
    pub entity: &'a DispenserBlockEntity,
}

impl BlockSource<'_> {
    pub(crate) fn facing(&self) -> Direction {
        self.state.get_value(&BlockStateProperties::FACING)
    }

    /// The point items leave from: 0.7 blocks from the center towards `facing`.
    pub(crate) fn dispense_position(&self) -> DVec3 {
        let (step_x, step_y, step_z) = self.facing().offset();
        let center = DVec3::new(
            f64::from(self.pos.x()) + 0.5,
            f64::from(self.pos.y()) + 0.5,
            f64::from(self.pos.z()) + 0.5,
        );
        center + 0.7 * DVec3::new(f64::from(step_x), f64::from(step_y), f64::from(step_z))
    }

    fn target(&self) -> BlockPos {
        self.pos.relative(self.facing())
    }
}

/// The behavior a dispenser uses for an item.
#[derive(Clone, Copy)]
pub(crate) enum DispenseBehavior {
    /// Drops one item (Vanilla `DefaultDispenseItemBehavior`).
    Default,
    /// Pours a water or lava bucket.
    FilledBucket(FluidRef),
    /// Fills an empty bucket from the fluid in front.
    EmptyBucket,
    /// Lights fire, campfires, candles or TNT.
    FlintAndSteel,
    /// Grows the crop or water plant in front.
    BoneMeal,
    /// Places a shulker box in front.
    ShulkerBox,
    /// Completes a golem in front, or puts the pumpkin on an entity's head.
    CarvedPumpkin,
}

impl DispenseBehavior {
    /// The behavior Vanilla registers for `item` in `DispenserBlock.DISPENSER_REGISTRY`.
    // TODO: Projectiles, armor, boats, minecarts, spawn eggs, shears, fish and powder snow buckets and the other
    // `DispenseItemBehavior.bootStrap` entries still use the default drop.
    pub(crate) fn for_item(item: &ItemStack) -> Self {
        if item.is(&vanilla_items::WATER_BUCKET) {
            Self::FilledBucket(&vanilla_fluids::WATER)
        } else if item.is(&vanilla_items::LAVA_BUCKET) {
            Self::FilledBucket(&vanilla_fluids::LAVA)
        } else if item.is(&vanilla_items::BUCKET) {
            Self::EmptyBucket
        } else if item.is(&vanilla_items::FLINT_AND_STEEL) {
            Self::FlintAndSteel
        } else if item.is(&vanilla_items::BONE_MEAL) {
            Self::BoneMeal
        } else if item.is(&vanilla_items::CARVED_PUMPKIN) {
            Self::CarvedPumpkin
        } else if shulker_box_block(item).is_some() {
            Self::ShulkerBox
        } else {
            Self::Default
        }
    }

    /// Fires `stack` and returns what stays in the slot.
    pub(crate) fn dispense(self, source: &BlockSource<'_>, stack: ItemStack) -> ItemStack {
        match self {
            Self::Default => default_dispense(source, stack),
            Self::FilledBucket(fluid) => {
                let result = filled_bucket(source, stack, fluid);
                play_default_sound_and_animation(source);
                result
            }
            Self::EmptyBucket => {
                let result = empty_bucket(source, stack);
                play_default_sound_and_animation(source);
                result
            }
            Self::FlintAndSteel | Self::BoneMeal | Self::ShulkerBox | Self::CarvedPumpkin => {
                let (result, success) = match self {
                    Self::FlintAndSteel => flint_and_steel(source, stack),
                    Self::BoneMeal => bone_meal(source, stack),
                    Self::CarvedPumpkin => carved_pumpkin(source, stack),
                    _ => shulker_box(source, stack),
                };
                source.world.level_event(
                    if success {
                        level_events::SOUND_DISPENSER_DISPENSE
                    } else {
                        level_events::SOUND_DISPENSER_FAIL
                    },
                    source.pos,
                    0,
                    None,
                );
                play_animation(source);
                result
            }
        }
    }
}

/// Vanilla `DefaultDispenseItemBehavior.dispense`.
pub(crate) fn default_dispense(source: &BlockSource<'_>, mut stack: ItemStack) -> ItemStack {
    let one = stack.split(1);
    spawn_item(
        source.world,
        one,
        DISPENSER_SPEED,
        source.facing(),
        source.dispense_position(),
    );
    play_default_sound_and_animation(source);
    stack
}

fn play_default_sound_and_animation(source: &BlockSource<'_>) {
    source
        .world
        .level_event(level_events::SOUND_DISPENSER_DISPENSE, source.pos, 0, None);
    play_animation(source);
}

fn play_animation(source: &BlockSource<'_>) {
    source.world.level_event(
        level_events::PARTICLES_SHOOT_SMOKE,
        source.pos,
        source.facing().get_3d_data_value(),
        None,
    );
}

/// Vanilla `DefaultDispenseItemBehavior.spawnItem`.
pub(crate) fn spawn_item(
    world: &Arc<World>,
    stack: ItemStack,
    speed: f64,
    direction: Direction,
    position: DVec3,
) {
    let y_offset = if direction.axis() == Axis::Y {
        0.125
    } else {
        0.15625
    };
    let pos = DVec3::new(position.x, position.y - y_offset, position.z);
    let (step_x, _, step_z) = direction.offset();
    let power = rand::random::<f64>() * 0.1 + 0.2;
    let spread = 0.017_227_5 * speed;
    let velocity = DVec3::new(
        triangle(f64::from(step_x) * power, spread),
        triangle(0.2, spread),
        triangle(f64::from(step_z) * power, spread),
    );
    world.spawn_item_with_velocity(pos, stack, velocity);
}

/// Vanilla `RandomSource.triangle(mean, deviation)`.
fn triangle(mean: f64, deviation: f64) -> f64 {
    mean + deviation * (rand::random::<f64>() - rand::random::<f64>())
}

/// Vanilla `DefaultDispenseItemBehavior.consumeWithRemainder`.
fn consume_with_remainder(
    source: &BlockSource<'_>,
    mut stack: ItemStack,
    remainder: ItemStack,
) -> ItemStack {
    stack.shrink(1);
    if stack.is_empty() {
        return remainder;
    }
    let rest = source.entity.insert_item(remainder);
    if !rest.is_empty() {
        spawn_item(
            source.world,
            rest,
            DISPENSER_SPEED,
            source.facing(),
            source.dispense_position(),
        );
        play_default_sound_and_animation(source);
    }
    stack
}

fn filled_bucket(source: &BlockSource<'_>, stack: ItemStack, fluid: FluidRef) -> ItemStack {
    if empty_contents(source.world, fluid, source.target(), None, true) {
        return consume_with_remainder(source, stack, ItemStack::new(&vanilla_items::BUCKET));
    }
    default_dispense(source, stack)
}

fn empty_bucket(source: &BlockSource<'_>, stack: ItemStack) -> ItemStack {
    let pos = source.target();
    let state = source.world.get_block_state(pos);
    let behavior = BLOCK_BEHAVIORS.get_behavior(state.get_block());
    // TODO: Remove fallback once all waterloggable blocks implement pickup_block.
    let picked = behavior
        .pickup_block(source.world, pos, state, None)
        .or_else(|| pickup_waterlogged_block(behavior, source.world, pos, state, None));
    let Some(result) = picked.filter(|result| !result.filled_bucket.is_empty()) else {
        return default_execute(source, stack);
    };
    source.world.game_event(
        &vanilla_game_events::FLUID_PICKUP,
        pos,
        &GameEventContext::new(None, None),
    );
    let filled = ItemStack::new(result.filled_bucket.item());
    consume_with_remainder(source, stack, filled)
}

/// `DefaultDispenseItemBehavior.execute`: the drop without sound and animation,
/// which the caller plays.
fn default_execute(source: &BlockSource<'_>, mut stack: ItemStack) -> ItemStack {
    let one = stack.split(1);
    spawn_item(
        source.world,
        one,
        DISPENSER_SPEED,
        source.facing(),
        source.dispense_position(),
    );
    stack
}

fn flint_and_steel(source: &BlockSource<'_>, mut stack: ItemStack) -> (ItemStack, bool) {
    let world = source.world;
    let direction = source.facing();
    let pos = source.target();
    let state = world.get_block_state(pos);
    let success = if FireBlock::can_be_placed_at(world, pos, direction) {
        world.set_block(
            pos,
            FireBlock::get_state(world.as_ref(), pos),
            UpdateFlags::UPDATE_ALL,
        );
        world.game_event(
            &vanilla_game_events::BLOCK_PLACE,
            pos,
            &GameEventContext::new(None, None),
        );
        true
    } else if can_light(state) {
        world.set_block(
            pos,
            state.set_value(&BlockStateProperties::LIT, true),
            UpdateFlags::UPDATE_ALL,
        );
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(None, None),
        );
        true
    } else if state.get_block() == &vanilla_blocks::TNT {
        if TntBlock::prime(world, pos) {
            world.remove_block(pos, false);
            true
        } else {
            false
        }
    } else {
        false
    };
    if success {
        stack.hurt_and_break(1, false);
    }
    (stack, success)
}

fn bone_meal(source: &BlockSource<'_>, mut stack: ItemStack) -> (ItemStack, bool) {
    let pos = source.target();
    let success = BoneMealItem::grow(source.world, pos)
        || BoneMealItem::grow_water_plant(source.world, pos, None);
    if success {
        stack.shrink(1);
        source.world.level_event(
            level_events::PARTICLES_AND_SOUND_PLANT_GROWTH,
            pos,
            15,
            None,
        );
    }
    (stack, success)
}

/// The shulker box block a shulker box item places, if `item` is one.
///
/// Vanilla registers `ShulkerBoxDispenseBehavior` for every `ShulkerBoxBlock` item;
/// those are the only block items that cannot go inside container items.
fn shulker_box_block(item: &ItemStack) -> Option<BlockRef> {
    if ITEM_BEHAVIORS
        .get_behavior(item.item())
        .can_fit_inside_container_items()
    {
        return None;
    }
    let block = REGISTRY.blocks.by_key(&item.item().key)?;
    (!BLOCK_BEHAVIORS
        .get_behavior(block)
        .fits_inside_container_items())
    .then_some(block)
}

/// Vanilla `ShulkerBoxDispenseBehavior.execute`.
fn shulker_box(source: &BlockSource<'_>, mut stack: ItemStack) -> (ItemStack, bool) {
    let Some(block) = shulker_box_block(&stack) else {
        return (stack, false);
    };
    let direction = source.facing();
    let pos = source.target();
    let against = if source.world.get_block_state(pos.below()).is_air() {
        direction
    } else {
        Direction::Up
    };
    let context = BlockPlaceContext::directional(source.world, pos, direction, &mut stack, against);
    let success = BlockItem::new(block).place(context).consumes_action();
    (stack, success)
}

/// Vanilla `DispenseItemBehavior.bootStrap` carved pumpkin behavior.
fn carved_pumpkin(source: &BlockSource<'_>, mut stack: ItemStack) -> (ItemStack, bool) {
    let world = source.world;
    let target = source.target();
    if world.get_block_state(target).is_air() && CarvedPumpkinBlock::can_spawn_golem(world, target)
    {
        world.set_block(
            target,
            vanilla_blocks::CARVED_PUMPKIN.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        world.game_event(
            &vanilla_game_events::BLOCK_PLACE,
            target,
            &GameEventContext::new(None, None),
        );
        stack.shrink(1);
        return (stack, true);
    }
    let success = dispense_equipment(source, &mut stack);
    (stack, success)
}

/// Vanilla `EquipmentDispenseItemBehavior.dispenseEquipment`: equips one item on
/// the first living entity in the block in front that accepts it.
pub(crate) fn dispense_equipment(source: &BlockSource<'_>, stack: &mut ItemStack) -> bool {
    let target = source.target();
    let search_box = WorldAabb::new(
        f64::from(target.x()),
        f64::from(target.y()),
        f64::from(target.z()),
        f64::from(target.x() + 1),
        f64::from(target.y() + 1),
        f64::from(target.z() + 1),
    );
    let candidates = source
        .world
        .get_entities_in_aabb_matching(&search_box, |entity| {
            entity
                .as_living_entity()
                .is_some_and(|living| living.can_equip_with_dispenser(stack))
        });
    let Some(living) = candidates
        .first()
        .and_then(|entity| entity.as_living_entity())
    else {
        return false;
    };

    let slot = match living.as_mob() {
        Some(mob) => mob.get_equipment_slot_for_item(stack),
        None => match stack.get_equippable() {
            Some(equippable) if living.can_use_slot(equippable.slot) => equippable.slot,
            _ => EquipmentSlot::MainHand,
        },
    };
    let equip = stack.split(1);
    let equipped = equip.copy_with_count(equip.count());
    let mut previous = ItemStack::empty();
    let mut equip = Some(equip);
    living.with_equipment_slot_mut(slot, &mut |item_stack| {
        if let Some(equip) = equip.take() {
            previous = mem::replace(item_stack, equip);
        }
    });
    living.on_equip_item(slot, &previous, &equipped);
    if let Some(mob) = living.as_mob() {
        mob.set_guaranteed_drop(slot);
        mob.set_persistence_required();
    }
    true
}
