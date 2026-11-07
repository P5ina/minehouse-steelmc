//! Vanilla `ZombifiedPiglin`: a neutral zombie of the Nether that calls the
//! whole group onto whoever hurts one of them.

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use rand::distr::uniform::SampleUniform;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::ZombifiedPiglinEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_blocks, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey, Identifier, WorldAabb};

use super::{
    ZombieState, ZombieVariant, ZombieVariantHooks, ZombieVariantSaveData, zombie_mob_base,
};
use crate::entity::ai::goal::{
    HurtByTargetGoal, NearestAttackableTargetGoal, ResetUniversalAngerTargetGoal,
    WaterAvoidingRandomStrollGoal, ZombieAttackGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::neutral_mob::{NeutralMob, NeutralMobState};
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySpawnReason, LivingEntity, LivingEntityBase, Mob,
    MobBase, SharedEntity, SpawnGroupData, SpawnRule,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `SPEED_MODIFIER_ATTACKING_ID`.
const SPEED_MODIFIER_ATTACKING_ID: Identifier = Identifier::vanilla_static("attacking");
/// Vanilla `SPEED_MODIFIER_ATTACKING`.
const SPEED_MODIFIER_ATTACKING_AMOUNT: f64 = 0.05;
/// Vanilla `FIRST_ANGER_SOUND_DELAY`: `TimeUtil.rangeOfSeconds(0, 1)`.
const FIRST_ANGER_SOUND_DELAY: (i32, i32) = (0, 20);
/// Vanilla `PERSISTENT_ANGER_TIME`: `TimeUtil.rangeOfSeconds(20, 39)`.
const PERSISTENT_ANGER_TIME: (i64, i64) = (400, 780);
/// Vanilla `ALERT_RANGE_Y`.
const ALERT_RANGE_Y: f64 = 10.0;
/// Vanilla `ALERT_INTERVAL`: `TimeUtil.rangeOfSeconds(4, 6)`.
const ALERT_INTERVAL: (i32, i32) = (80, 120);
/// Vanilla `populateDefaultEquipmentSlots`: one in twenty carry a spear.
const GOLDEN_SPEAR_ONE_IN: i32 = 20;
/// Vanilla `playAngerSound` volume and pitch multipliers.
const ANGER_SOUND_VOLUME: f32 = 2.0;
const ANGER_SOUND_PITCH: f32 = 1.8;
/// Vanilla constructor: `setPathfindingMalus(PathType.LAVA, 8.0F)`.
const LAVA_MALUS: f32 = 8.0;
/// Vanilla `NearestAttackableTargetGoal(this, Player.class, 10, ...)`.
const PLAYER_TARGET_INTERVAL: i32 = 10;

/// Server-only anger timers.
struct ZombifiedPiglinState {
    /// Vanilla `playFirstAngerSoundIn`.
    play_first_anger_sound_in: i32,
    /// Vanilla `ticksUntilNextAlert`.
    ticks_until_next_alert: i32,
}

/// A neutral zombified piglin.
#[entity_behavior(class = "ZombifiedPiglin", spawn_rule)]
pub struct ZombifiedPiglinEntity {
    pub(super) base: EntityBase,
    pub(super) entity_type: EntityTypeRef,
    pub(super) living_base: LivingEntityBase,
    pub(super) mob_base: MobBase,
    pub(super) entity_data: SyncMutex<ZombifiedPiglinEntityData>,
    pub(super) zombie_state: SyncMutex<ZombieState>,
    anger: NeutralMobState,
    piglin_state: SyncMutex<ZombifiedPiglinState>,
}

// SAFETY: The owner-scoped type key uniquely identifies ZombifiedPiglinEntity.
unsafe impl DowncastType for ZombifiedPiglinEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/zombified_piglin");
}

impl ZombifiedPiglinEntity {
    /// Vanilla `SpawnPlacements`: `checkZombifiedPiglinSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_zombified_piglin_spawn_rules;

    /// Vanilla `checkZombifiedPiglinSpawnRules`: anything but peaceful, and
    /// never on nether wart blocks.
    fn check_zombified_piglin_spawn_rules(
        _entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        _spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        level.difficulty() != Difficulty::Peaceful
            && level.get_block_state(pos.below()).get_block() != &vanilla_blocks::NETHER_WART_BLOCK
    }

