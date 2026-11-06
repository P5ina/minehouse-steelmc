use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::SalmonEntityData;
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
use crate::world::{ServerLevelAccessor, World};

/// Vanilla salmon: a schooling fish in three sizes.
#[entity_behavior(class = "Salmon")]
pub struct SalmonEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<SalmonEntityData>,
    school: SyncMutex<SchoolState>,
}

/// Vanilla `Salmon.Variant`: the size, which scales the bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalmonVariant {
    /// `small`: half-size hitbox.
    Small,
    /// `medium`: default hitbox.
    Medium,
    /// `large`: one and a half times the hitbox.
    Large,
}

impl SalmonVariant {
    const fn id(self) -> i32 {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
        }
    }

    /// Vanilla `ByIdMap.continuous` with the `CLAMP` strategy.
    const fn by_id(id: i32) -> Self {
        match id {
            i32::MIN..=0 => Self::Small,
            1 => Self::Medium,
            _ => Self::Large,
        }
    }

    const fn serialized_name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn from_serialized_name(name: &str) -> Option<Self> {
        match name {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    const fn bounding_box_scale(self) -> f32 {
        match self {
            Self::Small => 0.5,
            Self::Medium => 1.0,
            Self::Large => 1.5,
        }
    }

    /// The weighted spawn roll from `Salmon.finalizeSpawn`: 30 / 50 / 15.
    fn random() -> Self {
        match rand::random_range(0..95) {
            0..30 => Self::Small,
            30..80 => Self::Medium,
            _ => Self::Large,
        }
    }
}

const SALMON_MAX_SCHOOL_SIZE: i32 = 5;

// SAFETY: This key is owned by Steel and uniquely identifies `SalmonEntity`.
unsafe impl DowncastType for SalmonEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/salmon");
}

impl SalmonEntity {
    /// Creates a new salmon.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved salmon.
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

        let mut entity_data = SalmonEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let salmon = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            school: SyncMutex::new(SchoolState::new()),
        };
        salmon.refresh_dimensions();
        salmon
    }

    /// The salmon's size.
    #[must_use]
    pub fn variant(&self) -> SalmonVariant {
        SalmonVariant::by_id(*self.entity_data.lock().variant_type.get())
    }

    /// Sets the size and refreshes the bounding box.
    pub fn set_variant(&self, variant: SalmonVariant) {
        self.entity_data.lock().variant_type.set(variant.id());
        self.refresh_dimensions();
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for SalmonEntity {}

impl Entity for SalmonEntity {
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
            .scale(self.variant().bounding_box_scale())
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
        nbt.insert("type", self.variant().serialized_name());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fish(nbt);
        let variant = nbt
            .string("type")
            .and_then(|name| SalmonVariant::from_serialized_name(&name.to_string()))
            .unwrap_or(SalmonVariant::Medium);
        self.set_variant(variant);
    }
}

impl LivingEntity for SalmonEntity {
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
        Some(&sound_events::ENTITY_SALMON_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SALMON_DEATH)
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

impl Mob for SalmonEntity {
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
        Some(&sound_events::ENTITY_SALMON_AMBIENT)
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

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        water_animal_spawn_unobstructed(self, level)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_variant(SalmonVariant::random());
        self.finalize_spawn_schooling_fish(world, spawn_reason, group_data)
    }
}

impl PathfinderMob for SalmonEntity {}

impl AbstractFish for SalmonEntity {
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
        &sound_events::ENTITY_SALMON_FLOP
    }
}

impl AbstractSchoolingFish for SalmonEntity {
    fn school(&self) -> &SyncMutex<SchoolState> {
        &self.school
    }

    fn max_school_size(&self) -> i32 {
        SALMON_MAX_SCHOOL_SIZE
    }
}
