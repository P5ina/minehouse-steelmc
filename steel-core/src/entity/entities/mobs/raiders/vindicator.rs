//! Vindicator (Vanilla `Vindicator`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::VindicatorEntityData;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_enchantments, vanilla_entities, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::text::DisplayResolutor;
use steel_utils::{DowncastType, DowncastTypeKey};
use text_components::TextComponent;

use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    AvoidEntityGoal, FloatGoal, HoldGroundAttackGoal, HurtByTargetGoal, JohnnyFlag,
    LongDistancePatrolGoal, LookAtPlayerGoal, MeleeAttackGoal, NearestAttackableTargetGoal,
    RaiderOpenDoorGoal, RandomStrollGoal, VindicatorBreakDoorGoal, VindicatorJohnnyAttackGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::mob::spawn_equipment::populate_default_equipment_enchantments;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    AbstractIllager, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    Monster, PathfinderMob, PatrollingMonster, PatrollingMonsterState, Raider, RaiderState,
    SharedEntity, is_abstract_villager, sync_dirty_mob_effects,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::{World, raid};
use steel_utils::types::Difficulty;

const XP_REWARD: i32 = 5;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const TAG_JOHNNY: &str = "Johnny";
/// Vanilla `setCustomName`: this exact name turns a vindicator into "Johnny".
const JOHNNY_NAME: &str = "Johnny";
/// Vanilla `AvoidEntityGoal<>(this, Creaking.class, 8.0F, 1.0, 1.2)`.
const AVOID_CREAKING_DISTANCE: f32 = 8.0;
/// Vanilla `Raider.HoldGroundAttackGoal(this, 10.0F)`.
const HOLD_GROUND_RADIUS: f32 = 10.0;

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

/// A vindicator: an axe-wielding illager that guards woodland mansions.
#[entity_behavior(class = "Vindicator", spawn_rule)]
pub struct VindicatorEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    entity_data: SyncMutex<VindicatorEntityData>,
    johnny: JohnnyFlag,
}

// SAFETY: The owner-scoped type key uniquely identifies VindicatorEntity.
unsafe impl DowncastType for VindicatorEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/vindicator");
}

impl VindicatorEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new vindicator.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved vindicator.
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
        let mut entity_data = VindicatorEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let johnny = JohnnyFlag::default();

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
            goal_selector.add_goal(2, VindicatorBreakDoorGoal::new());
            goal_selector.add_goal(3, RaiderOpenDoorGoal::new());
            goal_selector.add_goal(4, HoldGroundAttackGoal::new(HOLD_GROUND_RADIUS));
            goal_selector.add_goal(5, MeleeAttackGoal::new(1.0, false));
            goal_selector.add_goal(8, RandomStrollGoal::new(0.6));
            goal_selector.add_goal(9, LookAtPlayerGoal::new_with_probability(3.0, 1.0));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(8.0, 0.02, |target, _| {
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
                NearestAttackableTargetGoal::new(true, |target, _| is_abstract_villager(target)),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            );
            target_selector.add_goal(4, VindicatorJohnnyAttackGoal::new(johnny.clone()));
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            patrolling: PatrollingMonsterState::new(),
            raider: RaiderState::new(),
            entity_data: SyncMutex::new(entity_data),
            johnny,
        }
    }

    /// Vanilla `isJohnny`.
    #[must_use]
    pub fn is_johnny(&self) -> bool {
        self.johnny.get()
    }

    /// Vanilla `Vindicator.populateDefaultEquipmentSlots`: an iron axe outside raids;
    /// raid members get theirs from `applyRaidBuffs`.
    fn populate_vindicator_equipment(&self) {
        if self.current_raid().is_some() {
            return;
        }
        let _ = self.living_base.equipment().lock().set(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::IRON_AXE),
        );
    }
}

impl DefaultEntityPush for VindicatorEntity {}

impl Entity for VindicatorEntity {
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

