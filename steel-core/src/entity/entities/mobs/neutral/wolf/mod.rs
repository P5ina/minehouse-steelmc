//! Vanilla wolf: a neutral pack hunter that players tame with bones, dye
//! collars for and dress in wolf armor.

mod goals;

use std::str::FromStr;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::{
    DYE, WOLF_COLLAR, WOLF_SOUND_VARIANT, WOLF_VARIANT,
};
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::item_stack_template::ItemStackTemplate;
use steel_registry::particle_type::{ItemParticleOption, ParticleData};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::WolfEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::wolf_sound_variant::{WolfAge, WolfSoundVariantRef};
use steel_registry::wolf_variant::WolfVariantRef;
use steel_registry::{
    DyeColor, REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _, sound_events,
    vanilla_attributes, vanilla_entities, vanilla_game_events, vanilla_items,
    vanilla_particle_types,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::InteractionHand;
use steel_utils::{
    BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey, Identifier,
};
use uuid::Uuid;

use goals::{BegGoal, WolfAvoidLlamaGoal};

use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    BreedGoal, FloatGoal, FollowOwnerGoal, HurtByTargetGoal, LeapAtTargetGoal, LookAtPlayerGoal,
    MeleeAttackGoal, NearestAttackableTargetGoal, NonTameRandomTargetGoal, OwnerTargetGoal,
    RandomLookAroundGoal, ResetUniversalAngerTargetGoal, SitWhenOrderedToGoal,
    TamableAnimalPanicGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entities::SheepEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::neutral_mob::{NeutralMob, NeutralMobState};
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    TamableAnimal, TamableAnimalBase, WolfPackData, sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

const WOLF_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.4375, 0.0)];
const WOLF_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.3,
    0.425,
    0.343_75,
    EntityAttachments::new(&WOLF_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const START_HEALTH: f64 = 8.0;
const TAME_HEALTH: f32 = 40.0;
/// Below this health a tame wolf whines instead of panting.
const WHINE_HEALTH: f32 = 20.0;
/// Vanilla `ARMOR_REPAIR_UNIT`: one scute repairs an eighth of the armor.
const ARMOR_REPAIR_UNIT: f32 = 0.125;
/// Vanilla `PERSISTENT_ANGER_TIME`: 20 to 39 seconds.
const PERSISTENT_ANGER_TICKS_MIN: i64 = 20 * 20;
const PERSISTENT_ANGER_TICKS_MAX: i64 = 39 * 20;
const WOLF_SOUND_VOLUME: f32 = 0.4;
const SITTING_MAX_HEAD_X_ROT: f32 = 20.0;
const MAX_SPAWN_CLUSTER_SIZE: i32 = 8;
const SHAKE_ANIM_STEP: f32 = 0.05;
const SHAKE_ANIM_END: f32 = 2.0;
/// Vanilla `Crackiness.WOLF_ARMOR` thresholds of remaining durability.
const ARMOR_CRACK_LOW: f32 = 0.95;
const ARMOR_CRACK_MEDIUM: f32 = 0.69;
const ARMOR_CRACK_HIGH: f32 = 0.32;
const TAG_COLLAR_COLOR: &str = "CollarColor";
const TAG_VARIANT: &str = "variant";
const TAG_SOUND_VARIANT: &str = "sound_variant";

#[entity_behavior(class = "Wolf", spawn_rule)]
/// Vanilla wolf.
pub struct WolfEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    tamable_base: TamableAnimalBase,
    anger: NeutralMobState,
    shake: SyncMutex<WolfShake>,
    entity_data: SyncMutex<WolfEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `WolfEntity`.
unsafe impl DowncastType for WolfEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/wolf");
}

/// Vanilla `isWet`, `isShaking` and the shake progress the server counts to
/// time the shake sound.
#[derive(Default)]
struct WolfShake {
    is_wet: bool,
    is_shaking: bool,
    shake_anim: f32,
    shake_anim_o: f32,
}

impl WolfShake {
    const fn cancel(&mut self) {
        self.is_shaking = false;
        self.shake_anim = 0.0;
        self.shake_anim_o = 0.0;
    }
}

/// Vanilla `Crackiness` levels of wolf armor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmorCrackiness {
    None,
    Low,
    Medium,
    High,
}

