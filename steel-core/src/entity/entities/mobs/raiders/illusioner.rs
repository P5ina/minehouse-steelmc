//! Illusioner (Vanilla `Illusioner`): the bow-wielding illager that hides behind mirror images.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::IllusionerEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::behavior::items::projectile_weapon::{
    get_mob_arrow, monster_projectile, spawn_arrow_using_shoot,
};
use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    AvoidEntityGoal, BlindnessSpell, FloatGoal, HurtByTargetGoal, LongDistancePatrolGoal,
    LookAtPlayerGoal, MirrorSpell, NearestAttackableTargetGoal, RandomStrollGoal,
    RangedBowAttackGoal, SpellcasterCastingSpellGoal, SpellcasterUseSpellGoal, weapon_holding_hand,
};
use crate::entity::damage::DamageSource;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    AbstractIllager, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    Monster, PathfinderMob, PatrollingMonster, PatrollingMonsterState, Raider, RaiderState,
    RangedAttackMob, SharedEntity, SpellcasterIllager, SpellcasterState, is_abstract_villager,
    sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 5;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `AvoidEntityGoal<>(this, Creaking.class, 8.0F, 1.0, 1.2)`.
const AVOID_CREAKING_DISTANCE: f32 = 8.0;
/// Vanilla `RangedBowAttackGoal<>(this, 0.5, 20, 15.0F)`.
const BOW_GOAL_SPEED: f64 = 0.5;
const BOW_GOAL_INTERVAL: i32 = 20;
const BOW_GOAL_RADIUS: f32 = 15.0;
/// Vanilla `Projectile.spawnProjectileUsingShoot(..., 1.6F, ...)`.
const ARROW_POWER: f32 = 1.6;
/// Vanilla `target.getY(0.3333333333333333)`.
const TARGET_HEIGHT_FRACTION: f64 = 0.333_333_333_333_333_3;
/// Vanilla `distanceToTarget * 0.2F`.
const ARROW_ARC: f32 = 0.2;
/// Vanilla `setUnseenMemoryTicks(300)` on every nearest-target goal.
const TARGET_UNSEEN_MEMORY_TICKS: i32 = 300;

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

/// An illusioner: turns invisible, blinds its target and shoots arrows.
#[entity_behavior(class = "Illusioner", spawn_rule)]
pub struct IllusionerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    spellcaster: SpellcasterState,
    entity_data: SyncMutex<IllusionerEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies IllusionerEntity.
unsafe impl DowncastType for IllusionerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/illusioner");
}

impl IllusionerEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new illusioner.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved illusioner.
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
        let mut entity_data = IllusionerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            // TODO: Raider goals (`ObtainRaidLeaderBannerGoal` 1, `PathfindToRaidGoal` 3,
            // `RaiderMoveThroughVillageGoal` 4, `RaiderCelebration` 5) come with raids.
            // Vanilla `PatrollingMonster.registerGoals`.
            goal_selector.add_goal(4, LongDistancePatrolGoal::new(0.7, 0.595));
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, SpellcasterCastingSpellGoal::new());
            goal_selector.add_goal(
                3,
                AvoidEntityGoal::with_selector(AVOID_CREAKING_DISTANCE, 1.0, 1.2, |target, _| {
                    target.entity_type() == &vanilla_entities::CREAKING
                }),
            );
            goal_selector.add_goal(4, SpellcasterUseSpellGoal::new(MirrorSpell::new()));
            goal_selector.add_goal(5, SpellcasterUseSpellGoal::new(BlindnessSpell::new()));
            goal_selector.add_goal(
                6,
                RangedBowAttackGoal::new(BOW_GOAL_SPEED, BOW_GOAL_INTERVAL, BOW_GOAL_RADIUS),
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
                })
                .with_unseen_memory_ticks(TARGET_UNSEEN_MEMORY_TICKS),
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
        }
    }
}

impl DefaultEntityPush for IllusionerEntity {}

impl Entity for IllusionerEntity {
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

