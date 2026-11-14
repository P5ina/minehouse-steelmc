//! Vanilla `DecoratedPotBlock`: a waterloggable pot that holds one stack and
//! shatters into its sherds.

use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_macros::block_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::ItemStackTemplate;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, BoolProperty, Direction, EnumProperty,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::particle_type::ParticleData;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::vanilla_enchantment_tags::EnchantmentTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, sound_events, vanilla_block_entity_types,
    vanilla_game_events, vanilla_particle_types,
};
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, Downcast as _, Identifier};

use crate::behavior::InventoryAccess;
use crate::behavior::block::schedule_water_tick_if_waterlogged;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation, BlockLootContext};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::entities::{DecoratedPotBlockEntity, WobbleStyle};
use crate::block_entity::{BLOCK_ENTITIES, BlockEntity as _, SharedBlockEntity};
use crate::entity::ai::path::PathComputationType;
use crate::entity::{Entity, Projectile};
use crate::inventory::container::calculate_redstone_signal_from_container;
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{ClipHitResult, ScheduledTickAccess, World};

const HORIZONTAL_FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;
const WATERLOGGED: &BoolProperty = &BlockStateProperties::WATERLOGGED;
const CRACKED: &BoolProperty = &BlockStateProperties::CRACKED;

/// Vanilla `DecoratedPotBlock.SHERDS_DYNAMIC_DROP_ID`.
const fn sherds_dynamic_drop_id() -> Identifier {
    Identifier::vanilla_static("sherds")
}

/// Decorated pot block behavior.
#[block_behavior]
pub struct DecoratedPotBlock {
    block: BlockRef,
}

impl DecoratedPotBlock {
    /// Creates decorated pot behavior for `block`.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }

    fn pot_at(world: &World, pos: BlockPos) -> Option<SharedBlockEntity> {
        world
            .get_block_entity(pos)
            .filter(|entity| entity.downcast_ref::<DecoratedPotBlockEntity>().is_some())
    }

    /// Whether `tool` cracks the pot it breaks: in `#breaks_decorated_pots`
    /// and without a `#prevents_decorated_pot_shattering` enchantment.
    fn shatters_pot(tool: &ItemStack) -> bool {
        if !tool.item().has_tag(&ItemTag::BREAKS_DECORATED_POTS) {
            return false;
        }
        let Some(enchantments) = tool.get_enchantments() else {
            return true;
        };
        !enchantments.iter().any(|(key, _)| {
            REGISTRY
                .enchantments
                .by_key(key)
                .is_some_and(|enchantment| {
                    REGISTRY.enchantments.is_in_tag(
                        enchantment,
                        &EnchantmentTag::PREVENTS_DECORATED_POT_SHATTERING,
                    )
                })
        })
    }

    /// Vanilla `useItemOn`'s insert condition: the pot is empty, or holds
    /// the same item with room for one more.
    fn accepts(pot_item: &ItemStack, held: &ItemStack) -> bool {
        !held.is_empty()
            && (pot_item.is_empty()
                || ItemStack::is_same_item_same_components(pot_item, held)
                    && pot_item.count() < pot_item.max_stack_size())
    }
}

