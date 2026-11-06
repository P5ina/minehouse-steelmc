//! Vanilla `Nautilus`.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::NautilusEntityData;
use steel_registry::{sound_events, vanilla_damage_types, vanilla_mob_effects};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};
use uuid::Uuid;

use super::ai::{self, NautilusAiKind};
use super::{
    AbstractNautilus, AbstractNautilusBase, NAUTILUS_LOOK_MAX_Y_ROT_FROM_CENTER,
    NAUTILUS_MOVE_CONTROL, check_nautilus_spawn_rules,
};
use crate::behavior::InteractionResult;
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::smooth_swimming::tick_smooth_swimming_look_control;
use crate::entity::damage::DamageSource;
use crate::entity::entities::mobs::passive::equine::PlayerRideableJumping;
use crate::entity::fish::{init_water_animal, water_animal_spawn_unobstructed};
use crate::entity::leash::Leashable as _;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, Entity, EntityBase, EntityBaseLoad,
    EntityEventSource as _, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, MobEffectInstance, PathfinderMob, SharedEntity, SpawnGroupData,
    SpawnRule, TamableAnimal, TamableAnimalBase, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, ServerLevelAccessor, World};

/// Vanilla `Nautilus.NAUTILUS_TOTAL_AIR_SUPPLY`.
const NAUTILUS_TOTAL_AIR_SUPPLY: i32 = 300;
/// Vanilla `Nautilus.BABY_DIMENSIONS` scale.
const BABY_SCALE: f32 = 0.5;
const BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(0.0, 0.5, 0.0)];
/// Vanilla `handleAirSupply`: dries out after this much air below zero.
const DRY_OUT_AIR: i32 = -20;
const DRY_OUT_DAMAGE: f32 = 2.0;

const NAUTILUS_AI: NautilusAiKind = NautilusAiKind {
    living: true,
    tempted_speed: 1.3,
    attack_speed: 0.6,
    dash_sound: &sound_events::ENTITY_NAUTILUS_DASH,
};

/// Vanilla nautilus: a tamable, rideable shell that lets its rider breathe.
#[entity_behavior(class = "Nautilus", spawn_rule)]
pub struct NautilusEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    tamable_base: TamableAnimalBase,
    nautilus_base: AbstractNautilusBase,
    entity_data: SyncMutex<NautilusEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `NautilusEntity`.
unsafe impl DowncastType for NautilusEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/nautilus");
}

impl NautilusEntity {
    /// Vanilla `SpawnPlacements` predicate: `AbstractNautilus.checkNautilusSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_nautilus_spawn_rules;

    /// Creates a new nautilus at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a nautilus from persisted base entity state.
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
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        init_water_animal(&mob_base);
        *mob_base.navigation().lock() = PathNavigation::water_bound(false);
        let mut entity_data = NautilusEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            animal_base: AnimalBase::new(),
            tamable_base: TamableAnimalBase::new(),
            nautilus_base: AbstractNautilusBase::new(),
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(
                &ai::memory_types(),
                ai::sensors(),
                ai::activities(&NAUTILUS_AI),
                None,
            ),
        }
    }

    /// Vanilla `Nautilus.handleAirSupply`: out of water it dries out.
    fn handle_air_supply(&self, world: &World, pre_tick_air_supply: i32) {
        if Entity::is_alive(self) && !self.is_in_water() {
            self.set_air_supply(pre_tick_air_supply - 1);
            if self.air_supply() <= DRY_OUT_AIR {
                self.set_air_supply(0);
                self.hurt(
                    world,
                    &DamageSource::environment(&vanilla_damage_types::DRY_OUT),
                    DRY_OUT_DAMAGE,
                );
            }
        } else {
            self.set_air_supply(NAUTILUS_TOTAL_AIR_SUPPLY);
        }
    }

    fn sound_for(
        &self,
        baby_water: SoundEventRef,
        baby_land: SoundEventRef,
        water: SoundEventRef,
        land: SoundEventRef,
    ) -> SoundEventRef {
        match (AgeableMob::is_baby(self), self.is_under_water()) {
            (true, true) => baby_water,
            (true, false) => baby_land,
            (false, true) => water,
            (false, false) => land,
        }
    }
}

impl AbstractNautilus for NautilusEntity {
    fn nautilus_base(&self) -> &AbstractNautilusBase {
        &self.nautilus_base
    }

