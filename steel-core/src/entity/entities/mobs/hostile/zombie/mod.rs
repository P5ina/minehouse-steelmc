//! Vanilla `Zombie`, `Husk`, `Drowned` and `Giant`.
//!
//! Zombies and husks share everything but their sounds, sunlight and the
//! mob they turn into after drowning; the drowned swims, throws tridents and
//! never converts. The giant is a plain monster with no AI goals at all.

pub use drowned::DrownedEntity;
pub use zombie_villager::ZombieVillagerEntity;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::{GiantEntityData, HuskEntityData, ZombieEntityData};
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, level_events, sound_events, vanilla_attributes,
    vanilla_entities, vanilla_items, vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::Difficulty;
use steel_utils::{
    BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey, Identifier,
};

use crate::entity::ai::goal::{
    BreakDoorGoal, HurtByTargetGoal, LookAtPlayerGoal, NearestAttackableTargetGoal,
    RandomLookAroundGoal, WaterAvoidingRandomStrollGoal, ZombieAttackGoal,
};
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::conversion::ConversionParams;
use crate::entity::damage::DamageSource;
use crate::entity::entities::{ChickenEntity, VillagerEntity};
use crate::entity::entity::EntityEventSource as _;
use crate::entity::mob::spawn_equipment::{
    populate_default_equipment_enchantments, populate_default_equipment_slots,
};
use crate::entity::spawn::create_entity_instance;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    MobEffectInstance, Monster, PathfinderMob, RangedAttackMob, SharedEntity, SpawnGroupData,
    SpawnRule, sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::{DifficultyInstance, ServerLevelAccessor, World};

const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const XP_REWARD: i32 = 5;
/// Vanilla `Zombie.getBaseExperienceReward` multiplies a baby's reward.
const BABY_XP_MULTIPLIER: f64 = 2.5;
const STEP_SOUND_VOLUME: f32 = 0.15;

const SPEED_MODIFIER_BABY_ID: Identifier = Identifier::vanilla_static("baby");
const SPEED_MODIFIER_BABY_AMOUNT: f64 = 0.5;
const RANDOM_SPAWN_BONUS_ID: Identifier = Identifier::vanilla_static("random_spawn_bonus");
const LEADER_ZOMBIE_BONUS_ID: Identifier = Identifier::vanilla_static("leader_zombie_bonus");
const ZOMBIE_RANDOM_SPAWN_BONUS_ID: Identifier =
    Identifier::vanilla_static("zombie_random_spawn_bonus");

/// Vanilla `Zombie.ZOMBIE_LEADER_CHANCE`.
const ZOMBIE_LEADER_CHANCE: f32 = 0.05;
/// Vanilla `Zombie.BREAK_DOOR_CHANCE`.
const BREAK_DOOR_CHANCE: f32 = 0.1;
/// Vanilla `Zombie.getSpawnAsBabyOdds`.
const BABY_SPAWN_CHANCE: f32 = 0.05;
const CHICKEN_JOCKEY_CHANCE: f64 = 0.05;
const PICK_UP_LOOT_CHANCE: f32 = 0.55;
const WEAPON_CHANCE_HARD: f32 = 0.05;
const WEAPON_CHANCE: f32 = 0.01;
const HALLOWEEN_PUMPKIN_CHANCE: f32 = 0.25;
const HALLOWEEN_LANTERN_CHANCE: f32 = 0.1;
const FIRE_ATTACK_CHANCE_PER_DIFFICULTY: f32 = 0.3;
/// Vanilla `Husk.doHurtTarget`: hunger ticks per whole point of regional difficulty.
const HUSK_HUNGER_TICKS_PER_DIFFICULTY: i32 = 140;

/// Vanilla `Zombie.drowningTracker`: ticks underwater before converting.
const DROWNING_AFFLICTION_TICKS: i32 = 600;
/// Vanilla `Zombie.drowningTracker`: ticks of shaking before the conversion.
const DROWNING_CONVERSION_TICKS: i32 = 300;
const NOT_CONVERTING: i32 = -1;

const BABY_WIDTH: f32 = 0.49;
const BABY_HEIGHT: f32 = 0.98;
const ZOMBIE_BABY_EYE_HEIGHT: f32 = 0.775;
const HUSK_BABY_EYE_HEIGHT: f32 = 0.825;
const ZOMBIE_VILLAGER_BABY_EYE_HEIGHT: f32 = 0.67;
const ZOMBIE_VILLAGER_BABY_VEHICLE_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.125, 0.0)];
const BABY_VEHICLE_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.1875, 0.0)];
const ZOMBIE_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    BABY_WIDTH,
    BABY_HEIGHT,
    ZOMBIE_BABY_EYE_HEIGHT,
    EntityAttachments::new(&[], &BABY_VEHICLE_ATTACHMENTS, &[], &[]),
);
/// Vanilla `ZombieVillager.BABY_DIMENSIONS`.
const ZOMBIE_VILLAGER_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    BABY_WIDTH,
    BABY_HEIGHT,
    ZOMBIE_VILLAGER_BABY_EYE_HEIGHT,
    EntityAttachments::new(&[], &ZOMBIE_VILLAGER_BABY_VEHICLE_ATTACHMENTS, &[], &[]),
);
const HUSK_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    BABY_WIDTH,
    BABY_HEIGHT,
    HUSK_BABY_EYE_HEIGHT,
    EntityAttachments::new(&[], &BABY_VEHICLE_ATTACHMENTS, &[], &[]),
);

/// Vanilla `Zombie.ZombieGroupData`, shared by a spawn group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZombieGroupData {
    /// Whether the zombies in this group spawn as babies.
    pub is_baby: bool,
    /// Whether a baby may become a chicken jockey.
    pub can_spawn_jockey: bool,
}