impl BlockBehavior for DecoratedPotBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(
            self.block
                .default_state()
                .set_value(HORIZONTAL_FACING, context.horizontal_direction())
                .set_value(WATERLOGGED, context.is_water_source())
                .set_value(CRACKED, false),
        )
    }

    fn update_shape(
        &self,
        state: BlockStateId,
        world: &dyn ScheduledTickAccess,
        pos: BlockPos,
        _direction: Direction,
        _neighbor_pos: BlockPos,
        _neighbor_state: BlockStateId,
    ) -> BlockStateId {
        schedule_water_tick_if_waterlogged(state, world, pos);
        state
    }

    fn use_item_on(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let Some(block_entity) = Self::pot_at(world, pos) else {
            return InteractionResult::Pass;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return InteractionResult::Pass;
        };

        let pot_item = pot.the_item();
        let infinite = player.has_infinite_materials();
        let inserted = inv.with_item(|held| {
            if !Self::accepts(&pot_item, held) {
                return None;
            }
            let item = held.item();
            Some((item, held.consume_and_return(1, infinite)))
        });
        let Some((used_item, inserted)) = inserted else {
            return InteractionResult::TryEmptyHandInteraction;
        };

        pot.wobble(WobbleStyle::Positive);
        player.award_stat(&vanilla_stat_types::ITEM_USED, used_item);
        let stored = if pot_item.is_empty() {
            pot.set_the_item(inserted.clone());
            inserted
        } else {
            pot.grow_the_item()
        };
        let pitch_bend = stored.count() as f32 / stored.max_stack_size() as f32;

        world.play_sound(
            &sound_events::BLOCK_DECORATED_POT_INSERT,
            SoundSource::Blocks,
            pos,
            1.0,
            0.7 + 0.5 * pitch_bend,
            None,
        );
        world.send_particles(
            ParticleData::simple(&vanilla_particle_types::DUST_PLUME),
            DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()) + 1.2,
                f64::from(pos.z()) + 0.5,
            ),
            7,
            DVec3::ZERO,
            0.0,
        );
        pot.set_changed();
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(Some(player as &dyn Entity), None),
        );
        InteractionResult::Success
    }

    fn use_without_item(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hit_result: &BlockHitResult,
        _inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let Some(block_entity) = Self::pot_at(world, pos) else {
            return InteractionResult::Pass;
        };
        let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>() else {
            return InteractionResult::Pass;
        };
        world.play_sound(
            &sound_events::BLOCK_DECORATED_POT_INSERT_FAIL,
            SoundSource::Blocks,
            pos,
            1.0,
            1.0,
            None,
        );
        pot.wobble(WobbleStyle::Negative);
        world.game_event(
            &vanilla_game_events::BLOCK_CHANGE,
            pos,
            &GameEventContext::new(Some(player as &dyn Entity), None),
        );
        InteractionResult::Success
    }

    fn is_pathfindable(&self, _state: BlockStateId, _path_type: PathComputationType) -> bool {
        false
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::DECORATED_POT,
            level,
            pos,
            state,
        ))
    }

    fn trigger_event(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        event: i32,
        data: i32,
    ) -> bool {
        world
            .get_block_entity(pos)
            .is_some_and(|block_entity| block_entity.trigger_event(event, data))
    }

    /// Vanilla `Containers.updateNeighboursAfterDestroy`.
    fn affect_neighbors_after_removal(
        &self,
        _state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        _moved_by_piston: bool,
    ) {
        world.update_neighbor_for_output_signal(pos, self.block);
    }

    /// Vanilla `getDrops`: the `sherds` dynamic drop yields the left, back,
    /// front and right faces.
    fn get_drops(
        &self,
        state: BlockStateId,
        context: &BlockLootContext<'_>,
    ) -> Option<Vec<ItemStack>> {
        let pot = context
            .block_entity()
            .and_then(|entity| entity.downcast_ref::<DecoratedPotBlockEntity>())?;
        let decorations = pot.decorations();
        let sherds = [
            decorations.left(),
            decorations.back(),
            decorations.front(),
            decorations.right(),
        ]
        .into_iter()
        .flatten()
        .map(ItemStackTemplate::create)
        .collect();
        let dynamic_drops = [(sherds_dynamic_drop_id(), sherds)];
        Some(
            context
                .with_dynamic_drops(&dynamic_drops)
                .default_drops(state),
        )
    }

    fn player_will_destroy(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
    ) -> BlockStateId {
        let shatters = {
            let inventory = player.inventory.lock();
            Self::shatters_pot(inventory.get_item_in_hand(InteractionHand::MainHand))
        };
        if !shatters {
            return state;
        }
        let cracked = state.set_value(CRACKED, true);
        world.set_block(pos, cracked, UpdateFlags::UPDATE_NONE);
        cracked
    }

    fn on_projectile_hit(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        hit: &ClipHitResult,
        projectile: &dyn Projectile,
    ) {
        let pos = hit.block_pos;
        if projectile.projectile_may_interact(world, pos) && projectile.may_break(world) {
            world.set_block(
                pos,
                state.set_value(CRACKED, true),
                UpdateFlags::UPDATE_NONE,
            );
            world.destroy_block_by_entity(pos, true, projectile.as_entity_event_source());
        }
    }

    fn get_clone_item_stack(
        &self,
        block: BlockRef,
        _state: BlockStateId,
        block_entity: Option<SharedBlockEntity>,
        _include_data: bool,
    ) -> Option<ItemStack> {
        if let Some(block_entity) = block_entity
            && let Some(pot) = block_entity.downcast_ref::<DecoratedPotBlockEntity>()
        {
            return Some(DecoratedPotBlockEntity::create_decorated_pot_instance(
                pot.decorations(),
            ));
        }
        Some(ItemStack::new(REGISTRY.items.by_block(block)))
    }

    fn has_analog_output_signal(&self, _state: BlockStateId) -> bool {
        true
    }

    fn get_analog_output_signal(
        &self,
        _state: BlockStateId,
        world: &World,
        pos: BlockPos,
        _direction: Direction,
    ) -> i32 {
        let Some(container_ref) = world
            .get_block_entity(pos)
            .and_then(ContainerRef::from_block_entity)
        else {
            return 0;
        };
        container_ref.unpack_loot_table(None);
        let guard = ContainerLockGuard::lock_all(&[&container_ref]);
        guard
            .get(container_ref.container_id())
            .map_or(0, calculate_redstone_signal_from_container)
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::data_components::PotDecorations;
    use steel_registry::data_components::vanilla_components::POT_DECORATIONS;
    use steel_registry::items::ItemRef;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::ChunkPos;

    use super::*;
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::block_entity::init_block_entities;
    use crate::entity::init_entities;
    use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};

    const POS: BlockPos = BlockPos::new(8, 64, 8);

    fn arena(name: &'static str) -> TestWorld {
        init_vanilla_registry();
        init_behaviors();
        init_block_entities();
        init_entities();
        let fixture = fresh_test_world(name);
        insert_ready_full_chunk(&fixture.world, ChunkPos::from_block_pos(POS));
        fixture
    }

    fn side(item: ItemRef) -> ItemStackTemplate {
        ItemStackTemplate::new(item)
    }

    /// Places a pot decorated like a pot item carrying `decorations`.
    fn place_pot(world: &Arc<World>, decorations: PotDecorations) -> SharedBlockEntity {
        assert!(world.set_block(
            POS,
            vanilla_blocks::DECORATED_POT.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let pot = world.get_block_entity(POS).expect("a decorated pot");
        let mut item = ItemStack::new(&vanilla_items::DECORATED_POT);
        item.set(POT_DECORATIONS, decorations);
        pot.apply_components_from_item_stack(&item);
        pot
    }

    fn decorations() -> PotDecorations {
        PotDecorations::new(
            Some(side(&vanilla_items::ARCHER_POTTERY_SHERD)),
            Some(side(&vanilla_items::BRICK)),
            Some(side(&vanilla_items::PRIZE_POTTERY_SHERD)),
            Some(side(&vanilla_items::SKULL_POTTERY_SHERD)),
        )
    }

    fn drops(world: &Arc<World>, pot: &SharedBlockEntity, cracked: bool) -> Vec<ItemStack> {
        let state = vanilla_blocks::DECORATED_POT
            .default_state()
            .set_value(CRACKED, cracked);
        BlockLootContext::new(world, POS)
            .with_block_entity(Some(pot.as_ref()))
            .get_drops(state)
    }

    #[test]
    fn a_cracked_pot_drops_its_sherds_left_back_front_right() {
        let fixture = arena("decorated_pot_cracked_drops");
        let pot = place_pot(&fixture.world, decorations());

        let dropped: Vec<_> = drops(&fixture.world, &pot, true)
            .iter()
            .map(|stack| stack.item().key.path.to_string())
            .collect();

        assert_eq!(
            dropped,
            [
                "brick",
                "archer_pottery_sherd",
                "skull_pottery_sherd",
                "prize_pottery_sherd"
            ]
        );
    }

    #[test]
    fn an_intact_pot_drops_itself_with_its_decorations() {
        let fixture = arena("decorated_pot_intact_drops");
        let pot = place_pot(&fixture.world, decorations());

        let dropped = drops(&fixture.world, &pot, false);

        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].is(&vanilla_items::DECORATED_POT));
        assert_eq!(dropped[0].get(POT_DECORATIONS), Some(&decorations()));
    }

    #[test]
    fn only_tools_that_break_pots_without_protection_crack_them() {
        init_vanilla_registry();
        assert!(DecoratedPotBlock::shatters_pot(&ItemStack::new(
            &vanilla_items::IRON_PICKAXE
        )));
        assert!(!DecoratedPotBlock::shatters_pot(&ItemStack::new(
            &vanilla_items::STICK
        )));
    }

    #[test]
    fn a_full_stack_or_another_item_is_not_accepted() {
        init_vanilla_registry();
        let diamonds = ItemStack::with_count(&vanilla_items::DIAMOND, 63);
        assert!(DecoratedPotBlock::accepts(
            &diamonds,
            &ItemStack::new(&vanilla_items::DIAMOND)
        ));
        assert!(!DecoratedPotBlock::accepts(
            &diamonds,
            &ItemStack::new(&vanilla_items::EMERALD)
        ));
        let full = ItemStack::with_count(&vanilla_items::DIAMOND, 64);
        assert!(!DecoratedPotBlock::accepts(
            &full,
            &ItemStack::new(&vanilla_items::DIAMOND)
        ));
        assert!(!DecoratedPotBlock::accepts(
            &ItemStack::empty(),
            &ItemStack::empty()
        ));
    }

    #[test]
    fn the_pot_wobbles_through_its_block_event() {
        let fixture = arena("decorated_pot_wobble");
        let world = &fixture.world;
        let pot = place_pot(world, PotDecorations::EMPTY);
        let state = world.get_block_state(POS);

        assert!(
            BLOCK_BEHAVIORS
                .get_behavior(state.get_block())
                .trigger_event(state, world, POS, 1, WobbleStyle::Negative.ordinal())
        );
        let wobble = pot
            .downcast_ref::<DecoratedPotBlockEntity>()
            .and_then(DecoratedPotBlockEntity::last_wobble)
            .expect("the pot records the wobble");
        assert_eq!(wobble.style, WobbleStyle::Negative);
    }
}
