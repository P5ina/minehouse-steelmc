//! Vanilla `Strider`: the Nether animal that walks on lava and can be ridden
//! with a warped fungus on a stick.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::shapes::VoxelShape;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::fluid::FluidState;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::StriderEntityData;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_entities, vanilla_items,
};
use steel_utils::axis::Axis;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{
    BlockLocalAabb, BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey,
};

use crate::behavior::{BlockStateBehaviorExt as _, InteractionResult};
use crate::entity::ai::goal::{
    BreedGoal, FollowParentGoal, LookAtPlayerGoal, PanicGoal, RandomLookAroundGoal,
    RandomStrollGoal, TemptGoal,
};
use crate::entity::ai::path::{PathComputationType, PathType};
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::damage::DamageSource;
use crate::entity::entities::mobs::hostile::ZombieGroupData;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::spawn::{AgeableMobGroupData, create_entity_instance};
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, ItemBasedSteering,
    ItemSteerable, LivingEntity, LivingEntityBase, LivingEntitySyncedData, Mob, MobBase,
    MoveResult, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule, dismount_helper,
    sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::player::Player;
use crate::world::{LevelReader, ServerLevelAccessor, World};

mod goals;
#[cfg(test)]
mod tests;

use goals::StriderGoToLavaGoal;

const STRIDER_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.656_25, 0.0)];
/// Vanilla `BABY_DIMENSIONS`.
const STRIDER_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.45,
    0.85,
    0.4375,
    EntityAttachments::new(&STRIDER_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
);
/// Vanilla `getLiquidCollisionShape`: `Block.column(16.0, 0.0, 8.0)`.
const LIQUID_COLLISION_BOXES: &[BlockLocalAabb] =
    &[BlockLocalAabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0)];
const LIQUID_COLLISION_SHAPE: VoxelShape = VoxelShape::from_boxes(LIQUID_COLLISION_BOXES);
/// Vanilla `SUFFOCATING_MODIFIER`: −34 % movement speed out of the heat.
const SUFFOCATING_MODIFIER_AMOUNT: f64 = -0.34;
const SUFFOCATE_STEERING_MODIFIER: f32 = 0.35;
const STEERING_MODIFIER: f32 = 0.55;
const ZOMBIFIED_PIGLIN_JOCKEY_CHANCE: u32 = 30;
const BABY_STRIDER_JOCKEY_CHANCE: u32 = 10;
/// Vanilla `AgeableMob.BABY_START_AGE`.
const BABY_START_AGE: i32 = -24000;

const fn suffocating_modifier_id() -> steel_utils::Identifier {
    steel_utils::Identifier::vanilla_static("suffocating")
}

/// A strider.
#[entity_behavior(class = "Strider", spawn_rule)]
pub struct StriderEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    steering: SyncMutex<ItemBasedSteering>,
    entity_data: SyncMutex<StriderEntityData>,
    /// A jockey from `finalizeSpawn` that mounts once both are in the world.
    ///
    /// Vanilla calls `startRiding` inside `finalizeSpawn`, before the strider
    /// joins the level; Steel can only link riders that are already in it.
    pending_jockey: SyncMutex<Option<SharedEntity>>,
}

// SAFETY: The owner-scoped type key uniquely identifies StriderEntity.
unsafe impl DowncastType for StriderEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/strider");
}

