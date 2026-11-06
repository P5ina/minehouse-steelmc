use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::CodEntityData;
use steel_registry::{sound_events, vanilla_attributes};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::entity::ai::goal::FollowFlockLeaderGoal;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    AbstractFish, AbstractSchoolingFish, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    PathfinderMob, SchoolState, SharedEntity, SpawnGroupData, WATER_ANIMAL_AMBIENT_SOUND_INTERVAL,
    handle_water_animal_air_supply, init_abstract_fish, sync_dirty_mob_effects,
    water_animal_experience_reward, water_animal_spawn_unobstructed,
};
use crate::physics::MoveResult;
use crate::world::World;

/// Vanilla cod: a schooling fish.
#[entity_behavior(class = "Cod")]
pub struct CodEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<CodEntityData>,
    school: SyncMutex<SchoolState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `CodEntity`.
unsafe impl DowncastType for CodEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/cod");
}

impl CodEntity {
    /// Creates a new cod.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved cod.
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
            .add_goal(5, FollowFlockLeaderGoal::new());

        let mut entity_data = CodEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            school: SyncMutex::new(SchoolState::new()),
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for CodEntity {}

impl Entity for CodEntity {
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
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        self.tick_school();
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
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fish(nbt);
    }
}

impl LivingEntity for CodEntity {
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
        Some(&sound_events::ENTITY_COD_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_COD_DEATH)
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
        self.fish_ai_step(entity)
    }
}

impl Mob for CodEntity {
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

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_COD_AMBIENT)
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

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_schooling_fish(world, spawn_reason, group_data)
    }
}

impl PathfinderMob for CodEntity {}

impl AbstractFish for CodEntity {
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
        &sound_events::ENTITY_COD_FLOP
    }
}

impl AbstractSchoolingFish for CodEntity {
    fn school(&self) -> &SyncMutex<SchoolState> {
        &self.school
    }
}
