//! Vanilla cat: a village and swamp-hut cat that players tame with fish, that
//! naps on beds and chests and brings gifts to its sleeping owner.

mod goals;

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::cat_sound_variant::{CatAge, CatSoundVariantRef};
use steel_registry::cat_variant::{CatVariantRef, SpawnCondition};
use steel_registry::data_components::vanilla_components::{
    CAT_COLLAR, CAT_SOUND_VARIANT, CAT_VARIANT, DYE,
};
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::CatEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    DyeColor, REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _,
    vanilla_attributes, vanilla_entities,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::InteractionHand;
use steel_utils::{Downcast as _, DowncastType, DowncastTypeKey, Identifier};
use uuid::Uuid;

use goals::{
    CatAvoidPlayersGoal, CatLieOnBlockGoal, CatRelaxOnOwnerGoal, CatSitOnBlockGoal, CatTemptGoal,
    TEMPT_SPEED_MOD,
};

use super::ocelot::{OcelotAttackGoal, SPRINT_SPEED_MOD, WALK_SPEED_MOD};
use crate::behavior::InteractionResult;
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{
    BreedGoal, FloatGoal, FollowOwnerGoal, LeapAtTargetGoal, LookAtPlayerGoal,
    NonTameRandomTargetGoal, SitWhenOrderedToGoal, TamableAnimalPanicGoal,
    WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::SheepEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    TamableAnimal, TamableAnimalBase, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

const CAT_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.3125, 0.0)];
const CAT_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.3,
    0.35,
    0.343_75,
    EntityAttachments::new(&CAT_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const AMBIENT_SOUND_INTERVAL: i32 = 120;
/// Wild cats despawn far from players once they are this old.
const DESPAWN_MIN_TICKS: i32 = 2400;
const LIE_ON_BLOCK_SPEED: f64 = 1.1;
const SIT_ON_BLOCK_SPEED: f64 = 0.8;
const BEG_SOUND_INTERVAL: i32 = 100;
const PURR_INTERVAL: i32 = 5;
const TAG_COLLAR_COLOR: &str = "CollarColor";
const TAG_VARIANT: &str = "variant";
const TAG_SOUND_VARIANT: &str = "sound_variant";

#[entity_behavior(class = "Cat", spawn_rule)]
/// Vanilla cat.
pub struct CatEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    tamable_base: TamableAnimalBase,
    /// Mirrors vanilla `temptGoal.isRunning()` for the begging sound.
    tempting: Arc<AtomicBool>,
    entity_data: SyncMutex<CatEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `CatEntity`.
unsafe impl DowncastType for CatEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/cat");
}

/// Vanilla `ItemTags.CAT_FOOD`.
fn is_cat_food(item_stack: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item_stack.item(), &ItemTag::CAT_FOOD)
}

/// Vanilla `Turtle.BABY_ON_LAND_SELECTOR` on turtles.
fn is_baby_turtle_on_land(target: &dyn LivingEntity) -> bool {
    target.entity_type() == &vanilla_entities::TURTLE
        && target.as_ageable_mob().is_some_and(AgeableMob::is_baby)
        && !target.is_in_water()
}

impl CatEntity {
    /// Vanilla `SpawnPlacements` predicate: `Animal.checkAnimalSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Animal>::check_animal_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new cat at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a cat from persisted base entity state.
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
        let mut entity_data = CatEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let tempting = Arc::new(AtomicBool::new(false));

