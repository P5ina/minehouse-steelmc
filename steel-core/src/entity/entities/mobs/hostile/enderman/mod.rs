//! Vanilla `Enderman`: a tall neutral monster that teleports, carries blocks
//! and turns on players who stare at it.

use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::{PotionContents, vanilla_components};
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::EndermanEntityData;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::vanilla_potion_tags::PotionTag;
use steel_registry::{
    level_events, sound_events, vanilla_attributes, vanilla_enchantments, vanilla_entities,
    vanilla_game_events, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::{
    BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey, Identifier,
};

use crate::behavior::BlockLootContext;
use crate::block_entity::block_state_nbt;
use crate::entity::ai::goal::{
    FloatGoal, HurtByTargetGoal, LookAtPlayerGoal, MeleeAttackGoal, NearestAttackableTargetGoal,
    RandomLookAroundGoal, ResetUniversalAngerTargetGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::damage::DamageSource;
use crate::entity::entities::{LingeringPotionEntity, SplashPotionEntity};
use crate::entity::leash::Leashable as _;
use crate::entity::projectile::ThrowableItemProjectile as _;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster, NeutralMob,
    NeutralMobState, PathfinderMob, SharedEntity, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::game_event::GameEventContext;
use crate::world::{LevelReader, World};

mod goals;
#[cfg(test)]
mod tests;

use goals::{
    EndermanFreezeWhenLookedAt, EndermanLeaveBlockGoal, EndermanLookForPlayerGoal,
    EndermanTakeBlockGoal,
};

/// Vanilla `SPEED_MODIFIER_ATTACKING`: +0.15 movement speed while targeting.
const SPEED_MODIFIER_ATTACKING_AMOUNT: f64 = 0.15;
/// Vanilla `MIN_DEAGGRESSION_TIME`: ticks after a target change before daylight
/// can make the enderman teleport away.
const MIN_DEAGGRESSION_TIME: i32 = 600;
/// Vanilla `PERSISTENT_ANGER_TIME = TimeUtil.rangeOfSeconds(20, 39)`, in ticks.
const PERSISTENT_ANGER_TICKS_MIN: i64 = 20 * 20;
const PERSISTENT_ANGER_TICKS_MAX: i64 = 39 * 20;
/// Vanilla `MAX_TELEPORT_DIAMETER`.
const MAX_TELEPORT_DIAMETER: f64 = 64.0;
/// Attempts made by `repeatedlyTryToTeleport`.
const TELEPORT_ATTEMPTS: i32 = 64;
/// Vanilla `isLookingAtMe` cone for a stare.
const STARE_CONE: f64 = 0.025;
const TAG_CARRIED_BLOCK_STATE: &str = "carriedBlockState";

const fn speed_modifier_attacking_id() -> Identifier {
    Identifier::vanilla_static("attacking")
}

/// An enderman.
#[entity_behavior(class = "Enderman", spawn_rule)]
pub struct EndermanEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<EndermanEntityData>,
    anger: NeutralMobState,
    /// Vanilla `targetChangeTime`.
    target_change_time: SyncMutex<i32>,
}

// SAFETY: The owner-scoped type key uniquely identifies EndermanEntity.
unsafe impl DowncastType for EndermanEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/enderman");
}

