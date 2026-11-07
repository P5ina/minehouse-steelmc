//! Evoker (Vanilla `Evoker`): the spell-casting illager of woodland mansions.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::EvokerEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::{Downcast as _, DowncastType, DowncastTypeKey};

use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    AvoidEntityGoal, FangsSpell, FloatGoal, HurtByTargetGoal, LongDistancePatrolGoal,
    LookAtPlayerGoal, NearestAttackableTargetGoal, RandomStrollGoal, SpellcasterCastingSpellGoal,
    SpellcasterUseSpellGoal, SummonVexSpell, WololoSpell, WololoTarget, add_raider_goals,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::entities::VexEntity;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    AbstractIllager, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    Monster, PathfinderMob, PatrollingMonster, PatrollingMonsterState, Raider, RaiderState,
    SharedEntity, SpellcasterIllager, SpellcasterState, entity_selector, is_abstract_villager,
    sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 10;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `AvoidEntityGoal<>(this, Player.class / Creaking.class, 8.0F, 0.6, 1.0)`.
const AVOID_DISTANCE: f32 = 8.0;
/// Vanilla `setUnseenMemoryTicks(300)` on the player and villager target goals.
const TARGET_UNSEEN_MEMORY_TICKS: i32 = 300;

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

/// An evoker: summons vexes, conjures fangs and turns blue sheep red.
#[entity_behavior(class = "Evoker", spawn_rule)]
pub struct EvokerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    spellcaster: SpellcasterState,
    entity_data: SyncMutex<EvokerEntityData>,
    wololo_target: WololoTarget,
}

// SAFETY: The owner-scoped type key uniquely identifies EvokerEntity.
unsafe impl DowncastType for EvokerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/evoker");
}

impl EvokerEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new evoker.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved evoker.
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
        mob_base.set_xp_reward(XP_REWARD);
        let mut entity_data = EvokerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let wololo_target = WololoTarget::default();

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            // Vanilla `PatrollingMonster.registerGoals`, then `Raider.registerGoals`.
            goal_selector.add_goal(4, LongDistancePatrolGoal::new(0.7, 0.595));
            add_raider_goals(&mut goal_selector);
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            let look_target = wololo_target.clone();
            goal_selector.add_goal(
                1,
                SpellcasterCastingSpellGoal::with_look_fallback(Box::new(move |_| {
                    look_target.get()
                })),
            );
            goal_selector.add_goal(
                2,
                AvoidEntityGoal::with_selector(AVOID_DISTANCE, 0.6, 1.0, |target, _| {
                    target.entity_type() == &vanilla_entities::PLAYER
                        && entity_selector::no_creative_or_spectator(
                            target.as_entity_event_source(),
                        )
                }),
            );
            goal_selector.add_goal(
                3,
                AvoidEntityGoal::with_selector(AVOID_DISTANCE, 0.6, 1.0, |target, _| {
                    target.entity_type() == &vanilla_entities::CREAKING
                        && entity_selector::no_creative_or_spectator(
                            target.as_entity_event_source(),
                        )
                }),
            );
            goal_selector.add_goal(4, SpellcasterUseSpellGoal::new(SummonVexSpell::new()));
            goal_selector.add_goal(5, SpellcasterUseSpellGoal::new(FangsSpell::new()));
            goal_selector.add_goal(
                6,
                SpellcasterUseSpellGoal::new(WololoSpell::new(wololo_target.clone())),
            );
            goal_selector.add_goal(8, RandomStrollGoal::new(0.6));
            goal_selector.add_goal(9, LookAtPlayerGoal::new_with_probability(3.0, 1.0));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(8.0, 0.02, |target, _| {
                    target.as_mob().is_some()
                }),
            );

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                HurtByTargetGoal::new()
                    .with_ignored_damage_selector(is_raider)
                    .set_alert_others([]),
            );
            target_selector.add_goal(
                2,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true)
                    .with_unseen_memory_ticks(TARGET_UNSEEN_MEMORY_TICKS),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(false, |target, _| is_abstract_villager(target))
                    .with_unseen_memory_ticks(TARGET_UNSEEN_MEMORY_TICKS),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(false, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            patrolling: PatrollingMonsterState::new(),
            raider: RaiderState::new(),
            spellcaster: SpellcasterState::new(),
            entity_data: SyncMutex::new(entity_data),
            wololo_target,
        }
    }

    /// Vanilla `getWololoTarget`.
    #[must_use]
    pub fn wololo_target(&self) -> Option<SharedEntity> {
        self.wololo_target.get()
    }
}

