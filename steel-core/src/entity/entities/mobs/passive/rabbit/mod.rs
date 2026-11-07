//! Vanilla rabbit (`Rabbit`).
//!
//! Rabbits move only in hops: the move control sets the speed for the next
//! hop, and the custom AI step starts a hop whenever the rabbit is on the
//! ground and wants to move. They raid fully grown carrots, flee players,
//! wolves and monsters, and the rare killer bunny attacks instead.

mod goals;

use std::borrow::Cow;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::RABBIT_VARIANT;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::entity_variant::RabbitVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::RabbitEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_entities,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey, Identifier};
use text_components::TextComponent;
use text_components::translation::TranslatedMessage;

use goals::{RabbitAvoidEntityGoal, RabbitPanicGoal, RaidGardenGoal};

use crate::behavior::InteractionResult;
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{
    BreedGoal, ClimbOnTopOfPowderSnowGoal, FloatGoal, HurtByTargetGoal, LookAtPlayerGoal,
    MeleeAttackGoal, NearestAttackableTargetGoal, TemptGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::navigation::path_move_target;
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, DefaultEntityPush, Entity,
    EntityBase, EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, LivingEntitySyncedData, Mob, MobBase, MoveResult, PathfinderMob,
    SharedEntity, SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Rabbit.BABY_DIMENSIONS`.
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.24, 0.4, 0.39);
/// Vanilla `Rabbit.FLEE_SPEED_MOD`.
pub(super) const FLEE_SPEED_MOD: f64 = 2.2;
const BABY_JUMP_HEIGHT: f64 = 0.5;
const ADULT_JUMP_HEIGHT: f64 = 1.5;
const JUMP_DELAY_TICKS: i32 = 10;
const PANIC_JUMP_DELAY_TICKS: i32 = 3;
const JUMP_DURATION_IN_TICKS: i32 = 15;
/// Vanilla `Rabbit.MORE_CARROTS_DELAY`.
const MORE_CARROTS_DELAY: i32 = 40;
const EVIL_ATTACK_POWER_INCREMENT: f64 = 5.0;
const EVIL_ARMOR_VALUE: f64 = 8.0;
const KILLER_BUNNY_NAME_KEY: &str = "entity.minecraft.killer_bunny";

/// Vanilla `Rabbit.EVIL_ATTACK_POWER_MODIFIER`.
const fn evil_attack_power_modifier() -> Identifier {
    Identifier::vanilla_static("evil")
}

/// Hop state vanilla keeps on the rabbit and its custom controls.
#[derive(Debug, Clone, Copy, Default)]
struct HopState {
    jump_ticks: i32,
    jump_duration: i32,
    was_on_ground: bool,
    jump_delay_ticks: i32,
    more_carrot_ticks: i32,
    /// Vanilla `RabbitJumpControl.canJump`.
    can_jump: bool,
    /// Vanilla `RabbitMoveControl.nextJumpSpeed`.
    next_jump_speed: f64,
}

/// Vanilla rabbit entity.
#[entity_behavior(class = "Rabbit", spawn_rule)]
pub struct RabbitEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    hop: SyncMutex<HopState>,
    entity_data: SyncMutex<RabbitEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `RabbitEntity`.
unsafe impl DowncastType for RabbitEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/rabbit");
}

/// Vanilla `Rabbit.checkRabbitSpawnRules`.
fn check_rabbit_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn rand::Rng,
) -> bool {
    level
        .get_block_state(pos.below())
        .get_block()
        .has_tag(&BlockTag::RABBITS_SPAWNABLE_ON)
        && <RabbitEntity as Animal>::is_bright_enough_to_spawn(level, pos)
}

/// Vanilla `Rabbit.getRandomRabbitVariant`.
fn random_rabbit_variant(world: &World, pos: BlockPos) -> RabbitVariant {
    let biome = world.biome(pos);
    let random_val = rand::random_range(0..100);
    if biome.is_some_and(|biome| biome.has_tag(&BiomeTag::SPAWNS_WHITE_RABBITS)) {
        if random_val < 80 {
            RabbitVariant::White
        } else {
            RabbitVariant::WhiteSplotched
        }
    } else if biome.is_some_and(|biome| biome.has_tag(&BiomeTag::SPAWNS_GOLD_RABBITS)) {
        RabbitVariant::Gold
    } else if random_val < 50 {
        RabbitVariant::Brown
    } else if random_val < 90 {
        RabbitVariant::Salt
    } else {
        RabbitVariant::Black
    }
}

/// Vanilla `(random.nextFloat() - random.nextFloat()) * 0.2F + 1.0F`.
fn random_pitch() -> f32 {
    (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0
}

impl RabbitEntity {
    /// Vanilla `SpawnPlacements` predicate: `Rabbit.checkRabbitSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_rabbit_spawn_rules;

    /// Creates a new rabbit entity.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a rabbit entity from saved base data.
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
        let mut entity_data = RabbitEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, ClimbOnTopOfPowderSnowGoal::new());
            goal_selector.add_goal(1, RabbitPanicGoal::new(FLEE_SPEED_MOD));
            goal_selector.add_goal(2, BreedGoal::new(0.8));
            goal_selector.add_goal(3, TemptGoal::new(1.0, Self::is_food, false));
            goal_selector.add_goal(4, RabbitAvoidEntityGoal::players());
            goal_selector.add_goal(4, RabbitAvoidEntityGoal::wolves());
            goal_selector.add_goal(4, RabbitAvoidEntityGoal::monsters());
            goal_selector.add_goal(5, RaidGardenGoal::new());
            goal_selector.add_goal(6, WaterAvoidingRandomStrollGoal::new(0.6));
            goal_selector.add_goal(11, LookAtPlayerGoal::new(10.0));
        }

        let rabbit = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            hop: SyncMutex::new(HopState::default()),
            entity_data: SyncMutex::new(entity_data),
        };
        rabbit.set_speed_modifier(0.0);
        rabbit
    }

    /// Returns whether the stack is vanilla rabbit food.
    #[must_use]
    pub fn is_food(item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::RABBIT_FOOD)
    }

    /// Vanilla `Rabbit.getVariant`.
    #[must_use]
    pub fn variant(&self) -> RabbitVariant {
        RabbitVariant::by_id(*self.entity_data.lock().variant_type.get())
    }

    /// Vanilla `Rabbit.setVariant`: the killer bunny gains armor, attack
    /// goals, extra attack damage and its name.
    pub fn set_variant(&self, variant: RabbitVariant) {
        if variant == RabbitVariant::Evil {
            self.attributes()
                .lock()
                .set_base_value(vanilla_attributes::ARMOR, EVIL_ARMOR_VALUE);
            self.mob_base
                .goal_selector()
                .lock()
                .add_goal(4, MeleeAttackGoal::new(1.4, true));
            {
                let mut target_selector = self.mob_base.target_selector().lock();
                target_selector.add_goal(1, HurtByTargetGoal::new().set_alert_others([]));
                target_selector.add_goal(
                    2,
                    NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
                );
                target_selector.add_goal(
                    2,
                    NearestAttackableTargetGoal::new(true, |target, _| {
                        target.entity_type() == &vanilla_entities::WOLF
                    }),
                );
            }
            self.attributes().lock().set_modifier(
                vanilla_attributes::ATTACK_DAMAGE,
                AttributeModifier {
                    id: evil_attack_power_modifier(),
                    amount: EVIL_ATTACK_POWER_INCREMENT,
                    operation: AttributeModifierOperation::AddValue,
                },
                false,
            );
            if self.custom_name().is_none() {
                self.set_custom_name(Some(TextComponent::translated(TranslatedMessage {
                    key: Cow::Borrowed(KILLER_BUNNY_NAME_KEY),
                    fallback: None,
                    args: None,
                })));
            }
        } else {
            self.attributes().lock().remove_modifier(
                vanilla_attributes::ATTACK_DAMAGE,
                &evil_attack_power_modifier(),
            );
        }

        self.entity_data.lock().variant_type.set(variant.id());
    }

    /// Vanilla `Rabbit.getJumpCompletion`, used by the client hop animation.
    #[must_use]
    pub fn jump_completion(&self, partial_tick: f32) -> f32 {
        let hop = self.hop.lock();
        if hop.jump_duration == 0 {
            0.0
        } else {
            (hop.jump_ticks as f32 + partial_tick) / hop.jump_duration as f32
        }
    }

    /// Vanilla `Rabbit.wantsMoreFood`.
    pub(super) fn wants_more_food(&self) -> bool {
        self.hop.lock().more_carrot_ticks <= 0
    }

    /// Vanilla `rabbit.moreCarrotTicks = 40` after eating a carrot.
    pub(super) fn ate_carrot(&self) {
        self.hop.lock().more_carrot_ticks = MORE_CARROTS_DELAY;
    }

    /// Vanilla `Rabbit.setSpeedModifier`: sets the speed of both the path and
    /// the next hop.
    pub(super) fn set_speed_modifier(&self, speed: f64) {
        self.mob_base.navigation().lock().set_speed_modifier(speed);
        let wanted = self
            .mob_base
            .controls()
            .lock()
            .move_control
            .wanted_position();
        self.set_rabbit_wanted_position(wanted, speed);
    }

    /// Vanilla `RabbitMoveControl.setWantedPosition`: rabbits always swim at
    /// 1.5 and remember the last positive speed for the next hop.
    fn set_rabbit_wanted_position(&self, position: DVec3, speed_modifier: f64) {
        let speed_modifier = if self.is_in_water() {
            1.5
        } else {
            speed_modifier
        };
        self.mob_base
            .controls()
            .lock()
            .move_control
            .set_wanted_position(position, speed_modifier);
        if speed_modifier > 0.0 {
            self.hop.lock().next_jump_speed = speed_modifier;
        }
    }

    /// Vanilla `Rabbit.startJumping`.
    fn start_jumping(&self) {
        self.set_jumping(true);
        let mut hop = self.hop.lock();
        hop.jump_duration = JUMP_DURATION_IN_TICKS;
        hop.jump_ticks = 0;
    }

    /// Vanilla `Rabbit.facePoint`: turns the body without the look control.
    fn face_point(&self, face_x: f64, face_z: f64) {
        let position = self.position();
        let y_rot = ((face_z - position.z).atan2(face_x - position.x) as f32).to_degrees() - 90.0;
        let (_, x_rot) = self.rotation();
        self.set_rotation((y_rot, x_rot));
    }

    /// Vanilla `Rabbit.checkLandingDelay`: the pause after a hop, shorter
    /// when panicking.
    fn check_landing_delay(&self) {
        let speed_modifier = self
            .mob_base
            .controls()
            .lock()
            .move_control
            .speed_modifier();
        let mut hop = self.hop.lock();
        hop.jump_delay_ticks = if speed_modifier < FLEE_SPEED_MOD {
            JUMP_DELAY_TICKS
        } else {
            PANIC_JUMP_DELAY_TICKS
        };
        hop.can_jump = false;
    }

    /// Vanilla `path.getNextEntityPos(this)` for an unfinished path.
    fn next_path_entity_pos(&self) -> Option<DVec3> {
        let width = self.bounding_box().width();
        let navigation = self.mob_base.navigation().lock();
        let path = navigation.path().filter(|path| !path.is_done())?;
        path_move_target(path, width)
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for RabbitEntity {}

impl Entity for RabbitEntity {
    fn is_hard_collision_relevant(&self) -> bool {
        false
    }

    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(RABBIT_VARIANT) {
            self.set_variant(*variant);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            BABY_DIMENSIONS.scale(scale)
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

    fn sound_source(&self) -> SoundSource {
        if self.variant() == RabbitVariant::Evil {
            SoundSource::Hostile
        } else {
            SoundSource::Neutral
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("RabbitType", self.variant().id());
        nbt.insert("MoreCarrotTicks", self.hop.lock().more_carrot_ticks);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.set_variant(
            nbt.int("RabbitType")
                .map_or(RabbitVariant::Brown, RabbitVariant::by_id),
        );
        self.hop.lock().more_carrot_ticks = nbt.int("MoreCarrotTicks").unwrap_or(0);
    }
}

impl LivingEntity for RabbitEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn living_synced_data(&self) -> Option<&dyn LivingEntitySyncedData> {
        Some(&self.entity_data)
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
        Some(&sound_events::ENTITY_RABBIT_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_RABBIT_DEATH)
    }

    /// Vanilla `Rabbit.setJumping`: every hop is heard.
    fn set_jumping(&self, jumping: bool) {
        self.living_base().set_jumping(jumping);
        if jumping {
            self.play_sound(
                &sound_events::ENTITY_RABBIT_JUMP,
                self.sound_volume(),
                random_pitch() * 0.8,
            );
        }
    }

    /// Vanilla `Rabbit.getJumpPower`: low hops while strolling, high ones
    /// up a step or against a wall.
    fn get_jump_power(&self) -> f32 {
        let (speed_modifier, wanted_y) = {
            let controls = self.mob_base.controls().lock();
            (
                controls.move_control.speed_modifier(),
                controls.move_control.wanted_position().y,
            )
        };
        let y = self.position().y;
        let mut base_jump_power = 0.3_f32;
        if speed_modifier <= 0.6 {
            base_jump_power = 0.2;
        }
        if self
            .next_path_entity_pos()
            .is_some_and(|next| next.y > y + 0.5)
        {
            base_jump_power = 0.5;
        }
        if self.horizontal_collision() || (self.is_jumping() && wanted_y > y + 0.5) {
            base_jump_power = 0.5;
        }
        self.get_jump_power_with_multiplier(base_jump_power / 0.42)
    }

    /// Vanilla `Rabbit.jumpFromGround`: a standing hop also pushes forward,
    /// and clients play the hop animation.
    fn jump_from_ground(&self) {
        self.default_jump_from_ground();
        let speed_modifier = self
            .mob_base
            .controls()
            .lock()
            .move_control
            .speed_modifier();
        if speed_modifier > 0.0 {
            let velocity = self.velocity();
            if velocity.x * velocity.x + velocity.z * velocity.z < 0.01 {
                let height = if AgeableMob::is_baby(self) {
                    BABY_JUMP_HEIGHT
                } else {
                    ADULT_JUMP_HEIGHT
                };
                self.move_relative(0.1, DVec3::new(0.0, height, 1.0));
            }
        }
        self.broadcast_entity_event(EntityStatus::Jump);
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    /// Vanilla `Rabbit.aiStep`: counts the hop animation down.
    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        let finished_hop = {
            let mut hop = self.hop.lock();
            if hop.jump_ticks != hop.jump_duration {
                hop.jump_ticks += 1;
                false
            } else if hop.jump_duration != 0 {
                hop.jump_ticks = 0;
                hop.jump_duration = 0;
                true
            } else {
                false
            }
        };
        if finished_hop {
            self.set_jumping(false);
        }
        result
    }
}

impl AgeableMob for RabbitEntity {
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

    /// Vanilla `Rabbit.getBreedOffspring`: usually one parent's coat, one in
    /// twenty times the coat of the biome.
    fn get_breed_offspring(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        let offspring = self.create_breed_offspring(world)?;
        let mut variant = random_rabbit_variant(world, self.block_position());
        if rand::random_range(0..20) != 0 {
            variant = match partner.as_entity_event_source().downcast_ref::<Self>() {
                Some(partner) if rand::random::<bool>() => partner.variant(),
                _ => self.variant(),
            };
        }
        if let Some(baby) = offspring.downcast_ref::<Self>() {
            baby.set_variant(variant);
        }
        Some(offspring)
    }
}

impl Animal for RabbitEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        RabbitEntity::is_food(item_stack)
    }
}

