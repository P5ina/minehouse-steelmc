use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{init_vanilla_registry, vanilla_damage_types, vanilla_entities};
use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

use super::flight::{GhastMoveState, tick_ghast_move_control};
use super::{GhastEntity, GhastShootFireballGoal, is_reflected_fireball};
use crate::entity::ai::goal::Goal;
use crate::entity::damage::DamageSource;
use crate::entity::entities::{LargeFireballEntity, PigEntity};
use crate::entity::{Entity, Mob, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn add<E: Entity + 'static>(world: &Arc<World>, entity: E) -> SharedEntity {
    let entity: SharedEntity = Arc::new(entity);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("entity should attach to the loaded chunk");
    entity
}

#[test]
fn a_ghast_charges_for_a_second_then_shoots_one_large_fireball() {
    init_vanilla_registry();
    init_entities();
    let world_fixture = fresh_test_world("ghast_shoot");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));

    let entity = add(
        world,
        GhastEntity::new(
            &vanilla_entities::GHAST,
            next_entity_id(),
            DVec3::new(8.0, 100.0, 1.0),
            Arc::downgrade(world),
        ),
    );
    let target = add(
        world,
        PigEntity::new(
            &vanilla_entities::PIG,
            next_entity_id(),
            DVec3::new(8.0, 100.0, 14.0),
            Arc::downgrade(world),
        ),
    );
    let ghast = entity
        .downcast_ref::<GhastEntity>()
        .expect("ghast is concrete");
    ghast.set_explosion_power(2);
    assert!(ghast.set_target(Some(&target)));

    let fireballs = || -> Vec<SharedEntity> {
        world
            .get_entities_in_aabb(&WorldAabb::new(-16.0, 80.0, -16.0, 32.0, 120.0, 32.0))
            .into_iter()
            .filter(|candidate| candidate.downcast_ref::<LargeFireballEntity>().is_some())
            .collect()
    };

    let mut goal = GhastShootFireballGoal::new();
    assert!(goal.can_use(ghast));
    goal.start(ghast);
    for _ in 0..10 {
        goal.tick(ghast, &entity);
    }
    assert!(!ghast.is_charging(), "the mouth opens after the warning");
    goal.tick(ghast, &entity);
    assert!(ghast.is_charging());
    for _ in 0..8 {
        goal.tick(ghast, &entity);
    }
    assert!(fireballs().is_empty());
    goal.tick(ghast, &entity);
    let shot = fireballs();
    assert_eq!(shot.len(), 1, "the twentieth tick fires");
    assert!(!ghast.is_charging(), "the shot resets the charge");
    let fireball = shot[0]
        .downcast_ref::<LargeFireballEntity>()
        .expect("large fireball");
    assert_eq!(fireball.explosion_power(), 2);
    assert!(
        fireball.velocity().z > 0.0,
        "the fireball flies at the target"
    );

    for _ in 0..59 {
        goal.tick(ghast, &entity);
    }
    assert_eq!(
        fireballs().len(),
        1,
        "two seconds of cooldown, then a new charge"
    );
    goal.stop(ghast);
    assert!(!ghast.is_charging());
}

#[test]
fn the_move_control_pushes_toward_the_wanted_position() {
    init_vanilla_registry();
    let world_fixture = fresh_test_world("ghast_fly");
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let entity = add(
        world,
        GhastEntity::new(
            &vanilla_entities::GHAST,
            next_entity_id(),
            DVec3::new(8.0, 100.0, 8.0),
            Arc::downgrade(world),
        ),
    );
    let ghast = entity
        .downcast_ref::<GhastEntity>()
        .expect("ghast is concrete");
    ghast.set_wanted_position(DVec3::new(8.0, 110.0, 8.0), 1.0);
    let mut state = GhastMoveState::default();
    tick_ghast_move_control(ghast, &mut state);
    let expected = 0.06 * 5.0 / 3.0;
    assert!((ghast.velocity().y - expected).abs() < 1.0e-6);
    tick_ghast_move_control(ghast, &mut state);
    assert!(
        (ghast.velocity().y - expected).abs() < 1.0e-6,
        "the next push waits a few ticks"
    );
}

#[test]
fn only_a_fireball_sent_back_by_a_player_counts_as_reflected() {
    init_vanilla_registry();
    let ghast: SharedEntity = Arc::new(GhastEntity::new(
        &vanilla_entities::GHAST,
        1,
        DVec3::ZERO,
        Weak::new(),
    ));
    let fireball: SharedEntity = Arc::new(LargeFireballEntity::new(
        &vanilla_entities::FIREBALL,
        2,
        DVec3::ZERO,
        Weak::new(),
    ));
    let own_shot = DamageSource::environment(&vanilla_damage_types::FIREBALL)
        .with_direct_entity(fireball)
        .with_causing_entity(Arc::clone(&ghast));
    assert!(!is_reflected_fireball(&own_shot));
    let melee = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
        .with_direct_entity(Arc::clone(&ghast))
        .with_causing_entity(ghast);
    assert!(!is_reflected_fireball(&melee));
}

#[test]
fn explosion_power_round_trips_as_a_byte() {
    init_vanilla_registry();
    let ghast = GhastEntity::new(&vanilla_entities::GHAST, 1, DVec3::ZERO, Weak::new());
    ghast.set_explosion_power(4);
    let mut nbt = NbtCompound::new();
    ghast.save_additional(&mut nbt);
    assert_eq!(nbt.byte("ExplosionPower"), Some(4));

    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = GhastEntity::new(&vanilla_entities::GHAST, 2, DVec3::ZERO, Weak::new());
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.explosion_power(), 4);
}
