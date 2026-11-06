//! Iron golem (Vanilla `IronGolem`).

use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::IronGolemEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities, vanilla_items};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    HurtByTargetGoal, LookAtPlayerGoal, MeleeAttackGoal, MoveTowardsTargetGoal,
    NearestAttackableTargetGoal, RandomLookAroundGoal, ResetUniversalAngerTargetGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::pushing::EntityPush;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, NeutralMob, NeutralMobState,
    PathfinderMob, SharedEntity, sync_dirty_mob_effects,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

const PLAYER_CREATED_FLAG: i8 = 1;
const IRON_INGOT_HEAL_AMOUNT: f32 = 25.0;
const DEFAULT_STEP_HEIGHT: f32 = 1.0;
/// Vanilla `AbstractGolem.getAmbientSoundInterval`.
const GOLEM_AMBIENT_SOUND_INTERVAL: i32 = 120;
const ATTACK_ANIMATION_TICKS: i32 = 10;
const OFFER_FLOWER_TICKS: i32 = 400;
/// Vanilla `PERSISTENT_ANGER_TIME = TimeUtil.rangeOfSeconds(20, 39)`, in ticks.
const PERSISTENT_ANGER_TICKS_MIN: i64 = 20 * 20;
const PERSISTENT_ANGER_TICKS_MAX: i64 = 39 * 20;
/// One in this many pushes by an enemy makes the golem target it.
const PUSHED_BY_ENEMY_TARGET_CHANCE: u32 = 20;
const ATTACK_KNOCKUP: f64 = 0.4;

/// Health fractions under which the golem shows more cracks (Vanilla `Crackiness.GOLEM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crackiness {
    None,
    Low,
    Medium,
    High,
}

impl Crackiness {
    fn by_fraction(fraction: f32) -> Self {
        if fraction < 0.25 {
            Self::High
        } else if fraction < 0.5 {
            Self::Medium
        } else if fraction < 0.75 {
            Self::Low
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct AnimationTicks {
    attack: i32,
    offer_flower: i32,
}

/// An iron golem: guards villages and the player who built it.
// TODO: The village goals (`MoveBackToVillageGoal`, `GolemRandomStrollInVillageGoal`,
// `OfferFlowerGoal`, `DefendVillageTargetGoal`) and `checkSpawnObstruction` come with villagers.
#[entity_behavior(class = "IronGolem", spawn_rule)]
pub struct IronGolemEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<IronGolemEntityData>,
    animation: SyncMutex<AnimationTicks>,
    anger: NeutralMobState,
}

// SAFETY: The owner-scoped type key uniquely identifies IronGolemEntity.
unsafe impl DowncastType for IronGolemEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/iron_golem");
}

impl IronGolemEntity {
    /// Vanilla `SpawnPlacements` predicate: `Mob.checkMobSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Creates a new iron golem.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved iron golem.
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
        let mut entity_data = IronGolemEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, MeleeAttackGoal::new(1.0, true));
            goal_selector.add_goal(2, MoveTowardsTargetGoal::new(0.9, 32.0));
            goal_selector.add_goal(7, LookAtPlayerGoal::new(6.0));
            goal_selector.add_goal(8, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(2, HurtByTargetGoal::new());
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
                        .is_some_and(|golem| golem.is_angry_at(target, world))
                }),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new_with_interval(5, false, false, |target, _| {
                    target.as_mob().is_some()
                        && target.is_enemy()
                        && target.entity_type() != &vanilla_entities::CREEPER
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
            animation: SyncMutex::new(AnimationTicks::default()),
            anger: NeutralMobState::new(),
        }
    }

    /// Whether a player built this golem; such golems never attack players.
    #[must_use]
    pub fn is_player_created(&self) -> bool {
        *self.entity_data.lock().flags.get() & PLAYER_CREATED_FLAG != 0
    }

    /// Marks the golem as built by a player.
    pub fn set_player_created(&self, value: bool) {
        let mut entity_data = self.entity_data.lock();
        let current = *entity_data.flags.get();
        entity_data.flags.set(if value {
            current | PLAYER_CREATED_FLAG
        } else {
            current & !PLAYER_CREATED_FLAG
        });
    }

    /// Ticks left of the arm-swing attack animation.
    #[must_use]
    pub fn attack_animation_tick(&self) -> i32 {
        self.animation.lock().attack
    }

    /// Ticks left of holding out a poppy to a villager.
    #[must_use]
    pub fn offer_flower_tick(&self) -> i32 {
        self.animation.lock().offer_flower
    }

    /// Holds out (or puts away) a poppy for a villager.
    pub fn offer_flower(&self, offer: bool) {
        if offer {
            self.animation.lock().offer_flower = OFFER_FLOWER_TICKS;
            self.broadcast_entity_event(EntityStatus::OfferFlower);
        } else {
            self.animation.lock().offer_flower = 0;
            self.broadcast_entity_event(EntityStatus::StopOfferFlower);
        }
    }

    fn crackiness(&self) -> Crackiness {
        Crackiness::by_fraction(self.get_health() / self.get_max_health())
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl NeutralMob for IronGolemEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(rand::random_range(
            PERSISTENT_ANGER_TICKS_MIN..=PERSISTENT_ANGER_TICKS_MAX,
        ));
    }
}

impl DefaultEntityPush for IronGolemEntity {}