/// Which `Zombie` subclass an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZombieVariant {
    Zombie,
    Husk,
    Drowned,
    ZombieVillager,
}

impl ZombieVariant {
    const fn baby_dimensions(self) -> EntityDimensions {
        match self {
            Self::Zombie | Self::Drowned => ZOMBIE_BABY_DIMENSIONS,
            Self::Husk => HUSK_BABY_DIMENSIONS,
            Self::ZombieVillager => ZOMBIE_VILLAGER_BABY_DIMENSIONS,
        }
    }

    /// `in_water` picks the drowned's underwater sounds.
    fn ambient_sound(self, in_water: bool) -> SoundEventRef {
        match self {
            Self::Zombie => &sound_events::ENTITY_ZOMBIE_AMBIENT,
            Self::Husk => &sound_events::ENTITY_HUSK_AMBIENT,
            Self::Drowned if in_water => &sound_events::ENTITY_DROWNED_AMBIENT_WATER,
            Self::Drowned => &sound_events::ENTITY_DROWNED_AMBIENT,
            Self::ZombieVillager => &sound_events::ENTITY_ZOMBIE_VILLAGER_AMBIENT,
        }
    }

    fn hurt_sound(self, in_water: bool) -> SoundEventRef {
        match self {
            Self::Zombie => &sound_events::ENTITY_ZOMBIE_HURT,
            Self::Husk => &sound_events::ENTITY_HUSK_HURT,
            Self::Drowned if in_water => &sound_events::ENTITY_DROWNED_HURT_WATER,
            Self::Drowned => &sound_events::ENTITY_DROWNED_HURT,
            Self::ZombieVillager => &sound_events::ENTITY_ZOMBIE_VILLAGER_HURT,
        }
    }

    fn death_sound(self, in_water: bool) -> SoundEventRef {
        match self {
            Self::Zombie => &sound_events::ENTITY_ZOMBIE_DEATH,
            Self::Husk => &sound_events::ENTITY_HUSK_DEATH,
            Self::Drowned if in_water => &sound_events::ENTITY_DROWNED_DEATH_WATER,
            Self::Drowned => &sound_events::ENTITY_DROWNED_DEATH,
            Self::ZombieVillager => &sound_events::ENTITY_ZOMBIE_VILLAGER_DEATH,
        }
    }

    fn step_sound(self) -> SoundEventRef {
        match self {
            Self::Zombie => &sound_events::ENTITY_ZOMBIE_STEP,
            Self::Husk => &sound_events::ENTITY_HUSK_STEP,
            Self::Drowned => &sound_events::ENTITY_DROWNED_STEP,
            Self::ZombieVillager => &sound_events::ENTITY_ZOMBIE_VILLAGER_STEP,
        }
    }

    /// Vanilla `Zombie.convertsInWater`: the drowned has nothing to become
    /// and the zombie villager overrides it off.
    const fn converts_in_water(self) -> bool {
        !matches!(self, Self::Drowned | Self::ZombieVillager)
    }

    /// Vanilla `convertsToWhenDrowning`.
    fn converts_to_when_drowning(self) -> EntityTypeRef {
        match self {
            Self::Zombie | Self::Drowned | Self::ZombieVillager => &vanilla_entities::DROWNED,
            Self::Husk => &vanilla_entities::ZOMBIE,
        }
    }

    /// Vanilla `getConversionSound`.
    const fn conversion_sound(self) -> i32 {
        match self {
            Self::Zombie | Self::Drowned | Self::ZombieVillager => {
                level_events::SOUND_ZOMBIE_TO_DROWNED
            }
            Self::Husk => level_events::SOUND_HUSK_TO_ZOMBIE,
        }
    }

    /// Vanilla `HurtByTargetGoal.alertOthers` matches the hurt mob's class and
    /// its subclasses: a zombie alerts every zombie kind, while a husk, a
    /// drowned or a zombie villager only alerts its own kind.
    fn alert_matcher(self) -> fn(&dyn Entity) -> bool {
        match self {
            Self::Zombie => is_zombie_family,
            Self::Husk => |entity| entity.entity_type() == &vanilla_entities::HUSK,
            Self::Drowned => |entity| entity.entity_type() == &vanilla_entities::DROWNED,
            Self::ZombieVillager => {
                |entity| entity.entity_type() == &vanilla_entities::ZOMBIE_VILLAGER
            }
        }
    }
}

/// Entity types whose vanilla class is `Zombie` or a subclass of it, minus
/// the zombified piglin that vanilla `setAlertOthers(ZombifiedPiglin.class)` excludes.
fn is_zombie_family(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::ZOMBIE
        || entity_type == &vanilla_entities::HUSK
        || entity_type == &vanilla_entities::DROWNED
        || entity_type == &vanilla_entities::ZOMBIE_VILLAGER
}

/// Vanilla `Zombie.DOOR_BREAKING_PREDICATE`.
fn door_breaking_difficulty(difficulty: Difficulty) -> bool {
    difficulty == Difficulty::Hard
}

/// Vanilla `SpecialDates.isHalloween`: October 31st in the server's time zone.
pub(super) fn is_halloween() -> bool {
    use chrono::Datelike as _;
    let today = chrono::Local::now();
    today.month() == 10 && today.day() == 31
}

/// Server-only zombie state that is not synced to clients.
struct ZombieState {
    can_break_doors: bool,
    /// Vanilla `ConversionTracker.afflictionTime`: ticks spent with eyes underwater.
    in_water_time: i32,
    /// Vanilla `ConversionTracker.conversionTime`.
    conversion_time: i32,
    /// A chicken (or, for a drowned, a zombie nautilus) picked at spawn that
    /// this zombie mounts once it is in the world.
    ///
    /// Vanilla calls `startRiding` inside `finalizeSpawn`, before the zombie
    /// joins the level; Steel can only link riders that are already in it.
    pending_jockey_mount: Option<SharedEntity>,
    /// Vanilla `Drowned.searchingForLand`.
    searching_for_land: bool,
    /// Vanilla `Drowned.rangedAttackUncertainty`; negative uses the default.
    ranged_attack_uncertainty: f32,
}

