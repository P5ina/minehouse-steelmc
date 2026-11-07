//! Working at the job site (vanilla `WorkAtPoi` and `WorkAtComposter`)
//! and between secondary job sites (vanilla `SecondaryPoiSensor` and
//! `StrollToPoiList`).

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_blocks, vanilla_items};
use steel_utils::GlobalPos;

use super::super::{VillagerEntity, secondary_poi};
use super::poi::closer_to_center_than;
use crate::entity::Entity as _;
use crate::entity::ai::brain::behavior::{Behavior, OneShot};
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{
    JOB_SITE, LAST_WORKED_AT_POI, LOOK_TARGET, PositionTracker, SECONDARY_JOB_SITE, WALK_TARGET,
    WalkTarget,
};
use crate::entity::ai::brain::{BehaviorControl, Memories, MemoryStatus, Sensor};
use crate::inventory::container::Container as _;
use crate::world::World;

/// Vanilla `WorkAtPoi.CHECK_COOLDOWN`.
const CHECK_COOLDOWN: i64 = 300;
/// Vanilla `WorkAtPoi.DISTANCE`.
const DISTANCE: f64 = 1.73;
/// Vanilla `WorkAtComposter.makeBread`: no more bread once it carries this much.
const MAX_BREAD: i32 = 36;
/// Vanilla `WorkAtComposter.makeBread`: loaves baked per session.
const MAX_BREAD_TO_MAKE: i32 = 3;
/// Vanilla `WorkAtComposter.makeBread`: wheat per loaf.
const WHEAT_PER_BREAD: i32 = 3;
/// Vanilla `SecondaryPoiSensor.SCAN_RATE`.
const SECONDARY_POI_SCAN_RATE: i32 = 40;
/// Vanilla `StrollToPoiList`: ticks before it picks the next site.
const TIME_BETWEEN_STROLLS: i64 = 100;

/// Whether the villager stands at its job site in this dimension.
fn at_job_site(world: &World, body: &VillagerEntity) -> bool {
    body.brain
        .memories()
        .lock()
        .get(&JOB_SITE)
        .is_some_and(|site| {
            site.dimension == world.key && closer_to_center_than(site.pos, body, DISTANCE)
        })
}

const WORK_AT_POI_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&JOB_SITE, MemoryStatus::ValuePresent),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
];

/// Vanilla `WorkAtPoi`: now and then works at the job site, which plays
/// the work sound and restocks the trades; farmers use `WorkAtComposter`,
/// which also bakes bread.
pub(super) struct WorkAtPoi {
    last_check: i64,
    /// Vanilla `WorkAtComposter`.
    composter: bool,
}

impl WorkAtPoi {
    /// Vanilla `new WorkAtPoi()`.
    pub(super) const fn new() -> Self {
        Self {
            last_check: 0,
            composter: false,
        }
    }

    /// Vanilla `new WorkAtComposter()`.
    pub(super) const fn at_composter() -> Self {
        Self {
            last_check: 0,
            composter: true,
        }
    }

    /// Vanilla `useWorkstation`.
    fn use_workstation(&self, world: &World, body: &VillagerEntity) {
        if !self.composter {
            return;
        }
        let Some(site) = body.brain.memories().lock().get(&JOB_SITE) else {
            return;
        };
        if world.get_block_state(site.pos).get_block() == &vanilla_blocks::COMPOSTER {
            make_bread(body);
            // TODO(villager brain): vanilla also empties a full composter and
            // fills it with spare seeds (`compostItems`) once composters
            // take items.
        }
    }
}

impl Behavior<VillagerEntity> for WorkAtPoi {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &WORK_AT_POI_ENTRY
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        let game_time = world.game_time();
        if game_time - self.last_check < CHECK_COOLDOWN || rand::random_range(0..2) != 0 {
            return false;
        }
        self.last_check = game_time;
        at_job_site(world, body)
    }

    fn start(&mut self, world: &Arc<World>, body: &VillagerEntity, time: i64) {
        {
            let mut memories = body.brain.memories().lock();
            memories.set(&LAST_WORKED_AT_POI, Some(time));
            if let Some(site) = memories.get(&JOB_SITE) {
                memories.set(&LOOK_TARGET, Some(PositionTracker::block(site.pos)));
            }
        }
        body.play_work_sound();
        self.use_workstation(world, body);
        if body.should_restock(world) {
            body.restock(world.game_time());
        }
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        at_job_site(world, body)
    }
}

