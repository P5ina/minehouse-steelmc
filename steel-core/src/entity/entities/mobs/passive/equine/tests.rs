use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::HorseVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

use super::chested::AbstractChestedHorse as _;
use super::*;
use crate::entity::{Animal, Entity, SharedEntity};

fn round_trip(
    save: impl FnOnce(&mut NbtCompound),
    load: impl FnOnce(BorrowedNbtCompoundView<'_, '_>),
) {
    let mut nbt = NbtCompound::new();
    save(&mut nbt);
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));
    load((&borrowed).into());
}

#[test]
fn offspring_attribute_stays_in_range() {
    for _ in 0..1000 {
        let Some(value) = create_offspring_attribute(0.1, 0.3, 0.1125, 0.3375) else {
            panic!("a non-empty range always yields a value");
        };
        assert!((0.1125..=0.3375).contains(&value), "{value}");
    }
    assert_eq!(create_offspring_attribute(1.0, 2.0, 3.0, 3.0), None);
}

#[test]
fn generated_attributes_match_vanilla_bounds() {
    assert_eq!(generate_max_health(|_| 0).to_bits(), 15.0_f32.to_bits());
    assert_eq!(
        generate_max_health(|bound| bound - 1).to_bits(),
        30.0_f32.to_bits()
    );
    assert!((generate_jump_strength(|| 1.0) - 1.0).abs() < 1e-6);
    assert!((generate_speed(|| 1.0) - 0.3375).abs() < 1e-6);
}

#[test]
fn every_equine_exposes_horse_capabilities() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    let mule = MuleEntity::new(&vanilla_entities::MULE, 3, DVec3::ZERO, Weak::new());
    let skeleton = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        4,
        DVec3::ZERO,
        Weak::new(),
    );
    let zombie =
        ZombieHorseEntity::new(&vanilla_entities::ZOMBIE_HORSE, 5, DVec3::ZERO, Weak::new());
    for entity in [&horse as &dyn Entity, &donkey, &mule, &skeleton, &zombie] {
        assert!(entity.is_abstract_horse());
        assert!(entity.as_player_rideable_jumping().is_some());
    }
    assert!(!horse.is_tamed());
    assert!(
        !horse
            .as_player_rideable_jumping()
            .is_some_and(PlayerRideableJumping::can_jump)
    );
}

#[test]
fn horse_and_donkey_can_mate_but_mules_cannot() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    let mule = MuleEntity::new(&vanilla_entities::MULE, 3, DVec3::ZERO, Weak::new());
    for parent in [&horse as &dyn AbstractHorse, &donkey] {
        parent.set_tamed(true);
        parent.set_in_love(None);
    }
    mule.set_tamed(true);
    mule.set_in_love(None);

    assert!(Animal::can_mate(&horse, &donkey));
    assert!(Animal::can_mate(&donkey, &horse));
    assert!(!Animal::can_mate(&mule, &horse));
    assert!(!Animal::can_mate(&horse, &mule));
}

#[test]
fn horse_food_differs_from_zombie_horse_food() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    let zombie =
        ZombieHorseEntity::new(&vanilla_entities::ZOMBIE_HORSE, 2, DVec3::ZERO, Weak::new());
    let apple = ItemStack::new(&vanilla_items::APPLE);
    assert!(Animal::is_food(&horse, &apple));
    assert!(!Animal::is_food(&zombie, &apple));
    assert!(!Animal::can_fall_in_love(&zombie));
}

#[test]
fn horse_persists_taming_temper_and_coat() {
    init_vanilla_registry();
    let horse = HorseEntity::new(&vanilla_entities::HORSE, 1, DVec3::ZERO, Weak::new());
    horse.set_tamed(true);
    horse.set_temper(42);
    horse.set_variant_and_markings(HorseVariant::Chestnut, 3);

    let loaded = HorseEntity::new(&vanilla_entities::HORSE, 2, DVec3::ZERO, Weak::new());
    round_trip(
        |nbt| horse.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.is_tamed());
    assert_eq!(loaded.temper(), 42);
    assert_eq!(loaded.variant(), HorseVariant::Chestnut);
    assert_eq!(loaded.markings(), 3);
}

