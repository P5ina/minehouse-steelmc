use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::mob_effect::MobEffectRef;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_mob_effects,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, WorldAabb};

use crate::behavior::init_behaviors;
use crate::entity::{
    MobEffectInstance, RemovalReason, SharedEntity, create_entity_instance, init_entities,
};
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

const POS: BlockPos = BlockPos::new(8, 64, 8);

fn world_with_floor(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(POS));
    for x in 6..=10 {
        for z in 6..=10 {
            world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    fixture
}

fn spawn_pig_with(world: &Arc<World>, effect: MobEffectRef) -> SharedEntity {
    let pig = create_entity_instance(world, &vanilla_entities::PIG, DVec3::new(8.5, 64.0, 8.5))
        .expect("pig factory");
    world.try_add_entity(Arc::clone(&pig)).expect("pig added");
    let living = pig.as_living_entity().expect("pig is living");
    assert!(living.add_mob_effect(MobEffectInstance::with_duration(effect, 200, 0)));
    pig
}

fn count_near(world: &Arc<World>, entity_type: EntityTypeRef) -> usize {
    let bounds = WorldAabb::new(4.0, 60.0, 4.0, 13.0, 70.0, 13.0);
    world
        .get_entities_in_aabb(&bounds)
        .iter()
        .filter(|entity| entity.entity_type() == entity_type && !entity.is_removed())
        .count()
}

fn cobwebs_around(world: &Arc<World>) -> usize {
    let mut count = 0;
    for x in 7..=9 {
        for y in 63..=65 {
            for z in 7..=9 {
                if world.get_block_state(BlockPos::new(x, y, z)).get_block()
                    == &vanilla_blocks::COBWEB
                {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn oozing_mob_killed_splits_into_two_size_two_slimes() {
    let fixture = world_with_floor("oozing_death");
    let world = &fixture.world;
    let pig = spawn_pig_with(world, vanilla_mob_effects::OOZING);

    pig.set_removed(RemovalReason::Killed);

    let slimes: Vec<_> = world
        .get_entities_in_aabb(&WorldAabb::new(4.0, 60.0, 4.0, 13.0, 70.0, 13.0))
        .into_iter()
        .filter(|entity| entity.entity_type() == &vanilla_entities::SLIME)
        .collect();
    assert_eq!(slimes.len(), 2);
    for slime in slimes {
        assert_eq!(slime.as_cube_mob().expect("slime").get_size(), 2);
    }
    let living = pig.as_living_entity().expect("pig is living");
    assert_eq!(living.active_mob_effects().len(), 0);
}

#[test]
fn discarded_oozing_mob_spawns_nothing_but_forgets_effects() {
    let fixture = world_with_floor("oozing_discard");
    let world = &fixture.world;
    let pig = spawn_pig_with(world, vanilla_mob_effects::OOZING);

    pig.set_removed(RemovalReason::Discarded);

    assert_eq!(count_near(world, &vanilla_entities::SLIME), 0);
    assert_eq!(
        pig.as_living_entity()
            .expect("living")
            .active_mob_effects()
            .len(),
        0
    );
}

#[test]
fn weaving_mob_killed_leaves_cobwebs_on_sturdy_floor() {
    let fixture = world_with_floor("weaving_death");
    let world = &fixture.world;
    // Each death makes 15 random tries over the 3x3x3 cube; only the 9 cells at
    // y=64 are air above a sturdy face, so a single death can miss them all.
    let mut webs = 0;
    for _ in 0..5 {
        spawn_pig_with(world, vanilla_mob_effects::WEAVING).set_removed(RemovalReason::Killed);
        webs = cobwebs_around(world);
        if webs > 0 {
            break;
        }
    }
    assert!((1..=3).contains(&webs), "placed {webs} cobwebs");
    for x in 7..=9 {
        for z in 7..=9 {
            for y in [63, 65] {
                assert_ne!(
                    world.get_block_state(BlockPos::new(x, y, z)).get_block(),
                    &vanilla_blocks::COBWEB
                );
            }
        }
    }
}

#[test]
fn weaving_needs_mob_griefing_for_non_players() {
    let fixture = world_with_floor("weaving_no_griefing");
    let world = &fixture.world;
    assert!(world.set_game_rule(&MOB_GRIEFING, false));
    let pig = spawn_pig_with(world, vanilla_mob_effects::WEAVING);

    pig.set_removed(RemovalReason::Killed);

    assert_eq!(cobwebs_around(world), 0);
}
