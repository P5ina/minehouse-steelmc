//! The evoker's three spells (Vanilla `Evoker.EvokerSummonSpellGoal`,
//! `Evoker.EvokerAttackSpellGoal` and `Evoker.EvokerWololoSpellGoal`).

use std::f32::consts::PI;
use std::f64::consts::PI as PI_F64;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::Direction;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{DyeColor, sound_events, vanilla_entities, vanilla_game_events};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, Downcast as _, axis::Axis};

use super::spellcaster_goals::Spell;
use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::{EvokerFangsEntity, SheepEntity, VexEntity};
use crate::entity::{
    ENTITIES, EntitySpawnReason, IllagerSpell, PathfinderMob, SharedEntity, SpellcasterIllager,
    next_entity_id,
};
use crate::world::LevelReader as _;
use crate::world::game_event::GameEventContext;

/// Vanilla `EvokerSummonSpellGoal.vexCountTargeting` range.
const VEX_COUNT_RANGE: f64 = 16.0;
const VEXES_PER_CAST: usize = 3;
/// Vanilla `random.nextInt(8) + 1 > vexes`.
const MAX_VEX_ROLL: i32 = 8;
/// Vanilla `setLimitedLife(20 * (30 + random.nextInt(90)))`.
const VEX_MIN_LIFE_SECONDS: i32 = 30;
const VEX_LIFE_SPREAD_SECONDS: i32 = 90;
/// Vanilla `distanceToSqr(target) < 9.0`: fangs circle the evoker instead of a line.
const CLOSE_FANGS_DISTANCE_SQR: f64 = 9.0;
const WOLOLO_RANGE: f64 = 16.0;
const WOLOLO_VERTICAL_RANGE: f64 = 4.0;

/// The sheep the evoker is turning red, shared between the evoker and its goals.
#[derive(Clone, Default)]
pub(crate) struct WololoTarget(Arc<SyncMutex<Option<SharedEntity>>>);

impl WololoTarget {
    /// Vanilla `getWololoTarget`.
    #[must_use]
    pub(crate) fn get(&self) -> Option<SharedEntity> {
        self.0.lock().clone()
    }

    fn set(&self, target: Option<SharedEntity>) {
        *self.0.lock() = target;
    }
}

/// Vanilla `Evoker.EvokerSummonSpellGoal`.
pub(crate) struct SummonVexSpell {
    vex_count_targeting: TargetingConditions,
}

impl SummonVexSpell {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            vex_count_targeting: TargetingConditions::for_non_combat()
                .range(VEX_COUNT_RANGE)
                .ignore_line_of_sight()
                .ignore_invisibility_testing(),
        }
    }

    fn nearby_vex_count(&self, mob: &dyn PathfinderMob) -> usize {
        let Some(world) = mob.level() else {
            return 0;
        };
        let search_box =
            mob.bounding_box()
                .inflate_xyz(VEX_COUNT_RANGE, VEX_COUNT_RANGE, VEX_COUNT_RANGE);
        world
            .get_entities_in_aabb_matching(&search_box, |entity| {
                entity.downcast_ref::<VexEntity>().is_some()
                    && entity.as_living_entity().is_some_and(|vex| {
                        self.vex_count_targeting
                            .test(world.as_ref(), Some(mob), vex)
                    })
            })
            .len()
    }
}

impl Spell for SummonVexSpell {
    fn spell(&self) -> IllagerSpell {
        IllagerSpell::SummonVex
    }

    fn casting_time(&self) -> i32 {
        100
    }

    fn casting_interval(&self) -> i32 {
        340
    }