#[test]
fn donkey_persists_its_chest() {
    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    donkey.set_chest(true);

    let loaded = DonkeyEntity::new(&vanilla_entities::DONKEY, 2, DVec3::ZERO, Weak::new());
    round_trip(
        |nbt| donkey.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.has_chest());
    assert_eq!(loaded.inventory_columns(), 5);
}

#[test]
fn skeleton_horse_persists_trap_state() {
    init_vanilla_registry();
    let horse = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        1,
        DVec3::ZERO,
        Weak::new(),
    );
    horse.set_trap(true);

    let loaded = SkeletonHorseEntity::new(
        &vanilla_entities::SKELETON_HORSE,
        2,
        DVec3::ZERO,
        Weak::new(),
    );
    round_trip(
        |nbt| horse.save_additional(nbt),
        |nbt| loaded.load_additional(nbt),
    );

    assert!(loaded.is_trap());
}

fn shared<T: Entity + 'static>(entity: T) -> SharedEntity {
    Arc::new(entity)
}

#[test]
fn chested_donkey_menu_has_saddle_armor_storage_and_player_slots() {
    use steel_utils::locks::IntoShared as _;

    use crate::player::player_inventory::PlayerInventory;

    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    donkey.set_chest(true);
    donkey.create_inventory();
    let storage = donkey.horse_base().inventory();
    let columns = donkey.inventory_columns();
    let mount = shared(donkey);

    let menu = menu::horse_inventory_menu(
        &PlayerInventory::new().into_shared(),
        1,
        &mount,
        storage,
        columns,
    );

    assert_eq!(menu.behavior().slot_count(), 2 + 15 + 36);
}

#[test]
fn equipment_container_equips_the_mount() {
    use crate::inventory::container::Container as _;
    use crate::inventory::equipment::EquipmentSlot;

    init_vanilla_registry();
    let horse = shared(HorseEntity::new(
        &vanilla_entities::HORSE,
        1,
        DVec3::ZERO,
        Weak::new(),
    ));
    let mut saddle = menu::MountEquipmentContainer::new(&horse, EquipmentSlot::Saddle);

    saddle.set_item(0, ItemStack::new(&vanilla_items::SADDLE));

    let Some(horse) = horse.as_abstract_horse() else {
        panic!("a horse is an abstract horse");
    };
    assert!(horse.is_saddled());
    assert_eq!(saddle.get_max_stack_size(), 1);
}

#[test]
fn resizing_the_inventory_invalidates_open_menus() {
    init_vanilla_registry();
    let donkey = DonkeyEntity::new(&vanilla_entities::DONKEY, 1, DVec3::ZERO, Weak::new());
    let before = donkey.horse_base().inventory();
    assert!(!donkey.horse_base().has_inventory_changed(&before));

    donkey.set_chest(true);
    donkey.create_inventory();

    assert!(donkey.horse_base().has_inventory_changed(&before));
}

mod skeleton_trap {
    use std::sync::Arc;

    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use steel_utils::{ChunkPos, Downcast as _, WorldAabb};

