//! Vanilla `EmptyMapItem` and `MapItem`.

use std::ptr;
use std::sync::Arc;

use steel_macros::item_behavior;
use steel_math::trig;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::Direction;
use steel_registry::data_components::components::{MapDecorationEntry, MapDecorations, MapId};
use steel_registry::data_components::vanilla_components::{
    MAP_DECORATIONS, MAP_ID, MAP_POST_PROCESSING, MapPostProcessing,
};
use steel_registry::equipment::EquipmentSlotType;
use steel_registry::item_stack::ItemStack;
use steel_registry::map_color::{MapColor, MapColorBrightness};
use steel_registry::map_decoration_type::MapDecorationType;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{RegistryReference, sound_events, vanilla_blocks, vanilla_items};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, SectionPos};

use crate::behavior::context::UseOnContext;
use crate::behavior::{InteractionResult, InventoryTickContext, ItemBehavior, UseItemContext};
use crate::chunk::heightmap::HeightmapType;
use crate::entity::Entity as _;
use crate::fluid::state::fluid_state_to_block;
use crate::inventory::container::Container as _;
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::LevelReader as _;
use crate::world::World;
use crate::world::maps::{MAP_SIZE, MapItemSavedData, java_floor};

/// Vanilla `MapItem.create`: a filled map with fresh saved data.
pub fn create_map(
    world: &World,
    origin_x: i32,
    origin_z: i32,
    scale: u8,
    tracking_position: bool,
    unlimited_tracking: bool,
) -> ItemStack {
    let mut map = ItemStack::new(&vanilla_items::FILLED_MAP);
    apply_new_saved_data(
        world,
        &mut map,
        origin_x,
        origin_z,
        scale,
        tracking_position,
        unlimited_tracking,
    );
    map
}

/// Vanilla `MapItem.applyNewSavedData`: gives `map` fresh saved data under a new id.
pub fn apply_new_saved_data(
    world: &World,
    map: &mut ItemStack,
    origin_x: i32,
    origin_z: i32,
    scale: u8,
    tracking_position: bool,
    unlimited_tracking: bool,
) {
    let data = MapItemSavedData::create_fresh(
        f64::from(origin_x),
        f64::from(origin_z),
        scale,
        tracking_position,
        unlimited_tracking,
        world.key.clone(),
    );
    let id = world.free_map_id();
    world.set_map_data(id, data);
    map.set(MAP_ID, id);
}

/// Vanilla `MapItem.renderBiomePreviewMap`: outlines the watery biomes of an
/// exploration map before anyone has walked it.
pub fn render_biome_preview_map(world: &World, map: &ItemStack) {
    let Some(data) = map.get(MAP_ID).and_then(|id| world.map_data(*id)) else {
        return;
    };
    let mut data = data.lock();
    if data.dimension != world.key {
        return;
    }
    let scale = 1 << data.scale;
    let unscaled_start_x = data.center_x / scale - 64;
    let unscaled_start_z = data.center_z / scale - 64;
    let sample_y = world.sea_level();

    let mut watery = [false; MAP_SIZE * MAP_SIZE];
    for row in 0..MAP_SIZE {
        for column in 0..MAP_SIZE {
            let pos = BlockPos::new(
                (unscaled_start_x + column as i32) * scale,
                sample_y,
                (unscaled_start_z + row as i32) * scale,
            );
            watery[row * MAP_SIZE + column] = world
                .biome_at(pos)
                .is_some_and(|biome| biome.has_tag(&BiomeTag::WATER_ON_MAP_OUTLINES));
        }
    }
    let is_watery = |x: usize, z: usize| watery[z * MAP_SIZE + x];

    for mx in 1..MAP_SIZE - 1 {
        for mz in 1..MAP_SIZE - 1 {
            let mut water_count = 0;
            for dx in 0..3 {
                for dz in 0..3 {
                    if (dx != 1 || dz != 1) && is_watery(mx + dx - 1, mz + dz - 1) {
                        water_count += 1;
                    }
                }
            }

            let mut brightness = MapColorBrightness::Lowest;
            let mut color = MapColor::NONE;
            if is_watery(mx, mz) {
                color = MapColor::COLOR_ORANGE;
                if water_count > 7 && mz % 2 == 0 {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "vanilla casts the scaled sine to an int"
                    )]
                    let wave = (mx as i32 + (trig::sin(mz as f32) * 7.0) as i32) / 8 % 5;
                    match wave {
                        0 | 4 => brightness = MapColorBrightness::Low,
                        1 | 3 => brightness = MapColorBrightness::Normal,
                        2 => brightness = MapColorBrightness::High,
                        _ => {}
                    }
                } else if water_count > 7 {
                    color = MapColor::NONE;
                } else if water_count > 5 {
                    brightness = MapColorBrightness::Normal;
                } else if water_count > 1 {
                    brightness = MapColorBrightness::Low;
                }
            } else if water_count > 0 {
                color = MapColor::COLOR_BROWN;
                brightness = if water_count > 3 {
                    MapColorBrightness::Normal
                } else {
                    MapColorBrightness::Lowest
                };
            }

            if color != MapColor::NONE {
                data.set_color(mx, mz, color.packed_id(brightness));
            }
        }
    }
}