    fn prepare_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_EVOKER_PREPARE_SUMMON)
    }

    fn can_use(
        &mut self,
        mob: &dyn PathfinderMob,
        default_can_use: bool,
        _next_attack_tick_count: i32,
    ) -> bool {
        if !default_can_use {
            return false;
        }
        let vexes = self.nearby_vex_count(mob);
        usize::try_from(rand::random_range(0..MAX_VEX_ROLL) + 1).is_ok_and(|roll| roll > vexes)
    }

    fn perform(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(world) = mob.level() else {
            return;
        };
        // TODO: Vanilla also puts each vex on the evoker's scoreboard team; Steel
        // has no teams yet.
        for _ in 0..VEXES_PER_CAST {
            let pos = mob.block_position().offset(
                -2 + rand::random_range(0..5),
                1,
                -2 + rand::random_range(0..5),
            );
            let spawn_position = DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()),
                f64::from(pos.z()) + 0.5,
            );
            let Some(vex_entity) = ENTITIES.create(
                &vanilla_entities::VEX,
                next_entity_id(),
                spawn_position,
                Arc::downgrade(&world),
            ) else {
                continue;
            };
            vex_entity.snap_to(spawn_position, 0.0, 0.0);
            if let Some(vex_mob) = vex_entity.as_mob() {
                let _ = vex_mob.finalize_spawn(&world, EntitySpawnReason::MobSummoned, None);
            }
            if let Some(vex) = vex_entity.downcast_ref::<VexEntity>() {
                vex.set_owner(entity);
                vex.set_bound_origin(Some(pos));
                vex.set_limited_life(
                    20 * (VEX_MIN_LIFE_SECONDS + rand::random_range(0..VEX_LIFE_SPREAD_SECONDS)),
                );
            }
            if let Err(error) = world.try_add_entity(vex_entity) {
                log::debug!("world rejected a summoned vex: {error}");
                continue;
            }
            world.game_event(
                &vanilla_game_events::ENTITY_PLACE,
                pos,
                &GameEventContext::new(Some(entity.as_entity_event_source()), None),
            );
        }
    }
}

/// Vanilla `Evoker.EvokerAttackSpellGoal`: rows or rings of fangs.
pub(crate) struct FangsSpell;

impl FangsSpell {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self
    }
}

/// Vanilla `EvokerAttackSpellGoal.createSpellEntity`: finds the floor under
/// `(x, z)` between `max_y` and `min_y - 1` and raises fangs on it.
fn create_spell_entity(
    entity: &SharedEntity,
    x: f64,
    z: f64,
    min_y: f64,
    max_y: f64,
    angle: f32,
    delay_ticks: i32,
) {
    let Some(world) = entity.level() else {
        return;
    };
    let mut pos = BlockPos::containing(x, max_y, z);
    let mut top_offset = 0.0;
    let mut success = false;
    loop {
        let below = pos.below();
        let below_state = world.get_block_state(below);
        if world.is_face_sturdy(below_state, below, Direction::Up) {
            if !world.get_block_state(pos).is_air() {
                let state = world.get_block_state(pos);
                let shape = BLOCK_BEHAVIORS
                    .get_behavior(state.get_block())
                    .get_collision_shape(
                        state,
                        world.as_ref(),
                        pos,
                        BlockCollisionContext::empty(),
                    );
                if !shape.is_empty() {
                    top_offset = shape.max(Axis::Y);
                }
            }
            success = true;
            break;
        }
        pos = pos.below();
        if f64::from(pos.y()) < min_y.floor() - 1.0 {
            break;
        }
    }

    if !success {
        return;
    }
    let position = DVec3::new(x, f64::from(pos.y()) + top_offset, z);
    let fangs = EvokerFangsEntity::new(
        &vanilla_entities::EVOKER_FANGS,
        next_entity_id(),
        position,
        Arc::downgrade(&world),
    );
    fangs.configure(angle, delay_ticks, entity);
    if let Err(error) = world.try_add_entity(Arc::new(fangs)) {
        log::debug!("world rejected evoker fangs: {error}");
        return;
    }
    world.game_event_at(
        &vanilla_game_events::ENTITY_PLACE,
        position,
        &GameEventContext::new(Some(entity.as_entity_event_source()), None),
    );
}