impl StriderEntity {
    /// Vanilla `Strider.getDismountLocationForPassenger` without the base-entity fallback:
    /// the first floor that is not lava, ahead of the passenger and up to 45° to either side.
    fn strider_dismount_location(&self, passenger: &dyn Entity) -> Option<DVec3> {
        let world = self.level()?;
        let width = f64::from(self.base().dimensions().width);
        let passenger_width = f64::from(passenger.base().dimensions().width);
        let passenger_yaw = passenger.rotation().0;
        let position = self.position();
        let bounds = self.bounding_box();
        let (collider_top, collider_bottom) = (bounds.max_y(), bounds.min_y() - 0.5);

        let mut targets: Vec<BlockPos> = Vec::new();
        for yaw_offset in [0.0, -22.5, 22.5, -45.0, 45.0] {
            let direction = dismount_helper::collision_horizontal_escape_vector(
                width,
                passenger_width,
                passenger_yaw + yaw_offset,
            );
            let mut target = BlockPos::containing(
                position.x + direction.x,
                collider_top,
                position.z + direction.z,
            );
            let mut y = collider_top;
            while y > collider_bottom {
                if !targets.contains(&target) {
                    targets.push(target);
                }
                target = target.below();
                y -= 1.0;
            }
        }

        for target in targets {
            if world
                .get_block_state(target)
                .get_fluid_state()
                .fluid_id
                .has_tag(&FluidTag::LAVA)
            {
                continue;
            }
            let floor_height = dismount_helper::block_floor_height_at(&world, target);
            if !dismount_helper::is_block_floor_valid(floor_height) {
                continue;
            }
            let location = dismount_helper::up_from_bottom_center_of(target, floor_height);
            for &pose in dismount_helper::dismount_poses(passenger) {
                let pose_box = dismount_helper::local_bounds_for_pose(passenger, pose);
                if dismount_helper::can_passenger_dismount_to(
                    &world,
                    passenger,
                    &pose_box.translate(location),
                ) {
                    passenger.set_pose(pose);
                    return Some(location);
                }
            }
        }
        None
    }

    /// Vanilla `checkStriderSpawnRules`: the first block above the lava is air.
    pub(crate) const SPAWN_RULE: SpawnRule = |_entity_type, level, _spawn_reason, pos, _random| {
        let mut check_pos = pos.above();
        while level
            .get_block_state(check_pos)
            .get_fluid_state()
            .fluid_id
            .has_tag(&FluidTag::LAVA)
        {
            check_pos = check_pos.above();
        }
        level.get_block_state(check_pos).is_air()
    };

