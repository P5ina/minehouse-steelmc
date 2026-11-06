use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::TropicalFishEntityData;
use steel_registry::{DyeColor, sound_events, vanilla_attributes};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::entity::ai::goal::FollowFlockLeaderGoal;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    AbstractFish, AbstractSchoolingFish, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    PathfinderMob, SchoolSpawnGroupData, SchoolState, SharedEntity, SpawnGroupData,
    WATER_ANIMAL_AMBIENT_SOUND_INTERVAL, handle_water_animal_air_supply, init_abstract_fish,
    sync_dirty_mob_effects, water_animal_experience_reward, water_animal_spawn_unobstructed,
};
use crate::physics::MoveResult;
use crate::world::World;

/// Vanilla `TropicalFish.Base`: the body model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TropicalFishBase {
    /// Small body.
    Small,
    /// Large body.
    Large,
}

impl TropicalFishBase {
    const fn id(self) -> i32 {
        match self {
            Self::Small => 0,
            Self::Large => 1,
        }
    }
}

/// Vanilla `TropicalFish.Pattern`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(missing_docs, reason = "variant names are the vanilla pattern names")]
pub enum TropicalFishPattern {
    Kob,
    Sunstreak,
    Snooper,
    Dasher,
    Brinely,
    Spotty,
    Flopper,
    Stripey,
    Glitter,
    Blockfish,
    Betty,
    Clayfish,
}

impl TropicalFishPattern {
    /// Vanilla `Pattern.values()` order.
    pub const VALUES: [Self; 12] = [
        Self::Kob,
        Self::Sunstreak,
        Self::Snooper,
        Self::Dasher,
        Self::Brinely,
        Self::Spotty,
        Self::Flopper,
        Self::Stripey,
        Self::Glitter,
        Self::Blockfish,
        Self::Betty,
        Self::Clayfish,
    ];

    /// The body model and the pattern index within it.
    #[must_use]
    pub const fn base_and_index(self) -> (TropicalFishBase, i32) {
        match self {
            Self::Kob => (TropicalFishBase::Small, 0),
            Self::Sunstreak => (TropicalFishBase::Small, 1),
            Self::Snooper => (TropicalFishBase::Small, 2),
            Self::Dasher => (TropicalFishBase::Small, 3),
            Self::Brinely => (TropicalFishBase::Small, 4),
            Self::Spotty => (TropicalFishBase::Small, 5),
            Self::Flopper => (TropicalFishBase::Large, 0),
            Self::Stripey => (TropicalFishBase::Large, 1),
            Self::Glitter => (TropicalFishBase::Large, 2),
            Self::Blockfish => (TropicalFishBase::Large, 3),
            Self::Betty => (TropicalFishBase::Large, 4),
            Self::Clayfish => (TropicalFishBase::Large, 5),
        }
    }

    /// Vanilla `Pattern.getPackedId`: `base.id | index << 8`.
    #[must_use]
    pub const fn packed_id(self) -> i32 {
        let (base, index) = self.base_and_index();
        base.id() | index << 8
    }

    /// Vanilla `Pattern.byId`, a sparse map falling back to `KOB`.
    #[must_use]
    pub fn by_id(packed_id: i32) -> Self {
        Self::VALUES
            .into_iter()
            .find(|pattern| pattern.packed_id() == packed_id)
            .unwrap_or(Self::Kob)
    }
}

/// Vanilla `TropicalFish.Variant`: pattern plus base and pattern colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TropicalFishVariant {
    /// The pattern.
    pub pattern: TropicalFishPattern,
    /// The body color.
    pub base_color: DyeColor,
    /// The pattern color.
    pub pattern_color: DyeColor,
}

impl TropicalFishVariant {
    /// Vanilla `TropicalFish.DEFAULT_VARIANT`.
    pub const DEFAULT: Self = Self::new(TropicalFishPattern::Kob, DyeColor::White, DyeColor::White);

    /// Creates a variant.
    #[must_use]
    pub const fn new(
        pattern: TropicalFishPattern,
        base_color: DyeColor,
        pattern_color: DyeColor,
    ) -> Self {
        Self {
            pattern,
            base_color,
            pattern_color,
        }
    }

    /// Vanilla `Variant(int packedId)`.
    #[must_use]
    pub fn from_packed(packed: i32) -> Self {
        Self::new(
            TropicalFishPattern::by_id(packed & 0xFFFF),
            DyeColor::by_id(packed >> 16 & 0xFF),
            DyeColor::by_id(packed >> 24 & 0xFF),
        )
    }

    /// Vanilla `TropicalFish.packVariant`.
    #[must_use]
    pub const fn packed_id(self) -> i32 {
        self.pattern.packed_id() & 0xFFFF
            | (self.base_color.id() & 0xFF) << 16
            | (self.pattern_color.id() & 0xFF) << 24
    }
}

