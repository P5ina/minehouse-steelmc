//! Vanilla frog (`Frog`): hops onto lily pads, croaks and eats small slimes
//! and magma cubes with its tongue.

mod ai;
mod tadpole;
#[cfg(test)]
mod tests;

use std::str::FromStr as _;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::FROG_VARIANT;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::frog_variant::FrogVariantRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::FrogEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _, sound_events,
    vanilla_attributes,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, Identifier};

pub use tadpole::TadpoleEntity;

use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, IS_PREGNANT, LONG_JUMP_COOLDOWN_TICKS,
};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::PathType;
use crate::entity::ai::smooth_swimming::SmoothSwimmingMoveControl;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob,
    MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::{MoveResult, MoverType};
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Frog.FROG_FALL_DAMAGE_REDUCTION`.
const FROG_FALL_DAMAGE_REDUCTION: i32 = 5;
/// Vanilla `Frog.getHeadRotSpeed`.
const HEAD_ROT_SPEED: f32 = 35.0;
/// Vanilla `Frog.getMaxHeadYRot`.
const MAX_HEAD_Y_ROT: f32 = 5.0;
const WATER_DRAG: f64 = 0.9;
/// Vanilla `Frog`'s `SmoothSwimmingMoveControl(this, 85, 10, 0.02F, 0.1F, true)`.
const FROG_MOVE_CONTROL: SmoothSwimmingMoveControl = SmoothSwimmingMoveControl {
    max_turn_x: 85,
    max_turn_y: 10,
    in_water_speed_modifier: 0.02,
    outside_water_speed_modifier: 0.1,
    apply_gravity: true,
};

#[entity_behavior(class = "Frog", spawn_rule)]
/// Vanilla frog.
pub struct FrogEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<FrogEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `FrogEntity`.
unsafe impl DowncastType for FrogEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/frog");
}

impl FrogEntity {
    /// Vanilla `Frog.checkFrogSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_frog_spawn_rules;

    fn check_frog_spawn_rules(
        _entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        _spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::FROGS_SPAWNABLE_ON)
            && <Self as Animal>::is_bright_enough_to_spawn(level, pos)
    }

    /// Creates a new frog at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a frog from persisted base entity state.
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
        let ageable_base = AgeableMobBase::new();
        let animal_base = AnimalBase::new();
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        *mob_base.navigation().lock() = PathNavigation::frog();
        let mut entity_data = FrogEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let frog = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], ai::sensors(), ai::activities(), None),
        };
        frog.set_pathfinding_malus(PathType::Water, 4.0);
        frog.set_pathfinding_malus(PathType::Trapdoor, -1.0);
        frog
    }

    const fn brain(&self) -> &Brain<Self> {
        &self.brain
    }

    /// Vanilla `FrogAi.initMemories`.
    fn init_memories(&self) {
        self.brain.memories().lock().set(
            &LONG_JUMP_COOLDOWN_TICKS,
            Some(ai::TIME_BETWEEN_LONG_JUMPS.sample()),
        );
    }

    /// Vanilla `Frog.getVariant`.
    #[must_use]
    pub fn variant(&self) -> FrogVariantRef {
        self.entity_data.lock().variant.get().value()
    }

    /// Vanilla `Frog.setVariant`.
    pub fn set_variant(&self, variant: FrogVariantRef) {
        self.entity_data
            .lock()
            .variant
            .set(RegistryReference::new(variant));
    }

    /// Vanilla `Frog.getTongueTarget`.
    #[must_use]
    pub fn tongue_target(&self) -> Option<SharedEntity> {
        let id = (*self.entity_data.lock().tongue_target.get())?;
        let id = i32::try_from(id).ok()?;
        self.level()?.get_entity_by_id(id)
    }

    /// Vanilla `Frog.setTongueTarget`.
    pub fn set_tongue_target(&self, target: &dyn Entity) {
        let id = u32::try_from(target.id()).ok();
        self.entity_data.lock().tongue_target.set(id);
    }

    /// Vanilla `Frog.eraseTongueTarget`.
    pub fn erase_tongue_target(&self) {
        self.entity_data.lock().tongue_target.set(None);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for FrogEntity {}

impl Entity for FrogEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn is_hard_collision_relevant(&self) -> bool {
        false
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
    }

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
        SoundSource::Neutral
    }

    fn is_pushed_by_fluid(&self) -> bool {
        false
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_FROG_STEP, 0.15, 1.0);
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(FROG_VARIANT) {
            self.set_variant(variant.value());
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert("variant", self.variant().key.to_string());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        if let Some(variant) = nbt.string("variant")
            && let Ok(key) = Identifier::from_str(variant.to_str().as_ref())
            && let Some(variant) = REGISTRY.frog_variants.by_key(&key)
        {
            self.set_variant(variant);
        }
    }
}

