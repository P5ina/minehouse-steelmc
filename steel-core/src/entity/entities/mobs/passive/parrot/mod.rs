//! Vanilla parrot: a jungle bird that flies, imitates nearby monsters, and
//! once tamed with seeds perches on its owner's shoulders.

mod goals;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::PARROT_VARIANT;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::entity_variant::ParrotVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::ParrotEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_damage_types,
    vanilla_entities, vanilla_mob_effects,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::{Difficulty, GameType, InteractionHand};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};
use uuid::Uuid;

use goals::{LandOnOwnersShoulderGoal, ParrotWanderGoal};

use crate::behavior::InteractionResult;
use crate::entity::ai::flying_move::FlyingMoveControl;
use crate::entity::ai::goal::{
    FloatGoal, FollowMobGoal, FollowOwnerGoal, LookAtPlayerGoal, SitWhenOrderedToGoal,
    TamableAnimalPanicGoal,
};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::pushing::EntityPush;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, MobEffectInstance, PathfinderMob, RemovalReason, SharedEntity,
    SpawnGroupData, SpawnRule, TamableAnimal, TamableAnimalBase, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const TAG_VARIANT: &str = "Variant";
/// Vanilla `FlyingMoveControl(this, 10, false)`.
const PARROT_MOVE_CONTROL: FlyingMoveControl = FlyingMoveControl {
    max_turn: 10,
    hovers_in_place: false,
};
/// Vanilla `ShoulderRidingEntity.RIDE_COOLDOWN`.
const RIDE_COOLDOWN: i32 = 100;
/// One tick in this many a parrot tries to imitate a nearby mob.
const IMITATE_CHANCE: u32 = 400;
/// Range around the parrot that it listens for mobs to imitate.
const IMITATE_RANGE: f64 = 20.0;
const IMITATE_VOLUME: f32 = 0.7;
/// One ambient sound in this many is an imitation instead of a squawk.
const AMBIENT_IMITATE_CHANCE: u32 = 1000;
/// One seed in this many tames the parrot.
const TAME_CHANCE: u32 = 10;
/// Vanilla `MobEffects.POISON` lasts this long after a cookie.
const COOKIE_POISON_TICKS: i32 = 900;
const STEP_VOLUME: f32 = 0.15;
const FLY_VOLUME: f32 = 0.15;
const FLAP_SPEED_STEP: f32 = 0.3;
const FLAPPING_DECAY: f32 = 0.9;
const FALL_DRAG_Y: f64 = 0.6;
const FOLLOW_OWNER_SPEED: f64 = 1.0;
const FOLLOW_OWNER_START_DISTANCE: f32 = 5.0;
const FOLLOW_OWNER_STOP_DISTANCE: f32 = 1.0;
const FOLLOW_MOB_STOP_DISTANCE: f32 = 3.0;
const FOLLOW_MOB_AREA: f32 = 7.0;

