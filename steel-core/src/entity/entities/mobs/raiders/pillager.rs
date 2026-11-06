//! Pillager (Vanilla `Pillager`): the crossbow illager of patrols and outposts.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::PillagerEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_enchantments, vanilla_entities, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey, Identifier};

use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    AvoidEntityGoal, FloatGoal, HoldGroundAttackGoal, HurtByTargetGoal, LongDistancePatrolGoal,
    LookAtPlayerGoal, NearestAttackableTargetGoal, RandomStrollGoal, RangedCrossbowAttackGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::mob::spawn_equipment::{enchant_spawned_armor, enchant_spawned_weapon};
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    AbstractIllager, CrossbowAttackMob, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad,
    EntityMovementEmission, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, LivingEntitySyncedData, Mob, MobBase, Monster, PathfinderMob,
    PatrollingMonster, PatrollingMonsterState, Raider, RaiderState, RangedAttackMob, SharedEntity,
    is_abstract_villager, sync_dirty_mob_effects,
};
use crate::inventory::container::SimpleContainer;
use crate::inventory::prelude::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::{LevelReader, World};

const XP_REWARD: i32 = 5;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `Pillager.INVENTORY_SIZE`.
const INVENTORY_SIZE: usize = 5;
/// Vanilla `InventoryCarrier.TAG_INVENTORY`.
const TAG_INVENTORY: &str = "Inventory";
/// Vanilla `AvoidEntityGoal<>(this, Creaking.class, 8.0F, 1.0, 1.2)`.
const AVOID_CREAKING_DISTANCE: f32 = 8.0;
/// Vanilla `Raider.HoldGroundAttackGoal(this, 10.0F)`.
const HOLD_GROUND_RADIUS: f32 = 10.0;
/// Vanilla `RangedCrossbowAttackGoal<>(this, 1.0, 8.0F)`.
const CROSSBOW_ATTACK_RADIUS: f32 = 8.0;
/// Vanilla `Pillager.performRangedAttack` -> `performCrossbowAttack(this, 1.6F)`
/// (`CrossbowItem.MOB_ARROW_POWER`).
const MOB_ARROW_POWER: f32 = 1.6;
/// Vanilla `Pillager.enchantSpawnedWeapon`: 1 in this many get Piercing I.
const SPAWN_CROSSBOW_ENCHANT_ROLL: i32 = 300;

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

/// A pillager: a crossbow-armed illager that patrols and guards outposts.
#[entity_behavior(class = "Pillager", spawn_rule)]
pub struct PillagerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    entity_data: SyncMutex<PillagerEntityData>,
    inventory: SyncMutex<SimpleContainer>,
}

// SAFETY: The owner-scoped type key uniquely identifies PillagerEntity.
unsafe impl DowncastType for PillagerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/pillager");
}

