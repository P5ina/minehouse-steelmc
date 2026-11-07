//! Witch (Vanilla `Witch`).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::PotionContents;
use steel_registry::data_components::vanilla_components::{POTION_CONTENTS, POTION_DURATION_SCALE};
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::potion::PotionRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::WitchEntityData;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_entities, vanilla_game_events, vanilla_items,
    vanilla_mob_effects, vanilla_potions,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey, Identifier};

use crate::behavior::ITEM_BEHAVIORS;
use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    CanAttackFlag, FloatGoal, HealCooldown, HurtByTargetGoal, LongDistancePatrolGoal,
    LookAtPlayerGoal, NearestAttackableWitchTargetGoal, NearestHealableRaiderTargetGoal,
    RandomLookAroundGoal, RangedAttackGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::damage::DamageSource;
use crate::entity::entities::SplashPotionEntity;
use crate::entity::potion_contents::to_runtime_instance;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster,
    PathfinderMob, PatrollingMonster, PatrollingMonsterState, Raider, RaiderState, RangedAttackMob,
    SharedEntity, next_entity_id, spawn_mob_throwable_item_projectile, sync_dirty_mob_effects,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 5;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `SPEED_MODIFIER_DRINKING`: a witch slows down while it drinks.
const SPEED_MODIFIER_DRINKING_ID: Identifier = Identifier::vanilla_static("drinking");
const SPEED_MODIFIER_DRINKING_AMOUNT: f64 = -0.25;
const WATER_BREATHING_CHANCE: f32 = 0.15;
const FIRE_RESISTANCE_CHANCE: f32 = 0.15;
const HEALING_CHANCE: f32 = 0.05;
const SWIFTNESS_CHANCE: f32 = 0.5;
/// Vanilla drinks swiftness when its target is more than 11 blocks away.
const SWIFTNESS_TARGET_DISTANCE_SQR: f64 = 121.0;
/// Vanilla chance per tick for the hat-magic particles (entity event 15).
const HAT_MAGIC_CHANCE: f32 = 7.5E-4;
/// A raider at or below this much health gets a healing potion instead of regeneration.
const RAIDER_HEALING_HEALTH: f32 = 4.0;
const SLOWNESS_TARGET_DISTANCE: f64 = 8.0;
const POISON_MIN_TARGET_HEALTH: f32 = 8.0;
const WEAKNESS_TARGET_DISTANCE: f64 = 3.0;
const WEAKNESS_CHANCE: f32 = 0.25;
const CLOSE_THROW_DISTANCE: f64 = 2.0;
const CLOSE_THROW_POWER: f32 = 0.45;
const THROW_POWER: f32 = 0.75;
/// Vanilla `Witch.rangedAttackUncertainty`.
const THROW_UNCERTAINTY: f32 = 8.0;
/// Vanilla `getDamageAfterMagicAbsorb`: damage from `witch_resistant_to` sources is cut to 15%.
const WITCH_RESISTANCE_FACTOR: f32 = 0.15;

/// A witch: throws harmful splash potions and drinks helpful ones.
#[entity_behavior(class = "Witch", spawn_rule)]
pub struct WitchEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    entity_data: SyncMutex<WitchEntityData>,
    using_time: AtomicI32,
    heal_raiders_cooldown: HealCooldown,
    can_attack_players: CanAttackFlag,
}

// SAFETY: The owner-scoped type key uniquely identifies WitchEntity.
unsafe impl DowncastType for WitchEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/witch");
}

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

