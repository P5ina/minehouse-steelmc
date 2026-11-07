//! Vanilla fox: a night hunter that sleeps through the day, carries items in
//! its mouth and trusts the players who bred it.

mod goals;

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::{CTakeItemEntity, SoundSource};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::{CONSUMABLE, FOOD, FOX_VARIANT};
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::entity_variant::FoxVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::level_events;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::FoxEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_entities,
    vanilla_items,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, ChunkPos, Downcast as _, DowncastType, DowncastTypeKey};
use uuid::Uuid;

use goals::{
    DefendTrustedTargetGoal, FaceplantGoal, FoxAvoidGoal, FoxBreedGoal, FoxEatBerriesGoal,
    FoxFloatGoal, FoxFollowParentGoal, FoxLookAtPlayerGoal, FoxMeleeAttackGoal, FoxPanicGoal,
    FoxPounceGoal, FoxSearchForItemsGoal, FoxStrollThroughVillageGoal, PerchAndSearchGoal,
    SeekShelterGoal, SleepGoal, StalkPreyGoal,
};

use crate::behavior::{ITEM_BEHAVIORS, InteractionResult};
use crate::entity::ai::goal::{
    ClimbOnTopOfPowderSnowGoal, LeapAtTargetGoal, NearestAttackableTargetGoal,
    WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::living_base::LivingTravelInput;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, DefaultEntityPush, Entity,
    EntityBase, EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, EquipmentSlot,
    LivingEntity, LivingEntityBase, LivingEntityRef, Mob, MobBase, PathfinderMob, RemovalReason,
    SharedEntity, SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader as _, ServerLevelAccessor, World};

const FOX_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.375, 0.0)];

const FLAG_SITTING: i8 = 1;
const FLAG_CROUCHING: i8 = 4;
const FLAG_INTERESTED: i8 = 8;
const FLAG_POUNCING: i8 = 16;
const FLAG_SLEEPING: i8 = 32;
const FLAG_FACEPLANTED: i8 = 64;
const FLAG_DEFENDING: i8 = i8::MIN;
const BABY_SCALE: f32 = 0.6;
const BABY_EYE_HEIGHT: f32 = 0.343_75;
const MIN_TICKS_BEFORE_EAT: i32 = 600;
const MAX_CROUCH_AMOUNT: f32 = 5.0;
const CROUCH_STEP: f32 = 0.2;
const SPIT_PICKUP_DELAY: i32 = 40;
const REQUIRED_PATH_LENGTH: f32 = 32.0;
const SCREECH_PLAYER_RANGE: f64 = 16.0;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;

#[entity_behavior(class = "Fox", spawn_rule)]
/// Vanilla fox.
pub struct FoxEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    ticks_since_eaten: AtomicI32,
    crouch_amount: SyncMutex<f32>,
    target_goals_added: AtomicBool,
    entity_data: SyncMutex<FoxEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `FoxEntity`.
unsafe impl DowncastType for FoxEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/fox");
}

/// Vanilla `Fox.checkFoxSpawnRules`.
fn check_fox_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    level
        .get_block_state(pos.below())
        .get_block()
        .has_tag(&BlockTag::FOXES_SPAWNABLE_ON)
        && <FoxEntity as Animal>::is_bright_enough_to_spawn(level, pos)
}

/// Vanilla `Fox.STALKABLE_PREY`: chickens and rabbits.
pub(crate) fn is_stalkable_prey(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::CHICKEN || entity_type == &vanilla_entities::RABBIT
}

/// Vanilla `Fox.ALLOWED_ITEMS`.
fn is_allowed_item(item: &ItemEntity) -> bool {
    !item.has_pickup_delay() && Entity::is_alive(item)
}

fn is_consumable_food(item_stack: &ItemStack) -> bool {
    item_stack.has(FOOD) && item_stack.has(CONSUMABLE)
}

fn variant_by_biome(world: &World, pos: BlockPos) -> FoxVariant {
    if world
        .biome(pos)
        .is_some_and(|biome| biome.has_tag(&BiomeTag::SPAWNS_SNOW_FOXES))
    {
        FoxVariant::Snow
    } else {
        FoxVariant::Red
    }
}

