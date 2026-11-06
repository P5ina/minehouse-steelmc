//! Snow golem (Vanilla `SnowGolem`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::SnowGolemEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_blocks, vanilla_damage_types, vanilla_entities,
    vanilla_game_events, vanilla_items, vanilla_loot_tables,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::{InteractionHand, UpdateFlags};
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::behavior::{BLOCK_BEHAVIORS, InteractionResult};
use crate::enchantment_helper;
use crate::entity::ai::goal::{
    LookAtPlayerGoal, NearestAttackableTargetGoal, RandomLookAroundGoal, RangedAttackGoal,
    WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::SnowballEntity;
use crate::entity::living_entity::shearing_loot_items_with_rng;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob, Projectile,
    RangedAttackMob, SharedEntity, ThrowableItemProjectile, next_entity_id, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;
use crate::world::game_event::GameEventContext;

const PUMPKIN_FLAG: i8 = 16;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `AbstractGolem.getAmbientSoundInterval`.
const GOLEM_AMBIENT_SOUND_INTERVAL: i32 = 120;
const SNOWBALL_SHOOT_POWER: f32 = 1.6;
const SNOWBALL_UNCERTAINTY: f32 = 12.0;
/// Vanilla `WaterAvoidingRandomStrollGoal(this, 1.0, 1.0000001E-5F)`.
const STROLL_PROBABILITY: f32 = 1.000_000_1E-5;

/// A snow golem: throws snowballs at monsters and leaves a trail of snow.
#[entity_behavior(class = "SnowGolem")]
pub struct SnowGolemEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<SnowGolemEntityData>,
}

// SAFETY: The owner-scoped type key uniquely identifies SnowGolemEntity.
unsafe impl DowncastType for SnowGolemEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/snow_golem");
}

