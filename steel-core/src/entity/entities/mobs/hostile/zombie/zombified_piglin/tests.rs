//! Zombified piglin tests.

use std::io::Cursor;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{
    init_vanilla_registry, sound_events, vanilla_attributes, vanilla_blocks, vanilla_entities,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _, WorldAabb};

use super::{SPEED_MODIFIER_ATTACKING_ID, ZombifiedPiglinEntity};
use crate::behavior::init_behaviors;
use crate::entity::entities::{LightningBoltEntity, PigEntity, ZombieEntity};
use crate::entity::neutral_mob::NeutralMob as _;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, Mob, Monster, SharedEntity, init_entities,
    next_entity_id,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};
use crate::world::World;

fn loaded_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
    fixture
}

fn spawn_piglin(world: &Arc<World>, position: DVec3) -> Arc<ZombifiedPiglinEntity> {
    let piglin = Arc::new(ZombifiedPiglinEntity::new(
        &vanilla_entities::ZOMBIFIED_PIGLIN,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&piglin) as SharedEntity)
        .expect("zombified piglin should attach to the loaded chunk");
    piglin
}

fn spawn_zombie(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let zombie: SharedEntity = Arc::new(ZombieEntity::new(
        &vanilla_entities::ZOMBIE,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&zombie))
        .expect("zombie should attach to the loaded chunk");
    zombie
}

#[test]
fn never_spawns_on_nether_wart_blocks() {
    let fixture = loaded_world("zombified_piglin_spawn_rule");
    let world = &fixture.world;
    let pos = BlockPos::new(4, 5, 4);
    world.set_block(
        pos.below(),
        vanilla_blocks::NETHERRACK.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    let mut random = rand::rng();
    let rule = ZombifiedPiglinEntity::SPAWN_RULE;
    assert!(rule(
        &vanilla_entities::ZOMBIFIED_PIGLIN,
        world.as_ref(),
        EntitySpawnReason::Natural,
        pos,
        &mut random,
    ));
    world.set_block(
        pos.below(),
        vanilla_blocks::NETHER_WART_BLOCK.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    assert!(!rule(
        &vanilla_entities::ZOMBIFIED_PIGLIN,
        world.as_ref(),
        EntitySpawnReason::Natural,
        pos,
        &mut random,
    ));
}

#[test]
fn anger_speeds_it_up_and_changes_its_voice() {
    let fixture = loaded_world("zombified_piglin_anger");
    let world = &fixture.world;
    let piglin = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    assert_eq!(
        piglin.ambient_sound(),
        Some(&sound_events::ENTITY_ZOMBIFIED_PIGLIN_AMBIENT)
    );

    piglin.start_persistent_anger_timer();
    assert!(piglin.is_angry());
    piglin.custom_server_ai_step_zombified_piglin();
    assert!(piglin.attributes().lock().has_modifier(
        vanilla_attributes::MOVEMENT_SPEED,
        &SPEED_MODIFIER_ATTACKING_ID
    ));
    assert_eq!(
        piglin.ambient_sound(),
        Some(&sound_events::ENTITY_ZOMBIFIED_PIGLIN_ANGRY)
    );

    piglin.stop_being_angry();
    piglin.custom_server_ai_step_zombified_piglin();
    assert!(!piglin.attributes().lock().has_modifier(
        vanilla_attributes::MOVEMENT_SPEED,
        &SPEED_MODIFIER_ATTACKING_ID
    ));
}

#[test]
fn calls_idle_zombified_piglins_onto_its_target() {
    let fixture = loaded_world("zombified_piglin_alert");
    let world = &fixture.world;
    let caller = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    let idle = spawn_piglin(world, DVec3::new(9.5, 1.0, 6.5));
    let zombie = spawn_zombie(world, DVec3::new(12.5, 1.0, 6.5));
    let target = spawn_zombie(world, DVec3::new(3.5, 1.0, 3.5));

    caller.alert_others(world, &target);
    assert_eq!(
        idle.target().map(|entity| entity.id()),
        Some(target.id()),
        "the other zombified piglin joins in"
    );
    assert!(
        zombie.as_mob().and_then(Mob::target).is_none(),
        "plain zombies are not called"
    );
    assert!(caller.target().is_none(), "it does not call itself");
}

#[test]
fn a_fresh_target_schedules_the_first_growl_and_alert() {
    let fixture = loaded_world("zombified_piglin_set_target");
    let world = &fixture.world;
    let piglin = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    let target = spawn_zombie(world, DVec3::new(3.5, 1.0, 3.5));
    assert!(piglin.set_target(Some(&target)));
    let state = piglin.piglin_state.lock();
    assert!((0..=20).contains(&state.play_first_anger_sound_in));
    assert!((80..=120).contains(&state.ticks_until_next_alert));
}

#[test]
fn only_prevents_rest_while_angry_at_that_player() {
    let fixture = loaded_world("zombified_piglin_rest");
    let world = &fixture.world;
    let piglin = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Sleeper", 7).build();
    assert!(!Monster::is_preventing_player_rest(
        piglin.as_ref(),
        world,
        &player
    ));
}

#[test]
fn holds_a_golden_weapon() {
    let fixture = loaded_world("zombified_piglin_equipment");
    let world = &fixture.world;
    let piglin = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    piglin.populate_default_equipment();
    let mut item = None;
    piglin.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| {
        item = Some(stack.item().key.clone());
    });
    let item = item.expect("main hand should be readable");
    assert!(
        item.path == "golden_sword" || item.path == "golden_spear",
        "unexpected weapon {item}"
    );
}

#[test]
fn anger_round_trips() {
    let fixture = loaded_world("zombified_piglin_save");
    let world = &fixture.world;
    let piglin = spawn_piglin(world, DVec3::new(6.5, 1.0, 6.5));
    piglin.set_persistent_anger_end_time(1234);
    let mut saved = NbtCompound::new();
    piglin.save_additional(&mut saved);
    assert_eq!(saved.long("anger_end_time"), Some(1234));

    let loaded = spawn_piglin(world, DVec3::new(2.5, 1.0, 2.5));
    let bytes = {
        let mut out = Vec::new();
        saved.write(&mut out);
        out
    };
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("saved nbt should reborrow: {error}"));
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.persistent_anger_end_time(), 1234);
}

#[test]
fn lightning_turns_a_pig_into_a_persistent_zombified_piglin() {
    let fixture = loaded_world("zombified_piglin_from_pig");
    let world = &fixture.world;
    let pig = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        DVec3::new(8.5, 1.0, 8.5),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&pig) as SharedEntity)
        .expect("pig should attach to the loaded chunk");
    let bolt = LightningBoltEntity::new(
        &vanilla_entities::LIGHTNING_BOLT,
        next_entity_id(),
        DVec3::new(8.5, 1.0, 8.5),
        Arc::downgrade(world),
    );

    pig.thunder_hit(world, &bolt);

    assert!(pig.is_removed(), "the pig is replaced");
    let area = WorldAabb::new(0.0, -10.0, 0.0, 16.0, 20.0, 16.0);
    let piglins: Vec<_> = world
        .get_entities_in_aabb(&area)
        .into_iter()
        .filter(|entity| entity.downcast_ref::<ZombifiedPiglinEntity>().is_some())
        .collect();
    assert_eq!(piglins.len(), 1);
    let piglin = piglins[0]
        .downcast_ref::<ZombifiedPiglinEntity>()
        .expect("checked above");
    assert!(piglin.is_persistence_required());
    assert!(piglin.has_item_in_slot(EquipmentSlot::MainHand));
}