    /// Creates a new zombified piglin.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved zombified piglin.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = ZombifiedPiglinEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let piglin = Self {
            base,
            entity_type,
            living_base,
            mob_base: zombie_mob_base(ZombieVariant::ZombifiedPiglin),
            entity_data: SyncMutex::new(entity_data),
            zombie_state: SyncMutex::new(ZombieState::new()),
            anger: NeutralMobState::new(),
            piglin_state: SyncMutex::new(ZombifiedPiglinState {
                play_first_anger_sound_in: 0,
                ticks_until_next_alert: 0,
            }),
        };
        piglin.set_pathfinding_malus(PathType::Lava, LAVA_MALUS);
        piglin
    }

    /// Vanilla `Zombie.finalizeSpawn`, with a fresh random.
    pub(super) fn finalize_spawn_zombified_piglin(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut random = LegacyRandom::from_seed(rand::random());
        self.finalize_zombie_spawn(world, &mut random, spawn_reason, group_data)
    }

    /// Vanilla `populateDefaultEquipmentSlots`, as a pig struck by lightning
    /// calls it.
    pub(crate) fn populate_default_equipment(&self) {
        let mut random = LegacyRandom::from_seed(rand::random());
        populate_zombified_piglin_equipment(&self.living_base, &mut random);
    }

    /// Vanilla `customServerAiStep`: the attack speed boost, the first growl,
    /// persistent anger and calling the others.
    pub(super) fn custom_server_ai_step_zombified_piglin(&self) {
        let Some(world) = self.level() else {
            return;
        };
        if self.is_angry() {
            let is_baby = LivingEntity::is_baby(self);
            {
                let mut attributes = self.attributes().lock();
                if !is_baby
                    && !attributes.has_modifier(
                        vanilla_attributes::MOVEMENT_SPEED,
                        &SPEED_MODIFIER_ATTACKING_ID,
                    )
                {
                    let _ = attributes.add_modifier(
                        vanilla_attributes::MOVEMENT_SPEED,
                        AttributeModifier {
                            id: SPEED_MODIFIER_ATTACKING_ID,
                            amount: SPEED_MODIFIER_ATTACKING_AMOUNT,
                            operation: AttributeModifierOperation::AddValue,
                        },
                        false,
                    );
                }
            }
            self.maybe_play_first_anger_sound();
        } else {
            let mut attributes = self.attributes().lock();
            if attributes.has_modifier(
                vanilla_attributes::MOVEMENT_SPEED,
                &SPEED_MODIFIER_ATTACKING_ID,
            ) {
                let _ = attributes.remove_modifier(
                    vanilla_attributes::MOVEMENT_SPEED,
                    &SPEED_MODIFIER_ATTACKING_ID,
                );
            }
        }

        self.update_persistent_anger(&world, true);
        if let Some(target) = self.target() {
            self.maybe_alert_others(&world, &target);
        }
    }

    /// Vanilla `maybePlayFirstAngerSound`.
    fn maybe_play_first_anger_sound(&self) {
        let play = {
            let mut state = self.piglin_state.lock();
            if state.play_first_anger_sound_in > 0 {
                state.play_first_anger_sound_in -= 1;
                state.play_first_anger_sound_in == 0
            } else {
                false
            }
        };
        if play {
            self.play_anger_sound();
        }
    }

    /// Vanilla `maybeAlertOthers`: every few seconds, while it can see its
    /// target.
    fn maybe_alert_others(&self, world: &World, target: &SharedEntity) {
        {
            let mut state = self.piglin_state.lock();
            if state.ticks_until_next_alert > 0 {
                state.ticks_until_next_alert -= 1;
                return;
            }
        }
        if self.has_line_of_sight_cached(target.as_ref()) {
            self.alert_others(world, target);
        }
        self.piglin_state.lock().ticks_until_next_alert = sample(ALERT_INTERVAL);
    }

    /// Vanilla `alertOthers`: idle zombified piglins in follow range take the
    /// same target.
    fn alert_others(&self, world: &World, target: &SharedEntity) {
        let within = self
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(0.0);
        let position = self.position();
        let search_box = WorldAabb::new(
            position.x,
            position.y,
            position.z,
            position.x + 1.0,
            position.y + 1.0,
            position.z + 1.0,
        )
        .inflate_xyz(within, ALERT_RANGE_Y, within);
        let own_id = self.id();
        for other in world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.id() != own_id && !entity.is_spectator()
        }) {
            let Some(other_piglin) = other.downcast_ref::<Self>() else {
                continue;
            };
            if other_piglin.target().is_none() && !other.is_allied_to(target.as_ref()) {
                let _ = other_piglin.set_target(Some(target));
            }
        }
    }

    /// Vanilla `playAngerSound`.
    fn play_anger_sound(&self) {
        self.play_sound(
            &sound_events::ENTITY_ZOMBIFIED_PIGLIN_ANGRY,
            self.sound_volume() * ANGER_SOUND_VOLUME,
            self.voice_pitch() * ANGER_SOUND_PITCH,
        );
    }

    /// Vanilla `setTarget`: a fresh target schedules the growl and the first
    /// call for help.
    pub(super) fn set_target_zombified_piglin(&self, target: Option<&SharedEntity>) -> bool {
        if self.mob_base.target(|_| true).is_none() && target.is_some() {
            let mut state = self.piglin_state.lock();
            state.play_first_anger_sound_in = sample(FIRST_ANGER_SOUND_DELAY);
            state.ticks_until_next_alert = sample(ALERT_INTERVAL);
        }
        self.mob_base()
            .set_target(target, |target| self.is_valid_target(target))
    }
}

