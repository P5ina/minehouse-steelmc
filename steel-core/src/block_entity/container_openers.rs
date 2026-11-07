//! Vanilla `ContainerOpenersCounter`: counts the players viewing a container
//! block (chest, trapped chest, ender chest, barrel) and drives its lid,
//! sounds and game events.
//!
//! Opening and closing a menu adjust the count directly. While anyone is
//! counted, the block schedules a tick every [`CHECK_TICK_DELAY`] ticks that
//! recounts the players who still have the container open, so a viewer who
//! vanished without closing the menu is dropped.

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_game_events;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, WorldAabb};

use crate::entity::Entity as _;
use crate::player::Player;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Delay of the scheduled recheck tick, Vanilla `CHECK_TICK_DELAY`.
pub const CHECK_TICK_DELAY: i32 = 5;

/// Block-entity callbacks of a [`ContainerOpenersCounter`], the abstract
/// methods of Vanilla's class.
pub trait ContainerOpenersHooks {
    /// The first viewer arrived.
    fn on_open(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId);

    /// The last viewer left.
    fn on_close(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId);

    /// The count changed, or a recheck confirmed it.
    fn opener_count_changed(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        previous: i32,
        current: i32,
    );

    /// Whether `player`'s open menu is this container.
    fn is_own_container(&self, player: &Player) -> bool;
}

#[derive(Default)]
struct CounterState {
    open_count: i32,
    max_interaction_range: f64,
}

/// Counts a container block's viewers. Vanilla `ContainerOpenersCounter`.
///
/// The lock is never held across a hook: hooks update neighbors, and a
/// trapped chest's neighbors read [`Self::opener_count`] back.
#[derive(Default)]
pub struct ContainerOpenersCounter {
    state: SyncMutex<CounterState>,
}

impl ContainerOpenersCounter {
    /// Creates a counter with no viewers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current number of viewers. Vanilla `getOpenerCount`.
    #[must_use]
    pub fn opener_count(&self) -> i32 {
        self.state.lock().open_count
    }

    /// A viewer opened the container. Vanilla `incrementOpeners`.
    pub fn increment_openers(
        &self,
        hooks: &dyn ContainerOpenersHooks,
        player: &Player,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        max_interaction_range: f64,
    ) {
        let (previous, current) = {
            let mut counter = self.state.lock();
            let previous = counter.open_count;
            counter.open_count += 1;
            (previous, counter.open_count)
        };
        if previous == 0 {
            hooks.on_open(world, pos, state);
            world.game_event(
                &vanilla_game_events::CONTAINER_OPEN,
                pos,
                &GameEventContext::new(Some(player), None),
            );
            schedule_recheck(world, pos, state);
        }
        hooks.opener_count_changed(world, pos, state, previous, current);
        let mut counter = self.state.lock();
        counter.max_interaction_range = max_interaction_range.max(counter.max_interaction_range);
    }

    /// A viewer closed the container. Vanilla `decrementOpeners`.
    pub fn decrement_openers(
        &self,
        hooks: &dyn ContainerOpenersHooks,
        player: &Player,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let (previous, current) = {
            let mut counter = self.state.lock();
            let previous = counter.open_count;
            counter.open_count -= 1;
            (previous, counter.open_count)
        };
        if current == 0 {
            hooks.on_close(world, pos, state);
            world.game_event(
                &vanilla_game_events::CONTAINER_CLOSE,
                pos,
                &GameEventContext::new(Some(player), None),
            );
            self.state.lock().max_interaction_range = 0.0;
        }
        hooks.opener_count_changed(world, pos, state, previous, current);
    }

    /// The players near `pos` that have this container open. Vanilla
    /// `getEntitiesWithContainerOpen`; players are Steel's only container users.
    #[must_use]
    pub fn players_with_container_open(
        &self,
        hooks: &dyn ContainerOpenersHooks,
        world: &World,
        pos: BlockPos,
    ) -> Vec<Arc<Player>> {
        let range = self.state.lock().max_interaction_range + 4.0;
        let x = f64::from(pos.x());
        let y = f64::from(pos.y());
        let z = f64::from(pos.z());
        let aabb = WorldAabb::new(x, y, z, x + 1.0, y + 1.0, z + 1.0).inflate(range);
        // Collect first: the menu check takes the player's menu lock, which must
        // not nest inside the entity index lock.
        let candidates =
            world.get_entities_in_aabb_matching(&aabb, |entity| entity.as_player().is_some());
        candidates
            .iter()
            .filter_map(|entity| world.players.get_by_entity_id(entity.id()))
            .filter(|player| !player.is_spectator() && hooks.is_own_container(player))
            .collect()
    }