    fn set_custom_name(&self, custom_name: Option<TextComponent>) {
        let is_johnny_name = custom_name
            .as_ref()
            .is_some_and(|name| name.to_plain(&DisplayResolutor) == JOHNNY_NAME);
        self.default_set_custom_name(custom_name);
        if !self.is_johnny() && is_johnny_name {
            self.johnny.set(true);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_raider(nbt);
        if self.is_johnny() {
            nbt.insert(TAG_JOHNNY, 1_i8);
        }
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_raider(nbt);
        self.johnny
            .set(nbt.byte(TAG_JOHNNY).is_some_and(|value| value != 0));
    }
}

impl LivingEntity for VindicatorEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
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
        Some(&sound_events::ENTITY_VINDICATOR_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VINDICATOR_DEATH)
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

    fn die(&self, source: &DamageSource) {
        self.die_raider(source);
        self.default_die(source);
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_server_raider(world);
        self.default_hurt_server(world, source, amount)
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        self.raider_ai_step(entity);
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for VindicatorEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn custom_server_ai_step(&self) {
        if !self.is_no_ai() {
            let raided = self
                .level()
                .is_some_and(|world| world.is_raided(self.block_position()));
            self.mob_base.navigation().lock().set_can_open_doors(raided);
        }
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VINDICATOR_AMBIENT)
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.illager_can_attack(target)
    }

    fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_raider(dist_sqr)
    }

    fn requires_custom_persistence(&self) -> bool {
        self.requires_custom_persistence_raider()
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_raider(spawn_reason);
        let group_data = self.finalize_spawn_mob_base(world, spawn_reason, group_data);
        self.mob_base.navigation().lock().set_can_open_doors(true);
        let difficulty = world.current_difficulty_at(self.block_position());
        let mut random = LegacyRandom::from_seed(rand::random());
        self.populate_vindicator_equipment();
        populate_default_equipment_enchantments(self, &mut random, difficulty);
        group_data
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().raider.mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().raider.mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for VindicatorEntity {}

impl Monster for VindicatorEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for VindicatorEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for VindicatorEntity {
    fn raider_state(&self) -> &RaiderState {
        &self.raider
    }

    /// Vanilla `Vindicator.applyRaidBuffs`: an iron axe, enchanted by `raid/vindicator`
    /// (Sharpness I) or after wave 5 by `raid/vindicator_post_wave_5` (Sharpness II).
    fn apply_raid_buffs(&self, _world: &Arc<World>, wave: i32, enchant_odds: f32) {
        let mut axe = ItemStack::new(&vanilla_items::IRON_AXE);
        if rand::random::<f32>() <= enchant_odds {
            let level = if wave > raid::num_groups(Difficulty::Normal) {
                2
            } else {
                1
            };
            axe.upgrade_enchantment(vanilla_enchantments::SHARPNESS.key.clone(), level);
        }
        let _ = self
            .living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, axe);
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
        &sound_events::ENTITY_VINDICATOR_CELEBRATE
    }
}

impl AbstractIllager for VindicatorEntity {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use text_components::TextComponent;

    use super::VindicatorEntity;
    use crate::entity::entities::WitchEntity;
    use crate::entity::{Entity, Mob};

    fn vindicator(id: i32) -> VindicatorEntity {
        VindicatorEntity::new(&vanilla_entities::VINDICATOR, id, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn naming_it_johnny_sticks_through_save() {
        init_vanilla_registry();
        let mob = vindicator(1);
        mob.set_custom_name(Some(TextComponent::plain("Bob")));
        assert!(!mob.is_johnny());
        mob.set_custom_name(Some(TextComponent::plain("Johnny")));
        assert!(mob.is_johnny());
        mob.set_custom_name(Some(TextComponent::plain("Bob")));
        assert!(mob.is_johnny(), "vanilla never clears the Johnny flag");

        let mut nbt = NbtCompound::new();
        mob.save_additional(&mut nbt);
        assert_eq!(nbt.byte("Johnny"), Some(1));
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = vindicator(2);
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_johnny());
    }

    #[test]
    fn illagers_are_allies_but_witches_are_not() {
        init_vanilla_registry();
        let mob = vindicator(1);
        let other = vindicator(2);
        assert!(mob.is_allied_to(&other));
        let witch = WitchEntity::new(&vanilla_entities::WITCH, 3, DVec3::ZERO, Weak::new());
        assert!(!mob.is_allied_to(&witch));
        assert!(Mob::can_attack(&mob, &witch));
    }
}
