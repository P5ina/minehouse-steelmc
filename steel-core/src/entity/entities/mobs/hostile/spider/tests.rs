use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_registry::{
    init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_mob_effects,
};
use steel_utils::types::{Difficulty, UpdateFlags};
use steel_utils::{BlockPos, ChunkPos, Downcast as _};

use super::{CaveSpiderEntity, SpiderEntity};
use crate::behavior::init_behaviors;
use crate::entity::entities::CowEntity;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, Mob, MobEffectInstance, SharedEntity, SpawnGroupData,
    next_entity_id,
};
use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
use crate::world::World;

fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> SharedEntity {
    let entity: SharedEntity = Arc::new(entity);
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("test entity should attach to the loaded chunk");
    entity
}

fn spider(world: &Arc<World>, position: DVec3) -> SharedEntity {
    add(
        world,
        SpiderEntity::new(
            &vanilla_entities::SPIDER,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        ),
    )
}

#[test]
fn spiders_ignore_poison_but_take_other_effects() {
    init_vanilla_registry();
    let spider = SpiderEntity::new(&vanilla_entities::SPIDER, 1, DVec3::ZERO, Weak::new());
    let cave_spider =
        CaveSpiderEntity::new(&vanilla_entities::CAVE_SPIDER, 2, DVec3::ZERO, Weak::new());

    let poison = MobEffectInstance::with_duration(vanilla_mob_effects::POISON, 100, 0);
    let speed = MobEffectInstance::with_duration(vanilla_mob_effects::SPEED, 100, 0);
    assert!(!spider.can_be_affected(&poison));
    assert!(!cave_spider.can_be_affected(&poison));
    assert!(spider.can_be_affected(&speed));
}

#[test]
fn climbing_flag_drives_on_climbable() {
    init_vanilla_registry();
    let spider = SpiderEntity::new(&vanilla_entities::SPIDER, 1, DVec3::ZERO, Weak::new());
    assert!(!spider.on_climbable());

    spider.set_climbing(true);
    assert!(spider.is_climbing());
    assert!(spider.on_climbable());

    spider.set_climbing(false);
    assert!(!spider.on_climbable());
}

#[test]
fn spider_climbs_when_pushed_into_a_wall() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("spider_climbs_wall");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let stone = vanilla_blocks::STONE.default_state();
    for y in 64..70 {
        for z in 6..11 {
            world.set_block(BlockPos::new(10, y, z), stone, UpdateFlags::UPDATE_ALL);
        }
    }

    // Spiders are 1.4 wide: centered 0.71 from the wall, they touch it.
    let spider = spider(world, DVec3::new(9.29, 65.0, 8.5));
    let spider_ref = spider
        .downcast_ref::<SpiderEntity>()
        .expect("spider is concrete");
    spider.set_velocity(DVec3::new(0.2, 0.0, 0.0));
    Arc::clone(&spider).tick();

    assert!(
        spider_ref.is_climbing(),
        "a wall in the way starts climbing"
    );
}

#[test]
fn spider_finalize_spawn_applies_the_group_effect_forever() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("spider_spawn_effect");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let spider = spider(world, DVec3::new(8.5, 65.0, 8.5));
    let spider_ref = spider
        .downcast_ref::<SpiderEntity>()
        .expect("spider is concrete");

    let group = Some(SpawnGroupData::SpiderEffects(Some(
        vanilla_mob_effects::SPEED,
    )));
    let returned = spider_ref.finalize_spawn(world, EntitySpawnReason::Command, group);

    assert_eq!(returned, group);
    let effect = spider_ref
        .mob_effect(vanilla_mob_effects::SPEED)
        .expect("the group effect is applied");
    assert!(effect.is_infinite_duration());
}

#[test]
fn spider_finalize_spawn_without_group_creates_one() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("spider_spawn_group");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    world.set_difficulty(Difficulty::Easy);
    let spider = spider(world, DVec3::new(8.5, 65.0, 8.5));
    let spider_ref = spider
        .downcast_ref::<SpiderEntity>()
        .expect("spider is concrete");

    let returned = spider_ref.finalize_spawn(world, EntitySpawnReason::Command, None);

    // Below Hard there is never an effect, but the group is still created.
    assert_eq!(returned, Some(SpawnGroupData::SpiderEffects(None)));
    assert_eq!(spider_ref.active_mob_effects(), Vec::new());
}

#[test]
fn cave_spider_finalize_spawn_keeps_the_group_untouched() {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world("cave_spider_spawn");
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    let cave_spider = add(
        world,
        CaveSpiderEntity::new(
            &vanilla_entities::CAVE_SPIDER,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let cave_spider_ref = cave_spider
        .downcast_ref::<CaveSpiderEntity>()
        .expect("cave spider is concrete");

    let group = Some(SpawnGroupData::SpiderEffects(Some(
        vanilla_mob_effects::SPEED,
    )));
    assert_eq!(
        cave_spider_ref.finalize_spawn(world, EntitySpawnReason::Command, None),
        None
    );
    assert_eq!(
        cave_spider_ref.finalize_spawn(world, EntitySpawnReason::Command, group),
        group
    );
    assert_eq!(cave_spider_ref.active_mob_effects(), Vec::new());
}

fn cave_spider_bite_poison_ticks(difficulty: Difficulty, name: &'static str) -> Option<i32> {
    init_vanilla_registry();
    init_behaviors();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    world.set_difficulty(difficulty);
    let cave_spider = add(
        world,
        CaveSpiderEntity::new(
            &vanilla_entities::CAVE_SPIDER,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let cow = add(
        world,
        CowEntity::new(
            &vanilla_entities::COW,
            next_entity_id(),
            DVec3::new(9.5, 65.0, 8.5),
            Arc::downgrade(world),
        ),
    );
    let cave_spider_ref = cave_spider
        .downcast_ref::<CaveSpiderEntity>()
        .expect("cave spider is concrete");

    assert!(cave_spider_ref.do_hurt_target(&cave_spider, world, &cow));
    cow.as_living_entity()
        .expect("cow is living")
        .mob_effect(vanilla_mob_effects::POISON)
        .map(|effect| effect.duration())
}

#[test]
fn cave_spider_bite_poisons_by_difficulty() {
    assert_eq!(
        cave_spider_bite_poison_ticks(Difficulty::Easy, "cave_spider_bite_easy"),
        None
    );
    assert_eq!(
        cave_spider_bite_poison_ticks(Difficulty::Normal, "cave_spider_bite_normal"),
        Some(7 * 20)
    );
    assert_eq!(
        cave_spider_bite_poison_ticks(Difficulty::Hard, "cave_spider_bite_hard"),
        Some(15 * 20)
    );
}