/// Vanilla `Crackiness.WOLF_ARMOR.byDamage`.
fn armor_crackiness(damage: i32, max_damage: i32) -> ArmorCrackiness {
    let fraction = (max_damage - damage) as f32 / max_damage as f32;
    if fraction < ARMOR_CRACK_HIGH {
        ArmorCrackiness::High
    } else if fraction < ARMOR_CRACK_MEDIUM {
        ArmorCrackiness::Medium
    } else if fraction < ARMOR_CRACK_LOW {
        ArmorCrackiness::Low
    } else {
        ArmorCrackiness::None
    }
}

/// Vanilla `Wolf.PREY_SELECTOR`.
fn is_prey(target: &dyn LivingEntity) -> bool {
    let entity_type = target.entity_type();
    entity_type == &vanilla_entities::SHEEP
        || entity_type == &vanilla_entities::RABBIT
        || entity_type == &vanilla_entities::FOX
}

/// Vanilla `Turtle.BABY_ON_LAND_SELECTOR` on turtles.
fn is_baby_turtle_on_land(target: &dyn LivingEntity) -> bool {
    target.entity_type() == &vanilla_entities::TURTLE
        && target.as_ageable_mob().is_some_and(AgeableMob::is_baby)
        && !target.is_in_water()
}

/// Vanilla `instanceof AbstractSkeleton`.
fn is_abstract_skeleton(target: &dyn LivingEntity) -> bool {
    let entity_type = target.entity_type();
    entity_type == &vanilla_entities::SKELETON
        || entity_type == &vanilla_entities::STRAY
        || entity_type == &vanilla_entities::BOGGED
        || entity_type == &vanilla_entities::WITHER_SKELETON
        || entity_type == &vanilla_entities::PARCHED
}

/// Vanilla `Wolf.checkWolfSpawnRules`.
fn check_wolf_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    level
        .get_block_state(pos.below())
        .get_block()
        .has_tag(&BlockTag::WOLVES_SPAWNABLE_ON)
        && <WolfEntity as Animal>::is_bright_enough_to_spawn(level, pos)
}

fn panic_causes(_mob: &dyn PathfinderMob) -> Identifier {
    DamageTypeTag::PANIC_ENVIRONMENTAL_CAUSES
}

impl WolfEntity {
    /// Vanilla `SpawnPlacements` predicate: `Wolf.checkWolfSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_wolf_spawn_rules;

