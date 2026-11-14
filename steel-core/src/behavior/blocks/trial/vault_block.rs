//! Vanilla `VaultBlock`: takes a trial key from nearby players and ejects
//! its reward.

use std::sync::{Arc, Weak};

use steel_macros::block_behavior;
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
#[cfg(test)]
use steel_registry::blocks::properties::Direction;
use steel_registry::blocks::properties::{BlockStateProperties, VaultState};
use steel_registry::vanilla_block_entity_types;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use crate::behavior::InventoryAccess;
use crate::behavior::block::{BlockBehavior, BlockEntityCreation};
use crate::behavior::context::{BlockHitResult, BlockPlaceContext, InteractionResult};
use crate::block_entity::entities::VaultBlockEntity;
use crate::block_entity::{BLOCK_ENTITIES, BlockEntityTicker};
use crate::player::Player;
use crate::world::World;

/// Vault block behavior.
#[block_behavior]
pub struct VaultBlock {
    block: BlockRef,
}

impl VaultBlock {
    /// Creates vault behavior for `block`.
    #[must_use]
    pub const fn new(block: BlockRef) -> Self {
        Self { block }
    }
}

impl BlockBehavior for VaultBlock {
    fn get_state_for_placement(&self, context: &BlockPlaceContext<'_>) -> Option<BlockStateId> {
        Some(self.block.default_state().set_value(
            &BlockStateProperties::HORIZONTAL_FACING,
            context.horizontal_direction().opposite(),
        ))
    }

    /// Vanilla `useItemOn`: an active vault tries the held item as its key.
    fn use_item_on(
        &self,
        state: BlockStateId,
        world: &Arc<World>,
        pos: BlockPos,
        player: &Player,
        _hand: InteractionHand,
        _hit_result: &BlockHitResult,
        inv: &mut InventoryAccess,
    ) -> InteractionResult {
        let mut held = inv.with_item(|stack| stack.clone());
        if held.is_empty()
            || state.get_value(&BlockStateProperties::VAULT_STATE) != VaultState::Active
        {
            return InteractionResult::TryEmptyHandInteraction;
        }
        let Some(block_entity) = world.get_block_entity(pos) else {
            return InteractionResult::TryEmptyHandInteraction;
        };
        let Some(vault) = block_entity.downcast_ref::<VaultBlockEntity>() else {
            return InteractionResult::TryEmptyHandInteraction;
        };
        if vault.try_insert_key(world, pos, state, player, &mut held) {
            inv.with_item(|stack| *stack = held);
        }
        InteractionResult::SuccessServer
    }

    fn new_block_entity(
        &self,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> BlockEntityCreation {
        BlockEntityCreation::from_registered_factory(BLOCK_ENTITIES.create(
            &vanilla_block_entity_types::VAULT,
            level,
            pos,
            state,
        ))
    }

    fn get_block_entity_ticker(
        &self,
        _world: &Arc<World>,
        _state: BlockStateId,
        block_entity_type: BlockEntityTypeRef,
    ) -> Option<BlockEntityTicker> {
        BlockEntityTicker::for_matching_entity_tick(
            block_entity_type,
            &vanilla_block_entity_types::VAULT,
        )
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use steel_registry::item_stack::ItemStack;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};
    use steel_utils::ChunkPos;
    use steel_utils::types::UpdateFlags;

    use super::*;
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::block_entity::{SharedBlockEntity, init_block_entities};
    use crate::entity::Entity as _;
    use crate::entity::init_entities;
    use crate::inventory::container::Container as _;
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
    };

    const VAULT: BlockPos = BlockPos::new(8, 64, 8);

