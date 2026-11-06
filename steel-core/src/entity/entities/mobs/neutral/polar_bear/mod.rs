//! Vanilla polar bear: a neutral animal that stands up and roars before it
//! attacks, defends its cubs and hunts foxes.

mod goals;

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entities;
use steel_registry::{sound_events, vanilla_attributes};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, Identifier};

use goals::{PolarBearAttackPlayersGoal, PolarBearHurtByTargetGoal, PolarBearMeleeAttackGoal};

use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    FloatGoal, FollowParentGoal, LookAtPlayerGoal, NearestAttackableTargetGoal, PanicGoal,
    RandomLookAroundGoal, RandomStrollGoal, ResetUniversalAngerTargetGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::neutral_mob::{NeutralMob, NeutralMobState};
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, DefaultEntityPush, Entity,
    EntityBase, EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};
use steel_registry::vanilla_entity_data::PolarBearEntityData;

const POLAR_BEAR_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.625, 0.0)];
const POLAR_BEAR_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.7,
    0.7,
    0.343_75,
    EntityAttachments::new(&POLAR_BEAR_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `PERSISTENT_ANGER_TIME`: 20 to 39 seconds.
const PERSISTENT_ANGER_TICKS_MIN: i64 = 20 * 20;
const PERSISTENT_ANGER_TICKS_MAX: i64 = 39 * 20;
/// Ticks between two warning roars.
const WARNING_SOUND_COOLDOWN: i32 = 40;
const POLAR_BEAR_WATER_SLOW_DOWN: f32 = 0.98;

#[entity_behavior(class = "PolarBear", spawn_rule)]
/// Vanilla polar bear.
pub struct PolarBearEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    anger: NeutralMobState,
    warning_sound_ticks: AtomicI32,
    entity_data: SyncMutex<PolarBearEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PolarBearEntity`.
unsafe impl DowncastType for PolarBearEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/polar_bear");
}

/// Vanilla `PolarBear.checkPolarBearSpawnRules`: in biomes tagged
/// `polar_bears_spawn_on_alternate_blocks` bears spawn on ice instead of the
/// usual animal blocks.
fn check_polar_bear_spawn_rules(
    entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    random: &mut dyn Rng,
) -> bool {
    let alternate = level
        .biome(pos)
        .is_some_and(|biome| biome.has_tag(&BiomeTag::POLAR_BEARS_SPAWN_ON_ALTERNATE_BLOCKS));
    if !alternate {
        return <PolarBearEntity as Animal>::check_animal_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
            random,
        );
    }
    <PolarBearEntity as Animal>::is_bright_enough_to_spawn(level, pos)
        && level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::POLAR_BEARS_SPAWNABLE_ON_ALTERNATE)
}

fn panic_causes(mob: &dyn PathfinderMob) -> Identifier {
    if mob.as_ageable_mob().is_some_and(AgeableMob::is_baby) {
        DamageTypeTag::PANIC_CAUSES
    } else {
        DamageTypeTag::PANIC_ENVIRONMENTAL_CAUSES
    }
}

impl PolarBearEntity {
    /// Vanilla `SpawnPlacements` predicate: `PolarBear.checkPolarBearSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_polar_bear_spawn_rules;

    /// Creates a new polar bear at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a polar bear from persisted base entity state.
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
        let mut entity_data = PolarBearEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, PolarBearMeleeAttackGoal::new());
            goal_selector.add_goal(1, PanicGoal::new(2.0).with_panic_causes(panic_causes));
            goal_selector.add_goal(4, FollowParentGoal::new(1.25));
            goal_selector.add_goal(5, RandomStrollGoal::new(1.0));
            goal_selector.add_goal(6, LookAtPlayerGoal::new(6.0));
            goal_selector.add_goal(7, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, PolarBearHurtByTargetGoal::new());
            target_selector.add_goal(2, PolarBearAttackPlayersGoal::new());
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new_for_players_with_interval(
                    10,
                    true,
                    false,
                    |_, _| true,
                )
                .with_mob_selector(|mob, target, world| {
                    mob.as_neutral_mob()
                        .is_some_and(|bear| bear.is_angry_at(target, world))
                }),
            );
            target_selector.add_goal(
                4,
                NearestAttackableTargetGoal::new_with_interval(10, true, true, |target, _| {
                    target.entity_type() == &vanilla_entities::FOX
                })
                .with_mob_selector(|mob, _, _| {
                    !mob.as_ageable_mob().is_some_and(AgeableMob::is_baby)
                }),
            );
            target_selector.add_goal(5, ResetUniversalAngerTargetGoal::new(false));
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            anger: NeutralMobState::new(),
            warning_sound_ticks: AtomicI32::new(0),
            entity_data: SyncMutex::new(entity_data),
        }
    }

    /// Whether the bear stands on its hind legs, as it does just before an attack.
    #[must_use]
    pub fn is_standing(&self) -> bool {
        *self.entity_data.lock().standing.get()
    }

    /// Sets whether the bear stands on its hind legs.
    pub fn set_standing(&self, standing: bool) {
        self.entity_data.lock().standing.set(standing);
    }

    /// Vanilla `PolarBear.playWarningSound`: roars at most once every two seconds.
    pub(crate) fn play_warning_sound(&self) {
        if self.warning_sound_ticks.load(Ordering::Relaxed) <= 0 {
            self.make_sound(Some(&sound_events::ENTITY_POLAR_BEAR_WARNING));
            self.warning_sound_ticks
                .store(WARNING_SOUND_COOLDOWN, Ordering::Relaxed);
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl NeutralMob for PolarBearEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(rand::random_range(
            PERSISTENT_ANGER_TICKS_MIN..=PERSISTENT_ANGER_TICKS_MAX,
        ));
    }
}

impl DefaultEntityPush for PolarBearEntity {}

impl Entity for PolarBearEntity {
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
        let warning_sound_ticks = self.warning_sound_ticks.load(Ordering::Relaxed);
        if warning_sound_ticks > 0 {
            self.warning_sound_ticks
                .store(warning_sound_ticks - 1, Ordering::Relaxed);
        }
        if let Some(world) = self.level() {
            self.update_persistent_anger(&world, true);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            POLAR_BEAR_BABY_DIMENSIONS.scale(scale)
        } else if self.entity_type.fixed {
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
        SoundSource::Neutral
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_POLAR_BEAR_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        self.add_persistent_anger_save_data(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.read_persistent_anger_save_data(nbt);
    }
}

impl LivingEntity for PolarBearEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
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

    fn get_water_slow_down(&self) -> f32 {
        POLAR_BEAR_WATER_SLOW_DOWN
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_POLAR_BEAR_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_POLAR_BEAR_DEATH)
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

impl AgeableMob for PolarBearEntity {
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

    fn age_boundary_changed(&self, _baby: bool) {
        self.refresh_dimensions();
    }
}

impl Animal for PolarBearEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }
}

impl Mob for PolarBearEntity {
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
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if AgeableMob::is_baby(self) {
            Some(&sound_events::ENTITY_POLAR_BEAR_AMBIENT_BABY)
        } else {
            Some(&sound_events::ENTITY_POLAR_BEAR_AMBIENT)
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = group_data.unwrap_or(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_baby_spawn_chance(1.0),
        ));
        self.finalize_spawn_ageable_mob(world, spawn_reason, Some(group_data))
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        Animal::mob_interact_animal(self, player, hand)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for PolarBearEntity {}

#[cfg(test)]
mod tests;