    /// Creates a new wolf at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a wolf from persisted base entity state.
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
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::PowderSnow, -1.0);
            malus.set(PathType::OnTopOfPowderSnow, -1.0);
        }
        let mut entity_data = WolfEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(
                1,
                TamableAnimalPanicGoal::new(1.5).with_panic_causes(panic_causes),
            );
            goal_selector.add_goal(2, SitWhenOrderedToGoal::new());
            goal_selector.add_goal(3, WolfAvoidLlamaGoal::new());
            goal_selector.add_goal(4, LeapAtTargetGoal::new(0.4));
            goal_selector.add_goal(5, MeleeAttackGoal::new(1.0, true));
            goal_selector.add_goal(6, FollowOwnerGoal::new(1.0, 10.0, 2.0));
            goal_selector.add_goal(7, BreedGoal::new(1.0));
            goal_selector.add_goal(8, WaterAvoidingRandomStrollGoal::new(1.0));
            goal_selector.add_goal(9, BegGoal::new(8.0));
            goal_selector.add_goal(10, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(10, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, OwnerTargetGoal::owner_hurt_by());
            target_selector.add_goal(2, OwnerTargetGoal::owner_hurt());
            target_selector.add_goal(3, HurtByTargetGoal::new().set_alert_others([]));
            target_selector.add_goal(
                4,
                NearestAttackableTargetGoal::new_for_players_with_interval(
                    10,
                    true,
                    false,
                    |_, _| true,
                )
                .with_mob_selector(|mob, target, world| {
                    mob.as_neutral_mob()
                        .is_some_and(|wolf| wolf.is_angry_at(target, world))
                }),
            );
            target_selector.add_goal(
                5,
                NonTameRandomTargetGoal::new(false, |target, _| is_prey(target)),
            );
            target_selector.add_goal(
                6,
                NonTameRandomTargetGoal::new(false, |target, _| is_baby_turtle_on_land(target)),
            );
            target_selector.add_goal(
                7,
                NearestAttackableTargetGoal::new(false, |target, _| is_abstract_skeleton(target)),
            );
            target_selector.add_goal(8, ResetUniversalAngerTargetGoal::new(true));
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            tamable_base: TamableAnimalBase::new(),
            anger: NeutralMobState::new(),
            shake: SyncMutex::new(WolfShake::default()),
            entity_data: SyncMutex::new(entity_data),
        }
    }

    /// The wolf's coat.
    #[must_use]
    pub fn variant(&self) -> WolfVariantRef {
        self.entity_data.lock().variant.get().value()
    }

    /// Sets the wolf's coat.
    pub fn set_variant(&self, variant: WolfVariantRef) {
        self.entity_data
            .lock()
            .variant
            .set(RegistryReference::new(variant));
    }

    /// The wolf's set of sounds.
    #[must_use]
    pub fn sound_variant(&self) -> WolfSoundVariantRef {
        self.entity_data.lock().sound_variant.get().value()
    }

    /// Sets the wolf's set of sounds.
    pub fn set_sound_variant(&self, sound_variant: WolfSoundVariantRef) {
        self.entity_data
            .lock()
            .sound_variant
            .set(RegistryReference::new(sound_variant));
    }

    /// The color of the wolf's collar.
    #[must_use]
    pub fn collar_color(&self) -> DyeColor {
        DyeColor::by_id(*self.entity_data.lock().collar_color.get())
    }

    /// Dyes the wolf's collar.
    pub fn set_collar_color(&self, color: DyeColor) {
        self.entity_data.lock().collar_color.set(color.id());
    }

    /// Whether the wolf is begging, tilting its head at a player.
    #[must_use]
    pub fn is_interested(&self) -> bool {
        *self.entity_data.lock().interested.get()
    }

    /// Vanilla `setIsInterested`.
    pub fn set_is_interested(&self, interested: bool) {
        self.entity_data.lock().interested.set(interested);
    }

    fn sound_set(&self) -> &'static WolfAge {
        let sound_variant = self.sound_variant();
        if AgeableMob::is_baby(self) {
            &sound_variant.baby_sounds
        } else {
            &sound_variant.adult_sounds
        }
    }

    fn body_armor_item(&self) -> ItemStack {
        self.equipment_in_slot(EquipmentSlot::Body)
    }

    fn is_wearing_body_armor(&self) -> bool {
        !self.body_armor_item().is_empty()
    }

    /// Vanilla `canArmorAbsorb`.
    fn can_armor_absorb(&self, source: &DamageSource) -> bool {
        self.body_armor_item().is(&vanilla_items::WOLF_ARMOR)
            && !source.is(&DamageTypeTag::BYPASSES_WOLF_ARMOR)
    }

    /// Vanilla `tryToTame`: one bone in three tames the wolf, which then sits.
    fn try_to_tame(&self, player: &Player) {
        if rand::random_range(0..3) == 0 {
            self.tame(player);
            self.mob_base.navigation().lock().stop();
            let _ = self.set_target(None);
            self.set_ordered_to_sit(true);
            self.broadcast_entity_event(EntityStatus::TamingSucceeded);
        } else {
            self.broadcast_entity_event(EntityStatus::TamingFailed);
        }
    }

    fn set_variant_by_key(&self, key: &Identifier) {
        if let Some(variant) = REGISTRY.wolf_variants.by_key(key) {
            self.set_variant(variant);
        }
    }

    fn set_sound_variant_by_key(&self, key: &Identifier) {
        if let Some(sound_variant) = REGISTRY.wolf_sound_variants.by_key(key) {
            self.set_sound_variant(sound_variant);
        }
    }

    fn pick_random_sound_variant(&self) {
        let mut random = LegacyRandom::from_seed(rand::random());
        if let Some(sound_variant) = REGISTRY.wolf_sound_variants.pick_random(&mut random) {
            self.set_sound_variant(sound_variant);
        }
    }

    /// The wet-shake part of vanilla `Wolf.tick`, run on the server.
    fn tick_shake(&self) {
        let in_water_or_rain = self.is_in_water_or_rain();
        let mut shake = self.shake.lock();
        if in_water_or_rain {
            shake.is_wet = true;
            if shake.is_shaking {
                drop(shake);
                self.broadcast_entity_event(EntityStatus::CancelShakeWetness);
                self.shake.lock().cancel();
            }
            return;
        }
        if !shake.is_shaking {
            return;
        }
        let play_shake_sound = shake.shake_anim == 0.0;
        shake.shake_anim_o = shake.shake_anim;
        shake.shake_anim += SHAKE_ANIM_STEP;
        if shake.shake_anim_o >= SHAKE_ANIM_END {
            shake.is_wet = false;
            shake.cancel();
        }
        drop(shake);
        if play_shake_sound {
            let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0;
            self.play_sound(&sound_events::ENTITY_WOLF_SHAKE, self.sound_volume(), pitch);
            self.game_event(&vanilla_game_events::ENTITY_ACTION);
        }
    }

    /// The wet-shake start of vanilla `Wolf.aiStep`.
    fn start_shake_if_wet(&self) {
        let start = {
            let shake = self.shake.lock();
            shake.is_wet && !shake.is_shaking
        } && !self.is_path_finding()
            && self.on_ground();
        if start {
            {
                let mut shake = self.shake.lock();
                shake.is_shaking = true;
                shake.shake_anim = 0.0;
                shake.shake_anim_o = 0.0;
            }
            self.broadcast_entity_event(EntityStatus::ShakeWetness);
        }
    }

    /// Vanilla `mobInteract` for a tame wolf, before the `Animal` fallback.
    fn mob_interact_tame(
        &self,
        player: &Player,
        hand: InteractionHand,
        item_stack: &ItemStack,
    ) -> Option<InteractionResult> {
        if self.is_food(item_stack) && self.get_health() < self.get_max_health() {
            self.feed(player, hand, 2.0, 2.0);
            return Some(InteractionResult::Success);
        }

        let owned = self.is_owned_by(player);
        if REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::WOLF_COLLAR_DYES)
            && owned
        {
            let color = item_stack.get(DYE).copied();
            if let Some(color) = color
                && color != self.collar_color()
            {
                self.set_collar_color(color);
                self.use_player_item(player, hand);
                return Some(InteractionResult::Success);
            }
            return None;
        }

        if self.is_equippable_in_slot(item_stack, EquipmentSlot::Body)
            && !self.is_wearing_body_armor()
            && owned
            && !AgeableMob::is_baby(self)
        {
            let armor = item_stack.copy_with_count(1);
            self.living_base()
                .equipment()
                .lock()
                .set(EquipmentSlot::Body, armor.copy_with_count(1));
            self.on_equip_item(EquipmentSlot::Body, &ItemStack::empty(), &armor);
            self.set_guaranteed_drop(EquipmentSlot::Body);
            self.use_player_item(player, hand);
            return Some(InteractionResult::Success);
        }

        let armor = self.body_armor_item();
        if self.is_in_sitting_pose()
            && !armor.is_empty()
            && owned
            && armor.is_damaged()
            && armor.is_valid_repair_item(item_stack)
        {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
            self.play_sound(&sound_events::ITEM_WOLF_ARMOR_REPAIR, 1.0, 1.0);
            let repair_unit = (armor.get_max_damage() as f32 * ARMOR_REPAIR_UNIT) as i32;
            self.with_equipment_slot_mut(EquipmentSlot::Body, &mut |armor| {
                let repaired = (armor.get_damage_value() - repair_unit).max(0);
                armor.set_damage_value(repaired);
            });
            return Some(InteractionResult::Success);
        }

        let result = Animal::mob_interact_animal(self, player, hand);
        if !result.consumes_action() && owned {
            self.set_ordered_to_sit(!self.is_ordered_to_sit());
            self.set_jumping(false);
            self.mob_base.navigation().lock().stop();
            let _ = self.set_target(None);
            return Some(InteractionResult::SuccessWithoutItem);
        }
        Some(result)
    }
}

