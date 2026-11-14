//! Ominous item spawner (`OminousItemSpawner`).
//!
//! Ominous trial spawners drop their loot through this invisible entity: it
//! waits 60–120 ticks, warns with a sound 36 ticks early, then either shoots
//! a projectile item straight down or drops the item, and dies.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::blocks::properties::Direction;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_data::OminousItemSpawnerEntityData;
use steel_registry::{level_events, sound_events, vanilla_entities, vanilla_game_events};
use steel_utils::locks::SyncMutex;
use steel_utils::nbt::NbtNumeric as _;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::behavior::ITEM_BEHAVIORS;
use crate::behavior::projectile_item::{ProjectileItem, spawn_projectile_using_shoot};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::next_entity_id;
use crate::entity::{Entity, EntityBase, EntityBaseLoad, EntitySyncedData, SharedEntity};
use crate::world::World;
use crate::world::game_event::GameEventContext;

const TAG_ITEM: &str = "item";
const TAG_SPAWN_ITEM_AFTER_TICKS: &str = "spawn_item_after_ticks";
/// Fewest ticks before the item spawns.
const SPAWN_ITEM_DELAY_MIN: i64 = 60;
/// Most ticks before the item spawns.
const SPAWN_ITEM_DELAY_MAX: i64 = 120;
/// How early the about-to-spawn sound plays.
pub const TICKS_BEFORE_ABOUT_TO_SPAWN_SOUND: i64 = 36;

/// An ominous item spawner.
#[entity_behavior(class = "OminousItemSpawner")]
pub struct OminousItemSpawnerEntity {
    /// Common entity fields (id, uuid, position, etc.).
    base: EntityBase,
    /// Vanilla entity type registered for this implementation.
    entity_type: EntityTypeRef,
    /// Synced data carrying the item about to spawn.
    entity_data: SyncMutex<OminousItemSpawnerEntityData>,
    /// Vanilla `spawnItemAfterTicks`.
    spawn_item_after_ticks: SyncMutex<i64>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `OminousItemSpawnerEntity`.
unsafe impl DowncastType for OminousItemSpawnerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/ominous_item_spawner");
}

impl OminousItemSpawnerEntity {
    /// Creates an empty spawner that spawns on its first tick.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(OminousItemSpawnerEntityData::new()),
            spawn_item_after_ticks: SyncMutex::new(0),
        }
    }

    /// Creates a spawner from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(OminousItemSpawnerEntityData::new()),
            spawn_item_after_ticks: SyncMutex::new(0),
        }
    }

    /// Vanilla `OminousItemSpawner.create`: spawns `item` after a random
    /// 60–120 ticks.
    #[must_use]
    pub fn create(world: &Arc<World>, position: DVec3, item: ItemStack) -> Self {
        let spawner = Self::new(
            &vanilla_entities::OMINOUS_ITEM_SPAWNER,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        );
        *spawner.spawn_item_after_ticks.lock() =
            rand::random_range(SPAWN_ITEM_DELAY_MIN..=SPAWN_ITEM_DELAY_MAX);
        spawner.set_item(item);
        spawner
    }

    /// The item this spawner will spawn.
    #[must_use]
    pub fn item(&self) -> ItemStack {
        self.entity_data.lock().item.get().clone()
    }

    fn set_item(&self, item: ItemStack) {
        self.entity_data.lock().item.set(item);
    }

    /// Ticks after which the item spawns.
    #[must_use]
    pub fn spawn_item_after_ticks(&self) -> i64 {
        *self.spawn_item_after_ticks.lock()
    }

    /// Vanilla `tickServer`.
    fn tick_server(self: &Arc<Self>, world: &Arc<World>) {
        let tick_count = i64::from(self.tick_count());
        let spawn_item_after_ticks = self.spawn_item_after_ticks();
        if tick_count == spawn_item_after_ticks - TICKS_BEFORE_ABOUT_TO_SPAWN_SOUND {
            world.play_sound(
                &sound_events::BLOCK_TRIAL_SPAWNER_ABOUT_TO_SPAWN_ITEM,
                SoundSource::Neutral,
                BlockPos::from(self.position()),
                1.0,
                1.0,
                None,
            );
        }

        if tick_count >= spawn_item_after_ticks {
            self.spawn_item(world);
            self.kill(world);
        }
    }

    /// Vanilla `spawnItem`.
    fn spawn_item(self: &Arc<Self>, world: &Arc<World>) {
        let item = self.item();
        if item.is_empty() {
            return;
        }

        let spawned = if let Some(projectile_item) =
            ITEM_BEHAVIORS.get_behavior(item.item).as_projectile_item()
        {
            self.spawn_projectile(world, projectile_item, &item)
        } else {
            let entity: SharedEntity = Arc::new(ItemEntity::with_item(
                &vanilla_entities::ITEM,
                next_entity_id(),
                self.position(),
                item,
                Arc::downgrade(world),
            ));
            if let Err(error) = world.try_add_entity(Arc::clone(&entity)) {
                log::debug!("ominous item spawner could not drop its item: {error}");
            }
            entity
        };

        world.level_event(
            level_events::PARTICLES_TRIAL_SPAWNER_SPAWN_ITEM,
            BlockPos::from(self.position()),
            1,
            None,
        );
        world.game_event_at(
            &vanilla_game_events::ENTITY_PLACE,
            self.position(),
            &GameEventContext::new(Some(spawned.as_ref()), None),
        );
        self.set_item(ItemStack::empty());
    }

    /// Vanilla `spawnProjectile`: shoots the item straight down, owned by the
    /// spawner.
    fn spawn_projectile(
        self: &Arc<Self>,
        world: &Arc<World>,
        projectile_item: &dyn ProjectileItem,
        item: &ItemStack,
    ) -> SharedEntity {
        let config = projectile_item.create_dispense_config();
        let block_pos = BlockPos::from(self.position());
        if let Some(event) = config.override_dispense_event {
            world.level_event(event, block_pos, 0, None);
        }
        let direction = Direction::Down;
        let projectile = projectile_item.as_projectile(world, self.position(), item, direction);
        spawn_projectile_using_shoot(
            world,
            &projectile,
            item,
            direction.offset_vec().as_dvec3(),
            config.power,
            config.uncertainty,
        );
        if let Some(projectile) = projectile.as_projectile() {
            let owner: SharedEntity = Arc::<Self>::clone(self);
            projectile.set_owner_entity(Some(&owner));
        }
        projectile
    }
}