impl ZombieState {
    const fn new() -> Self {
        Self {
            can_break_doors: false,
            in_water_time: 0,
            conversion_time: 0,
            pending_jockey_mount: None,
            searching_for_land: false,
            ranged_attack_uncertainty: -1.0,
        }
    }
}

/// Builds the vanilla `Zombie.registerGoals` mob base.
fn zombie_mob_base(variant: ZombieVariant) -> MobBase {
    let mob_base = MobBase::new();
    mob_base.set_xp_reward(XP_REWARD);
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        goal_selector.add_goal(8, LookAtPlayerGoal::new(8.0));
        goal_selector.add_goal(8, RandomLookAroundGoal::new());
    }
    if variant == ZombieVariant::Drowned {
        drowned::add_behaviour_goals(&mob_base);
        return mob_base;
    }
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        // Vanilla priority 4 attacks turtle eggs; priority 2 uses spears and
        // priority 6 moves through villages. Steel has none of these goals yet.
        goal_selector.add_goal(3, ZombieAttackGoal::new(1.0, false));
        goal_selector.add_goal(7, WaterAvoidingRandomStrollGoal::new(1.0));

        let mut target_selector = mob_base.target_selector().lock();
        target_selector.add_goal(
            1,
            HurtByTargetGoal::new().set_alert_others_matching(variant.alert_matcher(), []),
        );
        target_selector.add_goal(
            2,
            NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
        );
        target_selector.add_goal(
            3,
            NearestAttackableTargetGoal::new(false, |target, _| {
                target.entity_type() == &vanilla_entities::VILLAGER
                    || target.entity_type() == &vanilla_entities::WANDERING_TRADER
            }),
        );
        target_selector.add_goal(
            3,
            NearestAttackableTargetGoal::new(true, |target, _| {
                target.entity_type() == &vanilla_entities::IRON_GOLEM
            }),
        );
        // Vanilla `Turtle.BABY_ON_LAND_SELECTOR`.
        target_selector.add_goal(
            5,
            NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
                target.entity_type() == &vanilla_entities::TURTLE
                    && target.is_baby()
                    && !target.is_in_water()
            }),
        );
    }
    mob_base
}

/// Vanilla `Zombie.handleAttributes`, usable on any zombie-like mob.
fn handle_zombie_attributes<M: Mob + ?Sized>(
    mob: &M,
    random: &mut impl Random,
    difficulty_modifier: f32,
    spawn_reason: EntitySpawnReason,
) -> bool {
    let mut attributes = mob.attributes().lock();
    // Vanilla `randomizeReinforcementsChance`.
    attributes.set_base_value(
        vanilla_attributes::SPAWN_REINFORCEMENTS,
        random.next_f64() * f64::from(0.1_f32),
    );
    attributes.set_modifier(
        vanilla_attributes::KNOCKBACK_RESISTANCE,
        AttributeModifier {
            id: RANDOM_SPAWN_BONUS_ID,
            amount: random.next_f64() * f64::from(0.05_f32),
            operation: AttributeModifierOperation::AddValue,
        },
        true,
    );
    let follow_range_modifier = random.next_f64() * 1.5 * f64::from(difficulty_modifier);
    if follow_range_modifier > 1.0 {
        attributes.set_modifier(
            vanilla_attributes::FOLLOW_RANGE,
            AttributeModifier {
                id: ZOMBIE_RANDOM_SPAWN_BONUS_ID,
                amount: follow_range_modifier,
                operation: AttributeModifierOperation::AddMultipliedTotal,
            },
            true,
        );
    }

    if random.next_f32() >= difficulty_modifier * ZOMBIE_LEADER_CHANCE {
        return false;
    }
    attributes.set_modifier(
        vanilla_attributes::SPAWN_REINFORCEMENTS,
        AttributeModifier {
            id: LEADER_ZOMBIE_BONUS_ID,
            amount: random.next_f64() * 0.25 + 0.5,
            operation: AttributeModifierOperation::AddValue,
        },
        true,
    );
    attributes.set_modifier(
        vanilla_attributes::MAX_HEALTH,
        AttributeModifier {
            id: LEADER_ZOMBIE_BONUS_ID,
            amount: random.next_f64() * 3.0 + 1.0,
            operation: AttributeModifierOperation::AddMultipliedTotal,
        },
        true,
    );
    drop(attributes);
    if !matches!(
        spawn_reason,
        EntitySpawnReason::Conversion
            | EntitySpawnReason::Load
            | EntitySpawnReason::DimensionTravel
    ) {
        mob.set_health(mob.get_max_health());
    }
    true
}

/// The save data a `Zombie` subclass adds on top of the zombie's own, for
/// vanilla `addAdditionalSaveData` / `readAdditionalSaveData` overrides.
trait ZombieVariantSaveData {
    fn save_variant_data(&self, _nbt: &mut NbtCompound) {}

    fn load_variant_data(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}
}