        {
            // Vanilla re-adds the avoid-players goal at priority 4 whenever the
            // cat's tameness changes; the goal here checks tameness itself,
            // which behaves the same.
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, TamableAnimalPanicGoal::new(1.5));
            goal_selector.add_goal(2, SitWhenOrderedToGoal::new());
            goal_selector.add_goal(3, CatRelaxOnOwnerGoal::new());
            goal_selector.add_goal(4, CatTemptGoal::new(Arc::clone(&tempting)));
            goal_selector.add_goal(4, CatAvoidPlayersGoal::new());
            goal_selector.add_goal(5, CatLieOnBlockGoal::new(LIE_ON_BLOCK_SPEED));
            goal_selector.add_goal(6, FollowOwnerGoal::new(1.0, 10.0, 5.0));
            goal_selector.add_goal(7, CatSitOnBlockGoal::new(SIT_ON_BLOCK_SPEED));
            goal_selector.add_goal(8, LeapAtTargetGoal::new(0.3));
            goal_selector.add_goal(9, OcelotAttackGoal::new());
            goal_selector.add_goal(10, BreedGoal::new(WALK_SPEED_MOD));
            goal_selector.add_goal(
                11,
                WaterAvoidingRandomStrollGoal::with_probability(WALK_SPEED_MOD, 1.000_000_1E-5),
            );
            goal_selector.add_goal(12, LookAtPlayerGoal::new(10.0));

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NonTameRandomTargetGoal::new(false, |target, _| {
                    target.entity_type() == &vanilla_entities::RABBIT
                }),
            );
            target_selector.add_goal(
                1,
                NonTameRandomTargetGoal::new(false, |target, _| is_baby_turtle_on_land(target)),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            tamable_base: TamableAnimalBase::new(),
            tempting,
            entity_data: SyncMutex::new(entity_data),
        }
    }

    /// The cat's coat.
    #[must_use]
    pub fn variant(&self) -> CatVariantRef {
        self.entity_data.lock().variant.get().value()
    }

    /// Sets the cat's coat.
    pub fn set_variant(&self, variant: CatVariantRef) {
        self.entity_data
            .lock()
            .variant
            .set(RegistryReference::new(variant));
    }

    /// The cat's set of sounds.
    #[must_use]
    pub fn sound_variant(&self) -> CatSoundVariantRef {
        self.entity_data.lock().sound_variant.get().value()
    }

    /// Sets the cat's set of sounds.
    pub fn set_sound_variant(&self, sound_variant: CatSoundVariantRef) {
        self.entity_data
            .lock()
            .sound_variant
            .set(RegistryReference::new(sound_variant));
    }

    /// The color of the cat's collar.
    #[must_use]
    pub fn collar_color(&self) -> DyeColor {
        DyeColor::by_id(*self.entity_data.lock().collar_color.get())
    }

    /// Dyes the cat's collar.
    pub fn set_collar_color(&self, color: DyeColor) {
        self.entity_data.lock().collar_color.set(color.id());
    }

    /// Whether the cat is lying down, on a bed or a warm block.
    #[must_use]
    pub fn is_lying(&self) -> bool {
        *self.entity_data.lock().is_lying.get()
    }

    /// Vanilla `setLying`.
    pub fn set_lying(&self, lying: bool) {
        self.entity_data.lock().is_lying.set(lying);
    }

    /// Whether the cat is settling next to its sleeping owner.
    #[must_use]
    pub fn is_relax_state_one(&self) -> bool {
        *self.entity_data.lock().relax_state_one.get()
    }

    fn set_relax_state_one(&self, relaxing: bool) {
        self.entity_data.lock().relax_state_one.set(relaxing);
    }

    fn sound_set(&self) -> &'static CatAge {
        let sound_variant = self.sound_variant();
        if AgeableMob::is_baby(self) {
            &sound_variant.baby_sounds
        } else {
            &sound_variant.adult_sounds
        }
    }

    /// Vanilla `hiss`, used by cats that scare phantoms off.
    pub fn hiss(&self) {
        self.play_sound(
            self.sound_set().hiss_sound,
            self.sound_volume(),
            self.voice_pitch(),
        );
    }

    /// Vanilla `tryToTame`: one fish in three tames the cat, which then sits.
    fn try_to_tame(&self, player: &Player) {
        if rand::random_range(0..3) == 0 {
            self.tame(player);
            self.set_ordered_to_sit(true);
            self.broadcast_entity_event(EntityStatus::TamingSucceeded);
        } else {
            self.broadcast_entity_event(EntityStatus::TamingFailed);
        }
    }

    fn set_variant_by_key(&self, key: &Identifier) {
        if let Some(variant) = REGISTRY.cat_variants.by_key(key) {
            self.set_variant(variant);
        }
    }

    fn set_sound_variant_by_key(&self, key: &Identifier) {
        if let Some(sound_variant) = REGISTRY.cat_sound_variants.by_key(key) {
            self.set_sound_variant(sound_variant);
        }
    }

    /// Vanilla `VariantUtils.selectVariantToSpawn` with the cat's spawn
    /// conditions: black in witch huts, all black under a bright moon.
    fn select_spawn_variant(&self, world: &World) -> Option<CatVariantRef> {
        let pos = self.block_position();
        let biome = world.biome_at(pos);
        let moon_brightness = world.moon_brightness();
        let mut random = LegacyRandom::from_seed(rand::random());
        REGISTRY.cat_variants.select_spawn_variant(
            |condition| match condition {
                SpawnCondition::Structure { structures } => {
                    world.structure_with_piece_at(structures, pos)
                }
                SpawnCondition::MoonBrightness { min, max } => {
                    SpawnCondition::matches_moon_brightness(*min, *max, moon_brightness)
                }
                SpawnCondition::Biome { biomes } => {
                    biome.is_some_and(|biome| SpawnCondition::matches_biome(biomes, biome))
                }
            },
            &mut random,
        )
    }

    /// Vanilla `Cat.tick`: begs while tempted and purrs while lying down.
    fn tick_cat_sounds(&self) {
        let tick_count = self.tick_count();
        if self.tempting.load(Ordering::Relaxed)
            && !self.is_tame()
            && tick_count % BEG_SOUND_INTERVAL == 0
        {
            self.play_sound(self.sound_set().beg_for_food_sound, 1.0, 1.0);
        }
        if (self.is_lying() || self.is_relax_state_one()) && tick_count % PURR_INTERVAL == 0 {
            let volume = 0.6 + 0.4 * (rand::random::<f32>() - rand::random::<f32>());
            self.play_sound(self.sound_set().purr_sound, volume, 1.0);
        }
    }

    /// Vanilla `mobInteract` for a cat its owner interacts with.
    fn mob_interact_owner(
        &self,
        player: &Player,
        hand: InteractionHand,
        item_stack: &ItemStack,
    ) -> InteractionResult {
        if REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::CAT_COLLAR_DYES)
        {
            if let Some(color) = item_stack.get(DYE).copied()
                && color != self.collar_color()
            {
                self.set_collar_color(color);
                self.use_player_item(player, hand);
                self.set_persistence_required();
                return InteractionResult::Success;
            }
        } else if self.is_food(item_stack) && self.get_health() < self.get_max_health() {
            self.feed(player, hand, 1.0, 1.0);
            return InteractionResult::Success;
        }

        let result = Animal::mob_interact_animal(self, player, hand);
        if result.consumes_action() {
            return result;
        }
        self.set_ordered_to_sit(!self.is_ordered_to_sit());
        InteractionResult::Success
    }
}