impl PillagerEntity {
    /// Vanilla `SpawnPlacements` predicate:
    /// `PatrollingMonster.checkPatrollingMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as PatrollingMonster>::check_patrolling_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
        )
    };

    /// Creates a new pillager.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved pillager.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        mob_base.set_xp_reward(XP_REWARD);
        let mut entity_data = PillagerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            // TODO: Raider goals (`ObtainRaidLeaderBannerGoal` 1, `PathfindToRaidGoal` 3,
            // `RaiderMoveThroughVillageGoal` 4, `RaiderCelebration` 5) come with raids.
            // Vanilla `PatrollingMonster.registerGoals`.
            goal_selector.add_goal(4, LongDistancePatrolGoal::new(0.7, 0.595));
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(
                1,
                AvoidEntityGoal::with_selector(AVOID_CREAKING_DISTANCE, 1.0, 1.2, |target, _| {
                    target.entity_type() == &vanilla_entities::CREAKING
                }),
            );
            goal_selector.add_goal(2, HoldGroundAttackGoal::new(HOLD_GROUND_RADIUS));
            goal_selector.add_goal(
                3,
                RangedCrossbowAttackGoal::new(1.0, CROSSBOW_ATTACK_RADIUS),
            );
            goal_selector.add_goal(8, RandomStrollGoal::new(0.6));
            goal_selector.add_goal(9, LookAtPlayerGoal::new_with_probability(15.0, 1.0));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(15.0, 0.02, |target, _| {
                    target.as_mob().is_some()
                }),
            );

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                HurtByTargetGoal::new()
                    .with_ignored_damage_selector(is_raider)
                    .set_alert_others([]),
            );
            target_selector.add_goal(
                2,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(false, |target, _| is_abstract_villager(target)),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            patrolling: PatrollingMonsterState::new(),
            raider: RaiderState::new(),
            entity_data: SyncMutex::new(entity_data),
            inventory: SyncMutex::new(SimpleContainer::new(INVENTORY_SIZE)),
        }
    }

    /// Vanilla `isChargingCrossbow`.
    #[must_use]
    pub fn is_charging_crossbow(&self) -> bool {
        *self.entity_data.lock().is_charging_crossbow.get()
    }

    /// Vanilla `InventoryCarrier.getInventory`.
    #[must_use]
    pub const fn inventory(&self) -> &SyncMutex<SimpleContainer> {
        &self.inventory
    }

    /// Vanilla `Pillager.populateDefaultEquipmentSlots`.
    fn populate_pillager_equipment(&self) {
        let _ = self.living_base.equipment().lock().set(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::CROSSBOW),
        );
    }

    /// Vanilla `Pillager.enchantSpawnedWeapon`: the usual weapon roll, then a
    /// 1 in 300 chance of the `pillager_spawn_crossbow` provider (Piercing I).
    fn enchant_pillager_weapon(&self, random: &mut LegacyRandom, world: &World) {
        let difficulty = world.current_difficulty_at(self.block_position());
        enchant_spawned_weapon(self, random, difficulty);
        if random.next_i32_bounded(SPAWN_CROSSBOW_ENCHANT_ROLL) == 0 {
            let mut equipment = self.living_base.equipment().lock();
            let weapon = equipment.get_mut(EquipmentSlot::MainHand);
            if weapon.is(&vanilla_items::CROSSBOW) {
                // Vanilla `SingleEnchantment(PIERCING, ConstantInt.of(1))`.
                weapon.upgrade_enchantment(vanilla_enchantments::PIERCING.key.clone(), 1);
            }
        }
        enchant_spawned_armor(self, random, difficulty);
    }

    fn save_inventory(&self, nbt: &mut NbtCompound) {
        let items = self
            .inventory
            .lock()
            .store_as_item_list()
            .into_iter()
            .filter_map(|stack| match stack.to_nbt_tag() {
                NbtTag::Compound(compound) => Some(compound),
                _ => None,
            })
            .collect();
        nbt.insert(TAG_INVENTORY, NbtList::Compound(items));
    }

    fn load_inventory(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let Some(compounds) = nbt.list(TAG_INVENTORY).and_then(|list| list.compounds()) else {
            return;
        };
        let items: Vec<ItemStack> = compounds
            .into_iter()
            .filter_map(|compound| ItemStack::from_borrowed_compound(&compound))
            .collect();
        self.inventory.lock().load_item_list(items);
    }
}

impl DefaultEntityPush for PillagerEntity {}

impl Entity for PillagerEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::All
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if self.entity_type.fixed {
            self.entity_type.dimensions
        } else {
            self.entity_type.dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn max_up_step(&self) -> f32 {
        self.attributes()
            .lock()
            .get_value(vanilla_attributes::STEP_HEIGHT)
            .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
    }

    fn sound_source(&self) -> SoundSource {
        Monster::sound_source_monster(self)
    }

    fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        Monster::fall_sounds_monster(self)
    }

    fn swim_sound(&self) -> SoundEventRef {
        Monster::swim_sound_monster(self)
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        Monster::swim_splash_sound_monster(self)
    }

    fn is_allied_to(&self, other: &dyn Entity) -> bool {
        self.illager_is_allied_to(other)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_raider(nbt);
        self.save_inventory(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_raider(nbt);
        self.load_inventory(nbt);
        self.set_can_pick_up_loot(true);
    }
}

impl LivingEntity for PillagerEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn living_synced_data(&self) -> Option<&dyn LivingEntitySyncedData> {
        Some(&self.entity_data)
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PILLAGER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PILLAGER_DEATH)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for PillagerEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PILLAGER_AMBIENT)
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.illager_can_attack(target)
    }

    fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_raider(dist_sqr)
    }

    fn max_spawn_cluster_size(&self) -> i32 {
        1
    }

    fn get_preferred_weapon_type(&self) -> Option<Identifier> {
        Some(ItemTag::PILLAGER_PREFERRED_WEAPONS)
    }

    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        let item_stack = item_entity.get_item();
        if item_stack.item().has_tag(&ItemTag::BANNERS) {
            // TODO: `Raider.pickUpItem` claims an ominous banner for the raid
            // leader once raids exist; without a raid it is `Mob.pickUpItem`.
            self.default_pick_up_item(world, item_entity);
        }
        // TODO: with an active raid, `wantsItem` (a white banner) goes into the
        // inventory (`InventoryCarrier`); `has_active_raid` is false until raids land.
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut random = LegacyRandom::from_seed(rand::random());
        self.populate_pillager_equipment();
        self.enchant_pillager_weapon(&mut random, world);
        self.finalize_spawn_raider(spawn_reason);
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().raider.mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().raider.mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for PillagerEntity {
    fn get_walk_target_value_in(&self, _pos: BlockPos, _level: &dyn LevelReader) -> f32 {
        0.0
    }
}

