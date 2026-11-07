//! Vanilla tadpole (`Tadpole`): hatches from frogspawn and grows into a frog.

mod ai;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::particle_type::ParticleData;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::TadpoleEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_particle_types::{PAUSE_MOB_GROWTH, RESET_MOB_GROWTH};
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_entities,
    vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::entity::EntityEventSource as _;
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::smooth_swimming::{
    SmoothSwimmingMoveControl, tick_smooth_swimming_look_control,
};
use crate::entity::damage::DamageSource;
use crate::entity::{
    AbstractFish, AgeableMobBase, ConversionParams, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity,
    WATER_ANIMAL_AMBIENT_SOUND_INTERVAL, handle_water_animal_air_supply, init_abstract_fish,
    sync_dirty_mob_effects, water_animal_experience_reward, water_animal_spawn_unobstructed,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Tadpole.ticksToBeFrog`.
pub(crate) const TICKS_TO_BE_FROG: i32 = 24_000;
/// Vanilla `Tadpole.createAttributes` max health.
const TADPOLE_MAX_HEALTH: f64 = 6.0;
/// Vanilla `Tadpole`'s age lock cooldown after a golden dandelion.
const AGE_LOCK_PARTICLE_TICKS: i32 = 40;
/// Vanilla `SmoothSwimmingLookControl(this, 10)`.
const LOOK_MAX_Y_ROT_FROM_CENTER: i32 = 10;
/// Vanilla `Tadpole`'s `SmoothSwimmingMoveControl(this, 85, 10, 0.02F, 0.1F, true)`.
const TADPOLE_MOVE_CONTROL: SmoothSwimmingMoveControl = SmoothSwimmingMoveControl {
    max_turn_x: 85,
    max_turn_y: 10,
    in_water_speed_modifier: 0.02,
    outside_water_speed_modifier: 0.1,
    apply_gravity: true,
};

/// The tadpole's own growth counter; it is not an `AgeableMob`.
#[derive(Debug, Clone, Copy, Default)]
struct TadpoleAge {
    age: i32,
    age_lock_particle_timer: i32,
}

/// Vanilla tadpole.
#[entity_behavior(class = "Tadpole")]
pub struct TadpoleEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<TadpoleEntityData>,
    age: SyncMutex<TadpoleAge>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TadpoleEntity`.
unsafe impl DowncastType for TadpoleEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/tadpole");
}

impl TadpoleEntity {
    /// Creates a new tadpole.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved tadpole.
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
        init_abstract_fish(&mob_base, |_fish_max_health| {
            living_base
                .attributes()
                .lock()
                .set_base_value(vanilla_attributes::MAX_HEALTH, TADPOLE_MAX_HEALTH);
        });
        *mob_base.navigation().lock() = PathNavigation::water_bound(false);

