use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::BlockRef;
use steel_registry::enchantment_effect::EnchantmentEffectRequirements;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_enchantments, vanilla_entities, vanilla_items,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Identifier};

use super::super::entity_requirements_state;
use super::tick_effects;
use crate::behavior::init_behaviors;
use crate::entity::{Entity, SharedEntity, create_entity_instance, init_entities};
use crate::inventory::equipment::EquipmentSlot;
use crate::test_support::{TestWorld, fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn world_with_floor(name: &'static str, floor: BlockRef) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(BlockPos::new(8, 64, 8)));
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(
                BlockPos::new(x, 63, z),
                floor.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    fixture
}

fn zombie_with_soul_speed(world: &Arc<World>) -> SharedEntity {
    let zombie =
        create_entity_instance(world, &vanilla_entities::ZOMBIE, DVec3::new(8.5, 64.0, 8.5))
            .expect("zombie factory");
    world
        .try_add_entity(Arc::clone(&zombie))
        .expect("zombie added");
    let mut boots = ItemStack::new(&vanilla_items::DIAMOND_BOOTS);
    boots.set_enchantments(&[(Identifier::vanilla_static("soul_speed"), 3)], false);
    let living = zombie.as_living_entity().expect("zombie is living");
    living.with_equipment_slot_mut(EquipmentSlot::Feet, &mut |stack| *stack = boots.clone());
    zombie.set_on_ground(true);
    zombie
}

fn particle_requirements() -> &'static EnchantmentEffectRequirements {
    vanilla_enchantments::SOUL_SPEED.effects.tick[0]
        .requirements
        .expect("soul speed particles have requirements")
}

fn particles_would_spawn(entity: &dyn Entity) -> Option<bool> {
    entity_requirements_state(particle_requirements(), entity, 3, None)
}

#[test]
fn soul_speed_particles_need_movement_on_soul_blocks_every_fifth_tick() {
    let fixture = world_with_floor("soul_speed_tick_particles", &vanilla_blocks::SOUL_SAND);
    let zombie = zombie_with_soul_speed(&fixture.world);

    zombie.base().set_tick_count(10);
    zombie.set_velocity(DVec3::ZERO);
    assert_eq!(particles_would_spawn(zombie.as_ref()), Some(false));

    zombie.set_velocity(DVec3::new(0.1, 0.0, 0.0));
    assert_eq!(particles_would_spawn(zombie.as_ref()), Some(true));

    zombie.base().set_tick_count(11);
    assert_eq!(particles_would_spawn(zombie.as_ref()), Some(false));

    zombie.base().set_tick_count(15);
    zombie.set_on_ground(false);
    assert_eq!(particles_would_spawn(zombie.as_ref()), Some(false));
}

#[test]
fn soul_speed_particles_skip_other_blocks() {
    let fixture = world_with_floor("soul_speed_tick_stone", &vanilla_blocks::STONE);
    let zombie = zombie_with_soul_speed(&fixture.world);
    zombie.base().set_tick_count(5);
    zombie.set_velocity(DVec3::new(0.1, 0.0, 0.0));
    assert_eq!(particles_would_spawn(zombie.as_ref()), Some(false));
}

#[test]
fn tick_effects_run_soul_speed_without_touching_the_boots() {
    let fixture = world_with_floor("soul_speed_tick_run", &vanilla_blocks::SOUL_SOIL);
    let zombie = zombie_with_soul_speed(&fixture.world);
    zombie.base().set_tick_count(20);
    zombie.set_velocity(DVec3::new(0.2, 0.0, 0.0));
    let living = zombie.as_living_entity().expect("zombie is living");

    tick_effects(&fixture.world, living);

    let mut damage = None;
    living.with_equipment_slot(EquipmentSlot::Feet, &mut |stack| {
        damage = Some(stack.get_damage_value());
    });
    assert_eq!(damage, Some(0));
}