/// Vanilla `MapItemSavedData.addTargetDecoration`: marks `pos` on the map item itself.
pub fn add_target_decoration(
    map: &mut ItemStack,
    pos: BlockPos,
    key: &str,
    decoration_type: RegistryReference<MapDecorationType>,
) {
    let entry = MapDecorationEntry::new(
        decoration_type,
        f64::from(pos.x()),
        f64::from(pos.z()),
        180.0,
    );
    let decorations = map
        .get(MAP_DECORATIONS)
        .cloned()
        .unwrap_or_else(MapDecorations::empty)
        .with_decoration(key.to_owned(), entry);
    map.set(MAP_DECORATIONS, decorations);
}

/// Vanilla `EmptyMapItem`.
#[item_behavior]
pub struct EmptyMapItem;

impl ItemBehavior for EmptyMapItem {
    /// Vanilla `EmptyMapItem.use`.
    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let player = context.player;
        let has_infinite_materials = player.has_infinite_materials();
        let emptied = context.inv.with_item(|item| {
            item.consume(1, has_infinite_materials);
            player.award_stat(&vanilla_stat_types::ITEM_USED, item.item());
            item.is_empty()
        });
        context.world.play_sound_at(
            &sound_events::UI_CARTOGRAPHY_TABLE_TAKE_RESULT,
            SoundSource::Players,
            player.position(),
            1.0,
            1.0,
            None,
        );
        let block_pos = player.block_position();
        let map = create_map(context.world, block_pos.x(), block_pos.z(), 0, true, false);
        if emptied {
            // Vanilla `InteractionResult.heldItemTransformedTo`.
            context.inv.with_item(|item| *item = map);
        } else {
            let leftover = context.inv.with_inventory(|inventory| {
                let mut copy = map.clone();
                if inventory.add(&mut copy) {
                    ItemStack::empty()
                } else {
                    map
                }
            });
            if !leftover.is_empty() {
                let _ = player.drop_item(leftover, false, Prediction::ServerOnly);
            }
        }
        InteractionResult::Success
    }
}

/// Vanilla `MapItem`.
#[item_behavior]
pub struct MapItem;