/// Vanilla `TropicalFish.COMMON_VARIANTS`.
pub const COMMON_TROPICAL_FISH_VARIANTS: [TropicalFishVariant; 22] = {
    use DyeColor::{Blue, Cyan, Gray, LightBlue, Lime, Orange, Pink, Purple, Red, White, Yellow};
    use TropicalFishPattern::{
        Betty, Blockfish, Brinely, Clayfish, Dasher, Flopper, Glitter, Kob, Snooper, Spotty,
        Stripey, Sunstreak,
    };
    [
        TropicalFishVariant::new(Stripey, Orange, Gray),
        TropicalFishVariant::new(Flopper, Gray, Gray),
        TropicalFishVariant::new(Flopper, Gray, Blue),
        TropicalFishVariant::new(Clayfish, White, Gray),
        TropicalFishVariant::new(Sunstreak, Blue, Gray),
        TropicalFishVariant::new(Kob, Orange, White),
        TropicalFishVariant::new(Spotty, Pink, LightBlue),
        TropicalFishVariant::new(Blockfish, Purple, Yellow),
        TropicalFishVariant::new(Clayfish, White, Red),
        TropicalFishVariant::new(Spotty, White, Yellow),
        TropicalFishVariant::new(Glitter, White, Gray),
        TropicalFishVariant::new(Clayfish, White, Orange),
        TropicalFishVariant::new(Dasher, Cyan, Pink),
        TropicalFishVariant::new(Brinely, Lime, LightBlue),
        TropicalFishVariant::new(Betty, Red, White),
        TropicalFishVariant::new(Snooper, Gray, Red),
        TropicalFishVariant::new(Blockfish, Red, White),
        TropicalFishVariant::new(Flopper, White, Yellow),
        TropicalFishVariant::new(Kob, Red, White),
        TropicalFishVariant::new(Sunstreak, Gray, White),
        TropicalFishVariant::new(Dasher, Cyan, Yellow),
        TropicalFishVariant::new(Flopper, Yellow, Yellow),
    ]
};

/// Vanilla tropical fish: a schooling fish with a packed pattern and colors.
#[entity_behavior(class = "TropicalFish")]
pub struct TropicalFishEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<TropicalFishEntityData>,
    school: SyncMutex<SchoolState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TropicalFishEntity`.
unsafe impl DowncastType for TropicalFishEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/tropical_fish");
}

impl TropicalFishEntity {
    /// Creates a new tropical fish.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved tropical fish.
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

        let mut entity_data = TropicalFishEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        entity_data
            .id_type_variant
            .set(TropicalFishVariant::DEFAULT.packed_id());

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            school: SyncMutex::new(SchoolState::new()),
        }
    }

    /// The packed pattern and colors.
    #[must_use]
    pub fn variant(&self) -> TropicalFishVariant {
        TropicalFishVariant::from_packed(self.packed_variant())
    }

    fn packed_variant(&self) -> i32 {
        *self.entity_data.lock().id_type_variant.get()
    }

    fn set_packed_variant(&self, packed: i32) {
        self.entity_data.lock().id_type_variant.set(packed);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for TropicalFishEntity {}

impl Entity for TropicalFishEntity {
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
        nbt.insert("Variant", self.packed_variant());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fish(nbt);
        // Vanilla's `Variant.CODEC` round-trips through the unpacked record,
        // so unknown pattern or color ids are normalized.
        let variant = nbt.int("Variant").map_or(
            TropicalFishVariant::DEFAULT,
            TropicalFishVariant::from_packed,
        );
        self.set_packed_variant(variant.packed_id());
    }
}

impl LivingEntity for TropicalFishEntity {
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
        Some(&sound_events::ENTITY_TROPICAL_FISH_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_TROPICAL_FISH_DEATH)
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

impl Mob for TropicalFishEntity {
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
        Some(&sound_events::ENTITY_TROPICAL_FISH_AMBIENT)
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
        let group_data = self.finalize_spawn_schooling_fish(world, spawn_reason, group_data);
        let shared_variant = match &group_data {
            Some(SpawnGroupData::School(school)) => school.tropical_fish_variant,
            _ => None,
        };
        let (variant, group_data) = if let Some(packed) = shared_variant {
            (TropicalFishVariant::from_packed(packed), group_data)
        } else if rand::random::<f32>() < 0.9 {
            let variant = COMMON_TROPICAL_FISH_VARIANTS
                [rand::random_range(0..COMMON_TROPICAL_FISH_VARIANTS.len())];
            let group_data = SpawnGroupData::School(SchoolSpawnGroupData {
                leader: self.id(),
                tropical_fish_variant: Some(variant.packed_id()),
            });
            (variant, Some(group_data))
        } else {
            // TODO(spawn-clusters): vanilla also clears `isSchool` here, which
            // only feeds `isMaxGroupSizeReached`; Steel has no cluster hook.
            let colors = DyeColor::VALUES;
            let variant = TropicalFishVariant::new(
                TropicalFishPattern::VALUES
                    [rand::random_range(0..TropicalFishPattern::VALUES.len())],
                colors[rand::random_range(0..colors.len())],
                colors[rand::random_range(0..colors.len())],
            );
            (variant, group_data)
        };
        self.set_packed_variant(variant.packed_id());
        group_data
    }
}

impl PathfinderMob for TropicalFishEntity {}

impl AbstractFish for TropicalFishEntity {
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
        &sound_events::ENTITY_TROPICAL_FISH_FLOP
    }
}

impl AbstractSchoolingFish for TropicalFishEntity {
    fn school(&self) -> &SyncMutex<SchoolState> {
        &self.school
    }
}