/// Vanilla `UniformInt.sample` over an inclusive range.
fn sample<T: SampleUniform + PartialOrd + Copy>((min, max): (T, T)) -> T {
    rand::random_range(min..=max)
}

/// Vanilla `ZombifiedPiglin.populateDefaultEquipmentSlots`: a golden sword,
/// or now and then a golden spear.
pub(super) fn populate_zombified_piglin_equipment(
    living_base: &LivingEntityBase,
    random: &mut impl Random,
) {
    let weapon = if random.next_i32_bounded(GOLDEN_SPEAR_ONE_IN) == 0 {
        &vanilla_items::GOLDEN_SPEAR
    } else {
        &vanilla_items::GOLDEN_SWORD
    };
    let _ = living_base
        .equipment()
        .lock()
        .set(EquipmentSlot::MainHand, ItemStack::new(weapon));
}

/// Vanilla `ZombifiedPiglin.addBehaviourGoals`.
pub(super) fn add_behaviour_goals(mob_base: &MobBase) {
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        // Vanilla priority 1 uses spears; Steel has no `SpearUseGoal` yet.
        goal_selector.add_goal(2, ZombieAttackGoal::new(1.0, false));
        goal_selector.add_goal(7, WaterAvoidingRandomStrollGoal::new(1.0));
    }
    let mut target_selector = mob_base.target_selector().lock();
    target_selector.add_goal(
        1,
        HurtByTargetGoal::new()
            .set_alert_others_matching(ZombieVariant::ZombifiedPiglin.alert_matcher(), []),
    );
    target_selector.add_goal(
        2,
        NearestAttackableTargetGoal::new_for_players_with_interval(
            PLAYER_TARGET_INTERVAL,
            true,
            false,
            |_, _| true,
        )
        .with_mob_selector(|mob, target, world| {
            mob.as_neutral_mob()
                .is_some_and(|piglin| piglin.is_angry_at(target, world))
        }),
    );
    target_selector.add_goal(3, ResetUniversalAngerTargetGoal::new(true));
}

impl NeutralMob for ZombifiedPiglinEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    /// Vanilla `startPersistentAngerTimer`.
    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(sample(PERSISTENT_ANGER_TIME));
    }
}

impl ZombieVariantSaveData for ZombifiedPiglinEntity {
    fn save_variant_data(&self, nbt: &mut NbtCompound) {
        self.add_persistent_anger_save_data(nbt);
    }

    fn load_variant_data(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.read_persistent_anger_save_data(nbt);
    }
}

impl ZombieVariantHooks for ZombifiedPiglinEntity {
    /// Vanilla `getAmbientSound`: it growls while angry.
    fn variant_ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_angry() {
            &sound_events::ENTITY_ZOMBIFIED_PIGLIN_ANGRY
        } else {
            &sound_events::ENTITY_ZOMBIFIED_PIGLIN_AMBIENT
        })
    }

    /// Vanilla `isPreventingPlayerRest`: only while angry at that player.
    fn variant_is_preventing_player_rest(&self, level: &World, player: &Player) -> Option<bool> {
        Some(self.is_angry_at(player, level))
    }
}

#[cfg(test)]
mod tests;