    /// Recounts the viewers. Vanilla `recheckOpeners`, run from the block's
    /// scheduled tick.
    pub fn recheck_openers(
        &self,
        hooks: &dyn ContainerOpenersHooks,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) {
        let players = self.players_with_container_open(hooks, world, pos);
        let max_interaction_range = players
            .iter()
            .map(|player| player.block_interaction_range())
            .fold(0.0, f64::max);
        let current = i32::try_from(players.len()).unwrap_or(i32::MAX);
        let previous = {
            let mut counter = self.state.lock();
            counter.max_interaction_range = max_interaction_range;
            counter.open_count
        };
        if previous != current {
            let now_open = current != 0;
            let was_open = previous != 0;
            if now_open && !was_open {
                hooks.on_open(world, pos, state);
                world.game_event(
                    &vanilla_game_events::CONTAINER_OPEN,
                    pos,
                    &GameEventContext::default(),
                );
            } else if !now_open {
                hooks.on_close(world, pos, state);
                world.game_event(
                    &vanilla_game_events::CONTAINER_CLOSE,
                    pos,
                    &GameEventContext::default(),
                );
            }
            self.state.lock().open_count = current;
        }
        hooks.opener_count_changed(world, pos, state, previous, current);
        if current > 0 {
            schedule_recheck(world, pos, state);
        }
    }
}

fn schedule_recheck(world: &World, pos: BlockPos, state: BlockStateId) {
    world.schedule_block_tick_default(pos, state.get_block(), CHECK_TICK_DELAY);
}