impl MapItem {
    /// Vanilla `MapItem.update`: redraws a stripe of map pixels around the holder.
    #[expect(
        clippy::too_many_lines,
        reason = "mirrors the single vanilla MapItem.update method"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "pixel coordinates are bounds checked against the 128x128 image"
    )]
    pub fn update(world: &World, player: &Arc<Player>, data: &mut MapItemSavedData) {
        if world.key != data.dimension {
            return;
        }
        let scale = 1_i32 << data.scale;
        let center_x = data.center_x;
        let center_z = data.center_z;
        let position = player.position();
        let player_img_x = java_floor(position.x - f64::from(center_x)) / scale + 64;
        let player_img_y = java_floor(position.z - f64::from(center_z)) / scale + 64;
        let mut radius = 128 / scale;
        let has_ceiling = world.dimension_type.has_ceiling;
        if has_ceiling {
            radius /= 2;
        }

        let step = {
            let holding = data.holding_player(player);
            holding.step = holding.step.wrapping_add(1);
            holding.step
        };
        let min_y = world.get_min_y();
        let mut found_consecutive_changes = false;
        let mut banner_columns = Vec::new();

        for img_x in (player_img_x - radius + 1)..(player_img_x + radius) {
            if (img_x & 15) != (step & 15) && !found_consecutive_changes {
                continue;
            }
            found_consecutive_changes = false;
            let mut previous_average_area_height = 0.0;

            for img_y in (player_img_y - radius - 1)..(player_img_y + radius) {
                if img_x < 0 || img_y < -1 || img_x >= 128 || img_y >= 128 {
                    continue;
                }
                let distance_to_player_sqr =
                    (img_x - player_img_x).pow(2) + (img_y - player_img_y).pow(2);
                let dither_black = distance_to_player_sqr > (radius - 2) * (radius - 2);
                let area_min_x = (center_x / scale + img_x - 64) * scale;
                let area_min_z = (center_z / scale + img_y - 64) * scale;
                let chunk_pos = ChunkPos::new(
                    SectionPos::block_to_section_coord(area_min_x),
                    SectionPos::block_to_section_coord(area_min_z),
                );

                // Vanilla loads the chunk here; maps only draw chunks around their
                // holder, which the holder's own view keeps loaded.
                let check_banners = data.banners().next().is_some();
                let Some(sample) = world.chunk_map.with_full_chunk(chunk_pos, |chunk| {
                    let mut colors = ColorCount::default();
                    let mut water_depth = 0;
                    let mut average_area_height = 0.0;
                    if has_ceiling {
                        let mut ceiling_noise =
                            area_min_x.wrapping_add(area_min_z.wrapping_mul(231_871));
                        ceiling_noise = ceiling_noise
                            .wrapping_mul(ceiling_noise)
                            .wrapping_mul(31_287_121)
                            .wrapping_add(ceiling_noise.wrapping_mul(11));
                        if (ceiling_noise >> 20) & 1 == 0 {
                            colors.add(vanilla_blocks::DIRT.default_state().get_map_color(), 10);
                        } else {
                            colors.add(vanilla_blocks::STONE.default_state().get_map_color(), 100);
                        }
                        average_area_height = 100.0;
                    } else {
                        for dx in 0..scale {
                            for dz in 0..scale {
                                let x = area_min_x + dx;
                                let z = area_min_z + dz;
                                let mut column_y = chunk.get_height(
                                    HeightmapType::WorldSurface,
                                    (x & 15) as usize,
                                    (z & 15) as usize,
                                );
                                let mut state: BlockStateId;
                                if column_y <= min_y {
                                    state = vanilla_blocks::BEDROCK.default_state();
                                } else {
                                    let mut pos;
                                    loop {
                                        column_y -= 1;
                                        pos = BlockPos::new(x, column_y, z);
                                        state = chunk.get_block_state(pos);
                                        if state.get_map_color() != MapColor::NONE
                                            || column_y <= min_y
                                        {
                                            break;
                                        }
                                    }
                                    if column_y > min_y && !state.get_fluid_state().is_empty() {
                                        let mut solid_y = column_y - 1;
                                        loop {
                                            let below =
                                                chunk.get_block_state(BlockPos::new(x, solid_y, z));
                                            solid_y -= 1;
                                            water_depth += 1;
                                            if solid_y <= min_y
                                                || below.get_fluid_state().is_empty()
                                            {
                                                break;
                                            }
                                        }
                                        state = correct_state_for_fluid_block(world, state, pos);
                                    }
                                }
                                if check_banners {
                                    banner_columns.push((x, z));
                                }
                                average_area_height +=
                                    f64::from(column_y) / f64::from(scale * scale);
                                colors.add(state.get_map_color(), 1);
                            }
                        }
                    }
                    (colors.most_common(), water_depth, average_area_height)
                }) else {
                    continue;
                };
                // Banner block entities are read after the chunk is released.
                for &(x, z) in &banner_columns {
                    data.check_banners(world, x, z);
                }
                banner_columns.clear();
                let (color, water_depth, average_area_height) = sample;
                let water_depth = water_depth / (scale * scale);
                let parity = (img_x + img_y) & 1;
                let brightness = if color == MapColor::WATER {
                    let diff = f64::from(water_depth) * 0.1 + f64::from(parity) * 0.2;
                    if diff < 0.5 {
                        MapColorBrightness::High
                    } else if diff > 0.9 {
                        MapColorBrightness::Low
                    } else {
                        MapColorBrightness::Normal
                    }
                } else {
                    let diff = (average_area_height - previous_average_area_height) * 4.0
                        / f64::from(scale + 4)
                        + (f64::from(parity) - 0.5) * 0.4;
                    if diff > 0.6 {
                        MapColorBrightness::High
                    } else if diff < -0.6 {
                        MapColorBrightness::Low
                    } else {
                        MapColorBrightness::Normal
                    }
                };
                previous_average_area_height = average_area_height;
                if img_y >= 0
                    && distance_to_player_sqr < radius * radius
                    && (!dither_black || parity != 0)
                {
                    found_consecutive_changes |= data.update_color(
                        img_x as usize,
                        img_y as usize,
                        color.packed_id(brightness),
                    );
                }
            }
        }
    }
}

