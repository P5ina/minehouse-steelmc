use std::io::Cursor;
use std::sync::{Arc, Weak};

use simdnbt::borrow::read_compound as read_borrowed_compound;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_frog_variants, vanilla_items,
};
use steel_utils::ChunkPos;
use steel_utils::types::UpdateFlags;

use super::ai::{Croak, ShootTongue, TongueState, can_eat};
use super::*;
use crate::behavior::init_behaviors;
use crate::entity::ai::brain::BehaviorControl as _;
use crate::entity::ai::brain::behavior::Behavior as _;
use crate::entity::ai::brain::behaviors::try_lay_spawn_on_fluid_near_land;
use crate::entity::ai::brain::memory_types::{UNREACHABLE_TONGUE_TARGETS, WALK_TARGET, WalkTarget};
use crate::entity::ai::path::{PathType, PathfindingContext};
use crate::entity::ai::walk::{MobPathSettings, WalkNodeEvaluator};
use crate::entity::entities::mobs::hostile::cube_mob::AbstractCubeMob as _;
use crate::entity::entities::{MagmaCubeEntity, SlimeEntity};
use crate::entity::{EntityPose, init_entities};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

fn frog(id: i32) -> FrogEntity {
    FrogEntity::new(&vanilla_entities::FROG, id, DVec3::ZERO, Weak::new())
}

#[test]
fn frogs_eat_only_the_smallest_cubes() {
    init_vanilla_registry();
    let slime = SlimeEntity::new(&vanilla_entities::SLIME, 1, DVec3::ZERO, Weak::new());
    slime.set_size(1, true);
    assert!(can_eat(&slime));
    slime.set_size(2, true);
    assert!(!can_eat(&slime));

    let magma_cube =
        MagmaCubeEntity::new(&vanilla_entities::MAGMA_CUBE, 2, DVec3::ZERO, Weak::new());
    magma_cube.set_size(1, true);
    assert!(can_eat(&magma_cube));

    assert!(!can_eat(&frog(3)));
}

#[test]
fn frogs_never_grow_up_and_love_slime_balls() {
    init_vanilla_registry();
    let adult = frog(1);
    assert!(!adult.can_be_a_baby());
    assert!(adult.is_food(&ItemStack::new(&vanilla_items::SLIME_BALL)));
    assert!(!adult.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
    assert!((adult.head_rot_speed() - HEAD_ROT_SPEED).abs() < f32::EPSILON);
    assert!(!adult.is_pushed_by_fluid());
}

#[test]
fn variant_survives_save_and_load() {
    init_vanilla_registry();
    let saved = frog(1);
    saved.set_variant(&vanilla_frog_variants::WARM);
    let mut nbt = NbtCompound::new();
    saved.save_additional(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));

    let loaded = frog(2);
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.variant().key, vanilla_frog_variants::WARM.key);
}

#[test]
fn tongue_target_keeps_the_head_pitch() {
    init_vanilla_registry();
    let hunter = frog(1);
    let prey = frog(7);
    assert!(hunter.look_control_resets_x_rot());
    hunter.set_tongue_target(&prey);
    assert_eq!(*hunter.entity_data.lock().tongue_target.get(), Some(7));
    assert!(!hunter.look_control_resets_x_rot());
    hunter.erase_tongue_target();
    assert!(hunter.look_control_resets_x_rot());
}

#[test]
fn croaking_only_starts_from_standing_and_lasts_three_seconds() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("frog_croak");
    let world = &fixture.world;
    let body = frog(1);
    let mut croak = Croak::new();

    body.set_pose(EntityPose::LongJumping);
    assert!(!croak.check_extra_start_conditions(world, &body));
    body.set_pose(EntityPose::Standing);
    assert!(croak.check_extra_start_conditions(world, &body));

    croak.start(world, &body, 0);
    assert_eq!(body.pose(), EntityPose::Croaking);
    for tick in 0..60 {
        assert!(croak.can_still_use(world, &body, tick));
        croak.tick(world, &body, tick);
    }
    assert!(!croak.can_still_use(world, &body, 60));
    croak.stop(world, &body, 60);
    assert_eq!(body.pose(), EntityPose::Standing);
}

#[test]
fn unreachable_prey_is_remembered_and_dropped() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("frog_unreachable_prey");
    let world = &fixture.world;
    let body = frog(1);
    let mut tongue = ShootTongue::new(
        &sound_events::ENTITY_FROG_TONGUE,
        &sound_events::ENTITY_FROG_EAT,
    );
    // No chunk is loaded, so no path leads to the prey.
    let prey: SharedEntity = Arc::new(SlimeEntity::new(
        &vanilla_entities::SLIME,
        2,
        DVec3::new(3.0, 0.0, 0.0),
        Weak::new(),
    ));
    body.brain()
        .memories()
        .lock()
        .set(&ATTACK_TARGET, Some(Arc::clone(&prey)));

    assert!(!tongue.check_extra_start_conditions(world, &body));
    let memories = body.brain().memories().lock();
    assert!(!memories.has_value(&ATTACK_TARGET));
    assert_eq!(
        memories.get(&UNREACHABLE_TONGUE_TARGETS),
        Some(vec![prey.uuid()])
    );
    assert_eq!(tongue.state(), TongueState::Done);
}

#[test]
fn lily_pads_are_open_ground_for_frogs() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("frog_lily_pad_path_type");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let pad = BlockPos::new(4, 64, 4);
    assert!(world.set_block(
        pad,
        vanilla_blocks::LILY_PAD.default_state(),
        UpdateFlags::UPDATE_NONE,
    ));
    let body = frog(1);
    let mut context = PathfindingContext::new(world.as_ref(), BlockPos::new(0, 64, 0));
    let frog_evaluator = WalkNodeEvaluator::frog(MobPathSettings::from_mob(&body));
    let amphibious = WalkNodeEvaluator::amphibious(MobPathSettings::from_mob(&body), true);
    let above = pad.above();
    assert_eq!(
        frog_evaluator.path_type(&mut context, above.x(), above.y(), above.z()),
        PathType::Open
    );
    assert_ne!(
        amphibious.path_type(&mut context, above.x(), above.y(), above.z()),
        PathType::Open
    );
}

#[test]
fn pregnant_frogs_lay_spawn_on_water_next_to_them() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("frog_lay_spawn");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let ground = BlockPos::new(4, 63, 4);
    assert!(world.set_block(
        ground,
        vanilla_blocks::STONE.default_state(),
        UpdateFlags::UPDATE_NONE,
    ));
    let water = ground.east();
    assert!(world.set_block(
        water,
        vanilla_blocks::WATER.default_state(),
        UpdateFlags::UPDATE_NONE,
    ));

    let body = FrogEntity::new(
        &vanilla_entities::FROG,
        1,
        DVec3::new(4.5, 64.0, 4.5),
        Arc::downgrade(world),
    );
    body.set_on_ground(true);
    {
        let mut memories = body.brain().memories().lock();
        memories.set(&IS_PREGNANT, Some(()));
        memories.set(
            &WALK_TARGET,
            Some(WalkTarget::to_point(DVec3::new(4.5, 64.0, 4.5), 1.0, 0)),
        );
    }
    let mut lay_spawn = try_lay_spawn_on_fluid_near_land(&vanilla_blocks::FROGSPAWN);
    assert!(lay_spawn.try_start(world, &body, 0));
    assert_eq!(
        world.get_block_state(water.above()).get_block(),
        &vanilla_blocks::FROGSPAWN
    );
    assert!(!body.brain().memories().lock().has_value(&IS_PREGNANT));
}