    fn is_dashing(&self) -> bool {
        *self.entity_data.lock().abstract_nautilus.dash.get()
    }

    fn set_dashing(&self, dashing: bool) {
        self.entity_data.lock().abstract_nautilus.dash.set(dashing);
    }

    fn dash_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_under_water() {
            &sound_events::ENTITY_NAUTILUS_DASH
        } else {
            &sound_events::ENTITY_NAUTILUS_DASH_LAND
        })
    }

    fn dash_ready_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_under_water() {
            &sound_events::ENTITY_NAUTILUS_DASH_READY
        } else {
            &sound_events::ENTITY_NAUTILUS_DASH_READY_LAND
        })
    }
}

impl PlayerRideableJumping for NautilusEntity {
    fn can_jump(&self) -> bool {
        self.is_saddled()
    }

    fn handle_start_jump(&self, _jump_scale: i32) {
        self.nautilus_handle_start_jump();
    }

    fn handle_stop_jump(&self) {}
}

impl TamableAnimal for NautilusEntity {
    fn tamable_base(&self) -> &TamableAnimalBase {
        &self.tamable_base
    }

    fn tamable_flags(&self) -> i8 {
        *self.entity_data.lock().tamable_animal().flags.get()
    }

    fn set_tamable_flags(&self, flags: i8) {
        self.entity_data
            .lock()
            .tamable_animal_mut()
            .flags
            .set(flags);
    }

    fn owner_uuid(&self) -> Option<Uuid> {
        *self.entity_data.lock().tamable_animal().owneruuid.get()
    }

    fn set_owner_uuid(&self, owner: Option<Uuid>) {
        self.entity_data
            .lock()
            .tamable_animal_mut()
            .owneruuid
            .set(owner);
    }
}