/// Vanilla `MapItem.getCorrectStateForFluidBlock`.
fn correct_state_for_fluid_block(
    world: &World,
    state: BlockStateId,
    pos: BlockPos,
) -> BlockStateId {
    let fluid_state = state.get_fluid_state();
    if !fluid_state.is_empty() && !world.is_face_sturdy(state, pos, Direction::Up) {
        fluid_state_to_block(fluid_state)
    } else {
        state
    }
}

/// Vanilla's `LinkedHashMultiset` of colors read through `Multisets.copyHighestCountFirst`:
/// the most common color, ties going to the one seen first.
#[derive(Default)]
struct ColorCount {
    counts: Vec<(MapColor, i32)>,
}

impl ColorCount {
    fn add(&mut self, color: MapColor, count: i32) {
        match self.counts.iter_mut().find(|(seen, _)| *seen == color) {
            Some((_, seen_count)) => *seen_count += count,
            None => self.counts.push((color, count)),
        }
    }

    fn most_common(&self) -> MapColor {
        let mut best: Option<(MapColor, i32)> = None;
        for &(color, count) in &self.counts {
            if best.is_none_or(|(_, best_count)| count > best_count) {
                best = Some((color, count));
            }
        }
        best.map_or(MapColor::NONE, |(color, _)| color)
    }
}

impl ItemBehavior for MapItem {
    /// Vanilla `MapItem.onCraftedPostProcess`: a scaled or locked result gets its own new map.
    fn on_crafted_post_process(&self, stack: &mut ItemStack, world: &World) {
        let Some(post_processing) = stack.get(MAP_POST_PROCESSING).copied() else {
            return;
        };
        stack.remove(MAP_POST_PROCESSING);
        let Some(original) = stack.get(MAP_ID).and_then(|id| world.map_data(*id)) else {
            return;
        };
        let data = match post_processing {
            MapPostProcessing::Lock => original.lock().locked(),
            MapPostProcessing::Scale => original.lock().scaled(),
        };
        let id = world.free_map_id();
        world.set_map_data(id, data);
        stack.set(MAP_ID, id);
    }

    /// Using a map on a banner marks the banner on the map, or unmarks it.
    fn use_on(&self, context: &mut UseOnContext) -> InteractionResult {
        let pos = context.hit_result.block_pos;
        if !context
            .world
            .get_block_state(pos)
            .get_block()
            .has_tag(&BlockTag::BANNERS)
        {
            return InteractionResult::Pass;
        }
        let data = context
            .inv
            .with_item(|item| item.get(MAP_ID).copied())
            .and_then(|map_id| context.world.map_data(map_id));
        if let Some(data) = data
            && !data.lock().toggle_banner(context.world, pos)
        {
            return InteractionResult::Fail;
        }
        InteractionResult::Success
    }

