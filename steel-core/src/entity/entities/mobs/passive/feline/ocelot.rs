//! Vanilla ocelot: a shy jungle cat that learns to trust players who feed it fish.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::{Rng, RngExt as _};
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
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::OcelotEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_entities,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{
    AvoidEntityGoal, BreedGoal, FloatGoal, Goal, GoalControls, LeapAtTargetGoal, LookAtPlayerGoal,
    NearestAttackableTargetGoal, TemptGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entity_selector;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, DefaultEntityPush, Entity,
    EntityBase, EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    level_contains_any_liquid, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Ocelot.CROUCH_SPEED_MOD`.
pub(super) const CROUCH_SPEED_MOD: f64 = 0.6;
/// Vanilla `Ocelot.WALK_SPEED_MOD`.
pub(super) const WALK_SPEED_MOD: f64 = 0.8;
/// Vanilla `Ocelot.SPRINT_SPEED_MOD`.
pub(super) const SPRINT_SPEED_MOD: f64 = 1.33;
const OCELOT_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.3125, 0.0)];
const OCELOT_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.3,
    0.35,
    0.343_75,
    EntityAttachments::new(&OCELOT_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const AMBIENT_SOUND_INTERVAL: i32 = 900;
/// Untrusting ocelots despawn far from players once they are this old.
const DESPAWN_MIN_TICKS: i32 = 2400;
/// Players must feed an ocelot from within this squared distance.
const FEED_DISTANCE_SQR: f64 = 9.0;
const AVOID_PLAYER_DISTANCE: f32 = 16.0;
/// Vanilla `OcelotAttackGoal`: gives up on targets further than 15 blocks.
const ATTACK_GIVE_UP_DISTANCE_SQR: f64 = 225.0;
const ATTACK_SPRINT_DISTANCE_SQR: f64 = 16.0;
const ATTACK_INTERVAL: i32 = 20;

#[entity_behavior(class = "Ocelot", spawn_rule)]
/// Vanilla ocelot.
pub struct OcelotEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    /// Mirrors vanilla `temptGoal.isRunning()` for [`Mob::mob_interact`].
    tempting: Arc<AtomicBool>,
    entity_data: SyncMutex<OcelotEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `OcelotEntity`.
unsafe impl DowncastType for OcelotEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/ocelot");
}

fn is_ocelot_food(item_stack: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item_stack.item(), &ItemTag::OCELOT_FOOD)
}

fn is_trusting(mob: &dyn PathfinderMob) -> bool {
    mob.downcast_ref::<OcelotEntity>()
        .is_some_and(OcelotEntity::is_trusting)
}

/// Vanilla `Ocelot.checkOcelotSpawnRules`: two spawn attempts in three succeed.
fn check_ocelot_spawn_rules(
    _entity_type: EntityTypeRef,
    _level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    _pos: BlockPos,
    random: &mut dyn Rng,
) -> bool {
    random.random_range(0..3) != 0
}

impl OcelotEntity {
    /// Vanilla `SpawnPlacements` predicate: `Ocelot.checkOcelotSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_ocelot_spawn_rules;

    /// Creates a new ocelot at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs an ocelot from persisted base entity state.
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
        let mut entity_data = OcelotEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let tempting = Arc::new(AtomicBool::new(false));

        {
            // Vanilla re-adds the avoid-players goal whenever trust changes; the
            // goal here checks the trust itself, which behaves the same.
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(
                3,
                TemptGoal::new(CROUCH_SPEED_MOD, is_ocelot_food, true)
                    .with_scare_condition(|mob| !is_trusting(mob))
                    .with_running_flag(Arc::clone(&tempting)),
            );
            goal_selector.add_goal(4, OcelotAvoidPlayersGoal::new());
            goal_selector.add_goal(7, LeapAtTargetGoal::new(0.3));
            goal_selector.add_goal(8, OcelotAttackGoal::new());
            goal_selector.add_goal(9, BreedGoal::new(WALK_SPEED_MOD));
            goal_selector.add_goal(
                10,
                WaterAvoidingRandomStrollGoal::with_probability(WALK_SPEED_MOD, 1.000_000_1E-5),
            );
            goal_selector.add_goal(11, LookAtPlayerGoal::new(10.0));

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new(false, |target, _| {
                    target.entity_type() == &vanilla_entities::CHICKEN
                }),
            );
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new_with_interval(10, false, false, |target, _| {
                    // Vanilla `Turtle.BABY_ON_LAND_SELECTOR`.
                    target.entity_type() == &vanilla_entities::TURTLE
                        && target.as_ageable_mob().is_some_and(AgeableMob::is_baby)
                        && !target.is_in_water()
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            tempting,
            entity_data: SyncMutex::new(entity_data),
        }
    }

    /// Whether this ocelot trusts players, after being fed.
    #[must_use]
    pub fn is_trusting(&self) -> bool {
        *self.entity_data.lock().trusting.get()
    }

    /// Sets whether this ocelot trusts players.
    pub fn set_trusting(&self, trusting: bool) {
        self.entity_data.lock().trusting.set(trusting);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for OcelotEntity {}

impl Entity for OcelotEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            OCELOT_BABY_DIMENSIONS.scale(scale)
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

    fn is_stepping_carefully(&self) -> bool {
        self.is_crouching() || self.is_suppressing_bounce()
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Trusting", i8::from(self.is_trusting()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.set_trusting(nbt.byte("Trusting").is_some_and(|trusting| trusting != 0));
    }
}

impl LivingEntity for OcelotEntity {
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
        Some(&sound_events::ENTITY_OCELOT_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_OCELOT_DEATH)
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

impl AgeableMob for OcelotEntity {
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

impl Animal for OcelotEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        is_ocelot_food(item_stack)
    }
}

impl Mob for OcelotEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `Ocelot.customServerAiStep`: crouches while stalking and sprints
    /// while fleeing or pouncing, read from the move control's speed.
    fn custom_server_ai_step(&self) {
        let wanted_speed = {
            let controls = self.mob_base.controls().lock();
            (controls.move_control.operation() == MoveControlOperation::MoveTo)
                .then(|| controls.move_control.speed_modifier())
        };
        match wanted_speed {
            Some(speed) if speed == CROUCH_SPEED_MOD => {
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

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        !self.is_trusting() && self.tick_count() > DESPAWN_MIN_TICKS
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_OCELOT_AMBIENT)
    }

    fn ambient_sound_interval(&self) -> i32 {
        AMBIENT_SOUND_INTERVAL
    }

    /// Vanilla `Ocelot.checkSpawnObstruction`: only above sea level, on grass
    /// or leaves.
    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        if !level.is_unobstructed_by_entities(self)
            || level_contains_any_liquid(level, self.bounding_box())
        {
            return false;
        }
        let pos = self.block_position();
        if pos.y() < level.sea_level() {
            return false;
        }
        let below = level.get_block_state(pos.below()).get_block();
        below == &vanilla_blocks::GRASS_BLOCK || below.has_tag(&BlockTag::LEAVES)
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
        let is_food = {
            let inventory = player.inventory.lock();
            is_ocelot_food(inventory.get_item_in_hand(hand))
        };
        if self.tempting.load(Ordering::Relaxed)
            && !self.is_trusting()
            && is_food
            && player.position().distance_squared(self.position()) < FEED_DISTANCE_SQR
        {
            self.use_player_item(player, hand);
            if rand::random_range(0..3) == 0 {
                self.set_trusting(true);
                self.broadcast_entity_event(EntityStatus::TrustingSucceeded);
            } else {
                self.broadcast_entity_event(EntityStatus::TrustingFailed);
            }
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

impl PathfinderMob for OcelotEntity {}

/// Vanilla `Ocelot.OcelotAvoidEntityGoal<Player>`: untrusting ocelots run from players.
struct OcelotAvoidPlayersGoal {
    avoid: AvoidEntityGoal,
}

impl OcelotAvoidPlayersGoal {
    fn new() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(
                AVOID_PLAYER_DISTANCE,
                WALK_SPEED_MOD,
                SPRINT_SPEED_MOD,
                |target, _| {
                    target.as_player().is_some()
                        && entity_selector::no_creative_or_spectator(
                            target.as_entity_event_source(),
                        )
                },
            ),
        }
    }
}

impl Goal for OcelotAvoidPlayersGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !is_trusting(mob) && self.avoid.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !is_trusting(mob) && self.avoid.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.avoid.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.avoid.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.avoid.tick(mob, entity);
    }
}