impl NeutralMob for WolfEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(rand::random_range(
            PERSISTENT_ANGER_TICKS_MIN..=PERSISTENT_ANGER_TICKS_MAX,
        ));
    }

    fn persistent_anger_end_time(&self) -> i64 {
        *self.entity_data.lock().anger_end_time.get()
    }

    fn set_persistent_anger_end_time(&self, end_time: i64) {
        self.entity_data.lock().anger_end_time.set(end_time);
    }
}

impl TamableAnimal for WolfEntity {
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

    fn apply_taming_side_effects(&self) {
        let tame = self.is_tame();
        self.attributes().lock().set_base_value(
            vanilla_attributes::MAX_HEALTH,
            if tame {
                f64::from(TAME_HEALTH)
            } else {
                START_HEALTH
            },
        );
        if tame {
            self.set_health(TAME_HEALTH);
        }
    }

    fn wants_to_attack(&self, target: &dyn LivingEntity, owner: &dyn LivingEntity) -> bool {
        let target_type = target.entity_type();
        if target_type == &vanilla_entities::CREEPER
            || target_type == &vanilla_entities::GHAST
            || target_type == &vanilla_entities::ARMOR_STAND
        {
            return false;
        }
        if let Some(wolf) = target.downcast_ref::<Self>() {
            return !wolf.is_tame()
                || wolf
                    .owner()
                    .is_none_or(|wolf_owner| wolf_owner.uuid() != owner.uuid());
        }
        // Vanilla also spares players their owner's team protects from
        // friendly fire; Steel's teams carry no friendly-fire setting, so
        // `Player.canHarmPlayer` always allows the attack.
        if target.is_tamed_horse() {
            return false;
        }
        !target
            .as_tamable_animal()
            .is_some_and(TamableAnimal::is_tame)
    }
}