    /// Creates a new strider.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved strider.
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
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::Water, -1.0);
            malus.set(PathType::Lava, 0.0);
            malus.set(PathType::FireInNeighbor, 0.0);
            malus.set(PathType::Fire, 0.0);
        }
        let mut entity_data = StriderEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, PanicGoal::new(1.65));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(
                3,
                TemptGoal::new(
                    1.4,
                    |item_stack| {
                        REGISTRY
                            .items
                            .is_in_tag(item_stack.item(), &ItemTag::STRIDER_TEMPT_ITEMS)
                    },
                    false,
                ),
            );
            goal_selector.add_goal(4, StriderGoToLavaGoal::new(1.0));
            goal_selector.add_goal(5, FollowParentGoal::new(1.0));
            goal_selector.add_goal(7, RandomStrollGoal::with_interval(1.0, 60));
            goal_selector.add_goal(8, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(8, RandomLookAroundGoal::new());
            goal_selector.add_goal(
                9,
                LookAtPlayerGoal::new_for_living_entities(8.0, 0.02, |target, _| {
                    target.entity_type() == &vanilla_entities::STRIDER
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
            steering: SyncMutex::new(ItemBasedSteering::new()),
            entity_data: SyncMutex::new(entity_data),
            pending_jockey: SyncMutex::new(None),
        }
    }

    /// Whether the strider is out of the heat (shivering, slower, purple).
    #[must_use]
    pub fn is_suffocating(&self) -> bool {
        *self.entity_data.lock().suffocating.get()
    }

    /// Vanilla `setSuffocating`.
    pub fn set_suffocating(&self, suffocating: bool) {
        self.entity_data.lock().suffocating.set(suffocating);
        let mut attributes = self.attributes().lock();
        if suffocating {
            attributes.set_modifier(
                vanilla_attributes::MOVEMENT_SPEED,
                AttributeModifier {
                    id: suffocating_modifier_id(),
                    amount: SUFFOCATING_MODIFIER_AMOUNT,
                    operation: AttributeModifierOperation::AddMultipliedBase,
                },
                false,
            );
        } else {
            attributes.remove_modifier(
                vanilla_attributes::MOVEMENT_SPEED,
                &suffocating_modifier_id(),
            );
        }
    }

    /// Whether the strider's tempt goal is running.
    fn is_being_tempted(&self) -> bool {
        self.mob_base
            .goal_selector()
            .lock()
            .has_running_tempt_goal()
    }

    /// The pre-tick part of vanilla `Strider.tick`.
    fn tick_heat_and_sounds(&self) {
        if self.is_being_tempted() && rand::random_range(0..140) == 0 {
            self.make_sound(Some(&sound_events::ENTITY_STRIDER_HAPPY));
        } else if self.is_panicking() && rand::random_range(0..60) == 0 {
            self.make_sound(Some(&sound_events::ENTITY_STRIDER_RETREAT));
        }

        if self.is_no_ai() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let is_warm =
            |state: BlockStateId| state.get_block().has_tag(&BlockTag::STRIDER_WARM_BLOCKS);
        let state_on = self
            .on_pos_legacy()
            .map_or(vanilla_blocks::AIR.default_state(), |pos| {
                world.get_block_state(pos)
            });
        let in_warm_blocks = is_warm(world.get_block_state(self.block_position()))
            || is_warm(state_on)
            || self.fluid_contact().lava_height() > 0.0;
        let on_warm_strider = self.vehicle().is_some_and(|vehicle| {
            vehicle
                .downcast_ref::<Self>()
                .is_some_and(|strider| !strider.is_suffocating())
        });
        self.set_suffocating(!in_warm_blocks && !on_warm_strider);
    }

    /// Vanilla `floatStrider`: rest on the lava surface, or bob up through it.
    fn float_strider(&self) {
        if !self.is_in_lava() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let block_position = self.block_position();
        let above_surface = self.position().y
            > f64::from(block_position.y()) + LIQUID_COLLISION_SHAPE.max(Axis::Y)
                - f64::from(1.0e-5_f32);
        let lava_above = world
            .get_block_state(block_position.above())
            .get_fluid_state()
            .fluid_id
            .has_tag(&FluidTag::LAVA);
        if above_surface && !lava_above {
            self.set_on_ground(true);
        } else {
            self.set_velocity(self.velocity() * 0.5 + DVec3::new(0.0, 0.05, 0.0));
        }
    }

    fn set_ridden_rotation(&self, controller_yaw: f32, controller_pitch: f32) {
        self.set_rotation((controller_yaw, controller_pitch * 0.5));
        self.base.set_old_yaw_to_current();
        let yaw = self.rotation().0;
        self.set_y_body_rot(yaw);
        self.set_y_head_rot(yaw);
    }

    /// Vanilla `spawnJockey`: finalize `jockey` on this strider, which it
    /// mounts once both are in the world.
    fn spawn_jockey(
        &self,
        world: &Arc<World>,
        jockey: &SharedEntity,
        jockey_group_data: Option<SpawnGroupData>,
    ) -> SpawnGroupData {
        jockey.set_rotation((self.rotation().0, 0.0));
        if let Some(mob) = jockey.as_mob() {
            let _ = mob.finalize_spawn(world, EntitySpawnReason::Jockey, jockey_group_data);
        }
        if world.try_add_entity(Arc::clone(jockey)).is_ok() {
            *self.pending_jockey.lock() = Some(Arc::clone(jockey));
        }
        SpawnGroupData::AgeableMob(AgeableMobGroupData::with_baby_spawn_chance(0.0))
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    /// Returns whether the stack is vanilla strider food.
    #[must_use]
    pub fn is_food(item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::STRIDER_FOOD)
    }
}

impl DefaultEntityPush for StriderEntity {}

impl Entity for StriderEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let jockey = self.pending_jockey.lock().take();
        if let Some(jockey) = jockey
            && !jockey.is_removed()
            && let Some(world) = self.level()
            && let Some(vehicle) = world.get_entity_by_id(self.id())
        {
            let _ = jockey.start_riding(&vehicle);
        }
        self.tick_heat_and_sounds();
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        self.float_strider();
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dismount_location_for_passenger(&self, passenger: &dyn Entity) -> DVec3 {
        self.strider_dismount_location(passenger)
            .unwrap_or_else(|| self.default_dismount_location_for_passenger(passenger))
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            STRIDER_BABY_DIMENSIONS.scale(scale)
        } else {
            self.entity_type.dimensions.scale(scale)
        }
    }

    fn controlling_passenger(&self) -> Option<SharedEntity> {
        if self.is_saddled()
            && let Some(passenger) = self.first_passenger()
            && passenger.as_player().is_some_and(|player| {
                player.is_holding(&mut |item_stack: &ItemStack| {
                    item_stack.is(&vanilla_items::WARPED_FUNGUS_ON_A_STICK)
                })
            })
        {
            return Some(passenger);
        }
        self.controlling_passenger_mob()
    }

    fn can_add_passenger(&self, _passenger: &dyn Entity) -> bool {
        !self.is_vehicle() && !self.is_eye_in_lava()
    }

    fn is_on_fire(&self) -> bool {
        false
    }

    fn next_step(&self) -> f32 {
        self.base().movement_progress().move_dist() + 0.6
    }

    fn check_fall_damage(
        self: Arc<Self>,
        vertical_movement: f64,
        on_ground: bool,
        on_state: BlockStateId,
        pos: BlockPos,
        world: &Arc<World>,
    ) {
        if self.is_in_lava() {
            self.reset_fall_distance();
        } else {
            self.default_check_fall_damage(vertical_movement, on_ground, on_state, pos, world);
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        let sound = if self.is_in_lava() {
            &sound_events::ENTITY_STRIDER_STEP_LAVA
        } else {
            &sound_events::ENTITY_STRIDER_STEP
        };
        self.play_sound(sound, 1.0, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
    }
}

impl LivingEntity for StriderEntity {
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
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_STRIDER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_STRIDER_DEATH)
    }

    fn can_stand_on_fluid(&self, fluid_state: FluidState) -> bool {
        fluid_state.fluid_id.has_tag(&FluidTag::LAVA)
    }

    fn liquid_collision_shape(&self) -> VoxelShape {
        LIQUID_COLLISION_SHAPE
    }

    fn can_use_slot(&self, slot: EquipmentSlot) -> bool {
        slot != EquipmentSlot::Saddle || (Entity::is_alive(self) && !AgeableMob::is_baby(self))
    }

    fn can_dispenser_equip_into_slot(&self, slot: EquipmentSlot) -> bool {
        slot == EquipmentSlot::Saddle || Mob::can_pick_up_loot(self)
    }

    fn equip_sound(&self, slot: EquipmentSlot, _stack: &ItemStack) -> Option<SoundEventRef> {
        (slot == EquipmentSlot::Saddle).then_some(&sound_events::ENTITY_STRIDER_SADDLE)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn tick_ridden(&self, controller: &Player, _ridden_input: DVec3) {
        let (yaw, pitch) = controller.rotation();
        self.set_ridden_rotation(yaw, pitch);
        ItemSteerable::tick_boost(self);
    }

    fn ridden_input(&self, _controller: &Player, _self_input: DVec3) -> DVec3 {
        DVec3::new(0.0, 0.0, 1.0)
    }

    fn ridden_speed(&self, _controller: &Player) -> f32 {
        let movement_speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED) as f32;
        let steering = if self.is_suffocating() {
            SUFFOCATE_STEERING_MODIFIER
        } else {
            STEERING_MODIFIER
        };
        movement_speed * steering * ItemSteerable::boost_factor(self)
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for StriderEntity {
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
}

impl Animal for StriderEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        StriderEntity::is_food(item_stack)
    }
}