impl SnowGolemEntity {
    /// Creates a new snow golem.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved snow golem.
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
        let mut entity_data = SnowGolemEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, RangedAttackGoal::new(1.25, 20, 10.0));
            goal_selector.add_goal(
                2,
                WaterAvoidingRandomStrollGoal::with_probability(1.0, STROLL_PROBABILITY),
            );
            goal_selector.add_goal(3, LookAtPlayerGoal::new(6.0));
            goal_selector.add_goal(4, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
                    target.as_mob().is_some() && target.is_enemy()
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
        }
    }

    /// Whether the golem still wears its carved pumpkin.
    #[must_use]
    pub fn has_pumpkin(&self) -> bool {
        *self.entity_data.lock().pumpkin.get() & PUMPKIN_FLAG != 0
    }

    /// Puts the carved pumpkin on or takes it off.
    pub fn set_pumpkin(&self, pumpkin: bool) {
        let mut entity_data = self.entity_data.lock();
        let current = *entity_data.pumpkin.get();
        entity_data.pumpkin.set(if pumpkin {
            current | PUMPKIN_FLAG
        } else {
            current & !PUMPKIN_FLAG
        });
    }

    /// Vanilla `Shearable.readyForShearing`.
    #[must_use]
    pub fn ready_for_shearing(&self) -> bool {
        self.has_pumpkin()
    }

    /// Vanilla `SnowGolem.shear`: drops the pumpkin from the golem's head.
    pub fn shear(&self, world: &World, sound_source: SoundSource, tool: &ItemStack) {
        world.play_sound_at(
            &sound_events::ENTITY_SNOW_GOLEM_SHEAR,
            sound_source,
            self.position(),
            1.0,
            1.0,
            None,
        );
        self.set_pumpkin(false);
        let eye_height = self.get_eye_height();
        let mut rng = rand::rng();
        for drop in shearing_loot_items_with_rng(
            self,
            &vanilla_loot_tables::SHEARING_SNOW_GOLEM,
            tool,
            &mut rng,
        ) {
            let _ = self.spawn_at_location(drop, eye_height);
        }
    }

    /// Melts in hot biomes and leaves snow layers where it stands.
    fn snow_golem_ai_step(&self) {
        let Some(world) = self.level() else {
            return;
        };
        if world.snow_golem_melts(self.position()) {
            self.hurt_server(
                &world,
                &DamageSource::environment(&vanilla_damage_types::ON_FIRE),
                1.0,
            );
        }
        if !world.get_game_rule(&MOB_GRIEFING) {
            return;
        }

        let snow = vanilla_blocks::SNOW.default_state();
        let snow_behavior = BLOCK_BEHAVIORS.get_behavior(&vanilla_blocks::SNOW);
        let position = self.position();
        for i in 0..4 {
            let x = (position.x + f64::from((i % 2 * 2 - 1) as f32 * 0.25)).floor() as i32;
            let y = position.y.floor() as i32;
            let z = (position.z + f64::from((i / 2 % 2 * 2 - 1) as f32 * 0.25)).floor() as i32;
            let snow_pos = BlockPos::new(x, y, z);
            if world.get_block_state(snow_pos).is_air()
                && snow_behavior.can_survive(snow, world.as_ref(), snow_pos)
            {
                world.set_block(snow_pos, snow, UpdateFlags::UPDATE_ALL);
                world.game_event(
                    &vanilla_game_events::BLOCK_PLACE,
                    snow_pos,
                    &GameEventContext::new(Some(self), Some(snow)),
                );
            }
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

impl RangedAttackMob for SnowGolemEntity {
    fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, _power: f32) {
        let position = self.position();
        let target_position = target.position();
        let xd = target_position.x - position.x;
        let yd = target.get_eye_y() - f64::from(1.1_f32);
        let zd = target_position.z - position.z;
        let yo = (xd * xd + zd * zd).sqrt() * f64::from(0.2_f32);

        if let Some(world) = self.level() {
            let mut item = ItemStack::new(&vanilla_items::SNOWBALL);
            let snowball = SnowballEntity::new(
                &vanilla_entities::SNOWBALL,
                next_entity_id(),
                DVec3::new(position.x, self.get_eye_y() - 0.1, position.z),
                Arc::downgrade(&world),
            );
            snowball.set_owner_entity(Some(entity));
            snowball.set_item_clamped(item.clone());
            snowball.shoot(
                DVec3::new(xd, yd + yo - snowball.position().y, zd),
                SNOWBALL_SHOOT_POWER,
                self.ranged_attack_uncertainty(&world),
            );
            let snowball: SharedEntity = Arc::new(snowball);
            match world.try_add_entity(Arc::clone(&snowball)) {
                Ok(()) => enchantment_helper::on_projectile_spawned(
                    &world,
                    &mut item,
                    snowball.as_ref(),
                    Some(entity),
                ),
                Err(error) => log::debug!("world rejected a snow golem snowball: {error}"),
            }
        }

        let pitch = 0.4 / (rand::random::<f32>() * 0.4 + 0.8);
        self.play_sound(&sound_events::ENTITY_SNOW_GOLEM_SHOOT, 1.0, pitch);
    }

    fn ranged_attack_uncertainty(&self, _world: &World) -> f32 {
        SNOWBALL_UNCERTAINTY
    }
}

impl DefaultEntityPush for SnowGolemEntity {}

impl Entity for SnowGolemEntity {
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

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("Pumpkin", i8::from(self.has_pumpkin()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.set_pumpkin(nbt.byte("Pumpkin").is_none_or(|b| b != 0));
    }
}

impl LivingEntity for SnowGolemEntity {
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
        Some(&sound_events::ENTITY_SNOW_GOLEM_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SNOW_GOLEM_DEATH)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        self.snow_golem_ai_step();
        result
    }
}

impl Mob for SnowGolemEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SNOW_GOLEM_AMBIENT)
    }

    fn ambient_sound_interval(&self) -> i32 {
        GOLEM_AMBIENT_SOUND_INTERVAL
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if !item_stack.is(&vanilla_items::SHEARS) || !self.ready_for_shearing() {
            return InteractionResult::Pass;
        }
        if let Some(world) = self.level() {
            self.shear(world.as_ref(), SoundSource::Players, &item_stack);
            self.game_event_with_source_entity(&vanilla_game_events::SHEAR, Some(player));
            player
                .inventory
                .lock()
                .hurt_item_in_hand(hand, 1, player.has_infinite_materials());
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

impl PathfinderMob for SnowGolemEntity {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};

    use super::SnowGolemEntity;
    use crate::entity::Entity;

    #[test]
    fn pumpkin_flag_round_trips_and_defaults_on() {
        init_vanilla_registry();
        let golem =
            SnowGolemEntity::new(&vanilla_entities::SNOW_GOLEM, 1, DVec3::ZERO, Weak::new());
        assert!(golem.has_pumpkin());
        golem.set_pumpkin(false);
        let mut nbt = NbtCompound::new();
        golem.save_additional(&mut nbt);
        assert_eq!(nbt.byte("Pumpkin"), Some(0));

        let mut bytes = Vec::new();
        NbtCompound::new().write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded =
            SnowGolemEntity::new(&vanilla_entities::SNOW_GOLEM, 2, DVec3::ZERO, Weak::new());
        loaded.set_pumpkin(false);
        loaded.load_additional((&borrowed).into());
        assert!(
            loaded.has_pumpkin(),
            "a missing Pumpkin tag means the golem has one"
        );
    }
}