/// Vanilla `WorkAtComposter.makeBread`: bakes up to three loaves from the
/// wheat it carries.
fn make_bread(body: &VillagerEntity) {
    let leftover = {
        let mut inventory = body.inventory().lock();
        if inventory.count_item(&vanilla_items::BREAD) > MAX_BREAD {
            return;
        }
        let wheat = inventory.count_item(&vanilla_items::WHEAT);
        let bread_to_make = MAX_BREAD_TO_MAKE.min(wheat / WHEAT_PER_BREAD);
        if bread_to_make == 0 {
            return;
        }
        let _wheat =
            inventory.remove_item_type(&vanilla_items::WHEAT, bread_to_make * WHEAT_PER_BREAD);
        inventory.add_item(&ItemStack::with_count(&vanilla_items::BREAD, bread_to_make))
    };
    if !leftover.is_empty() {
        let _dropped = body.spawn_at_location(leftover, 0.5);
    }
}

/// Vanilla `SecondaryPoiSensor`: remembers the profession's secondary job
/// sites (farmland for farmers) near the villager.
pub(super) struct SecondaryPoiSensor;

impl Sensor<VillagerEntity> for SecondaryPoiSensor {
    fn scan_rate(&self) -> i32 {
        SECONDARY_POI_SCAN_RATE
    }

    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&SECONDARY_JOB_SITE);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &VillagerEntity) {
        let blocks = secondary_poi(body.profession());
        let center = body.block_position();
        let mut job_sites = Vec::new();
        if !blocks.is_empty() {
            for x in -4..=4 {
                for y in -2..=2 {
                    for z in -4..=4 {
                        let pos = center.offset(x, y, z);
                        let block = world.get_block_state(pos).get_block();
                        if blocks.contains(&block) {
                            job_sites.push(GlobalPos::new(world.key.clone(), pos));
                        }
                    }
                }
            }
        }
        let mut memories = body.brain.memories().lock();
        if job_sites.is_empty() {
            memories.erase(&SECONDARY_JOB_SITE);
        } else {
            memories.set(&SECONDARY_JOB_SITE, Some(job_sites));
        }
    }
}

/// Vanilla `StrollToPoiList`: while close to `must_be_close_to`, walks to
/// a random one of the positions in `stroll_to` every five seconds.
pub(super) fn stroll_to_poi_list(
    stroll_to: &'static MemoryModuleType<Vec<GlobalPos>>,
    speed_modifier: f32,
    close_enough_dist: i32,
    max_distance_from_poi: i32,
    must_be_close_to: &'static MemoryModuleType<GlobalPos>,
) -> impl BehaviorControl<VillagerEntity> {
    let mut next_ok_start_time = 0_i64;
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(stroll_to, MemoryStatus::ValuePresent),
            MemoryRequirement::new(must_be_close_to, MemoryStatus::ValuePresent),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, time: i64| {
            let mut memories = body.brain.memories().lock();
            let (Some(targets), Some(stay_close_to)) =
                (memories.get_ref(stroll_to), memories.get(must_be_close_to))
            else {
                return false;
            };
            if targets.is_empty() {
                return false;
            }
            let target = &targets[rand::random_range(0..targets.len())];
            if target.dimension != world.key
                || !closer_to_center_than(stay_close_to.pos, body, f64::from(max_distance_from_poi))
            {
                return false;
            }
            let target_pos = target.pos;
            if time > next_ok_start_time {
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::block(target_pos),
                        speed_modifier,
                        close_enough_dist,
                    )),
                );
                next_ok_start_time = time + TIME_BETWEEN_STROLLS;
            }
            true
        },
    )
}