        let mut entity_data = TadpoleEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            age: SyncMutex::new(TadpoleAge::default()),
            brain: Brain::new(&[], ai::sensors(), ai::activities(), None),
        }
    }

    const fn brain(&self) -> &Brain<Self> {
        &self.brain
    }

    /// Vanilla `Tadpole.getAge`: ticks lived, a frog at [`TICKS_TO_BE_FROG`].
    #[must_use]
    pub fn age(&self) -> i32 {
        self.age.lock().age
    }

    /// Vanilla `Tadpole.isAgeLocked`.
    #[must_use]
    pub fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().age_locked.get()
    }

    fn set_age_locked(&self, locked: bool) {
        self.entity_data.lock().age_locked.set(locked);
    }

    /// Vanilla `Tadpole.setAge`: grows into a frog once old enough.
    pub fn set_age(&self, age: i32) {
        self.age.lock().age = age;
        if age >= TICKS_TO_BE_FROG {
            self.grow_up();
        }
    }

    /// Vanilla `Tadpole.ageUp(int)`.
    fn age_up_by_seconds(&self, seconds: i32) {
        self.set_age(self.age().saturating_add(seconds.saturating_mul(20)));
    }

    /// Vanilla `Tadpole.getTicksLeftUntilAdult`.
    fn ticks_left_until_adult(&self) -> i32 {
        (TICKS_TO_BE_FROG - self.age()).max(0)
    }

    /// Vanilla `Tadpole.ageUp()`: becomes a frog in place.
    fn grow_up(&self) {
        let Some(level) = self.level() else {
            return;
        };
        let dimensions = self.dimensions_for_pose(self.pose());
        let _ = self.convert_to(
            &vanilla_entities::FROG,
            ConversionParams::single(false, false),
            Some(&|frog: &dyn Mob| {
                let _ = frog.finalize_spawn(&level, EntitySpawnReason::Conversion, None);
                frog.set_persistence_required();
                frog.fudge_position_after_size_change(dimensions);
                self.play_sound(&sound_events::ENTITY_TADPOLE_GROW_UP, 0.15, 1.0);
            }),
        );
    }

    fn is_food(item: &ItemStack) -> bool {
        ai::is_frog_food(item)
    }

    /// Vanilla `Tadpole.feed`. The happy villager particle vanilla adds here
    /// is client-only.
    fn feed(&self, player: &Player, hand: InteractionHand) {
        Mob::use_player_item(self, player, hand);
        self.age_up_by_seconds(AgeableMobBase::get_speed_up_seconds_when_feeding(
            self.ticks_left_until_adult(),
        ));
    }

    /// Vanilla `AgeableMob.canUseGoldenDandelion` for a mob that is always a
    /// baby.
    fn can_use_golden_dandelion(&self, item_is_golden_dandelion: bool) -> bool {
        item_is_golden_dandelion
            && self.age.lock().age_lock_particle_timer == 0
            && !REGISTRY
                .entity_types
                .is_in_tag(self.entity_type, &EntityTypeTag::CANNOT_BE_AGE_LOCKED)
    }

    /// Vanilla `AgeableMob.setAgeLocked` with `Tadpole.setAgeLockedData`.
    fn toggle_age_lock(&self, player: &Player, hand: InteractionHand) {
        self.set_age_locked(!self.is_age_locked());
        self.set_age(0);
        self.age.lock().age_lock_particle_timer = AGE_LOCK_PARTICLE_TICKS;
        Mob::use_player_item(self, player, hand);
        let locked = self.is_age_locked();
        if locked {
            self.set_persistence_required();
        }
        if let Some(world) = self.level() {
            let sound = if locked {
                &sound_events::ITEM_GOLDEN_DANDELION_USE
            } else {
                &sound_events::ITEM_GOLDEN_DANDELION_UNUSE
            };
            world.play_sound(
                sound,
                SoundSource::Players,
                self.block_position(),
                1.0,
                1.0,
                None,
            );
        }
    }

    /// Vanilla `AgeableMob.makeAgeLockedParticle`.
    fn tick_age_lock_particles(&self) {
        let timer = {
            let mut age = self.age.lock();
            let timer = age.age_lock_particle_timer;
            if timer > 0 {
                age.age_lock_particle_timer = timer - 1;
            }
            timer
        };
        if timer <= 0 || timer % 2 != 0 {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let locked = self.is_age_locked();
        let dimensions = self.dimensions_for_pose(self.pose());
        let width = f64::from(dimensions.width);
        let height = f64::from(dimensions.height);
        let spread = || 2.0 * rand::random::<f64>() - 1.0;
        let position = self.position();
        let y_offset = if locked { f64::from(0.2_f32) } else { 0.0 };
        let particle = if locked {
            &PAUSE_MOB_GROWTH
        } else {
            &RESET_MOB_GROWTH
        };
        world.send_particles(
            ParticleData::simple(particle),
            DVec3::new(
                position.x + width * spread(),
                position.y + height * spread() * 0.2 + height + y_offset,
                position.z + width * spread(),
            ),
            1,
            DVec3::ZERO,
            0.0,
        );
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for TadpoleEntity {}

impl Entity for TadpoleEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
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
        let air_supply = self.air_supply();
        Mob::base_tick_mob(self);
        handle_water_animal_air_supply(self, air_supply);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(LivingEntity::get_scale(self))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn swim_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_FISH_SWIM
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {}

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_fish(nbt);
        nbt.insert("Brain", self.brain.save());
        nbt.insert("Age", self.age());
        nbt.insert("AgeLocked", i8::from(self.is_age_locked()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fish(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        self.set_age(nbt.int("Age").unwrap_or(0));
        self.set_age_locked(nbt.byte("AgeLocked").is_some_and(|value| value != 0));
    }
}

impl LivingEntity for TadpoleEntity {
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
        let clamped = health.clamp(0.0, self.get_max_health());
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_TADPOLE_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_TADPOLE_DEATH)
    }

    fn base_experience_reward(&self) -> i32 {
        water_animal_experience_reward()
    }

    /// Vanilla `Tadpole.shouldDropExperience`.
    fn should_drop_experience(&self) -> bool {
        false
    }

    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.fish_travel_in_water(entity, input)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    /// Vanilla `Tadpole.aiStep`: the fish step, then a tick of growth.
    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = self.fish_ai_step(entity);
        if !self.is_age_locked() {
            self.set_age(self.age().saturating_add(1));
        }
        self.tick_age_lock_particles();
        result
    }
}

impl Mob for TadpoleEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        TADPOLE_MOVE_CONTROL.tick(self);
    }

    fn tick_look_control(&self) {
        tick_smooth_swimming_look_control(self, LOOK_MAX_Y_ROT_FROM_CENTER);
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            ai::update_activity(self);
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        WATER_ANIMAL_AMBIENT_SOUND_INTERVAL
    }

    /// Vanilla `Tadpole.mobInteract`. Bucket pickup comes with the shared
    /// `Bucketable` layer (see [`AbstractFish`]).
    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let (is_food, is_golden_dandelion) = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            (
                Self::is_food(item_stack),
                item_stack.is(&vanilla_items::GOLDEN_DANDELION),
            )
        };
        if is_food && !self.is_age_locked() {
            self.feed(player, hand);
            return InteractionResult::Success;
        }
        if self.can_use_golden_dandelion(is_golden_dandelion) {
            self.toggle_age_lock(player, hand);
            return InteractionResult::Success;
        }
        InteractionResult::Pass
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        self.fish_remove_when_far_away()
    }

    fn requires_custom_persistence(&self) -> bool {
        self.fish_requires_custom_persistence()
    }

    fn mob_can_be_leashed(&self) -> bool {
        false
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        water_animal_spawn_unobstructed(self, level)
    }
}

impl PathfinderMob for TadpoleEntity {}

impl AbstractFish for TadpoleEntity {
    /// Vanilla `Tadpole.fromBucket`: always, so tadpoles never despawn.
    fn is_from_bucket(&self) -> bool {
        true
    }

    /// Vanilla `Tadpole.setFromBucket` ignores the flag.
    fn set_from_bucket(&self, _from_bucket: bool) {}

    fn flop_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_TADPOLE_FLOP
    }
}