/// Vanilla `Parrot.MOB_SOUND_MAP`: the sound a parrot makes for each mob.
static MOB_SOUNDS: [(EntityTypeRef, SoundEventRef); 42] = [
    (
        &vanilla_entities::BLAZE,
        &sound_events::ENTITY_PARROT_IMITATE_BLAZE,
    ),
    (
        &vanilla_entities::BOGGED,
        &sound_events::ENTITY_PARROT_IMITATE_BOGGED,
    ),
    (
        &vanilla_entities::BREEZE,
        &sound_events::ENTITY_PARROT_IMITATE_BREEZE,
    ),
    (
        &vanilla_entities::CAMEL_HUSK,
        &sound_events::ENTITY_PARROT_IMITATE_CAMEL_HUSK,
    ),
    (
        &vanilla_entities::CAVE_SPIDER,
        &sound_events::ENTITY_PARROT_IMITATE_SPIDER,
    ),
    (
        &vanilla_entities::CREAKING,
        &sound_events::ENTITY_PARROT_IMITATE_CREAKING,
    ),
    (
        &vanilla_entities::CREEPER,
        &sound_events::ENTITY_PARROT_IMITATE_CREEPER,
    ),
    (
        &vanilla_entities::DROWNED,
        &sound_events::ENTITY_PARROT_IMITATE_DROWNED,
    ),
    (
        &vanilla_entities::ELDER_GUARDIAN,
        &sound_events::ENTITY_PARROT_IMITATE_ELDER_GUARDIAN,
    ),
    (
        &vanilla_entities::ENDER_DRAGON,
        &sound_events::ENTITY_PARROT_IMITATE_ENDER_DRAGON,
    ),
    (
        &vanilla_entities::ENDERMITE,
        &sound_events::ENTITY_PARROT_IMITATE_ENDERMITE,
    ),
    (
        &vanilla_entities::EVOKER,
        &sound_events::ENTITY_PARROT_IMITATE_EVOKER,
    ),
    (
        &vanilla_entities::GHAST,
        &sound_events::ENTITY_PARROT_IMITATE_GHAST,
    ),
    (
        &vanilla_entities::HAPPY_GHAST,
        &sound_events::INTENTIONALLY_EMPTY,
    ),
    (
        &vanilla_entities::GUARDIAN,
        &sound_events::ENTITY_PARROT_IMITATE_GUARDIAN,
    ),
    (
        &vanilla_entities::HOGLIN,
        &sound_events::ENTITY_PARROT_IMITATE_HOGLIN,
    ),
    (
        &vanilla_entities::HUSK,
        &sound_events::ENTITY_PARROT_IMITATE_HUSK,
    ),
    (
        &vanilla_entities::ILLUSIONER,
        &sound_events::ENTITY_PARROT_IMITATE_ILLUSIONER,
    ),
    (
        &vanilla_entities::MAGMA_CUBE,
        &sound_events::ENTITY_PARROT_IMITATE_MAGMA_CUBE,
    ),
    (
        &vanilla_entities::PARCHED,
        &sound_events::ENTITY_PARROT_IMITATE_PARCHED,
    ),
    (
        &vanilla_entities::PHANTOM,
        &sound_events::ENTITY_PARROT_IMITATE_PHANTOM,
    ),
    (
        &vanilla_entities::PIGLIN,
        &sound_events::ENTITY_PARROT_IMITATE_PIGLIN,
    ),
    (
        &vanilla_entities::PIGLIN_BRUTE,
        &sound_events::ENTITY_PARROT_IMITATE_PIGLIN_BRUTE,
    ),
    (
        &vanilla_entities::PILLAGER,
        &sound_events::ENTITY_PARROT_IMITATE_PILLAGER,
    ),
    (
        &vanilla_entities::RAVAGER,
        &sound_events::ENTITY_PARROT_IMITATE_RAVAGER,
    ),
    (
        &vanilla_entities::SHULKER,
        &sound_events::ENTITY_PARROT_IMITATE_SHULKER,
    ),
    (
        &vanilla_entities::SILVERFISH,
        &sound_events::ENTITY_PARROT_IMITATE_SILVERFISH,
    ),
    (
        &vanilla_entities::SKELETON,
        &sound_events::ENTITY_PARROT_IMITATE_SKELETON,
    ),
    (
        &vanilla_entities::SLIME,
        &sound_events::ENTITY_PARROT_IMITATE_SLIME,
    ),
    (
        &vanilla_entities::SPIDER,
        &sound_events::ENTITY_PARROT_IMITATE_SPIDER,
    ),
    (
        &vanilla_entities::STRAY,
        &sound_events::ENTITY_PARROT_IMITATE_STRAY,
    ),
    (
        &vanilla_entities::VEX,
        &sound_events::ENTITY_PARROT_IMITATE_VEX,
    ),
    (
        &vanilla_entities::VINDICATOR,
        &sound_events::ENTITY_PARROT_IMITATE_VINDICATOR,
    ),
    (
        &vanilla_entities::WARDEN,
        &sound_events::ENTITY_PARROT_IMITATE_WARDEN,
    ),
    (
        &vanilla_entities::WITCH,
        &sound_events::ENTITY_PARROT_IMITATE_WITCH,
    ),
    (
        &vanilla_entities::WITHER,
        &sound_events::ENTITY_PARROT_IMITATE_WITHER,
    ),
    (
        &vanilla_entities::WITHER_SKELETON,
        &sound_events::ENTITY_PARROT_IMITATE_WITHER_SKELETON,
    ),
    (
        &vanilla_entities::ZOGLIN,
        &sound_events::ENTITY_PARROT_IMITATE_ZOGLIN,
    ),
    (
        &vanilla_entities::ZOMBIE,
        &sound_events::ENTITY_PARROT_IMITATE_ZOMBIE,
    ),
    (
        &vanilla_entities::ZOMBIE_HORSE,
        &sound_events::ENTITY_PARROT_IMITATE_ZOMBIE_HORSE,
    ),
    (
        &vanilla_entities::ZOMBIE_NAUTILUS,
        &sound_events::ENTITY_PARROT_IMITATE_ZOMBIE_NAUTILUS,
    ),
    (
        &vanilla_entities::ZOMBIE_VILLAGER,
        &sound_events::ENTITY_PARROT_IMITATE_ZOMBIE_VILLAGER,
    ),
];