/// Implements the entity traits zombies and husks share, mirroring vanilla `Zombie`.
///
/// `$finalize` is the entity's own `finalizeSpawn`.
macro_rules! impl_zombie_traits {
    ($entity:ty, $variant:expr, $finalize:ident) => {
        impl_zombie_traits!($entity, $variant, $finalize, entity {}, living {}, mob {});
    };
    (
        $entity:ty,
        $variant:expr,
        $finalize:ident,
        entity { $($entity_extra:tt)* },
        living { $($living_extra:tt)* },
        mob { $($mob_extra:tt)* }
    ) => {
        impl $entity {
            const VARIANT: ZombieVariant = $variant;

            /// Vanilla `Zombie.canBreakDoors`.
            #[must_use]
            pub fn can_break_doors(&self) -> bool {
                self.zombie_state.lock().can_break_doors
            }

            /// Vanilla `Zombie.setCanBreakDoors`: adds or removes the
            /// door-breaking goal and lets the navigation path through doors.
            pub fn set_can_break_doors(&self, can_break_doors: bool) {
                {
                    let mut state = self.zombie_state.lock();
                    if state.can_break_doors == can_break_doors {
                        return;
                    }
                    state.can_break_doors = can_break_doors;
                }
                self.mob_base
                    .navigation()
                    .lock()
                    .set_can_open_doors(can_break_doors);
                if can_break_doors {
                    self.mob_base
                        .goal_selector()
                        .lock()
                        .add_goal(1, BreakDoorGoal::new(door_breaking_difficulty));
                } else {
                    self.remove_all_goals(|goal| goal.is_break_door_goal());
                }
            }

            /// Vanilla `Zombie.isUnderWaterConverting`.
            #[must_use]
            pub fn is_under_water_converting(&self) -> bool {
                *self.entity_data.lock().zombie().drowned_conversion.get()
            }

            fn set_under_water_converting(&self, converting: bool) {
                self.entity_data
                    .lock()
                    .zombie_mut()
                    .drowned_conversion
                    .set(converting);
            }

            /// Vanilla `ConversionTracker.startConversion`.
            fn start_drowned_conversion(&self, time: i32) {
                self.zombie_state.lock().conversion_time = time;
                self.set_under_water_converting(true);
            }

            /// Vanilla `Zombie.setInWaterTime`.
            pub fn set_in_water_time(&self, in_water_time: i32) {
                self.zombie_state.lock().in_water_time = in_water_time;
            }

            /// Vanilla `Zombie.setConversionTime`.
            pub fn set_conversion_time(&self, conversion_time: i32) {
                self.zombie_state.lock().conversion_time = conversion_time;
            }

            /// Vanilla `ConversionTracker.tick` for the drowning conversion.
            fn tick_drowning(&self) {
                if self.level().is_none() || !LivingEntity::is_alive(self) || self.is_no_ai() {
                    return;
                }
                if !self.is_eye_in_water() {
                    self.zombie_state.lock().in_water_time = NOT_CONVERTING;
                    self.set_under_water_converting(false);
                    return;
                }
                if self.is_under_water_converting() {
                    let convert = {
                        let mut state = self.zombie_state.lock();
                        state.conversion_time -= 1;
                        state.conversion_time < 0
                    };
                    if convert {
                        self.do_drowned_conversion();
                    }
                } else {
                    let start = {
                        let mut state = self.zombie_state.lock();
                        state.in_water_time += 1;
                        state.in_water_time >= DROWNING_AFFLICTION_TICKS
                    };
                    if start {
                        self.start_drowned_conversion(DROWNING_CONVERSION_TICKS);
                    }
                }
            }

            /// Vanilla `ConversionTracker.doConversion`, then the zombie's
            /// post-conversion `handleAttributes`.
            fn do_drowned_conversion(&self) {
                let Some(world) = self.level() else {
                    return;
                };
                let Some(converted) = self.convert_to(
                    Self::VARIANT.converts_to_when_drowning(),
                    ConversionParams::single(true, true),
                    None,
                ) else {
                    return;
                };
                let difficulty = world
                    .current_difficulty_at(converted.block_position())
                    .special_multiplier();
                let mut random = LegacyRandom::from_seed(rand::random());
                if let Some(zombie) = converted.downcast_ref::<ZombieEntity>() {
                    zombie.handle_attributes(
                        &mut random,
                        difficulty,
                        EntitySpawnReason::Conversion,
                    );
                } else if let Some(husk) = converted.downcast_ref::<HuskEntity>() {
                    husk.handle_attributes(&mut random, difficulty, EntitySpawnReason::Conversion);
                } else if let Some(drowned) = converted.downcast_ref::<DrownedEntity>() {
                    drowned.handle_attributes(
                        &mut random,
                        difficulty,
                        EntitySpawnReason::Conversion,
                    );
                }
                if !converted.is_silent() {
                    world.level_event(
                        Self::VARIANT.conversion_sound(),
                        converted.block_position(),
                        0,
                        None,
                    );
                }
            }

            /// Vanilla `Zombie.handleAttributes`.
            fn handle_attributes(
                &self,
                random: &mut impl Random,
                difficulty_modifier: f32,
                spawn_reason: EntitySpawnReason,
            ) {
                if handle_zombie_attributes(self, random, difficulty_modifier, spawn_reason) {
                    self.set_can_break_doors(true);
                }
            }

            /// Vanilla `Zombie.populateDefaultEquipmentSlots`: armor, then a
            /// rare iron weapon.
            fn populate_zombie_equipment(
                &self,
                world: &World,
                random: &mut impl Random,
                difficulty: DifficultyInstance,
            ) {
                if Self::VARIANT == ZombieVariant::Drowned {
                    // Vanilla `Drowned.populateDefaultEquipmentSlots` replaces
                    // the zombie's armor and iron weapon.
                    drowned::populate_drowned_equipment(&self.living_base, random);
                    return;
                }
                populate_default_equipment_slots(self, world, random, difficulty);
                let weapon_chance = if world.difficulty() == Difficulty::Hard {
                    WEAPON_CHANCE_HARD
                } else {
                    WEAPON_CHANCE
                };
                if random.next_f32() < weapon_chance {
                    let weapon = match random.next_i32_bounded(6) {
                        0 => &vanilla_items::IRON_SWORD,
                        1 => &vanilla_items::IRON_SPEAR,
                        _ => &vanilla_items::IRON_SHOVEL,
                    };
                    let _ = self
                        .living_base
                        .equipment()
                        .lock()
                        .set(EquipmentSlot::MainHand, ItemStack::new(weapon));
                }
            }

            /// Vanilla `Zombie.finalizeSpawn`.
            fn finalize_zombie_spawn(
                &self,
                world: &Arc<World>,
                random: &mut impl Random,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                let difficulty = world.current_difficulty_at(self.block_position());
                let group_data = self.finalize_spawn_mob_base(world, spawn_reason, group_data);
                let difficulty_modifier = difficulty.special_multiplier();
                if spawn_reason != EntitySpawnReason::Conversion {
                    self.set_can_pick_up_loot(
                        random.next_f32() < PICK_UP_LOOT_CHANCE * difficulty_modifier,
                    );
                }

                let group_data = group_data.or_else(|| {
                    Some(SpawnGroupData::Zombie(ZombieGroupData {
                        is_baby: random.next_f32() < BABY_SPAWN_CHANCE,
                        can_spawn_jockey: true,
                    }))
                });

                if let Some(SpawnGroupData::Zombie(zombie_data)) = group_data {
                    if zombie_data.is_baby {
                        Mob::set_baby(self, true);
                        if zombie_data.can_spawn_jockey {
                            self.pick_chicken_jockey_mount(world, random, spawn_reason);
                        }
                    }
                    self.set_can_break_doors(
                        random.next_f32() < difficulty_modifier * BREAK_DOOR_CHANCE,
                    );
                    if spawn_reason != EntitySpawnReason::Conversion {
                        self.populate_zombie_equipment(world, random, difficulty);
                        populate_default_equipment_enchantments(self, random, difficulty);
                    }
                }

                if !self.has_item_in_slot(EquipmentSlot::Head)
                    && is_halloween()
                    && random.next_f32() < HALLOWEEN_PUMPKIN_CHANCE
                {
                    let head = if random.next_f32() < HALLOWEEN_LANTERN_CHANCE {
                        &vanilla_items::JACK_O_LANTERN
                    } else {
                        &vanilla_items::CARVED_PUMPKIN
                    };
                    let _ = self
                        .living_base
                        .equipment()
                        .lock()
                        .set(EquipmentSlot::Head, ItemStack::new(head));
                    self.set_equipment_drop_chance(EquipmentSlot::Head, 0.0);
                }

                self.handle_attributes(random, difficulty_modifier, spawn_reason);
                group_data
            }

            /// The chicken-jockey half of vanilla `Zombie.finalizeSpawn`: a
            /// nearby unridden chicken, or else a freshly spawned one.
            fn pick_chicken_jockey_mount(
                &self,
                world: &Arc<World>,
                random: &mut impl Random,
                spawn_reason: EntitySpawnReason,
            ) {
                if f64::from(random.next_f32()) < CHICKEN_JOCKEY_CHANCE {
                    let search_box = self.bounding_box().inflate_xyz(5.0, 3.0, 5.0);
                    let chicken = world
                        .get_entities_in_aabb_matching(&search_box, |entity| {
                            entity.entity_type() == &vanilla_entities::CHICKEN
                                && entity.is_alive()
                                && !entity.is_vehicle()
                                && !entity.is_passenger()
                        })
                        .into_iter()
                        .next();
                    if let Some(chicken) = chicken {
                        if let Some(chicken_ref) = chicken.downcast_ref::<ChickenEntity>() {
                            chicken_ref.set_chicken_jockey(true);
                        }
                        self.zombie_state.lock().pending_jockey_mount = Some(chicken);
                    }
                } else if f64::from(random.next_f32()) < CHICKEN_JOCKEY_CHANCE {
                    let Ok(chicken) =
                        create_entity_instance(world, &vanilla_entities::CHICKEN, self.position())
                    else {
                        return;
                    };
                    let (yaw, _) = self.rotation();
                    chicken.set_rotation((yaw, 0.0));
                    if let Some(chicken_mob) = chicken.as_mob() {
                        let _ = chicken_mob.finalize_spawn(world, EntitySpawnReason::Jockey, None);
                    }
                    if let Some(chicken_ref) = chicken.downcast_ref::<ChickenEntity>() {
                        chicken_ref.set_chicken_jockey(true);
                    }
                    let _ = spawn_reason;
                    if world.try_add_entity(Arc::clone(&chicken)).is_ok() {
                        self.zombie_state.lock().pending_jockey_mount = Some(chicken);
                    }
                }
            }

            fn update_dirty_mob_effect_entity_data(&self) {
                sync_dirty_mob_effects(&self.living_base, &self.entity_data);
            }
        }

        impl DefaultEntityPush for $entity {}

        impl Entity for $entity {
            fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
                Some(self)
            }

            fn base(&self) -> &EntityBase {
                &self.base
            }

            fn entity_type(&self) -> EntityTypeRef {
                self.entity_type
            }

            fn movement_emission(&self) -> EntityMovementEmission {
                EntityMovementEmission::All
            }

            fn tick(self: Arc<Self>) {
                let mount = self.zombie_state.lock().pending_jockey_mount.take();
                if let Some(mount) = mount
                    && !mount.is_removed()
                {
                    let _ = self.start_riding(&mount);
                }
                let entity = Arc::clone(&self).into_shared_entity();
                if let Some(living) = entity.as_living_entity() {
                    living.tick_living_entity(&entity);
                }
                if !self.is_removed() && Self::VARIANT.converts_in_water() {
                    self.tick_drowning();
                }
            }

            fn base_tick(&self) {
                Mob::base_tick_mob(self);
            }

            fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
                let scale = LivingEntity::get_scale(self);
                if LivingEntity::is_baby(self) {
                    Self::VARIANT.baby_dimensions().scale(scale)
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
                Monster::sound_source_monster(self)
            }

            fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
                Monster::fall_sounds_monster(self)
            }

            fn swim_sound(&self) -> SoundEventRef {
                if Self::VARIANT == ZombieVariant::Drowned {
                    &sound_events::ENTITY_DROWNED_SWIM
                } else {
                    Monster::swim_sound_monster(self)
                }
            }

            fn swim_splash_sound(&self) -> SoundEventRef {
                Monster::swim_splash_sound_monster(self)
            }

            fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
                self.play_sound(Self::VARIANT.step_sound(), STEP_SOUND_VOLUME, 1.0);
            }

            $($entity_extra)*

            /// Vanilla `Zombie.killedEntity`: a villager killed on Normal or
            /// Hard may come back as a zombie villager instead of dying.
            fn killed_entity(
                &self,
                world: &World,
                entity: &dyn LivingEntity,
                _source: &DamageSource,
            ) -> bool {
                !infect_villager(self, world, entity)
            }

            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_mob(nbt);
                nbt.insert("IsBaby", i8::from(LivingEntity::is_baby(self)));
                nbt.insert("CanBreakDoors", i8::from(self.can_break_doors()));
                let converting = self.is_under_water_converting();
                let eye_in_water = self.is_eye_in_water();
                let state = self.zombie_state.lock();
                // Vanilla `ConversionTracker.addAdditionalSaveData`.
                nbt.insert(
                    "DrownedConversionTime",
                    if converting {
                        state.conversion_time
                    } else {
                        NOT_CONVERTING
                    },
                );
                nbt.insert(
                    "InWaterTime",
                    if eye_in_water {
                        state.in_water_time
                    } else {
                        NOT_CONVERTING
                    },
                );
                drop(state);
                ZombieVariantSaveData::save_variant_data(self, nbt);
            }

            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_mob(nbt);
                Mob::set_baby(self, nbt.byte("IsBaby").is_some_and(|value| value != 0));
                self.set_can_break_doors(nbt.byte("CanBreakDoors").is_some_and(|value| value != 0));
                // Vanilla `ConversionTracker.readAdditionalSaveData`.
                self.zombie_state.lock().in_water_time = nbt.int("InWaterTime").unwrap_or(0);
                let conversion_time = nbt.int("DrownedConversionTime").unwrap_or(NOT_CONVERTING);
                if conversion_time == NOT_CONVERTING {
                    self.set_under_water_converting(false);
                } else {
                    self.start_drowned_conversion(conversion_time);
                }
                ZombieVariantSaveData::load_variant_data(self, nbt);
            }
        }

        impl LivingEntity for $entity {
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

            fn is_baby(&self) -> bool {
                *self.entity_data.lock().zombie().baby.get()
            }

            fn base_experience_reward(&self) -> i32 {
                if LivingEntity::is_baby(self) {
                    let xp_reward = self.mob_base.xp_reward();
                    self.mob_base
                        .set_xp_reward((f64::from(xp_reward) * BABY_XP_MULTIPLIER) as i32);
                }
                Mob::base_experience_reward_mob(self)
            }

            fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
                Some(Self::VARIANT.hurt_sound(self.is_in_water()))
            }

            fn death_sound(&self) -> Option<SoundEventRef> {
                Some(Self::VARIANT.death_sound(self.is_in_water()))
            }

            $($living_extra)*

            fn should_drop_experience(&self) -> bool {
                Monster::should_drop_experience_monster(self)
            }

            fn should_drop_loot(&self, world: &World) -> bool {
                Monster::should_drop_loot_monster(self, world)
            }

            fn server_ai_step(&self, entity: &SharedEntity) {
                Mob::mob_server_ai_step(self, entity);
            }

            fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
                Monster::monster_ai_step(self, entity)
            }
        }

        impl Monster for $entity {}

        impl PathfinderMob for $entity {}

        impl Mob for $entity {
            fn mob_base(&self) -> &MobBase {
                &self.mob_base
            }

            fn tick_goal_selectors(&self, entity: &SharedEntity) {
                PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
            }

            fn tick_path_navigation(&self) {
                PathfinderMob::tick_pathfinder_path_navigation(self);
            }

            fn ambient_sound(&self) -> Option<SoundEventRef> {
                Some(Self::VARIANT.ambient_sound(self.is_in_water()))
            }

            $($mob_extra)*

            fn mob_flags(&self) -> i8 {
                *self.entity_data.lock().mob().mob_flags.get()
            }

            fn set_mob_flags(&self, flags: i8) {
                self.entity_data.lock().mob_mut().mob_flags.set(flags);
            }

            fn finalize_spawn(
                &self,
                world: &Arc<World>,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                self.$finalize(world, spawn_reason, group_data)
            }

            /// Vanilla `Zombie.setBaby`: the synced flag plus a transient
            /// `+50%` speed modifier.
            fn set_baby(&self, baby: bool) {
                let changed = {
                    let mut entity_data = self.entity_data.lock();
                    let changed = *entity_data.zombie().baby.get() != baby;
                    entity_data.zombie_mut().baby.set(baby);
                    changed
                };
                {
                    let mut attributes = self.attributes().lock();
                    let _ = attributes.remove_modifier(
                        vanilla_attributes::MOVEMENT_SPEED,
                        &SPEED_MODIFIER_BABY_ID,
                    );
                    if baby {
                        let _ = attributes.add_modifier(
                            vanilla_attributes::MOVEMENT_SPEED,
                            AttributeModifier {
                                id: SPEED_MODIFIER_BABY_ID,
                                amount: SPEED_MODIFIER_BABY_AMOUNT,
                                operation: AttributeModifierOperation::AddMultipliedBase,
                            },
                            false,
                        );
                    }
                }
                // Vanilla `onSyncedDataUpdated(DATA_BABY_ID)`.
                if changed {
                    self.refresh_dimensions();
                }
            }

            fn can_hold_item(&self, item_stack: &ItemStack) -> bool {
                !(REGISTRY.items.is_in_tag(item_stack.item(), &ItemTag::EGGS)
                    && LivingEntity::is_baby(self)
                    && self.is_passenger())
            }

            fn wants_to_pick_up(&self, item_stack: &ItemStack) -> bool {
                // Vanilla `Drowned.wantsToPickUp` refuses spears.
                if Self::VARIANT == ZombieVariant::Drowned
                    && REGISTRY.items.is_in_tag(item_stack.item(), &ItemTag::SPEARS)
                {
                    return false;
                }
                !item_stack.is(&vanilla_items::GLOW_INK_SAC) && self.can_hold_item(item_stack)
            }

            fn do_hurt_target(
                &self,
                entity: &SharedEntity,
                world: &World,
                target: &SharedEntity,
            ) -> bool {
                if !self.default_do_hurt_target(entity, world, target) {
                    return false;
                }
                let difficulty = world
                    .current_difficulty_at(self.block_position())
                    .effective_difficulty();
                let main_hand_empty = !self.has_item_in_slot(EquipmentSlot::MainHand);
                if main_hand_empty
                    && self.is_on_fire()
                    && rand::random::<f32>() < difficulty * FIRE_ATTACK_CHANCE_PER_DIFFICULTY
                {
                    target.ignite_for_ticks(2 * (difficulty as i32) * 20);
                }
                if Self::VARIANT == ZombieVariant::Husk
                    && main_hand_empty
                    && let Some(living) = target.as_living_entity()
                {
                    let _ = living.add_mob_effect_with_source(
                        MobEffectInstance::with_duration(
                            vanilla_mob_effects::HUNGER,
                            HUSK_HUNGER_TICKS_PER_DIFFICULTY * (difficulty as i32),
                            0,
                        ),
                        Some(self),
                    );
                }
                true
            }
        }
    };
}