impl LivingEntity for FrogEntity {
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
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    /// Vanilla `Frog.calculateFallDamage`.
    fn calculate_fall_damage(&self, fall_distance: f64, damage_modifier: f32) -> i32 {
        let attributes = self.attributes().lock();
        let safe_fall_distance = attributes
            .get_value(vanilla_attributes::SAFE_FALL_DISTANCE)
            .unwrap_or(vanilla_attributes::SAFE_FALL_DISTANCE.default_value);
        let fall_damage_multiplier = attributes
            .get_value(vanilla_attributes::FALL_DAMAGE_MULTIPLIER)
            .unwrap_or(vanilla_attributes::FALL_DAMAGE_MULTIPLIER.default_value);
        drop(attributes);
        LivingEntityBase::calculate_fall_damage(
            fall_distance,
            damage_modifier,
            safe_fall_distance,
            fall_damage_multiplier,
        ) - FROG_FALL_DAMAGE_REDUCTION
    }

    /// Vanilla `Frog.travelInWater`.
    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.move_relative(self.get_speed(), input);
        let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
        self.set_velocity(self.velocity() * WATER_DRAG);
        result
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_FROG_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_FROG_DEATH)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for FrogEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob().age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob_mut()
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob_mut().baby.set(baby);
    }

    /// Vanilla `Frog.canBeABaby`: frogs grow from tadpoles instead.
    fn can_be_a_baby(&self) -> bool {
        false
    }
}

impl Animal for FrogEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::FROG_FOOD)
    }

    /// Vanilla `Frog.spawnChildFromBreeding`: no baby, the frog becomes
    /// pregnant and lays frogspawn later.
    fn spawn_child_from_breeding(&self, world: &Arc<World>, partner: &dyn Animal) {
        self.finalize_spawn_child_from_breeding(world, partner, None);
        self.brain.memories().lock().set(&IS_PREGNANT, Some(()));
    }

    fn play_eating_sound(&self) {
        let Some(world) = self.level() else {
            return;
        };
        world.play_sound_at(
            &sound_events::ENTITY_FROG_EAT,
            SoundSource::Neutral,
            self.position(),
            2.0,
            1.0,
            None,
        );
    }
}

impl Mob for FrogEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        FROG_MOVE_CONTROL.tick(self);
    }

    /// Vanilla `FrogLookControl.resetXRotOnTick`: keeps its pitch on the
    /// prey while its tongue is out.
    fn look_control_resets_x_rot(&self) -> bool {
        self.entity_data.lock().tongue_target.get().is_none()
    }

    fn head_rot_speed(&self) -> f32 {
        HEAD_ROT_SPEED
    }

    fn max_head_y_rot(&self) -> f32 {
        MAX_HEAD_Y_ROT
    }

    /// Vanilla `Frog.getTarget`: the brain's attack target.
    fn target(&self) -> Option<SharedEntity> {
        self.brain.memories().lock().get(&ATTACK_TARGET)
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            ai::update_activity(self);
        }
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_FROG_AMBIENT)
    }

    /// Vanilla `Frog.finalizeSpawn`.
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let biome = world.biome_at(self.block_position());
        let variant = biome.and_then(|biome| {
            let mut random = LegacyRandom::from_seed(rand::random());
            REGISTRY
                .frog_variants
                .select_spawn_variant(biome, &mut random)
        });
        if let Some(variant) = variant {
            self.set_variant(variant);
        }
        self.init_memories();
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for FrogEntity {}