impl Spell for FangsSpell {
    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Fangs
    }

    fn casting_time(&self) -> i32 {
        40
    }

    fn casting_interval(&self) -> i32 {
        100
    }

    fn prepare_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_EVOKER_PREPARE_ATTACK)
    }

    fn perform(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        let position = mob.position();
        let target_position = target.position();
        let min_y = target_position.y.min(position.y);
        let max_y = target_position.y.max(position.y) + 1.0;
        let angle_towards_target =
            (target_position.z - position.z).atan2(target_position.x - position.x) as f32;
        if position.distance_squared(target_position) < CLOSE_FANGS_DISTANCE_SQR {
            for i in 0..5 {
                let angle = angle_towards_target + i as f32 * PI * 0.4;
                create_spell_entity(
                    entity,
                    position.x + f64::from(angle.cos()) * 1.5,
                    position.z + f64::from(angle.sin()) * 1.5,
                    min_y,
                    max_y,
                    angle,
                    0,
                );
            }
            for i in 0..8 {
                let angle =
                    angle_towards_target + i as f32 * PI * 2.0 / 8.0 + (PI_F64 * 2.0 / 5.0) as f32;
                create_spell_entity(
                    entity,
                    position.x + f64::from(angle.cos()) * 2.5,
                    position.z + f64::from(angle.sin()) * 2.5,
                    min_y,
                    max_y,
                    angle,
                    3,
                );
            }
        } else {
            for i in 0..16 {
                let reach = 1.25 * f64::from(i + 1);
                create_spell_entity(
                    entity,
                    position.x + f64::from(angle_towards_target.cos()) * reach,
                    position.z + f64::from(angle_towards_target.sin()) * reach,
                    min_y,
                    max_y,
                    angle_towards_target,
                    i,
                );
            }
        }
    }
}

/// Vanilla `Evoker.EvokerWololoSpellGoal`: turns a nearby blue sheep red.
pub(crate) struct WololoSpell {
    target: WololoTarget,
    wololo_targeting: TargetingConditions,
}

impl WololoSpell {
    #[must_use]
    pub(crate) fn new(target: WololoTarget) -> Self {
        Self {
            target,
            wololo_targeting: TargetingConditions::for_non_combat()
                .range(WOLOLO_RANGE)
                .selector(|target, _| {
                    target
                        .downcast_ref::<SheepEntity>()
                        .is_some_and(|sheep| sheep.color() == DyeColor::Blue)
                }),
        }
    }
}

impl Spell for WololoSpell {
    fn spell(&self) -> IllagerSpell {
        IllagerSpell::Wololo
    }

    fn casting_time(&self) -> i32 {
        60
    }

    fn casting_interval(&self) -> i32 {
        140
    }

    fn cast_warmup_time(&self) -> i32 {
        40
    }

    fn prepare_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_EVOKER_PREPARE_WOLOLO)
    }

    fn can_use(
        &mut self,
        mob: &dyn PathfinderMob,
        _default_can_use: bool,
        next_attack_tick_count: i32,
    ) -> bool {
        if mob.target().is_some()
            || mob
                .as_spellcaster_illager()
                .is_none_or(SpellcasterIllager::is_casting_spell)
            || mob.tick_count() < next_attack_tick_count
        {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        if !world.get_game_rule(&MOB_GRIEFING) {
            return false;
        }
        let search_box =
            mob.bounding_box()
                .inflate_xyz(WOLOLO_RANGE, WOLOLO_VERTICAL_RANGE, WOLOLO_RANGE);
        let sheep = world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.downcast_ref::<SheepEntity>().is_some()
                && entity.as_living_entity().is_some_and(|sheep| {
                    self.wololo_targeting.test(world.as_ref(), Some(mob), sheep)
                })
        });
        if sheep.is_empty() {
            return false;
        }
        let picked = rand::random_range(0..sheep.len());
        self.target.set(sheep.into_iter().nth(picked));
        true
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob, attack_warmup_delay: i32) -> bool {
        self.target.get().is_some() && attack_warmup_delay > 0
    }

    fn stop(&mut self, _mob: &dyn PathfinderMob) {
        self.target.set(None);
    }

    fn perform(&mut self, _mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(target) = self.target.get()
            && target.is_alive()
            && let Some(sheep) = target.downcast_ref::<SheepEntity>()
        {
            sheep.set_color(DyeColor::Red);
        }
    }
}