impl Entity for NautilusEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
        self.tick_nautilus();
    }

    fn base_tick(&self) {
        let air_supply = self.air_supply();
        Mob::base_tick_mob(self);
        if !self.is_no_ai()
            && let Some(world) = self.level()
        {
            self.handle_air_supply(&world, air_supply);
        }
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let dimensions = if AgeableMob::is_baby(self) {
            let scaled = self.entity_type.dimensions.scale(BABY_SCALE);
            EntityDimensions::new_with_attachments(
                scaled.width,
                scaled.height,
                scaled.eye_height,
                EntityAttachments::new(&BABY_PASSENGER, &[], &[], &[]),
            )
        } else {
            self.entity_type.dimensions
        };
        dimensions.scale(LivingEntity::get_scale(self))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn is_pushed_by_fluid(&self) -> bool {
        false
    }

    fn max_air_supply(&self) -> i32 {
        NAUTILUS_TOTAL_AIR_SUPPLY
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {}

    fn swim_sound(&self) -> SoundEventRef {
        if AgeableMob::is_baby(self) {
            &sound_events::ENTITY_BABY_NAUTILUS_SWIM
        } else {
            &sound_events::ENTITY_NAUTILUS_SWIM
        }
    }

    fn controlling_passenger(&self) -> Option<SharedEntity> {
        self.nautilus_controlling_passenger()
    }

    fn considers_entity_as_ally(&self, other: &dyn Entity) -> bool {
        self.considers_entity_as_ally_tamable(other)
    }

    fn is_tame_owned_by(&self, owner: &dyn LivingEntity) -> bool {
        self.is_tame() && self.is_owned_by(owner.as_entity_event_source())
    }

    /// Vanilla `AbstractNautilus.interact`: any interaction keeps it around.
    fn interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        location: DVec3,
    ) -> InteractionResult {
        self.set_persistence_required();
        self.interact_mob(player, hand, location)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        self.save_tamable(nbt);
        nbt.insert("Brain", self.brain.save());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.load_tamable(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
    }
}

impl LivingEntity for NautilusEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
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
        Some(self.sound_for(
            &sound_events::ENTITY_BABY_NAUTILUS_HURT,
            &sound_events::ENTITY_BABY_NAUTILUS_HURT_LAND,
            &sound_events::ENTITY_NAUTILUS_HURT,
            &sound_events::ENTITY_NAUTILUS_HURT_LAND,
        ))
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(self.sound_for(
            &sound_events::ENTITY_BABY_NAUTILUS_DEATH,
            &sound_events::ENTITY_BABY_NAUTILUS_DEATH_LAND,
            &sound_events::ENTITY_NAUTILUS_DEATH,
            &sound_events::ENTITY_NAUTILUS_DEATH_LAND,
        ))
    }

    /// Vanilla `AbstractNautilus.hurtServer`: whoever hurt it angers it.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let was_hurt = self.default_hurt_server(world, source, amount);
        if was_hurt
            && let Some(attacker) = source.causing_entity()
            && let Some(attacker) = attacker.as_living_entity()
        {
            ai::set_anger_target(world, self, attacker);
        }
        was_hurt
    }

    fn can_be_affected(&self, effect: &MobEffectInstance) -> bool {
        effect.effect() != vanilla_mob_effects::POISON && self.default_can_be_affected(effect)
    }

    fn can_use_slot(&self, slot: EquipmentSlot) -> bool {
        self.can_use_nautilus_slot(slot)
    }

    fn can_dispenser_equip_into_slot(&self, slot: EquipmentSlot) -> bool {
        slot == EquipmentSlot::Body || slot == EquipmentSlot::Saddle || Mob::can_pick_up_loot(self)
    }

    fn equip_sound(&self, slot: EquipmentSlot, stack: &ItemStack) -> Option<SoundEventRef> {
        self.nautilus_equip_sound(slot, stack)
    }

    fn tick_ridden(&self, controller: &Player, _ridden_input: DVec3) {
        self.nautilus_tick_ridden(controller);
    }

    fn ridden_input(&self, controller: &Player, _self_input: DVec3) -> DVec3 {
        self.nautilus_ridden_input(controller)
    }

    fn ridden_speed(&self, _controller: &Player) -> f32 {
        self.nautilus_ridden_speed()
    }

    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.nautilus_travel_in_water(entity, input)
    }

    fn die(&self, source: &DamageSource) {
        self.announce_death_to_owner(source);
        self.default_die(source);
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

impl AgeableMob for NautilusEntity {
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

    /// Vanilla `Nautilus.getBreedOffspring`: a tame parent's baby is tame too.
    fn get_breed_offspring(
        &self,
        world: &Arc<World>,
        _partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        let offspring = self.create_breed_offspring(world)?;
        if self.is_tame()
            && let Some(baby) = offspring.as_tamable_animal()
        {
            baby.set_owner_uuid(self.owner_uuid());
            baby.set_tame(true, true);
        }
        Some(offspring)
    }
}

impl Animal for NautilusEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        self.is_nautilus_food(item_stack)
    }

    fn play_eating_sound(&self) {
        self.make_sound(Some(if AgeableMob::is_baby(self) {
            &sound_events::ENTITY_BABY_NAUTILUS_EAT
        } else {
            &sound_events::ENTITY_NAUTILUS_EAT
        }));
    }
}

impl Mob for NautilusEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        NAUTILUS_MOVE_CONTROL.tick(self);
    }

    fn tick_look_control(&self) {
        tick_smooth_swimming_look_control(self, NAUTILUS_LOOK_MAX_Y_ROT_FROM_CENTER);
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            self.brain
                .set_active_activity_to_first_valid(&ai::UPDATE_ACTIVITIES);
        }
        self.check_restriction();
        Animal::custom_server_ai_step_animal(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(self.sound_for(
            &sound_events::ENTITY_BABY_NAUTILUS_AMBIENT,
            &sound_events::ENTITY_BABY_NAUTILUS_AMBIENT_LAND,
            &sound_events::ENTITY_NAUTILUS_AMBIENT,
            &sound_events::ENTITY_NAUTILUS_AMBIENT_LAND,
        ))
    }

    fn use_player_item(&self, player: &Player, hand: InteractionHand) {
        self.use_nautilus_player_item(player, hand);
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        true
    }

    fn requires_custom_persistence(&self) -> bool {
        self.is_passenger() || self.is_leashed() || self.is_tame()
    }

    fn mob_can_be_leashed(&self) -> bool {
        !self.is_aggravated()
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        water_animal_spawn_unobstructed(self, level)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        ai::init_memories(self);
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        self.nautilus_mob_interact(player, hand)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for NautilusEntity {
    fn get_walk_target_value_in(&self, _pos: BlockPos, _level: &dyn LevelReader) -> f32 {
        0.0
    }
}
