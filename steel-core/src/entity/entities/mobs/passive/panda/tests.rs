use std::sync::Weak;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_entities, vanilla_items};

use super::{PandaEntity, PandaGene};
use crate::entity::{AgeableMob, Animal, Entity, EntityPose, Mob};

fn test_panda() -> PandaEntity {
    init_vanilla_registry();
    PandaEntity::new(&vanilla_entities::PANDA, 1, DVec3::ZERO, Weak::new())
}

#[test]
fn recessive_genes_only_show_in_pairs() {
    assert_eq!(
        PandaGene::variant_from_genes(PandaGene::Brown, PandaGene::Normal),
        PandaGene::Normal
    );
    assert_eq!(
        PandaGene::variant_from_genes(PandaGene::Brown, PandaGene::Brown),
        PandaGene::Brown
    );
    assert_eq!(
        PandaGene::variant_from_genes(PandaGene::Lazy, PandaGene::Weak),
        PandaGene::Lazy
    );
}

#[test]
fn random_gene_rolls_match_vanilla_weights() {
    let rolls: Vec<PandaGene> = (0..16).map(PandaGene::from_roll).collect();
    let count = |gene| rolls.iter().filter(|rolled| **rolled == gene).count();
    assert_eq!(count(PandaGene::Lazy), 1);
    assert_eq!(count(PandaGene::Worried), 1);
    assert_eq!(count(PandaGene::Playful), 1);
    assert_eq!(count(PandaGene::Aggressive), 1);
    assert_eq!(count(PandaGene::Weak), 5);
    assert_eq!(count(PandaGene::Brown), 2);
    assert_eq!(count(PandaGene::Normal), 5);
}

#[test]
fn genes_round_trip_through_ids_and_names() {
    for gene in PandaGene::ALL {
        assert_eq!(PandaGene::by_id(gene.id()), gene);
        assert_eq!(PandaGene::by_name(gene.serialized_name()), Some(gene));
    }
    assert_eq!(PandaGene::by_id(42), PandaGene::Normal);
}

#[test]
fn aggressive_gene_makes_the_panda_aggressive() {
    let panda = test_panda();
    assert!(!Mob::is_aggressive(&panda));
    panda.set_main_gene(PandaGene::Aggressive);
    assert!(Mob::is_aggressive(&panda));
    assert!(!panda.mob_can_be_leashed());
}

#[test]
fn flags_block_actions() {
    let panda = test_panda();
    assert!(panda.can_perform_action());
    panda.sit(true);
    assert!(panda.is_sitting());
    assert!(!panda.can_perform_action());
    panda.sit(false);
    panda.set_on_back(true);
    assert!(!panda.can_perform_action());
    panda.set_on_back(false);
    panda.eat(true);
    assert!(panda.is_eating());
    assert!(!panda.can_perform_action());
    panda.eat(false);
    panda.roll(true);
    assert!(!panda.can_perform_action());
}

#[test]
fn panda_eats_bamboo_and_picks_up_loot() {
    let panda = test_panda();
    assert!(panda.is_food(&ItemStack::new(&vanilla_items::BAMBOO)));
    assert!(!panda.is_food(&ItemStack::new(&vanilla_items::WHEAT)));
    assert!(panda.can_pick_up_loot());
}

#[test]
fn baby_panda_has_vanilla_dimensions() {
    let panda = test_panda();
    panda.set_age(panda.get_baby_start_age());
    assert!(AgeableMob::is_baby(&panda));
    let adult = vanilla_entities::PANDA.dimensions;
    let baby = panda.dimensions_for_pose(EntityPose::Standing);
    assert!((baby.width - adult.width * 0.5).abs() < 1e-6);
    assert!((baby.height - adult.height * 0.5).abs() < 1e-6);
    assert!((baby.eye_height - 0.281_25).abs() < 1e-6);
}

#[test]
fn cub_inherits_a_gene_from_each_parent() {
    let mother = test_panda();
    mother.set_main_gene(PandaGene::Lazy);
    mother.set_hidden_gene(PandaGene::Lazy);
    let father = test_panda();
    father.set_main_gene(PandaGene::Playful);
    father.set_hidden_gene(PandaGene::Playful);
    let mut mutated = 0;
    for _ in 0..200 {
        let cub = test_panda();
        cub.set_gene_from_parents(&mother, Some(&father));
        let genes = [cub.main_gene(), cub.hidden_gene()];
        if !(genes.contains(&PandaGene::Lazy) && genes.contains(&PandaGene::Playful)) {
            mutated += 1;
        }
    }
    // Each gene mutates 1 time in 32.
    assert!(mutated < 60, "{mutated} cubs lost a parent gene");
}