impl Mob for RabbitEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn set_wanted_position(&self, position: DVec3, speed_modifier: f64) {
        if let Some(vehicle) = self.controlled_mob_vehicle()
            && let Some(mob) = vehicle.as_mob()
        {
            mob.set_wanted_position(position, speed_modifier);
            return;
        }
        self.set_rabbit_wanted_position(position, speed_modifier);
    }

    /// Vanilla `RabbitMoveControl.tick`: no speed between hops.
    fn tick_move_control(&self) {
        let (wants_jump, operation) = {
            let controls = self.mob_base.controls().lock();
            (
                controls.jump_control.wants_jump(),
                controls.move_control.operation(),
            )
        };
        if self.on_ground() && !self.is_jumping() && !wants_jump {
            self.set_speed_modifier(0.0);
        } else if matches!(
            operation,
            MoveControlOperation::MoveTo | MoveControlOperation::Jumping
        ) {
            let next_jump_speed = self.hop.lock().next_jump_speed;
            self.set_speed_modifier(next_jump_speed);
        }
        self.default_tick_move_control();
    }

    /// Vanilla `RabbitJumpControl.tick`: a queued jump starts a hop, and the
    /// hop itself ends the jump.
    fn tick_jump_control(&self) {
        let jump = self.mob_base.controls().lock().jump_control.tick();
        if jump {
            self.start_jumping();
        }
    }

    /// Vanilla `Rabbit.customServerAiStep`: hops toward wherever the rabbit
    /// wants to go, and the killer bunny hops at its target.
    fn custom_server_ai_step(&self) {
        {
            let mut hop = self.hop.lock();
            if hop.jump_delay_ticks > 0 {
                hop.jump_delay_ticks -= 1;
            }
            if hop.more_carrot_ticks > 0 {
                hop.more_carrot_ticks = (hop.more_carrot_ticks - rand::random_range(0..3)).max(0);
            }
        }

        if self.on_ground() {
            if !self.hop.lock().was_on_ground {
                self.set_jumping(false);
                self.check_landing_delay();
            }

            if self.variant() == RabbitVariant::Evil
                && self.hop.lock().jump_delay_ticks == 0
                && let Some(target) = self.target()
                && self.position().distance_squared(target.position()) < 16.0
            {
                let target_position = target.position();
                self.face_point(target_position.x, target_position.z);
                let speed_modifier = self
                    .mob_base
                    .controls()
                    .lock()
                    .move_control
                    .speed_modifier();
                self.set_rabbit_wanted_position(target_position, speed_modifier);
                self.start_jumping();
                self.hop.lock().was_on_ground = true;
            }

            let (wants_jump, has_wanted, wanted) = {
                let controls = self.mob_base.controls().lock();
                (
                    controls.jump_control.wants_jump(),
                    controls.move_control.operation() == MoveControlOperation::MoveTo,
                    controls.move_control.wanted_position(),
                )
            };
            if wants_jump {
                let mut hop = self.hop.lock();
                if !hop.can_jump {
                    hop.can_jump = true;
                }
            } else if has_wanted && self.hop.lock().jump_delay_ticks == 0 {
                let pos = self.next_path_entity_pos().unwrap_or(wanted);
                self.face_point(pos.x, pos.z);
                self.start_jumping();
            }
        }

        self.hop.lock().was_on_ground = self.on_ground();
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_RABBIT_AMBIENT)
    }

    /// Vanilla `Rabbit.playAttackSound`: only the killer bunny.
    fn play_attack_sound(&self) {
        if self.variant() == RabbitVariant::Evil {
            self.play_sound(&sound_events::ENTITY_RABBIT_ATTACK, 1.0, random_pitch());
        }
    }

    /// Vanilla `Rabbit.finalizeSpawn`: a group shares one coat, and every
    /// rabbit after the first spawns as a baby.
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let random_variant = random_rabbit_variant(world, self.block_position());
        let (variant, ageable) = match group_data {
            Some(SpawnGroupData::Rabbit(ageable, variant)) => (variant, ageable),
            _ => (
                random_variant,
                AgeableMobGroupData::with_baby_spawn_chance(1.0),
            ),
        };
        self.set_variant(variant);
        match self.finalize_spawn_ageable_mob(
            world,
            spawn_reason,
            Some(SpawnGroupData::AgeableMob(ageable)),
        ) {
            Some(SpawnGroupData::AgeableMob(ageable)) => {
                Some(SpawnGroupData::Rabbit(ageable, variant))
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

impl PathfinderMob for RabbitEntity {}

#[cfg(test)]
mod tests;