    /// Vanilla `MapItem.inventoryTick`.
    fn inventory_tick(&self, context: &mut InventoryTickContext<'_>) {
        let Some((Some(map_id), static_decorations)) = context.with_item(|stack| {
            (
                stack.get(MAP_ID).copied(),
                stack.get(MAP_DECORATIONS).cloned(),
            )
        }) else {
            return;
        };
        let Some(data) = context.world.map_data(map_id) else {
            return;
        };
        let Some(player) = carrying_player(context) else {
            return;
        };
        let mut data = data.lock();
        data.tick_carried_by(&player, Some(map_id), static_decorations.as_ref(), None);
        if !data.locked
            && context
                .slot
                .is_some_and(|slot| slot.slot_type() == EquipmentSlotType::Hand)
        {
            Self::update(context.world, &player, &mut data);
        }
    }
}

/// The ticking owner as the shared player handle vanilla's map holders keep.
fn carrying_player(context: &InventoryTickContext<'_>) -> Option<Arc<Player>> {
    let player = context.owner.as_player()?;
    context
        .world
        .players
        .get_by_entity_id(player.id())
        .filter(|shared| ptr::eq(Arc::as_ptr(shared), player))
}

/// Vanilla `ServerPlayer.synchronizeSpecialItemUpdates` for every carried map.
pub(crate) fn synchronize_map_updates(player: &Arc<Player>) {
    let map_ids: Vec<MapId> = player
        .inventory
        .lock()
        .items()
        .iter()
        .filter(|stack| !stack.is_empty())
        .filter_map(|stack| stack.get(MAP_ID).copied())
        .collect();
    if map_ids.is_empty() {
        return;
    }
    let world = player.get_world();
    for id in map_ids {
        let Some(data) = world.map_data(id) else {
            continue;
        };
        let packet = data.lock().update_packet(id, player);
        if let Some(packet) = packet {
            player.send_packet(packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use steel_registry::blocks::properties::{Axis, BlockStateProperties};
    use steel_registry::data_components::vanilla_components::CUSTOM_NAME;
    use steel_registry::{RegistryReference, init_vanilla_registry, vanilla_map_decoration_types};
    use steel_utils::Identifier;
    use steel_utils::types::{InteractionHand, UpdateFlags};
    use text_components::TextComponent;

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::test_support::{TestPlayerBuilder, fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn map_colors_follow_the_block_state() {
        init_vanilla_registry();
        let upright = vanilla_blocks::OAK_LOG.default_state();
        let sideways = upright.set_value(&BlockStateProperties::AXIS, Axis::X);
        assert_eq!(upright.get_map_color(), MapColor::from_id(13));
        assert_eq!(sideways.get_map_color(), MapColor::from_id(34));
        assert_eq!(
            vanilla_blocks::WATER.default_state().get_map_color(),
            MapColor::WATER
        );
        assert_eq!(
            vanilla_blocks::AIR.default_state().get_map_color(),
            MapColor::NONE
        );
    }

    #[test]
    fn fresh_maps_snap_to_the_scale_grid() {
        let dimension = Identifier::new_static("minecraft", "overworld");
        let origin = MapItemSavedData::create_fresh(0.0, 0.0, 0, true, false, dimension.clone());
        assert_eq!((origin.center_x, origin.center_z), (0, 0));
        let east = MapItemSavedData::create_fresh(100.0, -65.0, 0, true, false, dimension);
        assert_eq!((east.center_x, east.center_z), (128, -128));
        let zoomed = east.scaled();
        assert_eq!(
            (zoomed.scale, zoomed.center_x, zoomed.center_z),
            (1, 64, -192)
        );
        let locked = east.locked();
        assert!(locked.locked && !east.locked);
    }

    #[test]
    fn empty_map_turns_into_a_filled_map_with_a_new_id() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("empty_map_use");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let player = TestPlayerBuilder::new(Arc::clone(world), "MapMaker", 1).build();
        player
            .base()
            .set_position_local(DVec3::new(100.5, 64.0, 3.5));
        player.inventory.lock().set_item_in_hand(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::MAP),
        );

        let mut context = UseItemContext::new(
            &player,
            InteractionHand::MainHand,
            world,
            Arc::clone(&player.inventory),
        );
        assert_eq!(
            EmptyMapItem.use_item(&mut context),
            InteractionResult::Success
        );

        let held = player
            .inventory
            .lock()
            .get_item_in_hand(InteractionHand::MainHand)
            .clone();
        assert!(held.is(&vanilla_items::FILLED_MAP));
        let id = *held.get(MAP_ID).expect("a filled map has an id");
        assert_eq!(id, MapId::new(0));
        let data = world.map_data(id).expect("the new map has data");
        let data = data.lock();
        assert_eq!((data.center_x, data.center_z, data.scale), (128, 0, 0));
        assert_eq!(world.free_map_id(), MapId::new(1));
    }

    #[test]
    fn holding_a_map_draws_the_terrain_and_marks_the_holder() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("map_update");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        assert!(world.set_block(
            BlockPos::new(0, 64, 0),
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "Cartographer", 1).build();
        player.base().set_position_local(DVec3::new(0.5, 65.0, 0.5));
        let map = create_map(world, 0, 0, 0, true, false);
        let id = *map.get(MAP_ID).expect("map id");
        player
            .inventory
            .lock()
            .set_item_in_hand(InteractionHand::MainHand, map);
        let data = world.map_data(id).expect("map data");
        let mut data = data.lock();

        data.tick_carried_by(&player, Some(id), None, None);
        for _ in 0..16 {
            MapItem::update(world, &player, &mut data);
        }
        assert_eq!(
            data.color(64, 64),
            MapColor::from_id(11).packed_id(MapColorBrightness::High)
        );
        assert!(data.is_dirty());

        let packet = data.update_packet(id, &player).expect("first sync");
        let patch = packet.color_patch.expect("the first sync sends the image");
        assert_eq!((patch.width, patch.height), (128, 128));
        let decorations = packet.decorations.expect("the first sync sends markers");
        assert_eq!(decorations.len(), 1);
        assert_eq!(
            decorations[0].decoration_type,
            RegistryReference::new(&vanilla_map_decoration_types::PLAYER)
        );
        assert_eq!((decorations[0].x, decorations[0].y), (1, 1));
        assert!(data.update_packet(id, &player).is_none());
    }

    #[test]
    fn maps_mark_banners_and_drop_them_when_the_banner_goes() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("map_banners");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let pos = BlockPos::new(3, 64, 5);
        assert!(world.set_block(
            pos,
            vanilla_blocks::RED_BANNER.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let mut named = ItemStack::new(&vanilla_items::RED_BANNER);
        named.set(CUSTOM_NAME, TextComponent::from("Base".to_string()));
        world
            .get_block_entity(pos)
            .expect("banner entity")
            .apply_components_from_item_stack(&named);

        let mut data = MapItemSavedData::create_fresh(0.0, 0.0, 0, true, false, world.key.clone());
        assert!(data.toggle_banner(world, pos));
        let marker = data.decorations().next().expect("a banner marker");
        assert_eq!(
            marker.decoration_type,
            RegistryReference::new(&vanilla_map_decoration_types::BANNER_RED)
        );
        assert_eq!(marker.name, Some(TextComponent::from("Base".to_string())));
        assert_eq!((marker.x, marker.y, marker.rot), (7, 11, 8));

        let reloaded = MapItemSavedData::from_nbt_bytes(&data.to_nbt_bytes()).expect("map NBT");
        assert_eq!(reloaded.banners().count(), 1);
        assert_eq!(reloaded.decorations().count(), 1);

        assert!(data.toggle_banner(world, pos), "a second use unmarks it");
        assert_eq!(data.decorations().count(), 0);
        assert!(data.toggle_banner(world, pos));

        assert!(world.set_block(
            pos,
            vanilla_blocks::AIR.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        data.check_banners(world.as_ref(), pos.x(), pos.z());
        assert_eq!(data.banners().count(), 0);
        assert_eq!(data.decorations().count(), 0);
        assert!(!data.toggle_banner(world, pos), "no banner to mark");
    }
}