impl Monster for PillagerEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for PillagerEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for PillagerEntity {
    fn raider_state(&self) -> &RaiderState {
        &self.raider
    }

    fn is_celebrating(&self) -> bool {
        *self.entity_data.lock().raider.is_celebrating.get()
    }

    fn set_celebrating(&self, celebrating: bool) {
        self.entity_data
            .lock()
            .raider
            .is_celebrating
            .set(celebrating);
    }

    fn celebrate_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_PILLAGER_CELEBRATE
    }
}

impl AbstractIllager for PillagerEntity {}

impl RangedAttackMob for PillagerEntity {
    fn perform_ranged_attack(&self, entity: &SharedEntity, _target: &SharedEntity, _power: f32) {
        self.perform_crossbow_attack(entity, MOB_ARROW_POWER);
    }
}

impl CrossbowAttackMob for PillagerEntity {
    fn set_charging_crossbow(&self, is_charging: bool) {
        self.entity_data
            .lock()
            .is_charging_crossbow
            .set(is_charging);
    }

    fn on_crossbow_attack_performed(&self) {
        self.set_no_action_time(0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Weak};

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::item_stack::ItemStack;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos, WorldAabb};

    use super::PillagerEntity;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::CowEntity;
    use crate::entity::{Entity, Mob, SharedEntity, next_entity_id};
    use crate::inventory::container::Container as _;
    use crate::inventory::equipment::EquipmentSlot;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    fn pillager(id: i32) -> PillagerEntity {
        PillagerEntity::new(&vanilla_entities::PILLAGER, id, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn inventory_round_trips_and_loading_enables_loot_pickup() {
        init_vanilla_registry();
        let mob = pillager(1);
        let _ = mob
            .inventory()
            .lock()
            .add_item(&ItemStack::with_count(&vanilla_items::WHITE_BANNER, 1));
        assert!(!mob.can_pick_up_loot());

        let mut nbt = NbtCompound::new();
        mob.save_additional(&mut nbt);
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = pillager(2);
        loaded.load_additional((&borrowed).into());
        assert!(
            loaded
                .inventory()
                .lock()
                .get_item(0)
                .is(&vanilla_items::WHITE_BANNER)
        );
        assert!(
            loaded.can_pick_up_loot(),
            "vanilla sets canPickUpLoot on load"
        );
    }

    #[test]
    fn pillagers_spawn_alone() {
        init_vanilla_registry();
        assert_eq!(pillager(1).max_spawn_cluster_size(), 1);
    }

    #[test]
    fn pillager_charges_its_crossbow_and_shoots_its_target() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("pillager_shoots_crossbow");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let stone = vanilla_blocks::STONE.default_state();
        for x in 0..16 {
            for z in 0..16 {
                world.set_block(BlockPos::new(x, 64, z), stone, UpdateFlags::UPDATE_ALL);
            }
        }

        let pillager = Arc::new(PillagerEntity::new(
            &vanilla_entities::PILLAGER,
            next_entity_id(),
            DVec3::new(3.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        pillager.living_base.equipment().lock().set(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::CROSSBOW),
        );
        let pillager_entity: SharedEntity = Arc::<PillagerEntity>::clone(&pillager);
        world
            .try_add_entity(Arc::clone(&pillager_entity))
            .expect("pillager should attach to the loaded chunk");
        let cow: SharedEntity = Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(9.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::clone(&cow))
            .expect("cow should attach to the loaded chunk");
        assert!(pillager.set_target(Some(&cow)));

        let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 80.0, 16.0);
        let mut saw_charging = false;
        let mut fired = false;
        for _ in 0..200 {
            Arc::clone(&pillager_entity).tick();
            saw_charging |= pillager.is_charging_crossbow();
            fired = world
                .get_entities_in_aabb(&area)
                .iter()
                .any(|entity| entity.entity_type().key == vanilla_entities::ARROW.key);
            if fired {
                break;
            }
        }
        assert!(saw_charging, "the pillager charges its crossbow first");
        assert!(fired, "a charged crossbow fires an arrow at the target");
    }
}
