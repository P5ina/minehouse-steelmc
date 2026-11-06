use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::PotionContents;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::CreeperEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_entities, vanilla_game_events, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::entity::MobEffectInstance;
use crate::entity::ai::goal::{
    AvoidEntityGoal, FloatGoal, HurtByTargetGoal, LookAtPlayerGoal, MeleeAttackGoal,
    NearestAttackableTargetGoal, RandomLookAroundGoal, SwellGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::AreaEffectCloudEntity;
use crate::entity::entities::LightningBoltEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster, PathfinderMob,
    RemovalReason, SharedEntity, SpawnRule, next_entity_id, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;
use crate::world::explosion::{ExplosionInteraction, ExplosionOptions};

const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const DEFAULT_MAX_SWELL: i16 = 30;
const DEFAULT_EXPLOSION_RADIUS: i8 = 3;
const XP_REWARD: i32 = 5;
/// Ticks of fuse a fall can add, kept this many ticks short of exploding.
const FALL_SWELL_MARGIN: i32 = 5;
const FALL_SWELL_PER_BLOCK: f64 = 1.5;
const AVOID_FELINE_DISTANCE: f32 = 6.0;

const CLOUD_RADIUS: f32 = 2.5;
const CLOUD_RADIUS_ON_USE: f32 = -0.5;
const CLOUD_WAIT_TIME: i32 = 10;
const CLOUD_DURATION: i32 = 300;
const CLOUD_POTION_DURATION_SCALE: f32 = 0.25;

#[derive(Debug, Clone, Copy)]
struct SwellState {
    old_swell: i32,
    swell: i32,
    max_swell: i32,
    explosion_radius: i32,
}

/// A hostile creeper entity.
#[entity_behavior(class = "Creeper", spawn_rule)]
pub struct CreeperEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<CreeperEntityData>,
    swell: SyncMutex<SwellState>,
}

// SAFETY: The owner-scoped type key uniquely identifies CreeperEntity.
unsafe impl DowncastType for CreeperEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/creeper");
}

fn is_entity_type(target: &dyn LivingEntity, entity_type: EntityTypeRef) -> bool {
    target.entity_type() == entity_type
}