    fn arena(name: &'static str) -> TestWorld {
        init_vanilla_registry();
        init_behaviors();
        init_block_entities();
        init_entities();
        let fixture = fresh_test_world(name);
        insert_ready_full_chunk(&fixture.world, ChunkPos::from_block_pos(VAULT));
        assert!(fixture.world.set_block(
            VAULT,
            vanilla_blocks::VAULT.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        fixture
    }

    fn player_near(world: &Arc<World>, distance: f64) -> Arc<Player> {
        let player = TestPlayerBuilder::new(Arc::clone(world), "VaultTester", 1).build();
        player.snap_to(
            DVec3::new(
                f64::from(VAULT.x()) + distance + 0.5,
                f64::from(VAULT.y()),
                f64::from(VAULT.z()) + 0.5,
            ),
            0.0,
            0.0,
        );
        assert!(world.players.insert(Arc::clone(&player)));
        player
    }

    fn vault(world: &World) -> SharedBlockEntity {
        world.get_block_entity(VAULT).expect("a vault block entity")
    }

    fn vault_state(world: &World) -> VaultState {
        world
            .get_block_state(VAULT)
            .get_value(&BlockStateProperties::VAULT_STATE)
    }

    /// One game tick for the vault.
    fn tick(world: &Arc<World>) {
        vault(world).tick(world);
        world.level_data.write().advance_game_time();
    }

    fn use_held(world: &Arc<World>, player: &Player) -> InteractionResult {
        let state = world.get_block_state(VAULT);
        let mut inv =
            InventoryAccess::new(Arc::clone(&player.inventory), InteractionHand::MainHand);
        BLOCK_BEHAVIORS.get_behavior(state.get_block()).use_item_on(
            state,
            world,
            VAULT,
            player,
            InteractionHand::MainHand,
            &BlockHitResult {
                location: DVec3::new(8.5, 65.0, 8.5),
                direction: Direction::Up,
                block_pos: VAULT,
                miss: false,
                inside: false,
                world_border_hit: false,
            },
            &mut inv,
        )
    }

    fn activate(world: &Arc<World>) -> Arc<Player> {
        let player = player_near(world, 2.0);
        tick(world);
        assert_eq!(vault_state(world), VaultState::Active);
        player
    }

    #[test]
    fn a_player_in_range_turns_the_vault_on_and_it_shows_a_reward() {
        let fixture = arena("vault_activates");
        let world = &fixture.world;
        let player = activate(world);

        let block_entity = vault(world);
        let vault = block_entity
            .downcast_ref::<VaultBlockEntity>()
            .expect("a vault");
        assert_eq!(vault.connected_players(), [player.uuid()]);
        assert!(!vault.display_item().is_empty());
    }

    #[test]
    fn a_player_out_of_range_leaves_it_off() {
        let fixture = arena("vault_out_of_range");
        let world = &fixture.world;
        let _player = player_near(world, 5.0);
        tick(world);
        assert_eq!(vault_state(world), VaultState::Inactive);
    }

    #[test]
    fn a_key_unlocks_ejects_the_reward_and_the_player_is_turned_away_next_time() {
        let fixture = arena("vault_reward");
        let world = &fixture.world;
        let player = activate(world);
        player
            .inventory
            .lock()
            .set_item(0, ItemStack::with_count(&vanilla_items::TRIAL_KEY, 2));

        assert_eq!(use_held(world, &player), InteractionResult::SuccessServer);
        assert_eq!(vault_state(world), VaultState::Unlocking);
        assert_eq!(player.inventory.lock().get_item(0).count(), 1);
        let block_entity = vault(world);
        let vault = block_entity
            .downcast_ref::<VaultBlockEntity>()
            .expect("a vault");
        assert!(vault.has_rewarded_player(player.uuid()));
        assert_ne!(vault.items_to_eject().len(), 0);
        // The rewarded player no longer counts as connected.
        assert_eq!(vault.connected_players().len(), 0);

        let mut saw_ejecting = false;
        for _ in 0..400 {
            tick(world);
            saw_ejecting |= vault_state(world) == VaultState::Ejecting;
            if vault_state(world) == VaultState::Inactive {
                break;
            }
        }
        assert!(saw_ejecting);
        assert_eq!(vault_state(world), VaultState::Inactive);
        assert_eq!(vault.items_to_eject().len(), 0);
        assert!(vault.display_item().is_empty());

        // Even with the vault forced back on, the same player is turned away.
        let active = world
            .get_block_state(VAULT)
            .set_value(&BlockStateProperties::VAULT_STATE, VaultState::Active);
        assert!(world.set_block(VAULT, active, UpdateFlags::UPDATE_ALL));
        assert_eq!(use_held(world, &player), InteractionResult::SuccessServer);
        assert_eq!(player.inventory.lock().get_item(0).count(), 1);
        assert_eq!(vault_state(world), VaultState::Active);
    }

    #[test]
    fn the_wrong_item_is_refused_and_kept() {
        let fixture = arena("vault_wrong_item");
        let world = &fixture.world;
        let player = activate(world);
        player
            .inventory
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::OMINOUS_TRIAL_KEY));

        assert_eq!(use_held(world, &player), InteractionResult::SuccessServer);
        assert_eq!(vault_state(world), VaultState::Active);
        assert!(
            player
                .inventory
                .lock()
                .get_item(0)
                .is(&vanilla_items::OMINOUS_TRIAL_KEY)
        );
    }

    #[test]
    fn an_inactive_vault_passes_the_click_on() {
        let fixture = arena("vault_inactive_click");
        let world = &fixture.world;
        let player = TestPlayerBuilder::new(Arc::clone(world), "VaultTester", 1).build();
        player
            .inventory
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::TRIAL_KEY));
        assert_eq!(
            use_held(world, &player),
            InteractionResult::TryEmptyHandInteraction
        );
    }
}