impl Entity for IronGolemEntity {
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
        let scale = LivingEntity::get_scale(self);
        if self.entity_type.fixed {
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

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_IRON_GOLEM_STEP, 1.0, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("PlayerCreated", i8::from(self.is_player_created()));
        self.add_persistent_anger_save_data(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.set_player_created(nbt.byte("PlayerCreated").is_some_and(|b| b != 0));
        self.read_persistent_anger_save_data(nbt);
    }
}

impl LivingEntity for IronGolemEntity {
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
        Some(&sound_events::ENTITY_IRON_GOLEM_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_IRON_GOLEM_DEATH)
    }

    fn decrease_air_supply(&self, current_supply: i32) -> i32 {
        current_supply
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let previous = self.crackiness();
        let was_hurt = self.default_hurt_server(world, source, amount);
        if was_hurt && self.crackiness() != previous {
            self.play_sound(&sound_events::ENTITY_IRON_GOLEM_DAMAGE, 1.0, 1.0);
        }
        was_hurt
    }

    fn push_entities_in_order(&self, entities: &[SharedEntity]) {
        for entity in entities {
            if entity.is_enemy()
                && entity.entity_type() != &vanilla_entities::CREEPER
                && rand::random_range(0..PUSHED_BY_ENEMY_TARGET_CHANCE) == 0
            {
                let _ = self.set_target(Some(entity));
            }
        }
        EntityPush::push_entities_in_order(self, entities);
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        {
            let mut animation = self.animation.lock();
            animation.attack = (animation.attack - 1).max(0);
            animation.offer_flower = (animation.offer_flower - 1).max(0);
        }
        if let Some(world) = self.level() {
            self.update_persistent_anger(&world, true);
        }
        result
    }
}

impl Mob for IronGolemEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound_interval(&self) -> i32 {
        GOLEM_AMBIENT_SOUND_INTERVAL
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        let is_player = target.entity_type() == &vanilla_entities::PLAYER;
        if (self.is_player_created() && is_player)
            || target.entity_type() == &vanilla_entities::CREEPER
        {
            return false;
        }
        self.default_can_attack_mob(target)
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        self.animation.lock().attack = ATTACK_ANIMATION_TICKS;
        self.broadcast_entity_event(EntityStatus::StartAttacking);
        let attack_damage = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as f32;
        let damage = if attack_damage as i32 > 0 {
            attack_damage / 2.0 + rand::random_range(0..attack_damage as i32) as f32
        } else {
            attack_damage
        };
        let damage_source = self.mob_attack_damage_source(&ItemStack::empty(), entity);
        let hurt = target.hurt(world, &damage_source, damage);
        if hurt {
            let knockback_resistance = target.as_living_entity().map_or(0.0, |living| {
                living
                    .attributes()
                    .lock()
                    .get_value(vanilla_attributes::KNOCKBACK_RESISTANCE)
                    .unwrap_or(0.0)
            });
            let scale = (1.0 - knockback_resistance).max(0.0);
            target.set_velocity(
                target.velocity() + DVec3::new(0.0, f64::from(ATTACK_KNOCKUP as f32) * scale, 0.0),
            );
            target.mark_velocity_sync();
            let context = EnchantmentPostAttackContext::new(target, &damage_source);
            enchantment_helper::do_post_attack_effects_with_item_source(
                world,
                target.as_ref(),
                &self.equipment_in_slot(EquipmentSlot::MainHand),
                &context,
            );
        }
        self.play_sound(&sound_events::ENTITY_IRON_GOLEM_ATTACK, 1.0, 1.0);
        hurt
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let holds_iron = player
            .inventory
            .lock()
            .get_item_in_hand(hand)
            .is(&vanilla_items::IRON_INGOT);
        if !holds_iron {
            return InteractionResult::Pass;
        }
        let health_before = self.get_health();
        self.heal(IRON_INGOT_HEAL_AMOUNT);
        if self.get_health().total_cmp(&health_before).is_eq() {
            return InteractionResult::Pass;
        }
        let pitch = 1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2;
        self.play_sound(&sound_events::ENTITY_IRON_GOLEM_REPAIR, 1.0, pitch);
        if !player.has_infinite_materials() {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
        }
        InteractionResult::Success
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for IronGolemEntity {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};

    use super::{Crackiness, IronGolemEntity};
    use crate::entity::Entity;

    #[test]
    fn crackiness_thresholds_match_vanilla_golem() {
        assert_eq!(Crackiness::by_fraction(1.0), Crackiness::None);
        assert_eq!(Crackiness::by_fraction(0.75), Crackiness::None);
        assert_eq!(Crackiness::by_fraction(0.74), Crackiness::Low);
        assert_eq!(Crackiness::by_fraction(0.49), Crackiness::Medium);
        assert_eq!(Crackiness::by_fraction(0.24), Crackiness::High);
    }

    #[test]
    fn player_created_round_trips() {
        init_vanilla_registry();
        let golem =
            IronGolemEntity::new(&vanilla_entities::IRON_GOLEM, 1, DVec3::ZERO, Weak::new());
        golem.set_player_created(true);
        let mut nbt = NbtCompound::new();
        golem.save_additional(&mut nbt);
        assert_eq!(nbt.byte("PlayerCreated"), Some(1));
        assert_eq!(nbt.long("anger_end_time"), Some(-1));

        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded =
            IronGolemEntity::new(&vanilla_entities::IRON_GOLEM, 2, DVec3::ZERO, Weak::new());
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_player_created());
    }
}