impl DefaultEntityPush for EvokerEntity {}

impl Entity for EvokerEntity {
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

    /// Vanilla `Evoker.considersEntityAsAlly`: also sides with vexes summoned by
    /// itself or by a fellow illager.
    fn is_allied_to(&self, other: &dyn Entity) -> bool {
        if other.uuid() == self.uuid() || self.illager_is_allied_to(other) {
            return true;
        }
        other
            .downcast_ref::<VexEntity>()
            .and_then(VexEntity::root_owner)
            .is_some_and(|root_owner| {
                root_owner.uuid() == self.uuid() || self.illager_is_allied_to(root_owner.as_ref())
            })
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_spellcaster(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_spellcaster(nbt);
    }
}

impl LivingEntity for EvokerEntity {
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
        Some(&sound_events::ENTITY_EVOKER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_EVOKER_DEATH)
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

    fn die(&self, source: &DamageSource) {
        self.die_raider(source);
        self.default_die(source);
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_server_raider(world);
        self.default_hurt_server(world, source, amount)
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        self.raider_ai_step(entity);
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for EvokerEntity {
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
        self.custom_server_ai_step_spellcaster();
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_EVOKER_AMBIENT)
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        self.illager_can_attack(target)
    }

    fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_raider(dist_sqr)
    }

    fn requires_custom_persistence(&self) -> bool {
        self.requires_custom_persistence_raider()
    }

    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        if !self.pick_up_item_raider(world, item_entity) {
            self.default_pick_up_item(world, item_entity);
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_raider(spawn_reason);
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self
            .entity_data
            .lock()
            .spellcaster_illager
            .raider
            .mob
            .mob_flags
            .get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data
            .lock()
            .spellcaster_illager
            .raider
            .mob
            .mob_flags
            .set(flags);
    }
}

impl PathfinderMob for EvokerEntity {}

impl Monster for EvokerEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for EvokerEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for EvokerEntity {
    fn raider_state(&self) -> &RaiderState {
        &self.raider
    }

    fn is_celebrating(&self) -> bool {
        *self
            .entity_data
            .lock()
            .spellcaster_illager
            .raider
            .is_celebrating
            .get()
    }

    fn set_celebrating(&self, celebrating: bool) {
        self.entity_data
            .lock()
            .spellcaster_illager
            .raider
            .is_celebrating
            .set(celebrating);
    }

    fn celebrate_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_EVOKER_CELEBRATE
    }
}

impl AbstractIllager for EvokerEntity {}

impl SpellcasterIllager for EvokerEntity {
    fn spellcaster_state(&self) -> &SpellcasterState {
        &self.spellcaster
    }

    fn set_synced_spell(&self, spell_id: i8) {
        self.entity_data
            .lock()
            .spellcaster_illager
            .spell_casting
            .set(spell_id);
    }

    fn casting_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_EVOKER_CAST_SPELL
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};

    use super::EvokerEntity;
    use crate::entity::{Entity, IllagerSpell, SpellcasterIllager};

    #[test]
    fn spell_ticks_round_trip_and_count_down() {
        init_vanilla_registry();
        let evoker = EvokerEntity::new(&vanilla_entities::EVOKER, 1, DVec3::ZERO, Weak::new());
        evoker.set_spell_casting_time(2);
        evoker.set_is_casting_spell(IllagerSpell::Fangs);
        assert_eq!(evoker.current_spell(), IllagerSpell::Fangs);

        let mut nbt = NbtCompound::new();
        evoker.save_additional(&mut nbt);
        assert_eq!(nbt.int("SpellTicks"), Some(2));
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = EvokerEntity::new(&vanilla_entities::EVOKER, 2, DVec3::ZERO, Weak::new());
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_casting_spell());
        loaded.custom_server_ai_step_spellcaster();
        loaded.custom_server_ai_step_spellcaster();
        assert!(!loaded.is_casting_spell());
    }
}