impl CreeperEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new creeper entity instance.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved creeper entity.
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
        let mut entity_data = CreeperEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(2, SwellGoal::new());
            goal_selector.add_goal(
                3,
                AvoidEntityGoal::with_selector(AVOID_FELINE_DISTANCE, 1.0, 1.2, |target, _| {
                    is_entity_type(target, &vanilla_entities::OCELOT)
                }),
            );
            goal_selector.add_goal(
                3,
                AvoidEntityGoal::with_selector(AVOID_FELINE_DISTANCE, 1.0, 1.2, |target, _| {
                    is_entity_type(target, &vanilla_entities::CAT)
                }),
            );
            goal_selector.add_goal(4, MeleeAttackGoal::new(1.0, false));
            goal_selector.add_goal(5, WaterAvoidingRandomStrollGoal::new(0.8));
            goal_selector.add_goal(6, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(6, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            );
            target_selector.add_goal(2, HurtByTargetGoal::new());
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            swell: SyncMutex::new(SwellState {
                old_swell: 0,
                swell: 0,
                max_swell: i32::from(DEFAULT_MAX_SWELL),
                explosion_radius: i32::from(DEFAULT_EXPLOSION_RADIUS),
            }),
        }
    }

    /// Returns `1` while swelling, `-1` while calming down.
    pub fn swell_dir(&self) -> i32 {
        *self.entity_data.lock().swell_dir.get()
    }

    /// Sets whether the creeper swells (`1`) or calms down (`-1`).
    pub fn set_swell_dir(&self, dir: i32) {
        self.entity_data.lock().swell_dir.set(dir);
    }

    /// Returns whether the creeper is charged.
    pub fn is_powered(&self) -> bool {
        *self.entity_data.lock().is_powered.get()
    }

    /// Sets whether the creeper is charged, as a lightning strike does.
    pub fn set_powered(&self, powered: bool) {
        self.entity_data.lock().is_powered.set(powered);
    }

    /// Returns whether the creeper was lit and will explode.
    pub fn is_ignited(&self) -> bool {
        *self.entity_data.lock().is_ignited.get()
    }

    /// Lights the fuse; the creeper explodes once fully swollen.
    pub fn ignite(&self) {
        self.entity_data.lock().is_ignited.set(true);
    }

    /// Returns the current fuse progress in ticks.
    pub fn swell(&self) -> i32 {
        self.swell.lock().swell
    }

    /// Returns the fuse length in ticks.
    pub fn max_swell(&self) -> i32 {
        self.swell.lock().max_swell
    }

    /// Returns the explosion radius before the charged multiplier.
    pub fn explosion_radius(&self) -> i32 {
        self.swell.lock().explosion_radius
    }

    /// Advances the fuse; returns `true` when the creeper should explode.
    fn tick_swell(&self) -> bool {
        if self.is_ignited() {
            self.set_swell_dir(1);
        }
        let dir = self.swell_dir();
        let (primed, explode) = {
            let mut state = self.swell.lock();
            state.old_swell = state.swell;
            let primed = dir > 0 && state.swell == 0;
            state.swell = (state.swell + dir).max(0);
            let explode = state.swell >= state.max_swell;
            if explode {
                state.swell = state.max_swell;
            }
            (primed, explode)
        };
        if primed {
            self.play_sound(&sound_events::ENTITY_CREEPER_PRIMED, 1.0, 0.5);
            self.game_event(&vanilla_game_events::PRIME_FUSE);
        }
        explode
    }

    fn explode_creeper(self: &Arc<Self>) {
        let Some(world) = self.level() else {
            return;
        };
        let multiplier = if self.is_powered() { 2.0 } else { 1.0 };
        let radius = self.explosion_radius() as f32 * multiplier;
        self.living_base.mark_death_processed();
        let mut options = ExplosionOptions::new(self.position(), radius, ExplosionInteraction::Mob);
        options.source = Some(Arc::clone(self) as SharedEntity);
        world.explode(options);
        self.spawn_lingering_cloud(&world);
        self.set_removed(RemovalReason::Discarded);
    }

    /// Leaves the creeper's active effects behind as a shrinking cloud.
    fn spawn_lingering_cloud(&self, world: &Arc<World>) {
        let effects: Vec<_> = self
            .living_base
            .active_mob_effects()
            .iter()
            .map(MobEffectInstance::to_registry_instance)
            .collect();
        if effects.is_empty() {
            return;
        }
        let cloud = Arc::new(AreaEffectCloudEntity::new(
            &vanilla_entities::AREA_EFFECT_CLOUD,
            next_entity_id(),
            self.position(),
            Arc::downgrade(world),
        ));
        cloud.set_radius(CLOUD_RADIUS);
        cloud.set_radius_on_use(CLOUD_RADIUS_ON_USE);
        cloud.set_wait_time(CLOUD_WAIT_TIME);
        cloud.set_duration(CLOUD_DURATION);
        cloud.set_potion_duration_scale(CLOUD_POTION_DURATION_SCALE);
        cloud.set_radius_per_tick(-cloud.radius() / cloud.duration() as f32);
        cloud.set_potion_contents(PotionContents::new(None, None, effects, None));
        let cloud: SharedEntity = cloud;
        if let Err(error) = world.try_add_entity(cloud) {
            log::debug!("world rejected a creeper effect cloud: {error}");
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl DefaultEntityPush for CreeperEntity {}

impl Entity for CreeperEntity {
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

    fn thunder_hit(&self, world: &World, _lightning: &LightningBoltEntity) {
        self.default_thunder_hit(world);
        self.set_powered(true);
    }

    fn tick(self: Arc<Self>) {
        if LivingEntity::is_alive(&*self) && self.tick_swell() {
            self.explode_creeper();
        }
        let entity = self.into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn cause_fall_damage(
        self: Arc<Self>,
        fall_distance: f64,
        damage_modifier: f32,
        source: &DamageSource,
    ) -> bool {
        let damaged = self.cause_living_fall_damage(fall_distance, damage_modifier, source);
        let mut state = self.swell.lock();
        state.swell += (fall_distance * FALL_SWELL_PER_BLOCK) as i32;
        state.swell = state.swell.min(state.max_swell - FALL_SWELL_MARGIN);
        damaged
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
        let state = *self.swell.lock();
        nbt.insert("powered", i8::from(self.is_powered()));
        nbt.insert(
            "Fuse",
            i16::try_from(state.max_swell).unwrap_or(DEFAULT_MAX_SWELL),
        );
        nbt.insert(
            "ExplosionRadius",
            i8::try_from(state.explosion_radius).unwrap_or(DEFAULT_EXPLOSION_RADIUS),
        );
        nbt.insert("ignited", i8::from(self.is_ignited()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.set_powered(nbt.byte("powered").is_some_and(|b| b != 0));
        {
            let mut state = self.swell.lock();
            state.max_swell = i32::from(nbt.short("Fuse").unwrap_or(DEFAULT_MAX_SWELL));
            state.explosion_radius = i32::from(
                nbt.byte("ExplosionRadius")
                    .unwrap_or(DEFAULT_EXPLOSION_RADIUS),
            );
        }
        if nbt.byte("ignited").is_some_and(|b| b != 0) {
            self.ignite();
        }
    }
}

impl LivingEntity for CreeperEntity {
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
        Some(&sound_events::ENTITY_CREEPER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_CREEPER_DEATH)
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

impl Mob for CreeperEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn set_target(&self, target: Option<&SharedEntity>) -> bool {
        // Vanilla `Creeper.setTarget` ignores goats instead of clearing the target.
        if target.is_some_and(|target| target.entity_type() == &vanilla_entities::GOAT) {
            return false;
        }
        self.mob_base()
            .set_target(target, |target| self.is_valid_target(target))
    }

    fn max_fall_distance(&self) -> i32 {
        // Vanilla `Creeper.getMaxFallDistance`: risk all but one health point while chasing.
        let allowed_damage = if self.target().is_some() {
            self.get_health() - 1.0
        } else {
            0.0
        };
        (allowed_damage + 3.0).floor() as i32
    }

    fn do_hurt_target(
        &self,
        _entity: &SharedEntity,
        _world: &World,
        _target: &SharedEntity,
    ) -> bool {
        // Creepers deal damage by exploding, not by their melee swing.
        true
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if !item_stack.item.has_tag(&ItemTag::CREEPER_IGNITERS) {
            return InteractionResult::Pass;
        }

        let sound = if item_stack.is(&vanilla_items::FIRE_CHARGE) {
            &sound_events::ITEM_FIRECHARGE_USE
        } else {
            &sound_events::ITEM_FLINTANDSTEEL_USE
        };
        if let Some(world) = self.level() {
            world.play_sound_at(
                sound,
                self.sound_source(),
                self.position(),
                1.0,
                rand::random::<f32>() * 0.4 + 0.8,
                Some(player.id()),
            );
        }
        self.ignite();
        let mut inventory = player.inventory.lock();
        if item_stack.is_damageable_item() {
            inventory.hurt_item_in_hand(hand, 1, player.has_infinite_materials());
        } else if !player.has_infinite_materials() {
            inventory.shrink_item_in_hand(hand, 1);
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

impl Monster for CreeperEntity {}

impl PathfinderMob for CreeperEntity {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Weak};

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_damage_types, vanilla_entities};
    use steel_utils::{ChunkPos, Downcast as _};

    use super::{CreeperEntity, DEFAULT_MAX_SWELL};
    use crate::entity::damage::DamageSource;
    use crate::entity::{Entity, SharedEntity, next_entity_id};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn creeper_nbt_round_trip() {
        init_vanilla_registry();
        let creeper = CreeperEntity::new(&vanilla_entities::CREEPER, 1, DVec3::ZERO, Weak::new());
        assert!(!creeper.is_powered());
        assert!(!creeper.is_ignited());
        assert_eq!(creeper.swell_dir(), -1);
        assert_eq!(creeper.max_swell(), i32::from(DEFAULT_MAX_SWELL));

        creeper.set_powered(true);
        creeper.ignite();
        let mut nbt = NbtCompound::new();
        creeper.save_additional(&mut nbt);
        assert_eq!(nbt.byte("powered"), Some(1));
        assert_eq!(nbt.short("Fuse"), Some(DEFAULT_MAX_SWELL));
        assert_eq!(nbt.byte("ExplosionRadius"), Some(3));
        assert_eq!(nbt.byte("ignited"), Some(1));

        let mut nbt = NbtCompound::new();
        nbt.insert("powered", 1_i8);
        nbt.insert("Fuse", 12_i16);
        nbt.insert("ExplosionRadius", 7_i8);
        nbt.insert("ignited", 1_i8);
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = CreeperEntity::new(&vanilla_entities::CREEPER, 2, DVec3::ZERO, Weak::new());
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_powered());
        assert!(loaded.is_ignited());
        assert_eq!(loaded.max_swell(), 12);
        assert_eq!(loaded.explosion_radius(), 7);
    }

    #[test]
    fn fall_adds_fuse_but_never_explodes_by_itself() {
        init_vanilla_registry();
        let creeper = Arc::new(CreeperEntity::new(
            &vanilla_entities::CREEPER,
            1,
            DVec3::ZERO,
            Weak::new(),
        ));
        Arc::clone(&creeper).cause_fall_damage(
            10.0,
            1.0,
            &DamageSource::environment(&vanilla_damage_types::FALL),
        );
        assert_eq!(creeper.swell(), 15);
        Arc::clone(&creeper).cause_fall_damage(
            40.0,
            1.0,
            &DamageSource::environment(&vanilla_damage_types::FALL),
        );
        assert_eq!(creeper.swell(), creeper.max_swell() - 5);
    }

    #[test]
    fn ignited_creeper_explodes_when_the_fuse_runs_out() {
        init_vanilla_registry();
        let world_fixture = fresh_test_world("creeper_fuse");
        let world = &world_fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));

        let entity: SharedEntity = Arc::new(CreeperEntity::new(
            &vanilla_entities::CREEPER,
            next_entity_id(),
            DVec3::new(8.0, 65.0, 8.0),
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::clone(&entity))
            .expect("test creeper should attach to the loaded chunk");
        let creeper = entity
            .downcast_ref::<CreeperEntity>()
            .expect("creeper is concrete");
        creeper.ignite();

        for _ in 0..creeper.max_swell() - 1 {
            assert!(!creeper.tick_swell());
        }
        assert!(creeper.tick_swell(), "the last fuse tick explodes");
        assert_eq!(creeper.swell(), creeper.max_swell());
    }
}