    use super::super::skeleton_trap::SkeletonTrapGoal;
    use super::super::{AbstractHorse as _, SkeletonHorseEntity};
    use crate::behavior::init_behaviors;
    use crate::entity::ai::goal::Goal as _;
    use crate::entity::{AgeableMob as _, SharedEntity, init_entities, next_entity_id};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn sprung_trap_spawns_four_skeleton_horsemen() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world("skeleton_trap");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let position = DVec3::new(8.5, 65.0, 8.5);
        let horse: SharedEntity = Arc::new(SkeletonHorseEntity::new(
            &vanilla_entities::SKELETON_HORSE,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::clone(&horse))
            .expect("trap horse should attach to the loaded chunk");
        let Some(trap) = horse.downcast_ref::<SkeletonHorseEntity>() else {
            panic!("the entity is a skeleton horse");
        };
        trap.set_trap(true);
        trap.set_age(-24000);

        let Some(mob) = horse.as_pathfinder_mob() else {
            panic!("skeleton horses are pathfinder mobs");
        };
        SkeletonTrapGoal::new().tick(mob, &horse);

        assert!(!trap.is_trap());
        assert!(trap.is_tamed());
        assert!(!trap.is_baby());
        let rider = horse
            .first_passenger()
            .unwrap_or_else(|| panic!("the trap horse gets a skeleton rider"));
        assert_eq!(rider.entity_type(), &vanilla_entities::SKELETON);

        let around = WorldAabb::new(0.0, 0.0, 0.0, 16.0, 128.0, 16.0);
        let entities = world.get_entities_in_aabb(&around);
        let count = |entity_type| {
            entities
                .iter()
                .filter(|entity| entity.entity_type() == entity_type)
                .count()
        };
        assert_eq!(count(&vanilla_entities::SKELETON_HORSE), 4);
        assert_eq!(count(&vanilla_entities::SKELETON), 4);
        assert_eq!(count(&vanilla_entities::LIGHTNING_BOLT), 1);
        for entity in &entities {
            if entity.entity_type() == &vanilla_entities::SKELETON {
                let Some(skeleton) = entity.as_mob() else {
                    panic!("skeletons are mobs");
                };
                assert!(skeleton.is_persistence_required());
                assert!(entity.vehicle().is_some(), "every skeleton rides a horse");
            }
        }
    }
}

mod llama {
    use std::sync::{Arc, Weak};