mod drowned;

impl_zombie_traits!(
    DrownedEntity,
    ZombieVariant::Drowned,
    finalize_spawn_drowned,
    entity {
        /// Vanilla `Drowned.isPushedByFluid`.
        fn is_pushed_by_fluid(&self) -> bool {
            !self.is_swimming()
        }

        /// Vanilla `Drowned.updateSwimming`.
        fn update_swimming(&self) {
            self.set_shared_swimming(
                self.is_effective_ai() && self.is_under_water() && self.wants_to_swim(),
            );
        }

        fn as_ranged_attack_mob(&self) -> Option<&dyn RangedAttackMob> {
            Some(self)
        }
    },
    living {
        fn travel_in_water(
            &self,
            entity: &SharedEntity,
            input: DVec3,
            base_gravity: f64,
            is_falling: bool,
            old_y: f64,
        ) -> Option<MoveResult> {
            self.travel_in_water_drowned(entity, input, base_gravity, is_falling, old_y)
        }
    },
    mob {
        fn tick_move_control(&self) {
            self.tick_drowned_move_control();
        }

        /// Vanilla `Drowned.canReplaceCurrentItem`: never drops a nautilus shell.
        fn can_replace_current_item(
            &self,
            new_item_stack: &ItemStack,
            current_item_stack: &ItemStack,
            slot: EquipmentSlot,
        ) -> bool {
            !current_item_stack.is(&vanilla_items::NAUTILUS_SHELL)
                && self.default_can_replace_current_item(new_item_stack, current_item_stack, slot)
        }

        /// Vanilla `Drowned.checkSpawnObstruction`: water is fine.
        fn check_spawn_obstruction(
            &self,
            level: &dyn ServerLevelAccessor,
        ) -> bool {
            level.is_unobstructed_by_entities(self.as_entity_event_source())
        }
    }
);

