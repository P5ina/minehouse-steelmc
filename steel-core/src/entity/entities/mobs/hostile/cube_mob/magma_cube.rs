use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::MagmaCubeEntityData;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities};
use steel_utils::Identifier;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{
    AbstractCubeMob, CubeMobAttackGoal, CubeMobBase, CubeMobFloatGoal, CubeMobKeepOnJumpingGoal,
    CubeMobRandomDirectionGoal,
};
use crate::entity::ai::goal::NearestAttackableTargetGoal;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use steel_utils::types::Difficulty;

use crate::entity::SpawnRule;
use crate::entity::{
    AgeableMob, AgeableMobBase, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    PathfinderMob, RemovalReason, SharedEntity, SpawnGroupData, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

/// Vertical distance within which a magma cube notices players.
const PLAYER_TARGET_MAX_Y_DIFFERENCE: f64 = 4.0;

/// A magma cube: a fire-proof Nether slime that jumps higher and hits harder.
#[entity_behavior(class = "MagmaCube", spawn_rule)]
pub struct MagmaCubeEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    cube_mob_base: CubeMobBase,
    entity_data: SyncMutex<MagmaCubeEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies MagmaCubeEntity.
unsafe impl DowncastType for MagmaCubeEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/magma_cube");
}

impl MagmaCubeEntity {
    /// Vanilla `SpawnPlacements` predicate: `MagmaCube.checkMagmaCubeSpawnRules`, which
    /// only rejects Peaceful.
    pub(crate) const SPAWN_RULE: SpawnRule = |_entity_type, level, _spawn_reason, _pos, _random| {
        level.difficulty() != Difficulty::Peaceful
    };

    /// Creates a new magma cube of size 1.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved magma cube.
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
        let mut entity_data = MagmaCubeEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let y_rot = base.rotation().0;

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, CubeMobFloatGoal::new(&mob_base));
            goal_selector.add_goal(2, CubeMobAttackGoal::new());
            goal_selector.add_goal(4, CubeMobRandomDirectionGoal::new());
            goal_selector.add_goal(5, CubeMobKeepOnJumpingGoal);

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new_for_players_from_mob(
                    10,
                    true,
                    false,
                    |target, mob, _| {
                        (target.position().y - mob.position().y).abs()
                            <= PLAYER_TARGET_MAX_Y_DIFFERENCE
                    },
                ),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            );
        }

        let cube = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            cube_mob_base: CubeMobBase::new(y_rot),
            entity_data: SyncMutex::new(entity_data),
        };
        cube.refresh_dimensions();
        cube
    }
}

impl Entity for MagmaCubeEntity {
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
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        self.tick_landing();
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn set_removed(&self, reason: RemovalReason) {
        self.split_on_death();
        self.base().set_removed(reason);
    }

    fn push_entity(&self, entity: &dyn Entity) {
        self.cube_push(entity);
    }

    fn player_touch(self: Arc<Self>, player: &Arc<Player>) {
        let attacker: SharedEntity = Arc::clone(&self) as SharedEntity;
        self.cube_player_touch(&attacker, player.as_ref());
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.cube_dimensions()
    }

    fn refresh_dimensions(&self) {
        self.refresh_cube_dimensions();
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn is_on_fire(&self) -> bool {
        false
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_cube_mob(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_cube_mob_size(nbt);
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_cube_mob_was_on_ground(nbt);
    }
}

impl LivingEntity for MagmaCubeEntity {
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

    fn sound_volume(&self) -> f32 {
        self.cube_sound_volume()
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_tiny() {
            &sound_events::ENTITY_MAGMA_CUBE_HURT_SMALL
        } else {
            &sound_events::ENTITY_MAGMA_CUBE_HURT
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_tiny() {
            &sound_events::ENTITY_MAGMA_CUBE_DEATH_SMALL
        } else {
            &sound_events::ENTITY_MAGMA_CUBE_DEATH
        })
    }

    fn jump_from_ground(&self) {
        self.cube_jump_from_ground(self.get_size() as f32 * 0.1);
    }

    fn jump_in_liquid(&self, fluid_tag: &Identifier) {
        if fluid_tag != &FluidTag::LAVA {
            self.default_jump_in_liquid(fluid_tag);
            return;
        }
        let movement = self.velocity();
        self.set_velocity(DVec3::new(
            movement.x,
            f64::from(0.22_f32 + self.get_size() as f32 * 0.05),
            movement.z,
        ));
        self.mark_velocity_sync();
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        result
    }
}

impl Mob for MagmaCubeEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        self.tick_cube_move_control();
    }

    fn max_head_x_rot(&self) -> f32 {
        0.0
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_cube_mob(world, spawn_reason, group_data)
    }
}

impl AgeableMob for MagmaCubeEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn can_be_a_baby(&self) -> bool {
        false
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
}

impl AbstractCubeMob for MagmaCubeEntity {
    fn cube_mob_base(&self) -> &CubeMobBase {
        &self.cube_mob_base
    }

    fn get_size(&self) -> i32 {
        *self.entity_data.lock().abstract_cube_mob.id_size.get()
    }

    fn set_synced_size(&self, size: i32) {
        self.entity_data.lock().abstract_cube_mob.id_size.set(size);
    }

    fn jump_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_MAGMA_CUBE_JUMP
    }

    fn get_jump_delay(&self) -> i32 {
        (rand::random_range(0..20) + 10) * 4
    }

    fn can_deal_damage(&self) -> bool {
        self.is_effective_ai()
    }

    fn cube_attack_damage(&self) -> f32 {
        self.attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as f32
            + 2.0
    }

    fn squish_sound(&self) -> SoundEventRef {
        if self.is_tiny() {
            &sound_events::ENTITY_MAGMA_CUBE_SQUISH_SMALL
        } else {
            &sound_events::ENTITY_MAGMA_CUBE_SQUISH
        }
    }

    fn apply_size_attributes(&self, actual_size: i32, requested_size: i32) {
        let mut attributes = self.attributes().lock();
        attributes.set_base_value(vanilla_attributes::ATTACK_DAMAGE, f64::from(actual_size));
        // Vanilla scales armor by the unclamped size.
        attributes.set_base_value(vanilla_attributes::ARMOR, f64::from(requested_size * 3));
        drop(attributes);
        self.set_xp_reward(actual_size);
    }
}

impl PathfinderMob for MagmaCubeEntity {}