impl DefaultEntityPush for WolfEntity {}

impl Entity for WolfEntity {
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
            self.tick_shake();
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            WOLF_BABY_DIMENSIONS.scale(scale)
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

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(self.sound_set().step_sound, 0.15, 1.0);
    }

    fn considers_entity_as_ally(&self, other: &dyn Entity) -> bool {
        self.considers_entity_as_ally_tamable(other)
    }

    fn is_tame_owned_by(&self, owner: &dyn LivingEntity) -> bool {
        self.is_tame() && self.is_owned_by(owner.as_entity_event_source())
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(WOLF_VARIANT) {
            self.set_variant(variant.value());
        }
        if let Some(sound_variant) = item_stack.get(WOLF_SOUND_VARIANT) {
            self.set_sound_variant(sound_variant.value());
        }
        if let Some(color) = item_stack.get(WOLF_COLLAR) {
            self.set_collar_color(*color);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        self.save_tamable(nbt);
        nbt.insert(TAG_COLLAR_COLOR, self.collar_color().id() as i8);
        nbt.insert(TAG_VARIANT, self.variant().key.to_string());
        self.add_persistent_anger_save_data(nbt);
        nbt.insert(TAG_SOUND_VARIANT, self.sound_variant().key.to_string());
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
        self.set_collar_color(
            nbt.byte(TAG_COLLAR_COLOR)
                .map_or(DyeColor::Red, |color| DyeColor::by_id(i32::from(color))),
        );
        self.read_persistent_anger_save_data(nbt);
        if let Some(sound_variant) = nbt.string(TAG_SOUND_VARIANT)
            && let Ok(key) = Identifier::from_str(sound_variant.to_str().as_ref())
        {
            self.set_sound_variant_by_key(&key);
        }
    }
}

impl LivingEntity for WolfEntity {
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

    fn sound_volume(&self) -> f32 {
        WOLF_SOUND_VOLUME
    }

    fn hurt_sound(&self, source: &DamageSource) -> Option<SoundEventRef> {
        if self.can_armor_absorb(source) {
            Some(&sound_events::ITEM_WOLF_ARMOR_DAMAGE)
        } else {
            Some(self.sound_set().hurt_sound)
        }
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(self.sound_set().death_sound)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_invulnerable_to(world, source) {
            return false;
        }
        self.set_ordered_to_sit(false);
        self.default_hurt_server(world, source, amount)
    }

    fn actually_hurt(&self, world: &World, source: &DamageSource, amount: f32) {
        if !self.can_armor_absorb(source) {
            self.default_actually_hurt(world, source, amount);
            return;
        }
        let mut crackiness_before = ArmorCrackiness::None;
        let mut crackiness_after = ArmorCrackiness::None;
        let mut broke = false;
        let mut armor_item = &*vanilla_items::WOLF_ARMOR;
        let has_infinite_materials = self.has_infinite_materials();
        self.with_equipment_slot_mut(EquipmentSlot::Body, &mut |armor| {
            armor_item = armor.item;
            crackiness_before = armor_crackiness(armor.get_damage_value(), armor.get_max_damage());
            broke = armor.hurt_and_break(amount.ceil() as i32, has_infinite_materials);
            crackiness_after = if armor.is_empty() {
                ArmorCrackiness::None
            } else {
                armor_crackiness(armor.get_damage_value(), armor.get_max_damage())
            };
        });
        if broke {
            self.on_equipped_item_broken(armor_item, EquipmentSlot::Body);
        }
        if crackiness_before != crackiness_after {
            self.play_sound(&sound_events::ITEM_WOLF_ARMOR_CRACK, 1.0, 1.0);
            let position = self.position();
            world.send_particles(
                ParticleData::new(
                    &vanilla_particle_types::ITEM,
                    ItemParticleOption::new(ItemStackTemplate::new(
                        &vanilla_items::ARMADILLO_SCUTE,
                    )),
                ),
                DVec3::new(position.x, position.y + 1.0, position.z),
                20,
                DVec3::new(0.2, 0.1, 0.2),
                0.1,
            );
        }
    }