impl EndermanEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new enderman.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved enderman.
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
        mob_base
            .pathfinding_malus()
            .lock()
            .set(PathType::Water, -1.0);
        let mut entity_data = EndermanEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, EndermanFreezeWhenLookedAt::new());
            goal_selector.add_goal(2, MeleeAttackGoal::new(1.0, false));
            goal_selector.add_goal(7, WaterAvoidingRandomStrollGoal::with_probability(1.0, 0.0));
            goal_selector.add_goal(8, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(8, RandomLookAroundGoal::new());
            goal_selector.add_goal(10, EndermanLeaveBlockGoal);
            goal_selector.add_goal(11, EndermanTakeBlockGoal);

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, EndermanLookForPlayerGoal::new());
            target_selector.add_goal(2, HurtByTargetGoal::new());
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
                    target.entity_type() == &vanilla_entities::ENDERMITE
                }),
            );
            target_selector.add_goal(4, ResetUniversalAngerTargetGoal::new(false));
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            anger: NeutralMobState::new(),
            target_change_time: SyncMutex::new(0),
        }
    }

    /// The block the enderman is holding, if any.
    #[must_use]
    pub fn carried_block(&self) -> Option<BlockStateId> {
        *self.entity_data.lock().carry_state.get()
    }

    /// Vanilla `setCarriedBlock`.
    pub fn set_carried_block(&self, carried: Option<BlockStateId>) {
        self.entity_data.lock().carry_state.set(carried);
    }

    /// Whether the enderman is screaming at a target.
    #[must_use]
    pub fn is_creepy(&self) -> bool {
        *self.entity_data.lock().creepy.get()
    }

    /// Whether a player provoked it by staring.
    #[must_use]
    pub fn has_been_stared_at(&self) -> bool {
        *self.entity_data.lock().stared_at.get()
    }

    fn set_being_stared_at(&self) {
        self.entity_data.lock().stared_at.set(true);
    }

    /// Vanilla `isBeingStaredBy`: a player without a carved pumpkin looks at
    /// the enderman's eyes.
    pub(crate) fn is_being_stared_by(&self, player: &Player) -> bool {
        player_not_wearing_disguise_item(player)
            && self.is_looking_at_me(player, STARE_CONE, true, false, &[self.get_eye_y()])
    }

    /// Vanilla `teleport()`: a random spot up to 32 blocks away.
    pub(crate) fn teleport_randomly(&self) -> bool {
        if !LivingEntity::is_alive(self) {
            return false;
        }
        let position = self.position();
        let x = position.x + (rand::random::<f64>() - 0.5) * MAX_TELEPORT_DIAMETER;
        let y = position.y + f64::from(rand::random_range(0..64) - 32);
        let z = position.z + (rand::random::<f64>() - 0.5) * MAX_TELEPORT_DIAMETER;
        self.teleport_enderman(DVec3::new(x, y, z))
    }

    /// Vanilla `teleportTowards`: a spot about 16 blocks closer to `entity`.
    pub(crate) fn teleport_towards(&self, entity: &dyn Entity) -> bool {
        let position = self.position();
        let target = entity.position();
        let height = {
            let bounding_box = self.bounding_box();
            bounding_box.max_y() - bounding_box.min_y()
        };
        let direction = DVec3::new(
            position.x - target.x,
            position.y + height * 0.5 - entity.get_eye_y(),
            position.z - target.z,
        )
        .normalize_or_zero();
        let x = position.x + (rand::random::<f64>() - 0.5) * 8.0 - direction.x * 16.0;
        let y = position.y + f64::from(rand::random_range(0..16) - 8) - direction.y * 16.0;
        let z = position.z + (rand::random::<f64>() - 0.5) * 8.0 - direction.z * 16.0;
        self.teleport_enderman(DVec3::new(x, y, z))
    }

    /// Vanilla `teleport(x, y, z)`.
    pub(crate) fn teleport_enderman(&self, target: DVec3) -> bool {
        if self.is_passenger() {
            return false;
        }
        let Some(world) = self.level() else {
            return false;
        };
        let old_position = self.position();
        let teleported =
            self.random_teleport_avoiding(&world, target.x, target.y, target.z, true, &|state| {
                state
                    .get_block()
                    .has_tag(&BlockTag::ENDERMAN_DOES_NOT_TELEPORT_TO)
            });
        if teleported {
            world.game_event_at(
                &vanilla_game_events::TELEPORT,
                old_position,
                &GameEventContext::new(Some(self), None),
            );
            if !self.is_silent() {
                let old_block = BlockPos::from(old_position);
                let packed_difference = clamped_pack_difference_in_position(
                    old_block,
                    self.block_position(),
                    127,
                    127,
                    127,
                );
                world.level_event(
                    level_events::PARTICLES_ENDERMAN_TELEPORT,
                    old_block,
                    packed_difference,
                    None,
                );
                // Vanilla plays this at `xo/yo/zo`, which `teleportTo` has
                // already moved to the new position.
                world.play_sound_at(
                    &sound_events::ENTITY_ENDERMAN_TELEPORT,
                    self.sound_source(),
                    self.position(),
                    1.0,
                    1.0,
                    None,
                );
                self.play_sound(&sound_events::ENTITY_ENDERMAN_TELEPORT, 1.0, 1.0);
            }
        }
        teleported
    }

    /// Vanilla `repeatedlyTryToTeleport`.
    fn repeatedly_try_to_teleport(&self) {
        if self.is_passenger() {
            return;
        }
        for _ in 0..TELEPORT_ATTEMPTS {
            if self.teleport_randomly() {
                return;
            }
        }
    }

    /// Vanilla `customServerAiStep`: daylight makes the enderman lose its
    /// target and teleport away.
    fn flee_daylight(&self) {
        let Some(world) = self.level() else {
            return;
        };
        if !world.is_bright_outside()
            || self.tick_count() < *self.target_change_time.lock() + MIN_DEAGGRESSION_TIME
        {
            return;
        }
        let brightness = self.light_level_dependent_magic_value();
        if brightness > 0.5
            && world.can_see_sky(self.block_position())
            && rand::random::<f32>() * 30.0 < (brightness - 0.4) * 2.0
        {
            let _ = self.set_target(None);
            self.teleport_randomly();
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

/// Vanilla `LivingEntity.PLAYER_NOT_WEARING_DISGUISE_ITEM`.
fn player_not_wearing_disguise_item(player: &Player) -> bool {
    let mut disguised = false;
    player.with_equipment_slot(EquipmentSlot::Head, &mut |helmet| {
        disguised = helmet.item().has_tag(&ItemTag::GAZE_DISGUISE_EQUIPMENT);
    });
    !disguised
}

/// Vanilla `BlockUtil.clampedPackDifferenceInPosition`.
fn clamped_pack_difference_in_position(
    pos: BlockPos,
    test_pos: BlockPos,
    x_radius: i32,
    y_radius: i32,
    z_radius: i32,
) -> i32 {
    let x_radius = x_radius.clamp(0, 127);
    let y_radius = y_radius.clamp(0, 127);
    let z_radius = z_radius.clamp(0, 127);
    let x_diff = (test_pos.x() - pos.x()).clamp(-127, 127);
    let y_diff = (test_pos.y() - pos.y()).clamp(-127, 127);
    let z_diff = (test_pos.z() - pos.z()).clamp(-127, 127);
    ((x_diff + x_radius) & 0xFF) << 16
        | ((y_diff + y_radius) & 0xFF) << 8
        | (z_diff + z_radius) & 0xFF
}

/// The potion item of a thrown potion entity (vanilla `AbstractThrownPotion`).
fn thrown_potion_item(entity: &SharedEntity) -> Option<ItemStack> {
    if let Some(potion) = entity.downcast_ref::<SplashPotionEntity>() {
        return Some(potion.get_item());
    }
    entity
        .downcast_ref::<LingeringPotionEntity>()
        .map(LingeringPotionEntity::get_item)
}

impl NeutralMob for EndermanEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(rand::random_range(
            PERSISTENT_ANGER_TICKS_MIN..=PERSISTENT_ANGER_TICKS_MAX,
        ));
    }
}

