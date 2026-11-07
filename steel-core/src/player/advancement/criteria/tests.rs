use super::*;
use steel_registry::init_vanilla_registry;

#[test]
fn every_vanilla_criterion_decodes() {
    init_vanilla_registry();
    let mut failures = Vec::new();
    for &advancement in REGISTRY.advancements.advancements() {
        for (criterion, data) in advancement.criteria() {
            if let Err(error) = serde_json::from_str::<Conditions>(data.conditions) {
                failures.push(format!("{} {criterion}: {error}", advancement.key()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

mod triggers {
    use std::sync::Arc;

    use steel_registry::item_stack::ItemStack;
    use steel_registry::{vanilla_items, vanilla_menu_types};
    use steel_utils::Identifier;
    use steel_utils::locks::IntoShared as _;

    use super::super::predicates::LocationPredicate;
    use super::super::{TICK, init_criteria};
    use crate::behavior::init_behaviors;
    use crate::entity::Entity as _;
    use crate::inventory::container::{Container as _, SimpleContainer};
    use crate::inventory::menu::{MenuBuilder, kinds::BasicKind};
    use crate::player::Player;
    use crate::test_support::{TestPlayerBuilder, fresh_test_world};

    fn setup() {
        steel_registry::init_vanilla_registry();
        init_behaviors();
        init_criteria();
    }

    fn criterion_done(player: &Player, advancement: &str, criterion: &str) -> bool {
        let key: Identifier = advancement.parse().expect("advancement id parses");
        let advancement = REGISTRY
            .advancements
            .value_by_key(&key)
            .expect("vanilla advancement exists");
        player
            .advancements
            .lock()
            .progress
            .get_progress(advancement)
            .is_some_and(|progress| progress.is_criterion_done(criterion))
    }

    use super::REGISTRY;

    #[test]
    fn inventory_changed_grants_on_join_and_on_change() {
        setup();
        let fixture = fresh_test_world("criteria_inventory");
        let player = TestPlayerBuilder::new(Arc::clone(&fixture.world), "Crafter", 1).build();

        player
            .inventory
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::DIRT));
        player.init_inventory_menu();
        assert!(!criterion_done(
            &player,
            "minecraft:story/root",
            "crafting_table"
        ));

        player
            .inventory
            .lock()
            .set_item(1, ItemStack::new(&vanilla_items::CRAFTING_TABLE));
        player.broadcast_inventory_changes_and_trigger();
        assert!(criterion_done(
            &player,
            "minecraft:story/root",
            "crafting_table"
        ));
    }

    #[test]
    fn inventory_changed_on_join_reports_items_already_held() {
        setup();
        let fixture = fresh_test_world("criteria_inventory_join");
        let player = TestPlayerBuilder::new(Arc::clone(&fixture.world), "Holder", 1).build();
        player
            .inventory
            .lock()
            .set_item(5, ItemStack::new(&vanilla_items::CRAFTING_TABLE));

        player.init_inventory_menu();

        assert!(criterion_done(
            &player,
            "minecraft:story/root",
            "crafting_table"
        ));
    }

    #[test]
    fn items_outside_the_player_inventory_do_not_count() {
        setup();
        let fixture = fresh_test_world("criteria_inventory_chest");
        let player = TestPlayerBuilder::new(Arc::clone(&fixture.world), "Looker", 1).build();
        player.init_inventory_menu();

        let chest = SimpleContainer::new(9).into_shared();
        chest
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::CRAFTING_TABLE));
        player.open_menu("Chest", move |context| {
            let mut builder =
                MenuBuilder::new(&vanilla_menu_types::GENERIC_9X1, context.container_id);
            builder.section(chest, 9);
            builder.player_inventory(&context.player.inventory);
            builder.build(BasicKind {})
        });

        assert!(!criterion_done(
            &player,
            "minecraft:story/root",
            "crafting_table"
        ));

        player
            .inventory
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::CRAFTING_TABLE));
        player.broadcast_inventory_changes_and_trigger();
        assert!(criterion_done(
            &player,
            "minecraft:story/root",
            "crafting_table"
        ));
    }

    #[test]
    fn tick_trigger_grants_unconditional_criteria() {
        setup();
        let fixture = fresh_test_world("criteria_tick");
        let player = TestPlayerBuilder::new(Arc::clone(&fixture.world), "Ticker", 1).build();
        assert!(!criterion_done(
            &player,
            "minecraft:recipes/decorations/crafting_table",
            "unlock_right_away"
        ));

        player.trigger_player_criteria(&TICK);

        assert!(criterion_done(
            &player,
            "minecraft:recipes/decorations/crafting_table",
            "unlock_right_away"
        ));
    }

    #[test]
    fn location_predicate_checks_the_dimension() {
        setup();
        let fixture = fresh_test_world("criteria_location");
        let world = &fixture.world;
        let player = TestPlayerBuilder::new(Arc::clone(world), "Walker", 1).build();
        let pos = player.position();

        let same: LocationPredicate =
            serde_json::from_value(serde_json::json!({ "dimension": world.key.to_string() }))
                .expect("predicate decodes");
        let other_key = if world.key.to_string() == "minecraft:the_end" {
            "minecraft:overworld"
        } else {
            "minecraft:the_end"
        };
        let other: LocationPredicate =
            serde_json::from_value(serde_json::json!({ "dimension": other_key }))
                .expect("predicate decodes");

        assert!(same.matches(world, pos.x, pos.y, pos.z));
        assert!(!other.matches(world, pos.x, pos.y, pos.z));
    }
}