/// A hostile zombie.
#[entity_behavior(class = "Zombie", spawn_rule)]
pub struct ZombieEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<ZombieEntityData>,
    zombie_state: SyncMutex<ZombieState>,
}

// SAFETY: The owner-scoped type key uniquely identifies ZombieEntity.
unsafe impl DowncastType for ZombieEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/zombie");
}

impl ZombieEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new zombie.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved zombie.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = ZombieEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: zombie_mob_base(ZombieVariant::Zombie),
            entity_data: SyncMutex::new(entity_data),
            zombie_state: SyncMutex::new(ZombieState::new()),
        }
    }
}

impl_zombie_traits!(ZombieEntity, ZombieVariant::Zombie, finalize_spawn_zombie);

impl ZombieVariantSaveData for ZombieEntity {}

impl ZombieEntity {
    /// Vanilla `Zombie.finalizeSpawn`, with a fresh random.
    fn finalize_spawn_zombie(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut random = LegacyRandom::from_seed(rand::random());
        self.finalize_zombie_spawn(world, &mut random, spawn_reason, group_data)
    }
}

/// A hostile husk: a desert zombie that does not burn and starves its targets.
#[entity_behavior(class = "Husk", spawn_rule)]
pub struct HuskEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<HuskEntityData>,
    zombie_state: SyncMutex<ZombieState>,
}