    use glam::DVec3;
    use steel_registry::entity_variant::LlamaVariant;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos};

    use super::super::chested::AbstractChestedHorse as _;
    use super::super::{AbstractHorse as _, HorseEntity, Llama, LlamaEntity, TraderLlamaEntity};
    use super::round_trip;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::{LlamaSpitEntity, PigEntity};
    use crate::entity::{
        Animal, Entity, Projectile as _, SharedEntity, init_entities, next_entity_id,
    };
    use crate::inventory::container::Container as _;
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    fn llama(id: i32) -> LlamaEntity {
        LlamaEntity::new(&vanilla_entities::LLAMA, id, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn llama_persists_strength_coat_and_chest() {
        init_vanilla_registry();
        let llama_entity = llama(1);
        llama_entity.set_strength(4);
        llama_entity.set_variant(LlamaVariant::Brown);
        llama_entity.set_chest(true);

        let loaded = llama(2);
        round_trip(
            |nbt| llama_entity.save_additional(nbt),
            |nbt| loaded.load_additional(nbt),
        );

        assert_eq!(loaded.strength(), 4);
        assert_eq!(loaded.variant(), LlamaVariant::Brown);
        assert!(loaded.has_chest());
        assert_eq!(loaded.inventory_columns(), 4);
        assert_eq!(loaded.horse_base().inventory().lock().items().len(), 12);
    }

    #[test]
    fn strength_is_clamped_like_vanilla() {
        init_vanilla_registry();
        let llama_entity = llama(1);
        llama_entity.set_strength(9);
        assert_eq!(llama_entity.strength(), 5);
        llama_entity.set_strength(0);
        assert_eq!(llama_entity.strength(), 1);
        assert_eq!(llama_entity.llama_strength(), Some(1));
    }

    #[test]
    fn llamas_mate_with_trader_llamas_but_not_horses() {
        init_vanilla_registry();
        let llama_entity = llama(1);
        let trader =
            TraderLlamaEntity::new(&vanilla_entities::TRADER_LLAMA, 2, DVec3::ZERO, Weak::new());
        let horse = HorseEntity::new(&vanilla_entities::HORSE, 3, DVec3::ZERO, Weak::new());
        for parent in [&llama_entity as &dyn Llama, &trader] {
            parent.set_tamed(true);
            parent.set_in_love(None);
        }
        horse.set_tamed(true);
        horse.set_in_love(None);

        assert!(Animal::can_mate(&llama_entity, &trader));
        assert!(!Animal::can_mate(&llama_entity, &llama_entity));
        assert!(!Animal::can_mate(&llama_entity, &horse));
    }

    #[test]
    fn caravan_links_head_and_tail() {
        init_vanilla_registry();
        let head: SharedEntity = Arc::new(llama(1));
        let tail: SharedEntity = Arc::new(llama(2));
        let (Some(head_llama), Some(tail_llama)) = (head.as_llama(), tail.as_llama()) else {
            panic!("both entities are llamas");
        };

        tail_llama.join_caravan(&tail, &head);
        assert!(tail_llama.in_caravan());
        assert!(head_llama.has_caravan_tail());
        assert!(
            tail_llama
                .caravan_head()
                .is_some_and(|following| following.id() == head.id())
        );

        tail_llama.leave_caravan();
        assert!(!tail_llama.in_caravan());
        assert!(!head_llama.has_caravan_tail());
    }

    #[test]
    fn trader_llama_despawns_when_its_delay_runs_out() {
        init_vanilla_registry();
        let trader =
            TraderLlamaEntity::new(&vanilla_entities::TRADER_LLAMA, 1, DVec3::ZERO, Weak::new());
        trader.set_despawn_delay(2);
        trader.llama_ai_step_extra();
        assert!(!trader.is_removed());
        trader.llama_ai_step_extra();
        assert!(trader.is_removed());

        let tamed =
            TraderLlamaEntity::new(&vanilla_entities::TRADER_LLAMA, 2, DVec3::ZERO, Weak::new());
        tamed.set_tamed(true);
        tamed.set_despawn_delay(1);
        tamed.llama_ai_step_extra();
        assert!(!tamed.is_removed());
    }

    #[test]
    fn spit_spawns_in_front_of_the_mouth() {
        let spawn = LlamaSpitEntity::spawn_position(DVec3::new(0.0, 64.0, 0.0), 0.9, 65.7, 0.0);
        assert!((spawn.x - 0.0).abs() < 1e-9);
        assert!((spawn.y - (65.7 - f64::from(0.1_f32))).abs() < 1e-9);
        assert!((spawn.z - 0.95).abs() < 1e-6);
    }

    #[test]
    fn spit_hurts_for_one_point_and_credits_the_llama() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world("llama_spit_hit");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let owner: SharedEntity = Arc::new(LlamaEntity::new(
            &vanilla_entities::LLAMA,
            next_entity_id(),
            DVec3::new(4.5, 65.0, 4.5),
            Arc::downgrade(world),
        ));
        let pig: SharedEntity = Arc::new(PigEntity::new(
            &vanilla_entities::PIG,
            next_entity_id(),
            DVec3::new(8.5, 65.0, 8.5),
            Arc::downgrade(world),
        ));
        for entity in [&owner, &pig] {
            world
                .try_add_entity(Arc::clone(entity))
                .expect("entity should attach to the loaded chunk");
        }
        let spit = Arc::new(LlamaSpitEntity::new(
            &vanilla_entities::LLAMA_SPIT,
            next_entity_id(),
            DVec3::new(8.5, 65.5, 8.5),
            Arc::downgrade(world),
        ));
        spit.set_owner_entity(Some(&owner));
        let Some(pig_living) = pig.as_living_entity() else {
            panic!("pigs are living entities");
        };
        let health = pig_living.get_health();

        Arc::clone(&spit).on_hit_entity(&pig, pig.position());

        assert!((health - pig_living.get_health() - 1.0).abs() < 1e-6);
        assert!(
            pig_living
                .last_hurt_by_mob()
                .is_some_and(|attacker| attacker.id() == owner.id())
        );
    }

    #[test]
    fn spit_inside_a_block_is_discarded() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world("llama_spit_block");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        world.set_block(
            BlockPos::new(8, 70, 8),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        );
        let spit: Arc<LlamaSpitEntity> = Arc::new(LlamaSpitEntity::new(
            &vanilla_entities::LLAMA_SPIT,
            next_entity_id(),
            DVec3::new(8.5, 70.4, 8.5),
            Arc::downgrade(world),
        ));
        world
            .try_add_entity(Arc::<LlamaSpitEntity>::clone(&spit))
            .expect("spit should attach to the loaded chunk");

        Arc::clone(&spit).tick();

        assert!(spit.is_removed());
    }
}