impl WitchEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new witch.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved witch.
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
        let mut entity_data = WitchEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let heal_raiders_cooldown = HealCooldown::default();
        let can_attack_players = CanAttackFlag::default();

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            // TODO: Raider goals (`ObtainRaidLeaderBannerGoal` 1, `PathfindToRaidGoal` 3,
            // `RaiderMoveThroughVillageGoal` 4, `RaiderCelebration` 5) come with raids.
            goal_selector.add_goal(4, LongDistancePatrolGoal::new(0.7, 0.595));
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(2, RangedAttackGoal::new(1.0, 60, 10.0));
            goal_selector.add_goal(2, WaterAvoidingRandomStrollGoal::new(1.0));
            goal_selector.add_goal(3, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(3, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                HurtByTargetGoal::new().with_ignored_damage_selector(is_raider),
            );
            target_selector.add_goal(
                2,
                NearestHealableRaiderTargetGoal::new(
                    true,
                    heal_raiders_cooldown.clone(),
                    |target, _| {
                        target.as_raider().is_some_and(Raider::has_active_raid)
                            && target.entity_type() != &vanilla_entities::WITCH
                    },
                ),
            );
            target_selector.add_goal(
                3,
                NearestAttackableWitchTargetGoal::new_for_players(
                    10,
                    true,
                    false,
                    can_attack_players.clone(),
                ),
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
            using_time: AtomicI32::new(0),
            heal_raiders_cooldown,
            can_attack_players,
        }
    }

    /// Vanilla `isDrinkingPotion`.
    #[must_use]
    pub fn is_drinking_potion(&self) -> bool {
        *self.entity_data.lock().using_item.get()
    }

    /// Vanilla `setUsingItem`.
    pub fn set_using_item(&self, using: bool) {
        self.entity_data.lock().using_item.set(using);
    }

    fn main_hand_item(&self) -> ItemStack {
        self.equipment_in_slot(EquipmentSlot::MainHand)
    }

    /// Vanilla `setItemSlot(EquipmentSlot.MAINHAND, stack)`.
    fn set_main_hand_item(&self, stack: ItemStack) {
        let previous = self.main_hand_item();
        let current = stack.copy_with_count(stack.count());
        self.living_base()
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, stack);
        self.on_equip_item(EquipmentSlot::MainHand, &previous, &current);
    }

    fn drink_pitch() -> f32 {
        0.8 + rand::random::<f32>() * 0.4
    }

    /// Finishes drinking: applies the potion and lifts the drinking slowdown.
    fn finish_drinking(&self) {
        self.set_using_item(false);
        let item_stack = self.main_hand_item();
        self.set_main_hand_item(ItemStack::empty());
        if item_stack.is(&vanilla_items::POTION)
            && let Some(potion) = item_stack.get(POTION_CONTENTS)
        {
            let duration_scale = item_stack
                .get(POTION_DURATION_SCALE)
                .copied()
                .unwrap_or(1.0);
            for effect in potion.all_effects() {
                let scaled = effect.with_scaled_duration(duration_scale);
                self.add_mob_effect(to_runtime_instance(&scaled, scaled.duration()));
            }
        }
        self.game_event(&vanilla_game_events::DRINK);
        self.attributes().lock().remove_modifier(
            vanilla_attributes::MOVEMENT_SPEED,
            &SPEED_MODIFIER_DRINKING_ID,
        );
    }

    /// Vanilla picks a helpful potion to drink, checking the chances in order.
    fn potion_to_drink(&self) -> Option<PotionRef> {
        if rand::random::<f32>() < WATER_BREATHING_CHANCE
            && self.is_eye_in_water()
            && !self.has_mob_effect(vanilla_mob_effects::WATER_BREATHING)
        {
            return Some(&vanilla_potions::WATER_BREATHING);
        }
        if rand::random::<f32>() < FIRE_RESISTANCE_CHANCE
            && (self.is_on_fire()
                || self
                    .last_damage_source()
                    .is_some_and(|source| source.is(&DamageTypeTag::IS_FIRE)))
            && !self.has_mob_effect(vanilla_mob_effects::FIRE_RESISTANCE)
        {
            return Some(&vanilla_potions::FIRE_RESISTANCE);
        }
        if rand::random::<f32>() < HEALING_CHANCE && self.get_health() < self.get_max_health() {
            return Some(&vanilla_potions::HEALING);
        }
        if rand::random::<f32>() < SWIFTNESS_CHANCE
            && let Some(target) = self.target()
            && !self.has_mob_effect(vanilla_mob_effects::SPEED)
            && target.position().distance_squared(self.position()) > SWIFTNESS_TARGET_DISTANCE_SQR
        {
            return Some(&vanilla_potions::SWIFTNESS);
        }
        None
    }

    fn start_drinking(&self, world: &World, potion: PotionRef) {
        self.set_main_hand_item(PotionContents::create_item_stack(
            &vanilla_items::POTION,
            potion,
        ));
        let item_stack = self.main_hand_item();
        let use_duration = ITEM_BEHAVIORS
            .get_behavior(item_stack.item())
            .get_use_duration(&item_stack, self);
        self.using_time.store(use_duration, Ordering::Relaxed);
        self.set_using_item(true);
        if !self.is_silent() {
            world.play_sound_at(
                &sound_events::ENTITY_WITCH_DRINK,
                self.sound_source(),
                self.position(),
                1.0,
                Self::drink_pitch(),
                None,
            );
        }
        let mut attributes = self.attributes().lock();
        attributes.remove_modifier(
            vanilla_attributes::MOVEMENT_SPEED,
            &SPEED_MODIFIER_DRINKING_ID,
        );
        attributes.add_modifier(
            vanilla_attributes::MOVEMENT_SPEED,
            AttributeModifier {
                id: SPEED_MODIFIER_DRINKING_ID,
                amount: SPEED_MODIFIER_DRINKING_AMOUNT,
                operation: AttributeModifierOperation::AddValue,
            },
            false,
        );
    }

    /// The witch part of Vanilla `Witch.aiStep`, before `Raider.aiStep`.
    fn witch_ai_step(&self) {
        let Some(world) = self.level() else {
            return;
        };
        if !LivingEntity::is_alive(self) {
            return;
        }
        self.heal_raiders_cooldown.decrement();
        self.can_attack_players
            .set(self.heal_raiders_cooldown.get() <= 0);

        if self.is_drinking_potion() {
            if self.using_time.fetch_sub(1, Ordering::Relaxed) <= 0 {
                self.finish_drinking();
            }
        } else if let Some(potion) = self.potion_to_drink() {
            self.start_drinking(&world, potion);
        }

        if rand::random::<f32>() < HAT_MAGIC_CHANCE {
            self.broadcast_entity_event(EntityStatus::WitchHatMagic);
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl RangedAttackMob for WitchEntity {
    fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, _power: f32) {
        if self.is_drinking_potion() {
            return;
        }
        let Some(target_living) = target.as_living_entity() else {
            return;
        };
        let position = self.position();
        let target_position = target.position();
        let target_movement = target.velocity();
        let xd = target_position.x + target_movement.x - position.x;
        let yd = target.get_eye_y() - f64::from(1.1_f32) - position.y;
        let zd = target_position.z + target_movement.z - position.z;
        let dist = (xd * xd + zd * zd).sqrt();

        let potion: PotionRef = if target.as_raider().is_some() {
            let _ = self.set_target(None);
            if target_living.get_health() <= RAIDER_HEALING_HEALTH {
                &vanilla_potions::HEALING
            } else {
                &vanilla_potions::REGENERATION
            }
        } else if dist >= SLOWNESS_TARGET_DISTANCE
            && !target_living.has_mob_effect(vanilla_mob_effects::SLOWNESS)
        {
            &vanilla_potions::SLOWNESS
        } else if target_living.get_health() >= POISON_MIN_TARGET_HEALTH
            && !target_living.has_mob_effect(vanilla_mob_effects::POISON)
        {
            &vanilla_potions::POISON
        } else if dist <= WEAKNESS_TARGET_DISTANCE
            && !target_living.has_mob_effect(vanilla_mob_effects::WEAKNESS)
            && rand::random::<f32>() < WEAKNESS_CHANCE
        {
            &vanilla_potions::WEAKNESS
        } else {
            &vanilla_potions::HARMING
        };

        if let Some(world) = self.level() {
            let item_stack =
                PotionContents::create_item_stack(&vanilla_items::SPLASH_POTION, potion);
            let power = if dist <= CLOSE_THROW_DISTANCE {
                CLOSE_THROW_POWER
            } else {
                THROW_POWER
            };
            let uncertainty = self.ranged_attack_uncertainty(&world);
            let _ = spawn_mob_throwable_item_projectile(
                &world,
                entity,
                item_stack,
                DVec3::new(xd, yd + dist * f64::from(0.2_f32), zd),
                power,
                uncertainty,
                |spawn_pos| {
                    SplashPotionEntity::new(
                        &vanilla_entities::SPLASH_POTION,
                        next_entity_id(),
                        spawn_pos,
                        Arc::downgrade(&world),
                    )
                },
            );

            if !self.is_silent() {
                world.play_sound_at(
                    &sound_events::ENTITY_WITCH_THROW,
                    self.sound_source(),
                    self.position(),
                    1.0,
                    Self::drink_pitch(),
                    None,
                );
            }
        }
    }

    fn ranged_attack_uncertainty(&self, _world: &World) -> f32 {
        THROW_UNCERTAINTY
    }
}

impl DefaultEntityPush for WitchEntity {}

impl Entity for WitchEntity {
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
        self.update_dirty_mob_effect_entity_data();
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

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_raider(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_raider(nbt);
    }
}

impl LivingEntity for WitchEntity {
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
        Some(&sound_events::ENTITY_WITCH_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_WITCH_DEATH)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn get_damage_after_magic_absorb(&self, source: &DamageSource, damage: f32) -> f32 {
        let mut damage = self.default_get_damage_after_magic_absorb(source, damage);
        if source
            .causing_entity()
            .is_some_and(|causer| causer.uuid() == self.uuid())
        {
            damage = 0.0;
        }
        if source.is(&DamageTypeTag::WITCH_RESISTANT_TO) {
            damage *= WITCH_RESISTANCE_FACTOR;
        }
        damage
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
        self.witch_ai_step();
        self.raider_ai_step(entity);
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for WitchEntity {
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
        Some(&sound_events::ENTITY_WITCH_AMBIENT)
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
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().raider.mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().raider.mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for WitchEntity {}

impl Monster for WitchEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for WitchEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_be_leader(&self) -> bool {
        false
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for WitchEntity {
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
        &sound_events::ENTITY_WITCH_CELEBRATE
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use steel_utils::BlockPos;

    use steel_registry::vanilla_damage_types;

    use super::WitchEntity;
    use crate::entity::damage::DamageSource;
    use crate::entity::{Entity, LivingEntity, PatrollingMonster, Raider};

    #[test]
    fn magic_damage_is_cut_to_fifteen_percent() {
        init_vanilla_registry();
        let witch = WitchEntity::new(&vanilla_entities::WITCH, 1, DVec3::ZERO, Weak::new());
        let magic = DamageSource::environment(&vanilla_damage_types::MAGIC);
        let damage = witch.get_damage_after_magic_absorb(&magic, 10.0);
        assert!((damage - 1.5).abs() < 1e-6, "got {damage}");
        let fall = DamageSource::environment(&vanilla_damage_types::FALL);
        assert!((witch.get_damage_after_magic_absorb(&fall, 10.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn raider_and_patrol_fields_round_trip() {
        init_vanilla_registry();
        let witch = WitchEntity::new(&vanilla_entities::WITCH, 1, DVec3::ZERO, Weak::new());
        witch.set_wave(3);
        witch.set_can_join_raid(true);
        witch.set_patrol_target(BlockPos::new(10, 64, -20));
        let mut nbt = NbtCompound::new();
        witch.save_additional(&mut nbt);
        assert_eq!(nbt.int("Wave"), Some(3));
        assert_eq!(nbt.byte("CanJoinRaid"), Some(1));
        assert_eq!(nbt.byte("Patrolling"), Some(1));
        assert_eq!(nbt.byte("PatrolLeader"), Some(0));

        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = WitchEntity::new(&vanilla_entities::WITCH, 2, DVec3::ZERO, Weak::new());
        loaded.load_additional((&borrowed).into());
        assert_eq!(loaded.wave(), 3);
        assert!(loaded.can_join_raid());
        assert!(loaded.is_patrolling());
        assert_eq!(loaded.patrol_target(), Some(BlockPos::new(10, 64, -20)));
        assert!(!loaded.can_be_leader(), "witches never lead patrols");
    }
}