// SAFETY: The owner-scoped type key uniquely identifies HuskEntity.
unsafe impl DowncastType for HuskEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/husk");
}

impl HuskEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkSurfaceMonstersSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_surface_monsters_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
            random,
        )
    };
}

impl HuskEntity {
    /// Creates a new husk.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved husk.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = HuskEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: zombie_mob_base(ZombieVariant::Husk),
            entity_data: SyncMutex::new(entity_data),
            zombie_state: SyncMutex::new(ZombieState::new()),
        }
    }
}

impl_zombie_traits!(HuskEntity, ZombieVariant::Husk, finalize_spawn_husk);

impl ZombieVariantSaveData for HuskEntity {}

impl ZombieVariantSaveData for DrownedEntity {}

mod zombie_villager;

/// The villager half of vanilla `Zombie.killedEntity`: returns whether the
/// killed entity was converted, in which case it does not perish.
fn infect_villager(zombie: &dyn Entity, world: &World, victim: &dyn LivingEntity) -> bool {
    let difficulty = world.difficulty();
    if !matches!(difficulty, Difficulty::Normal | Difficulty::Hard) {
        return false;
    }
    let victim: &dyn Entity = victim;
    let Some(villager) = victim.downcast_ref::<VillagerEntity>() else {
        return false;
    };
    let mut random = LegacyRandom::from_seed(rand::random());
    if difficulty != Difficulty::Hard && random.next_bool() {
        return false;
    }
    convert_villager_to_zombie_villager(zombie, villager)
}