impl ItemSteerable for StriderEntity {
    fn item_based_steering(&self) -> &SyncMutex<ItemBasedSteering> {
        &self.steering
    }

    fn boost_time_total(&self) -> i32 {
        *self.entity_data.lock().boost_time.get()
    }

    fn set_boost_time_total(&self, boost_time_total: i32) {
        self.entity_data.lock().boost_time.set(boost_time_total);
    }
}

impl Mob for StriderEntity {
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
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        (!self.is_panicking() && !self.is_being_tempted())
            .then_some(&sound_events::ENTITY_STRIDER_AMBIENT)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if AgeableMob::is_baby(self) {
            return self.finalize_spawn_ageable_mob(world, spawn_reason, group_data);
        }
        let mut group_data = group_data;
        if rand::random_range(0..ZOMBIFIED_PIGLIN_JOCKEY_CHANCE) == 0
            && world.is_spawning_monsters()
        {
            if let Ok(jockey) =
                create_entity_instance(world, &vanilla_entities::ZOMBIFIED_PIGLIN, self.position())
            {
                let jockey_group_data = SpawnGroupData::Zombie(ZombieGroupData {
                    is_baby: rand::random::<f32>() < 0.05,
                    can_spawn_jockey: false,
                });
                group_data = Some(self.spawn_jockey(world, &jockey, Some(jockey_group_data)));
                if let Some(living) = jockey.as_living_entity() {
                    living.with_equipment_slot_mut(EquipmentSlot::MainHand, &mut |item| {
                        *item = ItemStack::new(&vanilla_items::WARPED_FUNGUS_ON_A_STICK);
                    });
                }
                self.with_equipment_slot_mut(EquipmentSlot::Saddle, &mut |item| {
                    *item = ItemStack::new(&vanilla_items::SADDLE);
                });
                self.set_guaranteed_drop(EquipmentSlot::Saddle);
            }
        } else if rand::random_range(0..BABY_STRIDER_JOCKEY_CHANCE) == 0 {
            if let Ok(jockey) =
                create_entity_instance(world, &vanilla_entities::STRIDER, self.position())
            {
                if let Some(ageable) = jockey.as_ageable_mob() {
                    ageable.set_age(BABY_START_AGE);
                }
                group_data = Some(self.spawn_jockey(world, &jockey, None));
            }
        } else {
            group_data = Some(SpawnGroupData::AgeableMob(
                AgeableMobGroupData::with_baby_spawn_chance(0.5),
            ));
        }
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        let has_food = StriderEntity::is_food(&item_stack);