    fn is_allied_to(&self, other: &dyn Entity) -> bool {
        self.illager_is_allied_to(other)
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

impl LivingEntity for IllusionerEntity {
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
        Some(&sound_events::ENTITY_ILLUSIONER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ILLUSIONER_DEATH)
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

impl Mob for IllusionerEntity {
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
        Some(&sound_events::ENTITY_ILLUSIONER_AMBIENT)
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

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let _ = self
            .living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, ItemStack::new(&vanilla_items::BOW));
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

impl PathfinderMob for IllusionerEntity {}

impl Monster for IllusionerEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for IllusionerEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for IllusionerEntity {
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
        &sound_events::ENTITY_ILLUSIONER_AMBIENT
    }
}

impl AbstractIllager for IllusionerEntity {}

impl SpellcasterIllager for IllusionerEntity {
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
        &sound_events::ENTITY_ILLUSIONER_CAST_SPELL
    }
}

impl RangedAttackMob for IllusionerEntity {
    /// Vanilla `Illusioner.performRangedAttack`.
    fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, power: f32) {
        let Some(world) = self.level() else {
            return;
        };
        let slot = match weapon_holding_hand(self, &vanilla_items::BOW) {
            InteractionHand::MainHand => EquipmentSlot::MainHand,
            InteractionHand::OffHand => EquipmentSlot::OffHand,
        };
        let bow = self.living_base.equipment().lock().get_ref(slot).clone();
        let projectile = monster_projectile(self, &bow);
        let arrow = get_mob_arrow(&world, entity, &projectile, power, Some(&bow));
        let position = self.position();
        let target_position = target.position();
        let target_box = target.bounding_box();
        let xd = target_position.x - position.x;
        let yd = target_position.y
            + (target_box.max_y() - target_box.min_y()) * TARGET_HEIGHT_FRACTION
            - arrow.position().y;
        let zd = target_position.z - position.z;
        let distance_to_target = (xd * xd + zd * zd).sqrt();
        spawn_arrow_using_shoot(
            &world,
            &arrow,
            &projectile,
            entity,
            DVec3::new(xd, yd + distance_to_target * f64::from(ARROW_ARC), zd),
            ARROW_POWER,
            self.ranged_attack_uncertainty(&world),
        );
        self.play_sound(
            &sound_events::ENTITY_SKELETON_SHOOT,
            1.0,
            1.0 / (rand::random::<f32>() * 0.4 + 0.8),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::item_stack::ItemStack;
    use steel_registry::{
        init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items, vanilla_mob_effects,
    };
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos, WorldAabb};

    use super::IllusionerEntity;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::CowEntity;
    use crate::entity::{LivingEntity, Mob, SharedEntity, next_entity_id};
    use crate::inventory::equipment::EquipmentSlot;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn illusioner_turns_invisible_and_shoots_its_target() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("illusioner_shoots");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let stone = vanilla_blocks::STONE.default_state();
        for x in 0..16 {
            for z in 0..16 {
                world.set_block(BlockPos::new(x, 64, z), stone, UpdateFlags::UPDATE_ALL);
            }
        }

        let illusioner = Arc::new(IllusionerEntity::new(
            &vanilla_entities::ILLUSIONER,
            next_entity_id(),
            DVec3::new(3.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        illusioner
            .living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, ItemStack::new(&vanilla_items::BOW));
        let illusioner_entity: SharedEntity = Arc::<IllusionerEntity>::clone(&illusioner);
        world
            .try_add_entity(Arc::clone(&illusioner_entity))
            .expect("illusioner should attach to the loaded chunk");
        let cow: SharedEntity = Arc::new(CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(9.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::clone(&cow))
            .expect("cow should attach to the loaded chunk");
        assert!(illusioner.set_target(Some(&cow)));

        let area = WorldAabb::new(0.0, 60.0, 0.0, 16.0, 80.0, 16.0);
        let mut fired = false;
        for _ in 0..200 {
            Arc::clone(&illusioner_entity).tick();
            fired |= world
                .get_entities_in_aabb(&area)
                .iter()
                .any(|entity| entity.entity_type().key == vanilla_entities::ARROW.key);
            if fired && illusioner.has_mob_effect(vanilla_mob_effects::INVISIBILITY) {
                break;
            }
        }
        assert!(
            illusioner.has_mob_effect(vanilla_mob_effects::INVISIBILITY),
            "the mirror spell turns the illusioner invisible"
        );
        assert!(fired, "the illusioner shoots an arrow at its target");
    }
}
