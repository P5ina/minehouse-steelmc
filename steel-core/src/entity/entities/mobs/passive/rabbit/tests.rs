use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::entity_variant::RabbitVariant;
use steel_registry::{init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos};

use super::RabbitEntity;
use super::goals::RaidGardenGoal;
use crate::behavior::init_behaviors;
use crate::entity::ai::goal::Goal;
use crate::entity::{Entity, LivingEntity, Mob, SharedEntity, init_entities};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

fn test_rabbit() -> RabbitEntity {
    init_vanilla_registry();
    RabbitEntity::new(&vanilla_entities::RABBIT, 1, DVec3::ZERO, Weak::new())
}

fn attack_damage(rabbit: &RabbitEntity) -> f64 {
    rabbit
        .attributes()
        .lock()
        .required_value(vanilla_attributes::ATTACK_DAMAGE)
}

#[test]
fn saves_variant_and_carrot_cooldown() {
    let rabbit = test_rabbit();
    rabbit.set_variant(RabbitVariant::Salt);
    rabbit.ate_carrot();
    let mut nbt = NbtCompound::new();
    rabbit.save_additional(&mut nbt);
    assert_eq!(nbt.int("RabbitType"), Some(5));
    assert_eq!(nbt.int("MoreCarrotTicks"), Some(40));

    let loaded = test_rabbit();
    let bytes = {
        let mut buffer = Vec::new();
        nbt.write(&mut buffer);
        buffer
    };
    let Ok(borrowed) = read_compound(&mut Cursor::new(&bytes[..])) else {
        panic!("saved rabbit NBT should read back");
    };
    loaded.load_additional((&borrowed).into());
    assert_eq!(loaded.variant(), RabbitVariant::Salt);
    assert!(!loaded.wants_more_food());
}

#[test]
fn killer_bunny_is_armored_named_and_hostile() {
    let rabbit = test_rabbit();
    assert_eq!(attack_damage(&rabbit), 3.0);
    assert_eq!(rabbit.sound_source(), SoundSource::Neutral);

    rabbit.set_variant(RabbitVariant::Evil);
    assert_eq!(attack_damage(&rabbit), 8.0);
    assert_eq!(rabbit.get_armor_value(), 8);
    assert_eq!(rabbit.sound_source(), SoundSource::Hostile);
    assert!(rabbit.custom_name().is_some());

    rabbit.set_variant(RabbitVariant::Brown);
    assert_eq!(attack_damage(&rabbit), 3.0);
}

#[test]
fn hop_power_follows_the_move_speed() {
    let rabbit = test_rabbit();
    rabbit.set_wanted_position(DVec3::new(4.0, 0.0, 0.0), 0.6);
    assert!((rabbit.get_jump_power() - 0.2).abs() < 1.0e-6);
    rabbit.set_wanted_position(DVec3::new(4.0, 0.0, 0.0), 1.0);
    assert!((rabbit.get_jump_power() - 0.3).abs() < 1.0e-6);
}

#[test]
fn landing_pause_is_shorter_when_panicking() {
    let rabbit = test_rabbit();
    rabbit.set_wanted_position(DVec3::new(4.0, 0.0, 0.0), 1.0);
    rabbit.check_landing_delay();
    assert_eq!(rabbit.hop.lock().jump_delay_ticks, 10);
    rabbit.set_wanted_position(DVec3::new(4.0, 0.0, 0.0), 2.2);
    rabbit.check_landing_delay();
    assert_eq!(rabbit.hop.lock().jump_delay_ticks, 3);
}

#[test]
fn rabbit_on_the_ground_hops_toward_where_it_wants_to_go() {
    let rabbit = test_rabbit();
    rabbit.set_on_ground(true);
    rabbit.hop.lock().was_on_ground = true;
    // As in vanilla, the constructor's `setSpeedModifier(0.0)` leaves a
    // wanted position until the move control first runs.
    rabbit.tick_move_control();

    rabbit.custom_server_ai_step();
    assert!(!rabbit.is_jumping(), "an idle rabbit keeps still");

    rabbit.set_wanted_position(DVec3::new(0.0, 0.0, 4.0), 0.6);
    rabbit.custom_server_ai_step();
    assert!(rabbit.is_jumping());
    assert_eq!(rabbit.hop.lock().jump_duration, 15);
    // Facing +Z is a yaw of 0 in Minecraft.
    assert!(rabbit.rotation().0.abs() < 1.0e-3);
}

#[test]
fn move_control_keeps_the_next_hop_speed() {
    let rabbit = test_rabbit();
    rabbit.set_wanted_position(DVec3::new(4.0, 0.0, 0.0), 1.25);
    rabbit.set_speed_modifier(0.0);
    assert_eq!(rabbit.hop.lock().next_jump_speed, 1.25);
}

#[test]
fn hungry_rabbit_nibbles_a_ripe_carrot() {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world("rabbit_raid_garden");
    let world = &fixture.world;
    let farmland = BlockPos::new(8, 64, 8);
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(farmland));
    assert!(world.set_block(
        farmland,
        vanilla_blocks::FARMLAND.default_state(),
        UpdateFlags::UPDATE_NONE,
    ));
    let ripe = vanilla_blocks::CARROTS
        .default_state()
        .set_value(&BlockStateProperties::AGE_7, 7_u8);
    assert!(world.set_block(farmland.above(), ripe, UpdateFlags::UPDATE_NONE));

    let rabbit = Arc::new(RabbitEntity::new(
        &vanilla_entities::RABBIT,
        1,
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    ));
    let entity: SharedEntity = Arc::clone(&rabbit) as SharedEntity;
    let mut goal = RaidGardenGoal::new();
    assert!(goal.can_use(rabbit.as_ref()));
    goal.start(rabbit.as_ref());
    goal.tick(rabbit.as_ref(), &entity);

    let carrots = world.get_block_state(farmland.above());
    assert_eq!(carrots.get_value(&BlockStateProperties::AGE_7), 6);
    assert!(!rabbit.wants_more_food());
    assert!(!goal.can_continue_to_use(rabbit.as_ref()));
}