/// Vanilla `getImitatedSound`.
fn imitated_sound(entity_type: EntityTypeRef) -> Option<SoundEventRef> {
    MOB_SOUNDS
        .iter()
        .find(|(mob, _)| *mob == entity_type)
        .map(|(_, sound)| *sound)
}

/// Vanilla `Parrot` flap animation state, also read for the flap sounds.
#[derive(Debug, Clone, Copy)]
struct FlapState {
    flap: f32,
    flap_speed: f32,
    flapping: f32,
    next_flap: f32,
}

impl Default for FlapState {
    fn default() -> Self {
        Self {
            flap: 0.0,
            flap_speed: 0.0,
            flapping: 1.0,
            next_flap: 1.0,
        }
    }
}

#[entity_behavior(class = "Parrot", spawn_rule)]
/// Vanilla parrot.
pub struct ParrotEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    tamable_base: TamableAnimalBase,
    entity_data: SyncMutex<ParrotEntityData>,
    flap_state: SyncMutex<FlapState>,
    /// Vanilla `ShoulderRidingEntity.rideCooldownCounter`.
    ride_cooldown_counter: SyncMutex<i32>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ParrotEntity`.
unsafe impl DowncastType for ParrotEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/parrot");
}

impl ParrotEntity {
    /// Vanilla `checkParrotSpawnRules`: on leaves, grass, logs or snow, in light.
    pub(crate) const SPAWN_RULE: SpawnRule = |_entity_type, level, _spawn_reason, pos, _random| {
        level
            .get_block_state(pos.below())
            .get_block()
            .has_tag(&BlockTag::PARROTS_SPAWNABLE_ON)
            && <Self as Animal>::is_bright_enough_to_spawn(level, pos)
    };

    /// Creates a new parrot at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a parrot from persisted base entity state.
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
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        let mut entity_data = ParrotEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut navigation = PathNavigation::flying();
            navigation.set_can_open_doors(false);
            navigation.set_can_float(true);
            *mob_base.navigation().lock() = navigation;
        }

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, TamableAnimalPanicGoal::new(1.25));
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(2, SitWhenOrderedToGoal::new());
            goal_selector.add_goal(
                2,
                FollowOwnerGoal::new(
                    FOLLOW_OWNER_SPEED,
                    FOLLOW_OWNER_START_DISTANCE,
                    FOLLOW_OWNER_STOP_DISTANCE,
                ),
            );
            goal_selector.add_goal(2, ParrotWanderGoal::new(1.0));
            goal_selector.add_goal(3, LandOnOwnersShoulderGoal::new());
            // Vanilla follows any mob of another class than the parrot.
            goal_selector.add_goal(
                3,
                FollowMobGoal::new(
                    1.0,
                    FOLLOW_MOB_STOP_DISTANCE,
                    FOLLOW_MOB_AREA,
                    |mob, other| mob.downcast_type_key() != other.downcast_type_key(),
                ),
            );
        }

        let parrot = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            animal_base: AnimalBase::new(),
            tamable_base: TamableAnimalBase::new(),
            entity_data: SyncMutex::new(entity_data),
            flap_state: SyncMutex::new(FlapState::default()),
            ride_cooldown_counter: SyncMutex::new(0),
        };
        parrot.set_pathfinding_malus(PathType::FireInNeighbor, -1.0);
        parrot.set_pathfinding_malus(PathType::Fire, -1.0);
        parrot.set_pathfinding_malus(PathType::Cocoa, -1.0);
        parrot
    }

    /// The parrot's feathers.
    #[must_use]
    pub fn variant(&self) -> ParrotVariant {
        ParrotVariant::by_id(*self.entity_data.lock().variant.get())
    }

    /// Sets the parrot's feathers.
    pub fn set_variant(&self, variant: ParrotVariant) {
        self.entity_data.lock().variant.set(variant.id());
    }

    /// Vanilla `isFlying`.
    #[must_use]
    pub fn is_flying(&self) -> bool {
        !self.on_ground()
    }

    /// Vanilla `canSitOnShoulder`.
    #[must_use]
    pub fn can_sit_on_shoulder(&self) -> bool {
        *self.ride_cooldown_counter.lock() > RIDE_COOLDOWN
    }

    /// Vanilla `ShoulderRidingEntity.setEntityOnShoulder`: the parrot leaves the
    /// world and rides on the player's shoulder as its saved tag.
    pub fn set_entity_on_shoulder(&self, player: &Player) -> bool {
        let mut tag = self.nbt_for_data_compare();
        tag.insert("id", self.entity_type.key.to_string());
        if player.set_entity_on_shoulder(tag) {
            self.set_removed(RemovalReason::Discarded);
            true
        } else {
            false
        }
    }

    /// Vanilla `Parrot.getPitch`.
    #[must_use]
    pub fn random_pitch() -> f32 {
        (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0
    }

    /// Vanilla `Parrot.getAmbient`: rarely an imitation, outside peaceful.
    #[must_use]
    pub fn ambient_sound_for(world: &World) -> SoundEventRef {
        if world.difficulty() != Difficulty::Peaceful
            && rand::random_range(0..AMBIENT_IMITATE_CHANCE) == 0
        {
            MOB_SOUNDS[rand::random_range(0..MOB_SOUNDS.len())].1
        } else {
            &sound_events::ENTITY_PARROT_AMBIENT
        }
    }

    /// Vanilla `Parrot.imitateNearbyMobs`: half the time, copies the sound of a
    /// random nearby mob the parrot knows. Returns whether it made a sound.
    pub fn imitate_nearby_mobs(world: &World, entity: &dyn Entity) -> bool {
        if !entity.is_alive() || entity.is_silent() || rand::random::<bool>() {
            return false;
        }
        let search_box = entity.bounding_box().inflate(IMITATE_RANGE);
        let mobs = world.get_entities_in_aabb_matching(&search_box, |candidate| {
            candidate.as_mob().is_some() && imitated_sound(candidate.entity_type()).is_some()
        });
        if mobs.is_empty() {
            return false;
        }
        let mob = &mobs[rand::random_range(0..mobs.len())];
        if mob.is_silent() {
            return false;
        }
        let Some(sound) = imitated_sound(mob.entity_type()) else {
            return false;
        };
        world.play_sound_at(
            sound,
            entity.sound_source(),
            entity.position(),
            IMITATE_VOLUME,
            Self::random_pitch(),
            None,
        );
        true
    }

    /// Vanilla `calculateFlapping`: flaps faster in the air and falls slowly.
    fn calculate_flapping(&self) {
        let on_ground = self.on_ground();
        {
            let mut state = self.flap_state.lock();
            let gain = if !on_ground && !self.is_passenger() {
                4.0
            } else {
                -1.0
            };
            state.flap_speed = (state.flap_speed + gain * FLAP_SPEED_STEP).clamp(0.0, 1.0);
            if !on_ground && state.flapping < 1.0 {
                state.flapping = 1.0;
            }
            state.flapping *= FLAPPING_DECAY;
            state.flap += state.flapping * 2.0;
        }
        let velocity = self.velocity();
        if !on_ground && velocity.y < 0.0 {
            self.set_velocity(DVec3::new(velocity.x, velocity.y * FALL_DRAG_Y, velocity.z));
        }
    }

    /// Vanilla `mobInteract` with cookies: they poison and kill the parrot.
    fn feed_cookie(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        self.use_player_item(player, hand);
        self.add_mob_effect(MobEffectInstance::with_duration(
            vanilla_mob_effects::POISON,
            COOKIE_POISON_TICKS,
            0,
        ));
        if (player.game_mode() == GameType::Creative || !self.is_invulnerable())
            && let Some(world) = self.level()
            && let Some(player_entity) = world.get_entity_by_id(player.id())
        {
            self.hurt(
                &world,
                &DamageSource::direct(&vanilla_damage_types::PLAYER_ATTACK, player_entity),
                f32::MAX,
            );
        }
        InteractionResult::Success
    }

    /// Vanilla `mobInteract` with seeds for a wild parrot.
    fn feed_seeds(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        self.use_player_item(player, hand);
        if !self.is_silent() {
            let pitch = 1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2;
            if let Some(world) = self.level() {
                world.play_sound_at(
                    &sound_events::ENTITY_PARROT_EAT,
                    self.sound_source(),
                    self.position(),
                    1.0,
                    pitch,
                    None,
                );
            }
        }
        if rand::random_range(0..TAME_CHANCE) == 0 {
            self.tame(player);
            self.broadcast_entity_event(EntityStatus::TamingSucceeded);
        } else {
            self.broadcast_entity_event(EntityStatus::TamingFailed);
        }
        InteractionResult::Success
    }
}

impl TamableAnimal for ParrotEntity {
    fn tamable_base(&self) -> &TamableAnimalBase {
        &self.tamable_base
    }

    fn tamable_flags(&self) -> i8 {
        *self.entity_data.lock().tamable_animal().flags.get()
    }

    fn set_tamable_flags(&self, flags: i8) {
        self.entity_data
            .lock()
            .tamable_animal_mut()
            .flags
            .set(flags);
    }

    fn owner_uuid(&self) -> Option<Uuid> {
        *self.entity_data.lock().tamable_animal().owneruuid.get()
    }

    fn set_owner_uuid(&self, owner: Option<Uuid>) {
        self.entity_data
            .lock()
            .tamable_animal_mut()
            .owneruuid
            .set(owner);
    }

    fn can_fly_to_owner(&self) -> bool {
        true
    }
}

impl Entity for ParrotEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        *self.ride_cooldown_counter.lock() += 1;
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        if self.entity_type.fixed {
            self.entity_type.dimensions
        } else {
            self.entity_type
                .dimensions
                .scale(LivingEntity::get_scale(self))
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
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

    fn is_pushable(&self) -> bool {
        true
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_PARROT_STEP, STEP_VOLUME, 1.0);
    }

    fn is_flapping(&self) -> bool {
        self.base().movement_progress().fly_dist() > self.flap_state.lock().next_flap
    }

    fn on_flap(&self) {
        self.play_sound(&sound_events::ENTITY_PARROT_FLY, FLY_VOLUME, 1.0);
        let fly_dist = self.base().movement_progress().fly_dist();
        let mut state = self.flap_state.lock();
        state.next_flap = fly_dist + state.flap_speed / 2.0;
    }

    /// Vanilla `checkFallDamage`: parrots never take fall damage.
    fn check_fall_damage(
        self: Arc<Self>,
        _vertical_movement: f64,
        _on_ground: bool,
        _on_state: BlockStateId,
        _pos: BlockPos,
        _world: &Arc<World>,
    ) {
    }

    fn considers_entity_as_ally(&self, other: &dyn Entity) -> bool {
        self.considers_entity_as_ally_tamable(other)
    }

    fn is_tame_owned_by(&self, owner: &dyn LivingEntity) -> bool {
        self.is_tame() && self.is_owned_by(owner.as_entity_event_source())
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(PARROT_VARIANT) {
            self.set_variant(*variant);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        self.save_tamable(nbt);
        nbt.insert(TAG_VARIANT, self.variant().id());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.load_tamable(nbt);
        self.set_variant(
            nbt.int(TAG_VARIANT)
                .map_or_else(ParrotVariant::default, ParrotVariant::by_id),
        );
    }
}

impl LivingEntity for ParrotEntity {
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

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PARROT_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PARROT_DEATH)
    }

    fn voice_pitch(&self) -> f32 {
        Self::random_pitch()
    }

    fn omnidirectional_air_mover(&self) -> bool {
        true
    }

    /// Vanilla `hurtServer`: a hit parrot stops sitting.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_invulnerable_to(world, source) {
            return false;
        }
        self.set_ordered_to_sit(false);
        self.default_hurt_server(world, source, amount)
    }

    /// Vanilla `doPush`: parrots never push players.
    fn push_entities_in_order(&self, entities: &[SharedEntity]) {
        let others = entities
            .iter()
            .filter(|entity| entity.as_player().is_none())
            .cloned()
            .collect::<Vec<_>>();
        EntityPush::push_entities_in_order(self, &others);
    }

    fn die(&self, source: &DamageSource) {
        self.announce_death_to_owner(source);
        self.default_die(source);
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        // Vanilla's jukebox dancing only drives the client animation.
        if rand::random_range(0..IMITATE_CHANCE) == 0
            && let Some(world) = self.level()
        {
            Self::imitate_nearby_mobs(&world, self);
        }
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        self.calculate_flapping();
        result
    }
}

impl AgeableMob for ParrotEntity {
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

    fn can_be_a_baby(&self) -> bool {
        false
    }
}

impl Animal for ParrotEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }

    fn can_mate(&self, _partner: &dyn Animal) -> bool {
        false
    }
}

impl Mob for ParrotEntity {
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
        PARROT_MOVE_CONTROL.tick(self);
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.can_attack_tamable(target)
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        self.level().map(|world| Self::ambient_sound_for(&world))
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_variant(ParrotVariant::VALUES[rand::random_range(0..ParrotVariant::VALUES.len())]);
        let group_data = group_data.or(Some(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_should_spawn_baby(false),
        )));
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if !self.is_tame()
            && REGISTRY
                .items
                .is_in_tag(item_stack.item(), &ItemTag::PARROT_FOOD)
        {
            return self.feed_seeds(player, hand);
        }
        if REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::PARROT_POISONOUS_FOOD)
        {
            return self.feed_cookie(player, hand);
        }
        if !self.is_flying() && self.is_tame() && self.is_owned_by(player) {
            self.set_ordered_to_sit(!self.is_ordered_to_sit());
            return InteractionResult::Success;
        }
        Animal::mob_interact_animal(self, player, hand)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for ParrotEntity {}

#[cfg(test)]
mod tests;