impl TamableAnimal for CatEntity {
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
}

impl DefaultEntityPush for CatEntity {}

impl Entity for CatEntity {
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
        if LivingEntity::is_alive(self.as_ref()) {
            self.tick_cat_sounds();
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            CAT_BABY_DIMENSIONS.scale(scale)
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

    fn is_stepping_carefully(&self) -> bool {
        self.is_crouching() || self.is_suppressing_bounce()
    }

    fn considers_entity_as_ally(&self, other: &dyn Entity) -> bool {
        self.considers_entity_as_ally_tamable(other)
    }

    fn is_tame_owned_by(&self, owner: &dyn LivingEntity) -> bool {
        self.is_tame() && self.is_owned_by(owner.as_entity_event_source())
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(CAT_VARIANT) {
            self.set_variant(variant.value());
        }
        if let Some(sound_variant) = item_stack.get(CAT_SOUND_VARIANT) {
            self.set_sound_variant(sound_variant.value());
        }
        if let Some(color) = item_stack.get(CAT_COLLAR) {
            self.set_collar_color(*color);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        self.save_tamable(nbt);
        nbt.insert(TAG_VARIANT, self.variant().key.to_string());
        nbt.insert(TAG_SOUND_VARIANT, self.sound_variant().key.to_string());
        nbt.insert(TAG_COLLAR_COLOR, self.collar_color().id() as i8);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.load_tamable(nbt);
        if let Some(variant) = nbt.string(TAG_VARIANT)
            && let Ok(key) = Identifier::from_str(variant.to_str().as_ref())
        {
            self.set_variant_by_key(&key);
        }
        if let Some(sound_variant) = nbt.string(TAG_SOUND_VARIANT)
            && let Ok(key) = Identifier::from_str(sound_variant.to_str().as_ref())
        {
            self.set_sound_variant_by_key(&key);
        }
        self.set_collar_color(
            nbt.byte(TAG_COLLAR_COLOR)
                .map_or(DyeColor::Red, |color| DyeColor::by_id(i32::from(color))),
        );
    }
}

impl LivingEntity for CatEntity {
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
        Some(self.sound_set().hurt_sound)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(self.sound_set().death_sound)
    }

    fn die(&self, source: &DamageSource) {
        self.announce_death_to_owner(source);
        self.default_die(source);
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

impl AgeableMob for CatEntity {
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

    /// Vanilla `Cat.getBreedOffspring`: the kitten takes a parent's coat, and a
    /// tame parent's owner and a mix of both collars.
    fn initialize_breed_offspring(&self, partner: &dyn AgeableMob, offspring: &dyn AgeableMob) {
        let Some(kitten) = offspring.downcast_ref::<Self>() else {
            return;
        };
        let Some(partner) = partner.downcast_ref::<Self>() else {
            return;
        };
        kitten.set_variant(if rand::random::<bool>() {
            self.variant()
        } else {
            partner.variant()
        });
        if self.is_tame() {
            kitten.set_owner_uuid(self.owner_uuid());
            kitten.set_tame(true, true);
            kitten.set_collar_color(SheepEntity::get_mixed_color(
                self.collar_color(),
                partner.collar_color(),
            ));
        }
    }
}

impl Animal for CatEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        is_cat_food(item_stack)
    }

    fn play_eating_sound(&self) {
        self.play_sound(self.sound_set().eat_sound, 1.0, 1.0);
    }

    fn can_mate(&self, partner: &dyn Animal) -> bool {
        if partner.uuid() == self.uuid() || !self.is_tame() {
            return false;
        }
        let Some(cat) = partner.downcast_ref::<Self>() else {
            return false;
        };
        cat.is_tame() && self.is_in_love() && cat.is_in_love()
    }
}

impl Mob for CatEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `Cat.customServerAiStep`: crouches while tempted and sprints
    /// while fleeing or pouncing, read from the move control's speed.
    fn custom_server_ai_step(&self) {
        let wanted_speed = {
            let controls = self.mob_base.controls().lock();
            (controls.move_control.operation() == MoveControlOperation::MoveTo)
                .then(|| controls.move_control.speed_modifier())
        };
        match wanted_speed {
            Some(speed) if speed == TEMPT_SPEED_MOD => {
                self.set_pose(EntityPose::Sneaking);
                self.set_sprinting(false);
            }
            Some(speed) if speed == SPRINT_SPEED_MOD => {
                self.set_pose(EntityPose::Standing);
                self.set_sprinting(true);
            }
            _ => {
                self.set_pose(EntityPose::Standing);
                self.set_sprinting(false);
            }
        }
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.can_attack_tamable(target)
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        !self.is_tame() && self.tick_count() > DESPAWN_MIN_TICKS
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        let sounds = self.sound_set();
        if !self.is_tame() {
            Some(sounds.stray_ambient_sound)
        } else if self.is_in_love() {
            Some(sounds.purr_sound)
        } else if rand::random_range(0..4) == 0 {
            Some(sounds.purreow_sound)
        } else {
            Some(sounds.ambient_sound)
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        AMBIENT_SOUND_INTERVAL
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = self.finalize_spawn_ageable_mob(world, spawn_reason, group_data);
        if let Some(variant) = self.select_spawn_variant(world) {
            self.set_variant(variant);
        }
        let mut random = LegacyRandom::from_seed(rand::random());
        if let Some(sound_variant) = REGISTRY.cat_sound_variants.pick_random(&mut random) {
            self.set_sound_variant(sound_variant);
        }
        group_data
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if self.is_tame() {
            if self.is_owned_by(player) {
                return self.mob_interact_owner(player, hand, &item_stack);
            }
        } else if self.is_food(&item_stack) {
            self.use_player_item(player, hand);
            self.try_to_tame(player);
            self.set_persistence_required();
            self.play_eating_sound();
            return InteractionResult::Success;
        }

        let result = Animal::mob_interact_animal(self, player, hand);
        if result.consumes_action() {
            self.set_persistence_required();
        }
        result
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for CatEntity {}

#[cfg(test)]
mod tests;