impl FoxEntity {
    /// Vanilla `SpawnPlacements` predicate: `Fox.checkFoxSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_fox_spawn_rules;

    /// Creates a new fox at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a fox from persisted base entity state.
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
        let follow_range = living_base
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(vanilla_attributes::FOLLOW_RANGE.default_value);
        mob_base
            .navigation()
            .lock()
            .set_required_path_length(REQUIRED_PATH_LENGTH, follow_range);
        let mut entity_data = FoxEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FoxFloatGoal::new(&mob_base));
            goal_selector.add_goal(0, ClimbOnTopOfPowderSnowGoal::new());
            goal_selector.add_goal(1, FaceplantGoal::new());
            goal_selector.add_goal(2, FoxPanicGoal::new());
            goal_selector.add_goal(3, FoxBreedGoal::new());
            goal_selector.add_goal(4, FoxAvoidGoal::players());
            goal_selector.add_goal(4, FoxAvoidGoal::wolves());
            goal_selector.add_goal(4, FoxAvoidGoal::polar_bears());
            goal_selector.add_goal(5, StalkPreyGoal);
            goal_selector.add_goal(6, FoxPounceGoal);
            goal_selector.add_goal(6, SeekShelterGoal::new());
            goal_selector.add_goal(7, FoxMeleeAttackGoal::new());
            goal_selector.add_goal(7, SleepGoal::new());
            goal_selector.add_goal(8, FoxFollowParentGoal::new());
            goal_selector.add_goal(9, FoxStrollThroughVillageGoal::new());
            goal_selector.add_goal(10, FoxEatBerriesGoal::new());
            goal_selector.add_goal(10, LeapAtTargetGoal::new(0.4));
            goal_selector.add_goal(11, WaterAvoidingRandomStrollGoal::new(1.0));
            goal_selector.add_goal(11, FoxSearchForItemsGoal);
            goal_selector.add_goal(12, FoxLookAtPlayerGoal::new());
            goal_selector.add_goal(13, PerchAndSearchGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(3, DefendTrustedTargetGoal::new());
        }

        let fox = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            ticks_since_eaten: AtomicI32::new(0),
            crouch_amount: SyncMutex::new(0.0),
            target_goals_added: AtomicBool::new(false),
            entity_data: SyncMutex::new(entity_data),
        };
        fox.set_pathfinding_malus(PathType::DamagingInNeighbor, 0.0);
        fox.set_pathfinding_malus(PathType::Damaging, 0.0);
        fox.set_can_pick_up_loot(true);
        fox
    }

    fn flag(&self, flag: i8) -> bool {
        *self.entity_data.lock().flags.get() & flag != 0
    }

    fn set_flag(&self, flag: i8, value: bool) {
        let mut data = self.entity_data.lock();
        let current = *data.flags.get();
        data.flags.set(if value {
            current | flag
        } else {
            current & !flag
        });
    }

    /// The fox's fur color.
    #[must_use]
    pub fn variant(&self) -> FoxVariant {
        FoxVariant::by_id(*self.entity_data.lock().variant_type.get())
    }

    /// Sets the fox's fur color.
    pub fn set_variant(&self, variant: FoxVariant) {
        self.entity_data.lock().variant_type.set(variant.id());
    }

    /// Whether the fox sits.
    #[must_use]
    pub fn is_sitting(&self) -> bool {
        self.flag(FLAG_SITTING)
    }

    /// Makes the fox sit or stand.
    pub fn set_sitting(&self, value: bool) {
        self.set_flag(FLAG_SITTING, value);
    }

    /// Whether the fox lies face down in the snow after a missed pounce.
    #[must_use]
    pub fn is_faceplanted(&self) -> bool {
        self.flag(FLAG_FACEPLANTED)
    }

    pub(crate) fn set_faceplanted(&self, value: bool) {
        self.set_flag(FLAG_FACEPLANTED, value);
    }

    /// Whether the fox defends a trusted player.
    #[must_use]
    pub fn is_defending(&self) -> bool {
        self.flag(FLAG_DEFENDING)
    }

    pub(crate) fn set_defending(&self, value: bool) {
        self.set_flag(FLAG_DEFENDING, value);
    }

    pub(crate) fn set_sleeping_flag(&self, value: bool) {
        self.set_flag(FLAG_SLEEPING, value);
    }

    /// Whether the fox is pouncing.
    #[must_use]
    pub fn is_pouncing(&self) -> bool {
        self.flag(FLAG_POUNCING)
    }

    pub(crate) fn set_is_pouncing(&self, value: bool) {
        self.set_flag(FLAG_POUNCING, value);
    }

    pub(crate) fn set_is_crouching(&self, value: bool) {
        self.set_flag(FLAG_CROUCHING, value);
    }

    /// Whether the fox tilts its head at its prey.
    #[must_use]
    pub fn is_interested(&self) -> bool {
        self.flag(FLAG_INTERESTED)
    }

    pub(crate) fn set_is_interested(&self, value: bool) {
        self.set_flag(FLAG_INTERESTED, value);
    }

    /// Vanilla `Fox.isFullyCrouched`.
    #[must_use]
    pub fn is_fully_crouched(&self) -> bool {
        *self.crouch_amount.lock() >= MAX_CROUCH_AMOUNT
    }

    pub(crate) fn reset_crouch_amount(&self) {
        *self.crouch_amount.lock() = 0.0;
    }

    /// The UUIDs of the up to two entities the fox trusts.
    #[must_use]
    pub fn trusted(&self) -> Vec<Uuid> {
        let data = self.entity_data.lock();
        [*data.trusted_id_0.get(), *data.trusted_id_1.get()]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Vanilla `Fox.addTrustedEntity`: fills the first slot, then keeps
    /// replacing the second.
    pub fn add_trusted(&self, uuid: Uuid) {
        let mut data = self.entity_data.lock();
        if data.trusted_id_0.get().is_some() {
            data.trusted_id_1.set(Some(uuid));
        } else {
            data.trusted_id_0.set(Some(uuid));
        }
    }

    fn clear_trusted(&self) {
        let mut data = self.entity_data.lock();
        data.trusted_id_0.set(None);
        data.trusted_id_1.set(None);
    }

    /// Vanilla `Fox.trusts`.
    #[must_use]
    pub fn trusts(&self, entity: &dyn Entity) -> bool {
        let uuid = entity.uuid();
        self.trusted().contains(&uuid)
    }

    /// Vanilla `Fox.wakeUp`.
    pub(crate) fn wake_up(&self) {
        self.set_sleeping_flag(false);
    }

    /// Vanilla `Fox.clearStates`.
    pub(crate) fn clear_states(&self) {
        self.set_is_interested(false);
        self.set_is_crouching(false);
        self.set_sitting(false);
        self.set_sleeping_flag(false);
        self.set_defending(false);
        self.set_faceplanted(false);
    }

    /// Vanilla `Fox.canMove`.
    #[must_use]
    pub fn can_move(&self) -> bool {
        !self.is_sleeping() && !self.is_sitting() && !self.is_faceplanted()
    }

    fn main_hand(&self) -> ItemStack {
        self.equipment_in_slot(EquipmentSlot::MainHand)
    }

    pub(crate) fn set_main_hand(&self, item_stack: ItemStack) {
        self.living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, item_stack);
    }

    fn can_eat(&self, item_stack: &ItemStack) -> bool {
        is_consumable_food(item_stack)
            && self.target().is_none()
            && self.on_ground()
            && !self.is_sleeping()
    }

    /// Vanilla `Fox.spitOutItem`: throws the held item ahead of itself.
    fn spit_out_item(&self, world: &Arc<World>, item_stack: ItemStack) {
        if item_stack.is_empty() {
            return;
        }
        let look = self.look_angle();
        let position = self.position();
        let pos = DVec3::new(position.x + look.x, position.y + 1.0, position.z + look.z);
        if let Some(item) = world.spawn_item(pos, item_stack) {
            item.set_pickup_delay(SPIT_PICKUP_DELAY);
            item.set_thrower(self.uuid());
            self.play_sound(&sound_events::ENTITY_FOX_SPIT, 1.0, 1.0);
        }
    }

    /// Vanilla `Fox.setTargetGoals`: red foxes hunt on land first, snow foxes
    /// fish first.
    fn set_target_goals(&self) {
        if self.target_goals_added.swap(true, Ordering::Relaxed) {
            return;
        }
        let land = NearestAttackableTargetGoal::new_with_interval(10, false, false, |target, _| {
            is_stalkable_prey(target.as_entity_event_source())
        });
        let turtle_egg =
            NearestAttackableTargetGoal::new_with_interval(10, false, false, |target, _| {
                target.entity_type() == &vanilla_entities::TURTLE
                    && target.as_ageable_mob().is_some_and(AgeableMob::is_baby)
                    && !target.is_in_water()
            });
        let fish = NearestAttackableTargetGoal::new_with_interval(20, false, false, |target, _| {
            target.as_schooling_fish().is_some()
        });
        let mut target_selector = self.mob_base.target_selector().lock();
        if self.variant() == FoxVariant::Red {
            target_selector.add_goal(4, land);
            target_selector.add_goal(4, turtle_egg);
            target_selector.add_goal(6, fish);
        } else {
            target_selector.add_goal(4, fish);
            target_selector.add_goal(6, land);
            target_selector.add_goal(6, turtle_egg);
        }
    }

    /// Vanilla `Fox.populateDefaultEquipmentSlots`: one fox in five spawns
    /// with something in its mouth.
    fn populate_default_equipment_slots(&self) {
        if rand::random::<f32>() >= 0.2 {
            return;
        }
        let odds = rand::random::<f32>();
        let item = if odds < 0.05 {
            &vanilla_items::EMERALD
        } else if odds < 0.2 {
            &vanilla_items::EGG
        } else if odds < 0.4 {
            if rand::random::<bool>() {
                &vanilla_items::RABBIT_FOOT
            } else {
                &vanilla_items::RABBIT_HIDE
            }
        } else if odds < 0.6 {
            &vanilla_items::WHEAT
        } else if odds < 0.8 {
            &vanilla_items::LEATHER
        } else {
            &vanilla_items::FEATHER
        };
        self.set_main_hand(ItemStack::new(item));
    }

    /// The fox-specific start of vanilla `Fox.aiStep`.
    fn ai_step_fox(&self, entity: &SharedEntity) {
        if LivingEntity::is_alive(self) && self.is_effective_ai() {
            let ticks = self.ticks_since_eaten.fetch_add(1, Ordering::Relaxed) + 1;
            let mut item = self.main_hand();
            if self.can_eat(&item) {
                if ticks > MIN_TICKS_BEFORE_EAT {
                    if let (Some(world), Some(user)) = (self.level(), LivingEntityRef::new(entity))
                    {
                        let behavior = ITEM_BEHAVIORS.get_behavior(item.item());
                        let remaining = behavior.finish_using(&mut item, &world, user);
                        self.set_main_hand(remaining);
                    }
                    self.ticks_since_eaten.store(0, Ordering::Relaxed);
                } else if ticks > MIN_TICKS_BEFORE_EAT - 40 && rand::random::<f32>() < 0.1 {
                    self.play_eating_sound();
                    self.broadcast_entity_event(EntityStatus::FoxEat);
                }
            }
            if self.target().is_none_or(|target| !target.is_alive()) {
                self.set_is_crouching(false);
                self.set_is_interested(false);
            }
        }

        if self.is_sleeping() || self.is_dead_or_dying() {
            self.set_jumping(false);
            let input = self.travel_input();
            self.set_travel_input(LivingTravelInput::new(0.0, input.vertical(), 0.0));
        }
    }

    /// The fox-specific half of vanilla `Fox.tick`.
    fn tick_fox(&self) {
        if self.is_effective_ai()
            && let Some(world) = self.level()
        {
            let in_water = self.is_in_water();
            if in_water || self.target().is_some() || world.is_thundering() {
                self.wake_up();
            }
            if in_water || self.is_sleeping() {
                self.set_sitting(false);
            }
            if self.is_faceplanted() && rand::random::<f32>() < 0.2 {
                let pos = self.block_position();
                let state = world.get_block_state(pos);
                world.level_event(
                    level_events::PARTICLES_DESTROY_BLOCK,
                    pos,
                    level_events::encode_block_state_data(u32::from(state.0)),
                    None,
                );
            }
        }

        let mut crouch_amount = self.crouch_amount.lock();
        if self.is_crouching() {
            *crouch_amount = (*crouch_amount + CROUCH_STEP).min(MAX_CROUCH_AMOUNT);
        } else {
            *crouch_amount = 0.0;
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

/// Vanilla `Fox.isPathClear`: no solid block between the fox and its target
/// up to three blocks high.
pub(crate) fn is_path_clear(fox: &FoxEntity, target: &dyn Entity) -> bool {
    let Some(world) = fox.level() else {
        return false;
    };
    let fox_pos = fox.position();
    let target_pos = target.position();
    let zdiff = target_pos.z - fox_pos.z;
    let xdiff = target_pos.x - fox_pos.x;
    let slope = zdiff / xdiff;
    for i in 0..6 {
        let fraction = f64::from(i as f32 / 6.0);
        let z = if slope == 0.0 { 0.0 } else { zdiff * fraction };
        let x = if slope == 0.0 {
            xdiff * fraction
        } else {
            z / slope
        };
        for j in 1..4 {
            let pos = BlockPos::containing(fox_pos.x + x, fox_pos.y + f64::from(j), fox_pos.z + z);
            if !world.get_block_state(pos).is_replaceable() {
                return false;
            }
        }
    }
    true
}

impl DefaultEntityPush for FoxEntity {}

impl Entity for FoxEntity {
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
        if !self.is_removed() {
            self.tick_fox();
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        let dimensions = if AgeableMob::is_baby(self) {
            let adult = self.entity_type.dimensions;
            EntityDimensions::new_with_attachments(
                adult.width * BABY_SCALE,
                adult.height * BABY_SCALE,
                BABY_EYE_HEIGHT,
                EntityAttachments::new(&FOX_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
            )
        } else {
            self.entity_type.dimensions
        };
        if self.entity_type.fixed {
            dimensions
        } else {
            dimensions.scale(scale)
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

    /// Vanilla `Fox.isCrouching`: the crouch flag, not the pose.
    fn is_crouching(&self) -> bool {
        self.flag(FLAG_CROUCHING)
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(FOX_VARIANT) {
            self.set_variant(*variant);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        let trusted: Vec<NbtTag> = self
            .trusted()
            .into_iter()
            .map(|uuid| NbtTag::IntArray(uuid_to_int_array(uuid)))
            .collect();
        nbt.insert("Trusted", NbtList::from(trusted));
        nbt.insert("Sleeping", i8::from(self.is_sleeping()));
        nbt.insert("Type", self.variant().serialized_name());
        nbt.insert("Sitting", i8::from(self.is_sitting()));
        nbt.insert("Crouching", i8::from(self.is_crouching()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.clear_trusted();
        let trusted: Vec<Vec<i32>> = nbt
            .list("Trusted")
            .and_then(|list| {
                list.int_arrays().map(|arrays| {
                    arrays
                        .iter()
                        .map(simdnbt::raw_list::RawList::to_vec)
                        .collect()
                })
            })
            .unwrap_or_default();
        for ints in trusted {
            if let [a, b, c, d] = ints[..] {
                self.add_trusted(int_array_to_uuid([a, b, c, d]));
            }
        }
        self.set_sleeping_flag(nbt.byte("Sleeping").is_some_and(|value| value != 0));
        self.set_variant(
            nbt.string("Type")
                .and_then(|name| FoxVariant::from_serialized_name(name.to_str().as_ref()))
                .unwrap_or_default(),
        );
        self.set_sitting(nbt.byte("Sitting").is_some_and(|value| value != 0));
        self.set_is_crouching(nbt.byte("Crouching").is_some_and(|value| value != 0));
        if self.level().is_some() {
            self.set_target_goals();
        }
    }
}

fn uuid_to_int_array(uuid: Uuid) -> Vec<i32> {
    let (most, least) = uuid.as_u64_pair();
    vec![
        (most >> 32) as i32,
        most as i32,
        (least >> 32) as i32,
        least as i32,
    ]
}

fn int_array_to_uuid(ints: [i32; 4]) -> Uuid {
    let most = (u64::from(ints[0] as u32) << 32) | u64::from(ints[1] as u32);
    let least = (u64::from(ints[2] as u32) << 32) | u64::from(ints[3] as u32);
    Uuid::from_u64_pair(most, least)
}

impl LivingEntity for FoxEntity {
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

    /// Vanilla `Fox.isSleeping`: the fox's own sleep flag.
    fn is_sleeping(&self) -> bool {
        self.flag(FLAG_SLEEPING)
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_FOX_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_FOX_DEATH)
    }

    fn can_dispenser_equip_into_slot(&self, slot: EquipmentSlot) -> bool {
        slot == EquipmentSlot::MainHand && self.can_pick_up_loot()
    }

    /// Vanilla `Fox.dropAllDeathLoot`: always drops what it carries.
    fn drop_all_death_loot(&self, source: &DamageSource) {
        let item = self.main_hand();
        if !item.is_empty() {
            self.spawn_at_location(item, 0.0);
            self.set_main_hand(ItemStack::empty());
        }
        self.default_drop_all_death_loot(source);
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        self.ai_step_fox(entity);
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        if self.is_defending() && rand::random::<f32>() < 0.05 {
            self.play_sound(&sound_events::ENTITY_FOX_AGGRO, 1.0, 1.0);
        }
        result
    }
}

impl AgeableMob for FoxEntity {
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

    /// Vanilla `Fox.getBreedOffspring`: the cub takes one parent's color.
    fn get_breed_offspring(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        let offspring = self.create_breed_offspring(world)?;
        let variant = if rand::random::<bool>() {
            self.variant()
        } else {
            partner
                .as_entity_event_source()
                .downcast_ref::<Self>()
                .map_or_else(|| self.variant(), Self::variant)
        };
        if let Some(baby) = offspring.downcast_ref::<Self>() {
            baby.set_variant(variant);
        }
        Some(offspring)
    }
}

impl Animal for FoxEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::FOX_FOOD)
    }

    fn play_eating_sound(&self) {
        self.play_sound(&sound_events::ENTITY_FOX_EAT, 1.0, 1.0);
    }

    /// Vanilla `FoxBreedGoal.breed`: the cub trusts the players who bred
    /// its parents.
    fn on_bred_offspring(&self, world: &Arc<World>, partner: &dyn Animal, offspring: &dyn Animal) {
        let Some(cub) = offspring.as_entity_event_source().downcast_ref::<Self>() else {
            return;
        };
        let online = |uuid: Option<Uuid>| {
            uuid.filter(|uuid| {
                world
                    .get_entity_by_uuid(uuid)
                    .is_some_and(|entity| entity.as_player().is_some())
            })
        };
        let animal_cause = online(self.love_cause_uuid());
        let partner_cause = online(partner.love_cause_uuid());
        if let Some(cause) = animal_cause {
            cub.add_trusted(cause);
        }
        if let Some(cause) = partner_cause
            && animal_cause != Some(cause)
        {
            cub.add_trusted(cause);
        }
    }
}

impl Mob for FoxEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `FoxMoveControl`: only moves while it can.
    fn tick_move_control(&self) {
        if self.can_move() {
            self.default_tick_move_control();
        }
    }

    /// Vanilla `FoxLookControl`: still while asleep.
    fn tick_look_control(&self) {
        if !self.is_sleeping() {
            self.default_tick_look_control();
        }
    }

    /// Vanilla `FoxLookControl.resetXRotOnTick`.
    fn look_control_resets_x_rot(&self) -> bool {
        !self.is_pouncing()
            && !self.is_crouching()
            && !self.is_interested()
            && !self.is_faceplanted()
    }

    fn custom_server_ai_step(&self) {
        Animal::custom_server_ai_step_animal(self);
    }

    /// Vanilla `Fox.setTarget`: losing the target ends the defense.
    fn set_target(&self, target: Option<&SharedEntity>) -> bool {
        if self.is_defending() && target.is_none() {
            self.set_defending(false);
        }
        self.mob_base()
            .set_target(target, |target| self.is_valid_target(target))
    }

    /// Vanilla `Fox.canHoldItem`.
    fn can_hold_item(&self, item_stack: &ItemStack) -> bool {
        let held = self.main_hand();
        held.is_empty()
            || self.ticks_since_eaten.load(Ordering::Relaxed) > 0
                && is_consumable_food(item_stack)
                && !is_consumable_food(&held)
    }

    /// Vanilla `Fox.pickUpItem`: keeps one item in its mouth, drops the rest
    /// and spits out what it held.
    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        let mut item_stack = item_entity.get_item();
        if !self.can_hold_item(&item_stack) {
            return;
        }
        let count = item_stack.count();
        if count > 1 {
            let rest = item_stack.split(count - 1);
            world.spawn_item(self.position(), rest);
        }
        self.spit_out_item(world, self.main_hand());
        self.set_main_hand(item_stack.split(1));
        self.set_guaranteed_drop(EquipmentSlot::MainHand);
        world.broadcast_to_nearby(
            ChunkPos::from_entity_pos(item_entity.position()),
            CTakeItemEntity::new(item_entity.id(), self.id(), item_stack.count()),
            None,
        );
        item_entity.set_removed(RemovalReason::Discarded);
        self.ticks_since_eaten.store(0, Ordering::Relaxed);
    }

    /// Vanilla `Fox.playAmbientSound`: the screech is loud.
    fn play_ambient_sound(&self) {
        let sound = self.ambient_sound();
        if sound == Some(&sound_events::ENTITY_FOX_SCREECH) {
            self.play_sound(&sound_events::ENTITY_FOX_SCREECH, 2.0, self.voice_pitch());
        } else {
            self.make_sound(sound);
        }
    }

    /// Vanilla `Fox.getAmbientSound`: sleeping, or a screech on dark nights
    /// with no player around.
    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if self.is_sleeping() {
            return Some(&sound_events::ENTITY_FOX_SLEEP);
        }
        if let Some(world) = self.level()
            && !world.is_bright_outside()
            && rand::random::<f32>() < 0.1
        {
            let area = self.bounding_box().inflate(SCREECH_PLAYER_RANGE);
            let player_nearby = world.has_entity_in_aabb_matching(&area, |entity| {
                entity.as_player().is_some() && !entity.is_spectator()
            });
            if !player_nearby {
                return Some(&sound_events::ENTITY_FOX_SCREECH);
            }
        }
        Some(&sound_events::ENTITY_FOX_AMBIENT)
    }

    fn on_offspring_spawned_from_egg(&self, spawner: &Player, offspring: &dyn Mob) {
        if let Some(cub) = offspring.as_entity_event_source().downcast_ref::<Self>() {
            cub.add_trusted(spawner.uuid());
        }
    }

    /// Vanilla `Fox.finalizeSpawn`.
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut variant = variant_by_biome(world, self.block_position());
        let mut is_baby = false;
        let ageable = match group_data {
            Some(SpawnGroupData::Fox(ageable, group_variant)) => {
                variant = group_variant;
                is_baby = ageable.group_size() >= 2;
                ageable
            }
            _ => AgeableMobGroupData::with_should_spawn_baby(false),
        };
        self.set_variant(variant);
        if is_baby {
            self.set_age(self.get_baby_start_age());
        }
        self.set_target_goals();
        self.populate_default_equipment_slots();
        let result = self.finalize_spawn_ageable_mob(
            world,
            spawn_reason,
            Some(SpawnGroupData::AgeableMob(ageable)),
        );
        match result {
            Some(SpawnGroupData::AgeableMob(ageable)) => {
                Some(SpawnGroupData::Fox(ageable, variant))
            }
            other => other,
        }
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

impl PathfinderMob for FoxEntity {}

#[cfg(test)]
mod tests;