/// Vanilla `Zombie.convertVillagerToZombieVillager`.
fn convert_villager_to_zombie_villager(zombie: &dyn Entity, villager: &VillagerEntity) -> bool {
    let after_conversion = |converted: &dyn Mob| {
        let converted: &dyn Entity = converted;
        let Some(zombie_villager) = converted.downcast_ref::<ZombieVillagerEntity>() else {
            return;
        };
        let Some(world) = zombie_villager.level() else {
            return;
        };
        zombie_villager.set_villager_data_finalized(villager.villager_data_finalized());
        zombie_villager.finalize_spawn(
            &world,
            EntitySpawnReason::Conversion,
            Some(SpawnGroupData::Zombie(ZombieGroupData {
                is_baby: false,
                can_spawn_jockey: true,
            })),
        );
        zombie_villager.set_villager_data(
            villager.villager_type(),
            villager.profession(),
            villager.villager_level(),
        );
        zombie_villager.set_gossips(villager.gossips());
        zombie_villager.set_trade_offers(Some(villager.with_offers(|offers| offers.clone())));
        zombie_villager.set_villager_xp(villager.villager_xp());
        if !zombie.is_silent() {
            world.level_event(
                level_events::SOUND_ZOMBIE_INFECTED,
                zombie.block_position(),
                0,
                None,
            );
        }
    };
    villager
        .convert_to(
            &vanilla_entities::ZOMBIE_VILLAGER,
            ConversionParams::single(true, true),
            Some(&after_conversion),
        )
        .is_some()
}

impl_zombie_traits!(
    ZombieVillagerEntity,
    ZombieVariant::ZombieVillager,
    finalize_spawn_zombie_villager,
    entity {},
    living {
        /// Vanilla `ZombieVillager.getVoicePitch`.
        fn voice_pitch(&self) -> f32 {
            self.voice_pitch_zombie_villager()
        }
    },
    mob {
        /// Vanilla `ZombieVillager.removeWhenFarAway`.
        fn remove_when_far_away(&self, _distance_sqr: f64) -> bool {
            self.remove_when_far_away_zombie_villager()
        }
    }
);

impl HuskEntity {
    /// Vanilla `Husk.finalizeSpawn`: the zombie spawn, then a second loot
    /// pickup roll. Vanilla also tries a camel husk mount with a parched
    /// rider for natural spawns; camel husks are not implemented in Steel yet.
    fn finalize_spawn_husk(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut random = LegacyRandom::from_seed(rand::random());
        let group_data = self.finalize_zombie_spawn(world, &mut random, spawn_reason, group_data);
        let difficulty_modifier = world
            .current_difficulty_at(self.block_position())
            .special_multiplier();
        if spawn_reason != EntitySpawnReason::Conversion {
            self.set_can_pick_up_loot(
                random.next_f32() < PICK_UP_LOOT_CHANCE * difficulty_modifier,
            );
        }
        group_data
    }
}

/// A giant: a huge zombie with no AI goals, only reachable through commands.
#[entity_behavior(class = "Giant", spawn_rule)]
pub struct GiantEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<GiantEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies GiantEntity.
unsafe impl DowncastType for GiantEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/giant");
}

impl GiantEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new giant.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved giant.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = GiantEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let mob_base = MobBase::new();
        mob_base.set_xp_reward(XP_REWARD);
        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
        }
    }
}

impl DefaultEntityPush for GiantEntity {}

impl Entity for GiantEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::All
    }

    fn tick(self: Arc<Self>) {
        let entity = self.into_shared_entity();
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
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn max_up_step(&self) -> f32 {
        self.attributes()
            .lock()
            .get_value(vanilla_attributes::STEP_HEIGHT)
            .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
    }

    fn sound_source(&self) -> SoundSource {
        Monster::sound_source_monster(self)
    }

    fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        Monster::fall_sounds_monster(self)
    }

    fn swim_sound(&self) -> SoundEventRef {
        Monster::swim_sound_monster(self)
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        Monster::swim_splash_sound_monster(self)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
    }
}

impl LivingEntity for GiantEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn hurt_sound(&self, source: &DamageSource) -> Option<SoundEventRef> {
        Monster::hurt_sound_monster(self, source)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Monster::death_sound_monster(self)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Monster::monster_ai_step(self, entity)
    }
}

impl Monster for GiantEntity {}

impl PathfinderMob for GiantEntity {}

impl Mob for GiantEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

#[cfg(test)]
mod tests;