/// Vanilla `OcelotAttackGoal`: stalks the target, sprints in for the kill and
/// bites once a second.
pub(super) struct OcelotAttackGoal {
    target: Option<SharedEntity>,
    attack_time: i32,
}

impl OcelotAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            target: None,
            attack_time: 0,
        }
    }
}

impl Goal for OcelotAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(target) = mob.target() else {
            return false;
        };
        self.target = Some(target);
        true
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        if !target.is_alive() {
            return false;
        }
        if mob.position().distance_squared(target.position()) > ATTACK_GIVE_UP_DISTANCE_SQR {
            return false;
        }
        !mob.mob_base().navigation().lock().is_done() || self.can_use(mob)
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target = None;
        mob.mob_base().navigation().lock().stop();
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let target_position = target.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
            30.0,
            30.0,
        );

        let width = f64::from(mob.base().dimensions().width);
        let melee_radius_sqr = width * 2.0 * (width * 2.0);
        let dist_sqr = mob.position().distance_squared(target_position);
        let speed_modifier = if dist_sqr > melee_radius_sqr && dist_sqr < ATTACK_SPRINT_DISTANCE_SQR
        {
            SPRINT_SPEED_MOD
        } else if dist_sqr < ATTACK_GIVE_UP_DISTANCE_SQR {
            CROUCH_SPEED_MOD
        } else {
            WALK_SPEED_MOD
        };
        let _ = mob.move_to_entity(target.as_ref(), speed_modifier);

        self.attack_time = (self.attack_time - 1).max(0);
        if dist_sqr <= melee_radius_sqr && self.attack_time <= 0 {
            self.attack_time = ATTACK_INTERVAL;
            if let Some(world) = mob.level() {
                let _ = mob.do_hurt_target(entity, &world, &target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;
    use std::sync::atomic::Ordering;

    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

    use super::OcelotEntity;
    use crate::entity::{AgeableMob, Animal, Entity, EntityPose, Mob};
    use steel_registry::item_stack::ItemStack;

    fn test_ocelot() -> OcelotEntity {
        init_vanilla_registry();
        OcelotEntity::new(&vanilla_entities::OCELOT, 1, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn ocelot_eats_fish_and_starts_untrusting() {
        let ocelot = test_ocelot();
        assert!(!ocelot.is_trusting());
        assert!(!ocelot.tempting.load(Ordering::Relaxed));
        assert!(ocelot.is_food(&ItemStack::new(&vanilla_items::COD)));
        assert!(!ocelot.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
    }

    #[test]
    fn untrusting_ocelots_despawn_trusting_ones_stay() {
        let ocelot = test_ocelot();
        assert!(!ocelot.remove_when_far_away(0.0));
        ocelot.set_trusting(true);
        assert!(!ocelot.remove_when_far_away(0.0));
        assert_eq!(ocelot.ambient_sound_interval(), 900);
    }

    #[test]
    fn baby_ocelot_has_vanilla_dimensions() {
        let ocelot = test_ocelot();
        ocelot.set_age(ocelot.get_baby_start_age());
        let baby = ocelot.dimensions_for_pose(EntityPose::Standing);
        assert!((baby.width - 0.3).abs() < 1e-6);
        assert!((baby.height - 0.35).abs() < 1e-6);
    }
}
