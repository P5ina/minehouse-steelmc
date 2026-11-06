use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::{CGameEvent, GameEventType};
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::PufferfishEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt, sound_events, vanilla_attributes, vanilla_damage_types,
    vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::GameType;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};

use crate::entity::ai::goal::{Goal, GoalControls};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    AbstractFish, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, MobEffectInstance,
    PathfinderMob, SharedEntity, WATER_ANIMAL_AMBIENT_SOUND_INTERVAL,
    handle_water_animal_air_supply, init_abstract_fish, sync_dirty_mob_effects,
    water_animal_experience_reward, water_animal_spawn_unobstructed,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

/// Vanilla `Pufferfish.STATE_SMALL`.
pub const PUFF_STATE_SMALL: i32 = 0;
/// Vanilla `Pufferfish.STATE_MID`.
pub const PUFF_STATE_MID: i32 = 1;
/// Vanilla `Pufferfish.STATE_FULL`.
pub const PUFF_STATE_FULL: i32 = 2;

/// Vanilla `Pufferfish.inflateCounter` and `deflateTimer`.
#[derive(Debug, Default)]
struct PuffTimers {
    inflate_counter: i32,
    deflate_timer: i32,
}

/// Vanilla pufferfish: puffs up near scary mobs and stings what it touches.
#[entity_behavior(class = "Pufferfish")]
pub struct PufferfishEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<PufferfishEntityData>,
    puff_timers: SyncMutex<PuffTimers>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PufferfishEntity`.
unsafe impl DowncastType for PufferfishEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/pufferfish");
}

/// Vanilla `Pufferfish.TARGETING_CONDITIONS` with its `SCARY_MOB` selector.
fn targeting_conditions() -> TargetingConditions {
    TargetingConditions::for_non_combat()
        .ignore_invisibility_testing()
        .ignore_line_of_sight()
        .selector(|target, _world| {
            if target
                .as_player()
                .is_some_and(|player| player.game_mode() == GameType::Creative)
            {
                return false;
            }
            !REGISTRY.entity_types.is_in_tag(
                target.entity_type(),
                &EntityTypeTag::NOT_SCARY_FOR_PUFFERFISH,
            )
        })
}

/// Vanilla `Pufferfish.getScale`.
const fn puff_scale(state: i32) -> f32 {
    match state {
        PUFF_STATE_SMALL => 0.5,
        PUFF_STATE_MID => 0.7,
        _ => 1.0,
    }
}

impl PufferfishEntity {
    /// Creates a new pufferfish.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved pufferfish.
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
        init_abstract_fish(&mob_base, |health| {
            living_base
                .attributes()
                .lock()
                .set_base_value(vanilla_attributes::MAX_HEALTH, health);
        });
        mob_base
            .goal_selector()
            .lock()
            .add_goal(1, PufferfishPuffGoal);

        let mut entity_data = PufferfishEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let pufferfish = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            puff_timers: SyncMutex::new(PuffTimers::default()),
        };
        pufferfish.refresh_dimensions();
        pufferfish
    }

    /// The current puff state: 0 small, 1 mid, 2 full.
    #[must_use]
    pub fn puff_state(&self) -> i32 {
        *self.entity_data.lock().puff_state.get()
    }

    /// Sets the puff state and refreshes the bounding box, as vanilla does
    /// from `onSyncedDataUpdated`.
    pub fn set_puff_state(&self, state: i32) {
        self.entity_data.lock().puff_state.set(state);
        self.refresh_dimensions();
    }

    /// Vanilla `Pufferfish.tick` before `super.tick()`.
    fn tick_puff_state(&self) {
        if !LivingEntity::is_alive(self) || !self.is_effective_ai() {
            return;
        }
        let mut timers = self.puff_timers.lock();
        let state = self.puff_state();
        if timers.inflate_counter > 0 {
            if state == PUFF_STATE_SMALL {
                self.make_sound(Some(&sound_events::ENTITY_PUFFER_FISH_BLOW_UP));
                self.set_puff_state(PUFF_STATE_MID);
            } else if timers.inflate_counter > 40 && state == PUFF_STATE_MID {
                self.make_sound(Some(&sound_events::ENTITY_PUFFER_FISH_BLOW_UP));
                self.set_puff_state(PUFF_STATE_FULL);
            }
            timers.inflate_counter += 1;
        } else if state != PUFF_STATE_SMALL {
            if timers.deflate_timer > 60 && state == PUFF_STATE_FULL {
                self.make_sound(Some(&sound_events::ENTITY_PUFFER_FISH_BLOW_OUT));
                self.set_puff_state(PUFF_STATE_MID);
            } else if timers.deflate_timer > 100 && state == PUFF_STATE_MID {
                self.make_sound(Some(&sound_events::ENTITY_PUFFER_FISH_BLOW_OUT));
                self.set_puff_state(PUFF_STATE_SMALL);
            }
            timers.deflate_timer += 1;
        }
    }

    /// Vanilla `Pufferfish.aiStep` after `super.aiStep()`: stings touching mobs.
    fn sting_touching_mobs(&self, entity: &SharedEntity) {
        if !LivingEntity::is_alive(self) || self.puff_state() <= 0 {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let conditions = targeting_conditions();
        let touching =
            world.get_entities_in_aabb_matching(&self.bounding_box().inflate(0.3), |other| {
                other.as_mob().is_some()
                    && other
                        .as_living_entity()
                        .is_some_and(|target| conditions.test(&world, Some(self), target))
            });
        for mob in touching {
            if mob.as_living_entity().is_some_and(LivingEntity::is_alive) {
                self.touch(&world, entity, &mob);
            }
        }
    }

    /// Vanilla `Pufferfish.touch`.
    fn touch(&self, world: &World, this: &SharedEntity, mob: &SharedEntity) {
        // TODO(teams): vanilla checks `doTeamsAllowDamage`; Steel teams have
        // no friendly-fire flag yet, so damage is always allowed.
        let puff_state = self.puff_state();
        if mob.hurt(
            world,
            &Self::mob_attack_source(this),
            (1 + puff_state) as f32,
        ) {
            if let Some(living) = mob.as_living_entity() {
                living.add_mob_effect_with_source(
                    MobEffectInstance::with_duration(
                        vanilla_mob_effects::POISON,
                        60 * puff_state,
                        0,
                    ),
                    Some(self as &dyn Entity),
                );
            }
            self.play_sound(&sound_events::ENTITY_PUFFER_FISH_STING, 1.0, 1.0);
        }
    }

    /// Vanilla `damageSources().mobAttack(this)`.
    fn mob_attack_source(this: &SharedEntity) -> DamageSource {
        DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
            .with_causing_entity(Arc::clone(this))
            .with_direct_entity(Arc::clone(this))
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for PufferfishEntity {}

impl Entity for PufferfishEntity {
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
        self.tick_puff_state();
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
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
            .scale(puff_scale(self.puff_state()))
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

    fn player_touch(self: Arc<Self>, player: &Arc<Player>) {
        // TODO(teams): vanilla checks `doTeamsAllowDamage` here as well.
        let puff_state = self.puff_state();
        if puff_state <= 0 {
            return;
        }
        let Some(world) = player.level() else {
            return;
        };
        let this = Arc::clone(&self).into_shared_entity();
        if !player.hurt(
            &world,
            &Self::mob_attack_source(&this),
            (1 + puff_state) as f32,
        ) {
            return;
        }
        if !self.is_silent() {
            player.send_packet(CGameEvent {
                event: GameEventType::PufferFishSting,
                data: 0.0,
            });
        }
        player.add_mob_effect_with_source(
            MobEffectInstance::with_duration(vanilla_mob_effects::POISON, 60 * puff_state, 0),
            Some(&*self as &dyn Entity),
        );
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_fish(nbt);
        nbt.insert("PuffState", self.puff_state());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fish(nbt);
        self.set_puff_state(
            nbt.int("PuffState")
                .unwrap_or(PUFF_STATE_SMALL)
                .min(PUFF_STATE_FULL),
        );
    }
}

impl LivingEntity for PufferfishEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
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
        Some(&sound_events::ENTITY_PUFFER_FISH_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PUFFER_FISH_DEATH)
    }

    fn base_experience_reward(&self) -> i32 {
        water_animal_experience_reward()
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

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = self.fish_ai_step(entity);
        self.sting_touching_mobs(entity);
        result
    }
}

impl Mob for PufferfishEntity {
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
        self.tick_fish_move_control();
    }

    fn ambient_sound_interval(&self) -> i32 {
        WATER_ANIMAL_AMBIENT_SOUND_INTERVAL
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

    fn check_spawn_obstruction(&self, world: &Arc<World>) -> bool {
        water_animal_spawn_unobstructed(self, world)
    }
}

impl PathfinderMob for PufferfishEntity {}

impl AbstractFish for PufferfishEntity {
    fn is_from_bucket(&self) -> bool {
        *self.entity_data.lock().abstract_fish().from_bucket.get()
    }

    fn set_from_bucket(&self, from_bucket: bool) {
        self.entity_data
            .lock()
            .abstract_fish_mut()
            .from_bucket
            .set(from_bucket);
    }

    fn flop_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_PUFFER_FISH_FLOP
    }
}

/// Vanilla `Pufferfish.PufferfishPuffGoal`: inflates while a scary living
/// entity is within two blocks.
struct PufferfishPuffGoal;

impl Goal for PufferfishPuffGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fish) = mob.downcast_ref::<PufferfishEntity>() else {
            return false;
        };
        let Some(world) = fish.level() else {
            return false;
        };
        let conditions = targeting_conditions();
        world.has_entity_in_aabb_matching(&fish.bounding_box().inflate(2.0), |other| {
            other
                .as_living_entity()
                .is_some_and(|target| conditions.test(&world, Some(fish), target))
        })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fish) = mob.downcast_ref::<PufferfishEntity>() {
            *fish.puff_timers.lock() = PuffTimers {
                inflate_counter: 1,
                deflate_timer: 0,
            };
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fish) = mob.downcast_ref::<PufferfishEntity>() {
            fish.puff_timers.lock().inflate_counter = 0;
        }
    }
}
