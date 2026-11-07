//! Vanilla `Breeze`: the trial chamber mob that slides, leaps and fires wind
//! charges.

mod ai;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::BreezeEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::enchantment_helper;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::memory_types::{ATTACK_TARGET, HURT_BY};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster, PathfinderMob,
    Projectile, ProjectileDeflection, SharedEntity, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 10;
/// Vanilla `Breeze.FALL_DISTANCE_SOUND_TRIGGER_THRESHOLD`.
const FALL_DISTANCE_SOUND_TRIGGER_THRESHOLD: f64 = 3.0;
/// Vanilla `BreezeAi.JUMP_CIRCLE_INNER_RADIUS`.
const JUMP_CIRCLE_INNER_RADIUS: f64 = 4.0;
/// Vertical reach of the inner jump circle (vanilla `withinInnerCircleRange`).
const JUMP_CIRCLE_INNER_HEIGHT: f64 = 10.0;

/// A breeze.
#[entity_behavior(class = "Breeze", spawn_rule)]
pub struct BreezeEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<BreezeEntityData>,
    brain: Brain<Self>,
}

// SAFETY: The owner-scoped type key uniquely identifies BreezeEntity.
unsafe impl DowncastType for BreezeEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/breeze");
}

impl BreezeEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkAnyLightMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Monster>::check_any_light_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
        )
    };

    /// Creates a new breeze.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved breeze.
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
        let mut entity_data = BreezeEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let brain = Brain::new(&[], ai::sensors(), ai::activities(), None);
        brain.set_default_activity(Activity::FIGHT);
        brain.use_default_activity();
        let breeze = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            brain,
        };
        breeze.set_pathfinding_malus(PathType::OnTopOfTrapdoor, -1.0);
        breeze.set_pathfinding_malus(PathType::Fire, -1.0);
        breeze
    }

    /// The breeze's brain memories.
    #[must_use]
    pub const fn memories(&self) -> &SyncMutex<Memories> {
        self.brain.memories()
    }

    /// Vanilla `Breeze.getHurtBy`: the living entity behind the last damage.
    #[must_use]
    pub fn hurt_by(&self) -> Option<SharedEntity> {
        self.brain
            .memories()
            .lock()
            .get_ref(&HURT_BY)?
            .causing_entity()
            .filter(|entity| entity.is_living_entity())
            .cloned()
    }

    /// Vanilla `Breeze.withinInnerCircleRange`.
    #[must_use]
    pub fn within_inner_circle_range(&self, target: DVec3) -> bool {
        let BlockPos(pos) = self.block_position();
        let center = pos.as_dvec3() + DVec3::splat(0.5);
        let offset = target - center;
        offset.x.mul_add(offset.x, offset.z * offset.z)
            < JUMP_CIRCLE_INNER_RADIUS * JUMP_CIRCLE_INNER_RADIUS
            && offset.y.abs() < JUMP_CIRCLE_INNER_HEIGHT
    }

    /// Vanilla `Breeze.getFiringYPosition`.
    #[must_use]
    pub fn firing_y_position(&self) -> f64 {
        f64::from(self.base.dimensions().height / 2.0 + 0.3) + self.position().y
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl DefaultEntityPush for BreezeEntity {}

impl Entity for BreezeEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `Breeze.getMovementEmission`.
    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    // Vanilla `Breeze.tick` only adds particles, animation states and the
    // whirl sound through `playLocalSound`, all of which are client-side.
    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
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

    /// Vanilla `Breeze.deflection`: reverses projectiles other than wind
    /// charges with the deflect sound.
    fn deflection(&self, projectile: &dyn Projectile) -> ProjectileDeflection {
        let projectile_type = projectile.entity_type();
        if projectile_type == &vanilla_entities::BREEZE_WIND_CHARGE
            || projectile_type == &vanilla_entities::WIND_CHARGE
        {
            ProjectileDeflection::None
        } else if REGISTRY
            .entity_types
            .is_in_tag(self.entity_type, &EntityTypeTag::DEFLECTS_PROJECTILES)
        {
            ProjectileDeflection::BreezeReverse
        } else {
            ProjectileDeflection::None
        }
    }

    /// Vanilla `Breeze.getFluidJumpThreshold`.
    fn get_fluid_jump_threshold(&self) -> f64 {
        self.get_eye_height()
    }

    /// Vanilla `Breeze.causeFallDamage`: lands with a sound after a long fall.
    fn cause_fall_damage(
        self: Arc<Self>,
        fall_distance: f64,
        damage_modifier: f32,
        source: &DamageSource,
    ) -> bool {
        if fall_distance > FALL_DISTANCE_SOUND_TRIGGER_THRESHOLD {
            self.play_sound(&sound_events::ENTITY_BREEZE_LAND, 1.0, 1.0);
        }
        self.cause_living_fall_damage(fall_distance, damage_modifier, source)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("Brain", self.brain.save());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
    }
}

impl LivingEntity for BreezeEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
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
        Some(&sound_events::ENTITY_BREEZE_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_BREEZE_DEATH)
    }

    /// Vanilla `Breeze.isInvulnerableTo`: breezes cannot hurt each other.
    fn is_invulnerable_to(&self, _world: &World, source: &DamageSource) -> bool {
        source
            .causing_entity()
            .is_some_and(|entity| entity.entity_type() == &vanilla_entities::BREEZE)
            || self.default_is_invulnerable_to(source)
            || enchantment_helper::is_immune_to_damage(self, source)
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

impl Mob for BreezeEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `Breeze.getTarget`: the brain's attack target.
    fn target(&self) -> Option<SharedEntity> {
        self.brain.memories().lock().get(&ATTACK_TARGET)
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
        }
        ai::update_activity(self);
    }

    /// Vanilla `Breeze.canAttack`: only players and iron golems.
    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        let target_type = target.entity_type();
        (target_type == &vanilla_entities::PLAYER || target_type == &vanilla_entities::IRON_GOLEM)
            && self.default_can_attack_mob(target)
    }

    fn max_head_y_rot(&self) -> f32 {
        30.0
    }

    fn head_rot_speed(&self) -> f32 {
        25.0
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.on_ground() {
            &sound_events::ENTITY_BREEZE_IDLE_GROUND
        } else {
            &sound_events::ENTITY_BREEZE_IDLE_AIR
        })
    }

    /// Vanilla `Breeze.playAmbientSound` plays through `playLocalSound`,
    /// which the server ignores.
    fn play_ambient_sound(&self) {}

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for BreezeEntity {}

impl Monster for BreezeEntity {}