        if !has_food && self.is_saddled() && !self.is_vehicle() && !player.is_secondary_use_active()
        {
            if let Some(world) = self.level()
                && let Some(vehicle) = world.get_entity_by_id(self.id())
            {
                player.start_riding(&vehicle);
            }
            return InteractionResult::Success;
        }

        let interaction_result = Animal::mob_interact_animal(self, player, hand);
        if !interaction_result.consumes_action() {
            if LivingEntity::is_equippable_in_slot(self, &item_stack, EquipmentSlot::Saddle) {
                return LivingEntity::interact_living_entity_with_equippable(self, player, hand);
            }
            return InteractionResult::Pass;
        }
        if has_food && !self.is_silent() {
            let pitch = 1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2;
            if let Some(world) = self.level() {
                world.play_sound_at(
                    &sound_events::ENTITY_STRIDER_EAT,
                    self.sound_source(),
                    self.position(),
                    1.0,
                    pitch,
                    None,
                );
            }
        }
        interaction_result
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        // Vanilla `level.isUnobstructed(this)`: only entities; lava is fine.
        level.is_unobstructed_by_entities(self.as_entity_event_source())
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for StriderEntity {
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        if level
            .get_block_state(pos)
            .get_fluid_state()
            .fluid_id
            .has_tag(&FluidTag::LAVA)
        {
            10.0
        } else if self.is_in_lava() {
            f32::NEG_INFINITY
        } else {
            0.0
        }
    }

    /// Vanilla `StriderPathNavigation.isStableDestination`: lava counts.
    fn is_stable_destination(&self, pos: BlockPos) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        world.get_block_state(pos).get_block() == &vanilla_blocks::LAVA
            || world.get_block_state(pos.below()).is_solid_render()
    }
}

/// Vanilla `StriderGoToLavaGoal.isValidTarget`: lava with room to stand above.
pub(super) fn is_lava_target(world: &World, pos: BlockPos) -> bool {
    world.get_block_state(pos).get_block() == &vanilla_blocks::LAVA
        && world
            .get_block_state(pos.above())
            .is_pathfindable(PathComputationType::Land)
}