impl DefaultEntityPush for EndermanEntity {}

impl Entity for EndermanEntity {
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

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
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

    fn projectile_receives_side_effects_on_hit(&self, was_hurt: bool) -> bool {
        was_hurt
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        if let Some(carried) = self.carried_block() {
            nbt.insert(TAG_CARRIED_BLOCK_STATE, block_state_nbt::save(carried));
        }
        self.add_persistent_anger_save_data(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        let carried = nbt
            .get(TAG_CARRIED_BLOCK_STATE)
            .and_then(block_state_nbt::load)
            .filter(|state| !state.is_air());
        self.set_carried_block(carried);
        self.read_persistent_anger_save_data(nbt);
    }
}

impl LivingEntity for EndermanEntity {
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

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ENDERMAN_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ENDERMAN_DEATH)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_invulnerable_to(world, source) {
            return false;
        }
        let potion_item = source.direct_entity().and_then(thrown_potion_item);
        if source.is(&DamageTypeTag::IS_PROJECTILE) && !self.is_passenger() {
            self.repeatedly_try_to_teleport();
            return false;
        }
        if let Some(potion_item) = potion_item {
            let contents = potion_item
                .get_or_default(vanilla_components::POTION_CONTENTS, PotionContents::empty());
            let hurt_with_clean_water = contents
                .is_in_tag(&PotionTag::HURTS_WATER_SENSITIVE_ENTITIES)
                && self.default_hurt_server(world, source, amount);
            self.repeatedly_try_to_teleport();
            return hurt_with_clean_water;
        }
        let hurt = self.default_hurt_server(world, source, amount);
        let by_living = source
            .causing_entity()
            .is_some_and(|entity| entity.as_living_entity().is_some());
        if !by_living && rand::random_range(0..10) != 0 {
            self.teleport_randomly();
        }
        hurt
    }

    fn drop_custom_death_loot(&self, _source: &DamageSource, _killed_by_player: bool) {
        let Some(carried) = self.carried_block() else {
            return;
        };
        let Some(world) = self.level() else {
            return;
        };
        // Vanilla `VanillaEnchantmentProviders.ENDERMAN_LOOT_DROP`: Silk Touch I.
        let mut tool = ItemStack::new(&vanilla_items::DIAMOND_AXE);
        tool.upgrade_enchantment(vanilla_enchantments::SILK_TOUCH.key.clone(), 1);
        let context = BlockLootContext::new(&world, self.block_position())
            .with_tool(&tool)
            .with_entity(Some(self));
        for item in World::block_drops(carried, &context) {
            self.spawn_at_location(item, 0.0);
        }
    }

    /// Vanilla `canRandomlyTeleportTo`: never onto water.
    fn can_randomly_teleport_to(&self, target: DVec3) -> bool {
        self.level().is_none_or(|world| {
            !world
                .get_block_state(BlockPos::from(target).below())
                .get_fluid_state()
                .fluid_id
                .has_tag(&FluidTag::WATER)
        })
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        // VANILLA CLIENT-LOCAL: the portal particles spawn on clients.
        self.living_base.set_jumping(false);
        if let Some(world) = self.level() {
            self.update_persistent_anger(&world, true);
        }
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for EndermanEntity {
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
        self.flee_daylight();
    }

    fn set_target(&self, target: Option<&SharedEntity>) -> bool {
        let accepted = self
            .mob_base()
            .set_target(target, |target| self.is_valid_target(target));
        let has_target = self.mob_base().target(|_| true).is_some();
        let modifier_id = speed_modifier_attacking_id();
        if has_target {
            *self.target_change_time.lock() = self.tick_count();
            self.entity_data.lock().creepy.set(true);
            let mut attributes = self.attributes().lock();
            if !attributes.has_modifier(vanilla_attributes::MOVEMENT_SPEED, &modifier_id) {
                attributes.add_modifier(
                    vanilla_attributes::MOVEMENT_SPEED,
                    AttributeModifier {
                        id: modifier_id,
                        amount: SPEED_MODIFIER_ATTACKING_AMOUNT,
                        operation: AttributeModifierOperation::AddValue,
                    },
                    false,
                );
            }
        } else {
            *self.target_change_time.lock() = 0;
            {
                let mut data = self.entity_data.lock();
                data.creepy.set(false);
                data.stared_at.set(false);
            }
            self.attributes()
                .lock()
                .remove_modifier(vanilla_attributes::MOVEMENT_SPEED, &modifier_id);
        }
        accepted
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_creepy() {
            &sound_events::ENTITY_ENDERMAN_SCREAM
        } else {
            &sound_events::ENTITY_ENDERMAN_AMBIENT
        })
    }

    fn requires_custom_persistence(&self) -> bool {
        self.is_passenger() || self.is_leashed() || self.carried_block().is_some()
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl Monster for EndermanEntity {}

impl PathfinderMob for EndermanEntity {
    fn get_walk_target_value_in(&self, _pos: BlockPos, _level: &dyn LevelReader) -> f32 {
        0.0
    }
}