    fn hurt_armor(&self, source: &DamageSource, damage: f32) {
        self.do_hurt_equipment(source, damage, &[EquipmentSlot::Body]);
    }

    fn die(&self, source: &DamageSource) {
        {
            let mut shake = self.shake.lock();
            shake.is_wet = false;
            shake.cancel();
        }
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
        self.start_shake_if_wet();
        if let Some(world) = self.level() {
            self.update_persistent_anger(&world, true);
        }
        result
    }
}

impl AgeableMob for WolfEntity {
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

    /// Vanilla `Wolf.getBreedOffspring`: the pup takes a parent's coat, and a
    /// tame parent's owner and a mix of both collars.
    fn initialize_breed_offspring(&self, partner: &dyn AgeableMob, offspring: &dyn AgeableMob) {
        let Some(pup) = offspring.downcast_ref::<Self>() else {
            return;
        };
        let Some(partner) = partner.downcast_ref::<Self>() else {
            return;
        };
        pup.set_variant(if rand::random::<bool>() {
            self.variant()
        } else {
            partner.variant()
        });
        if self.is_tame() {
            pup.set_owner_uuid(self.owner_uuid());
            pup.set_tame(true, true);
            pup.set_collar_color(SheepEntity::get_mixed_color(
                self.collar_color(),
                partner.collar_color(),
            ));
        }
        pup.pick_random_sound_variant();
    }
}

impl Animal for WolfEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::WOLF_FOOD)
    }

    fn can_mate(&self, partner: &dyn Animal) -> bool {
        if partner.uuid() == self.uuid() || !self.is_tame() {
            return false;
        }
        let Some(wolf) = partner.downcast_ref::<Self>() else {
            return false;
        };
        wolf.is_tame() && !wolf.is_in_sitting_pose() && self.is_in_love() && wolf.is_in_love()
    }
}

impl Mob for WolfEntity {
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

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.can_attack_tamable(target)
    }

    fn max_head_x_rot(&self) -> f32 {
        if self.is_in_sitting_pose() {
            SITTING_MAX_HEAD_X_ROT
        } else {
            40.0
        }
    }

    fn max_spawn_cluster_size(&self) -> i32 {
        MAX_SPAWN_CLUSTER_SIZE
    }

    fn mob_can_be_leashed(&self) -> bool {
        !self.is_angry()
    }

    fn can_shear_equipment(&self, player: &Player) -> bool {
        self.is_owned_by(player)
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        let sounds = self.sound_set();
        if self.is_angry() {
            Some(sounds.growl_sound)
        } else if rand::random_range(0..3) == 0 {
            if self.is_tame() && self.get_health() < WHINE_HEALTH {
                Some(sounds.whine_sound)
            } else {
                Some(sounds.pant_sound)
            }
        } else {
            Some(sounds.ambient_sound)
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let pack = if let Some(SpawnGroupData::WolfPack(pack)) = group_data {
            self.set_variant(pack.variant);
            Some(pack)
        } else {
            let mut random = LegacyRandom::from_seed(rand::random());
            world
                .biome_at(self.block_position())
                .and_then(|biome| {
                    REGISTRY
                        .wolf_variants
                        .select_spawn_variant(biome, &mut random)
                })
                .map(|variant| {
                    self.set_variant(variant);
                    WolfPackData::new(variant)
                })
        };
        self.pick_random_sound_variant();

        let Some(pack) = pack else {
            return self.finalize_spawn_ageable_mob(world, spawn_reason, group_data);
        };
        // `WolfPackData` is an `AgeableMobGroupData`; spawn the pack member as
        // one and hand the pack back so the next wolf shares its variant.
        match self.finalize_spawn_ageable_mob(
            world,
            spawn_reason,
            Some(SpawnGroupData::AgeableMob(pack.ageable)),
        ) {
            Some(SpawnGroupData::AgeableMob(ageable)) => {
                Some(SpawnGroupData::WolfPack(WolfPackData { ageable, ..pack }))
            }
            other => other,
        }
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if self.is_tame() {
            if let Some(result) = self.mob_interact_tame(player, hand, &item_stack) {
                return result;
            }
        } else if item_stack.is(&vanilla_items::BONE) && !self.is_angry() {
            self.use_player_item(player, hand);
            self.try_to_tame(player);
            return InteractionResult::SuccessServer;
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

impl PathfinderMob for WolfEntity {}

#[cfg(test)]
mod tests;