impl Entity for OminousItemSpawnerEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.default_tick();
        // VANILLA CLIENT-LOCAL: `tickClient` spawns the ominous spawning
        // particles; the server only ticks the countdown.
        if let Some(world) = self.level() {
            self.tick_server(&world);
        }
    }

    fn no_physics(&self) -> bool {
        true
    }

    fn can_add_passenger(&self, _passenger: &dyn Entity) -> bool {
        false
    }

    fn could_accept_passenger(&self) -> bool {
        false
    }

    fn piston_push_reaction(&self) -> PushReaction {
        PushReaction::IgnoreEntity
    }

    fn is_ignoring_block_triggers(&self) -> bool {
        true
    }

    fn hurt(&self, _world: &World, _source: &DamageSource, _amount: f32) -> bool {
        false
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let item = self.item();
        if !item.is_empty() {
            nbt.insert(TAG_ITEM, item.to_nbt_tag_ref());
        }
        nbt.insert(TAG_SPAWN_ITEM_AFTER_TICKS, self.spawn_item_after_ticks());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_item(
            nbt.compound(TAG_ITEM)
                .and_then(|item| ItemStack::from_borrowed_compound(&item))
                .unwrap_or_else(ItemStack::empty),
        );
        *self.spawn_item_after_ticks.lock() = nbt
            .get(TAG_SPAWN_ITEM_AFTER_TICKS)
            .and_then(|tag| tag.long_value())
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::{init_vanilla_registry, vanilla_damage_types, vanilla_items};

    use crate::test_support::test_world;

    use super::*;

    fn reload(nbt: &NbtCompound) -> OminousItemSpawnerEntity {
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(bytes.as_slice())).expect("valid NBT");
        let spawner = OminousItemSpawnerEntity::new(
            &vanilla_entities::OMINOUS_ITEM_SPAWNER,
            2,
            DVec3::ZERO,
            Weak::new(),
        );
        spawner.load_additional((&borrowed).into());
        spawner
    }

    #[test]
    fn create_picks_a_vanilla_delay() {
        init_vanilla_registry();
        let world = test_world();
        for _ in 0..200 {
            let spawner = OminousItemSpawnerEntity::create(
                world,
                DVec3::ZERO,
                ItemStack::new(&vanilla_items::DIAMOND),
            );
            assert!((60..=120).contains(&spawner.spawn_item_after_ticks()));
        }
    }

    #[test]
    fn saves_the_item_and_delay_and_skips_an_empty_item() {
        init_vanilla_registry();
        let world = test_world();
        let spawner = OminousItemSpawnerEntity::create(
            world,
            DVec3::ZERO,
            ItemStack::new(&vanilla_items::ARROW),
        );
        let mut nbt = NbtCompound::new();
        spawner.save_additional(&mut nbt);
        let reloaded = reload(&nbt);
        assert!(reloaded.item().is(&vanilla_items::ARROW));
        assert_eq!(
            reloaded.spawn_item_after_ticks(),
            spawner.spawn_item_after_ticks()
        );

        let empty = OminousItemSpawnerEntity::new(
            &vanilla_entities::OMINOUS_ITEM_SPAWNER,
            3,
            DVec3::ZERO,
            Weak::new(),
        );
        let mut nbt = NbtCompound::new();
        empty.save_additional(&mut nbt);
        assert!(nbt.get(TAG_ITEM).is_none());
        assert_eq!(nbt.long(TAG_SPAWN_ITEM_AFTER_TICKS), Some(0));
    }

    #[test]
    fn delay_loads_from_any_numeric_tag() {
        init_vanilla_registry();
        let mut nbt = NbtCompound::new();
        nbt.insert(TAG_SPAWN_ITEM_AFTER_TICKS, 75_i32);
        assert_eq!(reload(&nbt).spawn_item_after_ticks(), 75);
    }

    #[test]
    fn cannot_be_hurt() {
        init_vanilla_registry();
        let world = test_world();
        let spawner = OminousItemSpawnerEntity::create(
            world,
            DVec3::ZERO,
            ItemStack::new(&vanilla_items::DIAMOND),
        );
        let source = DamageSource::environment(&vanilla_damage_types::GENERIC);
        assert!(!spawner.hurt(world, &source, 10.0));
    }
}