/// Vanilla's container sound pitch, `random.nextFloat() * 0.1F + 0.9F`.
#[must_use]
pub fn container_sound_pitch() -> f32 {
    rand::random::<f32>() * 0.1 + 0.9
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::blocks::BlockRef;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::blocks::properties::{BlockStateProperties, ChestType, Direction};
    use steel_registry::{init_vanilla_registry, vanilla_blocks};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, BlockStateId, ChunkPos, Downcast as _, translations};
    use text_components::TextComponent;

    use crate::behavior::blocks::{ChestBlock, TrappedChestBlock, connected_direction};
    use crate::behavior::{BLOCK_BEHAVIORS, init_behaviors};
    use crate::block_entity::entities::{
        BarrelBlockEntity, ChestBlockEntity, EnderChestBlockEntity, ShulkerBoxBlockEntity,
    };
    use crate::block_entity::init_block_entities;
    use crate::entity::{Entity as _, init_entities};
    use crate::inventory::menu::kinds::{chest, chest_halves, ender_chest, shulker_box};
    use crate::player::{Player, ResetReason};
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
    };
    use crate::world::{SignalQueryContext, World};

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

    fn viewer(world: &Arc<World>, name: &str, entity_id: i32) -> Arc<Player> {
        let player = TestPlayerBuilder::new(Arc::clone(world), name, entity_id).build();
        player.set_client_loaded(true);
        player
            .teleport_to(DVec3::new(8.5, 64.0, 10.5))
            .expect("player teleport");
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        player
    }

    fn place(world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        assert!(world.set_block(pos, state, UpdateFlags::UPDATE_ALL));
    }

    fn chest_state(block: BlockRef, chest_type: ChestType) -> BlockStateId {
        block
            .default_state()
            .set_value(&BlockStateProperties::FACING, Direction::South)
            .set_value(&BlockStateProperties::CHEST_TYPE, chest_type)
    }

    /// Opens the chest menu the way `ChestBlock.useWithoutItem` does.
    fn open_chest(world: &Arc<World>, player: &Player, pos: BlockPos) {
        let state = world.get_block_state(pos);
        let containers = if state.get_block() == &vanilla_blocks::TRAPPED_CHEST {
            TrappedChestBlock::new(state.get_block()).combine(state, world, pos, false)
        } else {
            ChestBlock::new(state.get_block()).combine(state, world, pos, false)
        }
        .containers();
        let rows = containers.len() * 3;
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_CHEST.msg()),
            move |context| chest_halves(inventory, context.container_id, &containers, rows),
        );
    }

    fn chest_viewers(world: &World, pos: BlockPos) -> i32 {
        world
            .get_block_entity(pos)
            .and_then(|chest| {
                chest
                    .downcast_ref::<ChestBlockEntity>()
                    .map(ChestBlockEntity::opener_count)
            })
            .expect("a chest block entity")
    }

    /// Delivers the block events queued so far, then fires the viewer recheck
    /// the container scheduled, as its scheduled tick would 5 ticks later.
    fn fire_recheck(world: &Arc<World>, pos: BlockPos) {
        let state = world.get_block_state(pos);
        assert!(
            world.has_scheduled_block_tick(pos, state.get_block()),
            "an open container schedules its recheck"
        );
        world.run_block_events();
        BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .tick(state, world, pos);
    }

    /// The `(param_a, param_b)` of the block events queued at `pos`.
    fn events_at(world: &World, pos: BlockPos) -> Vec<(i32, i32)> {
        world
            .queued_block_events()
            .into_iter()
            .filter(|(event_pos, ..)| *event_pos == pos)
            .map(|(_, _, a, b)| (a, b))
            .collect()
    }

    #[test]
    fn chest_counts_viewers_and_sends_the_lid_event() {
        let fixture = arena("container_openers_chest");
        let world = &fixture.world;
        place(
            world,
            POS,
            chest_state(&vanilla_blocks::CHEST, ChestType::Single),
        );
        let player = viewer(world, "ChestViewer", 1);

        open_chest(world, &player, POS);
        assert_eq!(chest_viewers(world, POS), 1);
        assert_eq!(events_at(world, POS), vec![(1, 1)]);

        // The recheck keeps a viewer whose chest menu is open and, like
        // Vanilla, sends the count again.
        fire_recheck(world, POS);
        assert_eq!(chest_viewers(world, POS), 1);
        assert_eq!(events_at(world, POS), vec![(1, 1)]);

        world.run_block_events();
        player.close_container();
        assert_eq!(chest_viewers(world, POS), 0);
        assert_eq!(events_at(world, POS), vec![(1, 0)]);
    }

    #[test]
    fn chest_recheck_drops_a_viewer_out_of_reach() {
        let fixture = arena("container_openers_recheck");
        let world = &fixture.world;
        let pos = BlockPos::new(1, 64, 1);
        place(
            world,
            pos,
            chest_state(&vanilla_blocks::CHEST, ChestType::Single),
        );
        let player = viewer(world, "Vanished", 1);
        open_chest(world, &player, pos);
        assert_eq!(chest_viewers(world, pos), 1);

        // Beyond reach + 4 blocks the recheck no longer finds the viewer, even
        // though the menu was never closed: the lid closes.
        player
            .teleport_to(DVec3::new(14.5, 64.0, 14.5))
            .expect("player teleport");
        fire_recheck(world, pos);
        assert_eq!(chest_viewers(world, pos), 0);
        assert_eq!(events_at(world, pos), vec![(1, 0)]);
    }

    #[test]
    fn double_chest_counts_both_halves() {
        let fixture = arena("container_openers_double");
        let world = &fixture.world;
        let right = chest_state(&vanilla_blocks::CHEST, ChestType::Right);
        let left_pos = POS.relative(connected_direction(right));
        place(world, POS, right);
        place(
            world,
            left_pos,
            chest_state(&vanilla_blocks::CHEST, ChestType::Left),
        );
        let player = viewer(world, "DoubleViewer", 1);

        open_chest(world, &player, POS);
        assert_eq!(chest_viewers(world, POS), 1);
        assert_eq!(chest_viewers(world, left_pos), 1);
        assert_eq!(events_at(world, POS), vec![(1, 1)]);
        assert_eq!(events_at(world, left_pos), vec![(1, 1)]);

        fire_recheck(world, POS);
        fire_recheck(world, left_pos);
        assert_eq!(chest_viewers(world, POS), 1);
        assert_eq!(chest_viewers(world, left_pos), 1);

        player.close_container();
        assert_eq!(chest_viewers(world, POS), 0);
        assert_eq!(chest_viewers(world, left_pos), 0);
    }

    #[test]
    fn trapped_chest_power_follows_its_viewers() {
        let fixture = arena("container_openers_trapped");
        let world = &fixture.world;
        let state = chest_state(&vanilla_blocks::TRAPPED_CHEST, ChestType::Single);
        place(world, POS, state);
        let behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::TRAPPED_CHEST);
        let signal = |direction| {
            behavior.get_direct_signal(
                world.get_block_state(POS),
                world.as_ref(),
                POS,
                direction,
                SignalQueryContext::DEFAULT,
            )
        };
        assert!(behavior.is_signal_source(state, SignalQueryContext::DEFAULT));
        assert_eq!(signal(Direction::Up), 0);

        let first = viewer(world, "TrapA", 1);
        let second = viewer(world, "TrapB", 2);
        open_chest(world, &first, POS);
        open_chest(world, &second, POS);
        assert_eq!(chest_viewers(world, POS), 2);
        assert_eq!(signal(Direction::Up), 2);
        assert_eq!(signal(Direction::North), 0);

        first.close_container();
        second.close_container();
        assert_eq!(signal(Direction::Up), 0);

        let plain = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::CHEST);
        assert!(!plain.is_signal_source(
            chest_state(&vanilla_blocks::CHEST, ChestType::Single),
            SignalQueryContext::DEFAULT
        ));
    }

    #[test]
    fn barrel_opens_while_viewed() {
        let fixture = arena("container_openers_barrel");
        let world = &fixture.world;
        place(
            world,
            POS,
            vanilla_blocks::BARREL
                .default_state()
                .set_value(&BlockStateProperties::FACING, Direction::Up),
        );
        let player = viewer(world, "BarrelViewer", 1);
        let barrel = world.get_block_entity(POS).expect("a barrel block entity");
        let container = barrel.container_ref().expect("a barrel container");
        let inventory = Arc::clone(&player.inventory);
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_BARREL.msg()),
            move |context| chest(inventory, context.container_id, container, 3),
        );
        let is_open = || {
            world
                .get_block_state(POS)
                .get_value(&BlockStateProperties::OPEN)
        };
        assert!(is_open());
        assert_eq!(
            barrel
                .downcast_ref::<BarrelBlockEntity>()
                .map(BarrelBlockEntity::opener_count),
            Some(1)
        );

        fire_recheck(world, POS);
        assert!(is_open(), "the recheck must keep the barrel open");

        player.close_container();
        assert!(!is_open());
    }

    #[test]
    fn ender_chest_counts_its_active_viewers() {
        let fixture = arena("container_openers_ender");
        let world = &fixture.world;
        place(world, POS, vanilla_blocks::ENDER_CHEST.default_state());
        let player = viewer(world, "EnderViewer", 1);
        let block_entity = world.get_block_entity(POS).expect("an ender chest");
        let active = Arc::downgrade(&block_entity);
        let inventory = Arc::clone(&player.inventory);
        let container = Arc::clone(&player.ender_chest_inventory);
        player.open_menu(
            TextComponent::translated(translations::CONTAINER_ENDERCHEST.msg()),
            move |context| {
                container.lock().set_active_chest(active);
                ender_chest(inventory, context.container_id, container)
            },
        );
        let viewers = || {
            block_entity
                .downcast_ref::<EnderChestBlockEntity>()
                .map(EnderChestBlockEntity::opener_count)
        };
        assert_eq!(viewers(), Some(1));
        assert_eq!(events_at(world, POS), vec![(1, 1)]);

        fire_recheck(world, POS);
        assert_eq!(viewers(), Some(1));

        world.run_block_events();
        player.close_container();
        assert_eq!(viewers(), Some(0));
        assert_eq!(events_at(world, POS), vec![(1, 0)]);
    }

    #[test]
    fn shulker_box_sends_its_open_count() {
        let fixture = arena("container_openers_shulker");
        let world = &fixture.world;
        place(world, POS, vanilla_blocks::SHULKER_BOX.default_state());
        let first = viewer(world, "ShulkerA", 1);
        let second = viewer(world, "ShulkerB", 2);
        let block_entity = world.get_block_entity(POS).expect("a shulker box");
        assert!(
            block_entity
                .downcast_ref::<ShulkerBoxBlockEntity>()
                .is_some()
        );
        for player in [&first, &second] {
            let container = block_entity.container_ref().expect("a shulker container");
            let inventory = Arc::clone(&player.inventory);
            player.open_menu(
                TextComponent::translated(translations::CONTAINER_SHULKER_BOX.msg()),
                move |context| shulker_box(inventory, context.container_id, container),
            );
        }
        assert_eq!(events_at(world, POS), vec![(1, 1), (1, 2)]);

        // Closing the first resends (1, 1), an exact duplicate still queued.
        first.close_container();
        second.close_container();
        assert_eq!(events_at(world, POS), vec![(1, 1), (1, 2), (1, 0)]);
    }
}
