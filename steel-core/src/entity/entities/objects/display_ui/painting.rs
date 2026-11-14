//! Vanilla `Painting`.

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::seq::IndexedRandom as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_math::DEGREE_90;
use steel_registry::data_components::vanilla_components::{CUSTOM_NAME, PAINTING_VARIANT};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::painting_variant::PaintingVariantRef;
use steel_registry::vanilla_entity_data::PaintingEntityData;
use steel_registry::vanilla_game_rules::ENTITY_DROPS;
use steel_registry::vanilla_painting_variant_tags::PaintingVariantTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, RegistryReference, TaggedRegistryExt as _, sound_events,
    vanilla_entities, vanilla_items,
};
use steel_utils::axis::Axis;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, Direction, DowncastType, DowncastTypeKey, Identifier, WorldAabb};

use crate::entity::block_attached_entity::{BlockAttachedEntity, BlockAttachedEntityBase};
use crate::entity::damage::DamageSource;
use crate::entity::entities::LightningBoltEntity;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityBaseState, EntityMoveError, EntitySyncedData,
    HangingEntity, SharedEntity,
};
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::world::{Explosion, World};

/// Vanilla `Painting.DEPTH`.
const DEPTH: f64 = 0.0625;

/// Vanilla `Painting`.
#[entity_behavior(class = "Painting")]
pub struct PaintingEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<PaintingEntityData>,
    block_attached_entity_base: BlockAttachedEntityBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PaintingEntity`.
unsafe impl DowncastType for PaintingEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/painting");
}

impl PaintingEntity {
    /// Vanilla `Painting(EntityType, Level)`, placed where the generic entity factory puts it.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        let block_pos = BlockPos::containing(position.x, position.y, position.z);
        let painting = Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(PaintingEntityData::new()),
            block_attached_entity_base: BlockAttachedEntityBase::new(block_pos),
        };
        painting.set_direction(Direction::South);
        painting
    }

    /// Vanilla `Painting(level, blockPos, direction, variant)`.
    #[must_use]
    pub fn new_attached(
        id: i32,
        block_pos: BlockPos,
        direction: Direction,
        variant: PaintingVariantRef,
        world: Weak<World>,
    ) -> Self {
        let entity_type = &vanilla_entities::PAINTING;
        let painting = Self {
            base: EntityBase::new_with_state(
                id,
                EntityBaseState::new_with_bounding_box(
                    block_pos.0.as_dvec3() + DVec3::splat(0.5),
                    entity_type.dimensions,
                    WorldAabb::of_size(block_pos.0.as_dvec3(), 0.0, 0.0, 0.0),
                ),
                world,
            ),
            entity_type,
            entity_data: SyncMutex::new(PaintingEntityData::new()),
            block_attached_entity_base: BlockAttachedEntityBase::new(block_pos),
        };
        painting.set_variant(variant);
        painting.set_direction(direction);
        painting
    }

    /// Creates a painting from persistent entity data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        let position = load.position;
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(PaintingEntityData::new()),
            block_attached_entity_base: BlockAttachedEntityBase::new(BlockPos::containing(
                position.x, position.y, position.z,
            )),
        }
    }

    /// Vanilla `Painting.create`: the largest placeable variant that fits on the wall,
    /// picked at random among those of that size.
    #[must_use]
    pub fn create(
        world: &Arc<World>,
        id: i32,
        block_pos: BlockPos,
        direction: Direction,
    ) -> Option<Self> {
        let mut potential_variants: Vec<PaintingVariantRef> = REGISTRY
            .painting_variants
            .iter_tag(&PaintingVariantTag::PLACEABLE)
            .collect();
        let first = *potential_variants.first()?;
        let candidate = Self::new_attached(id, block_pos, direction, first, Arc::downgrade(world));
        potential_variants.retain(|variant| {
            candidate.set_variant(variant);
            candidate.survives()
        });
        let largest_area = potential_variants
            .iter()
            .map(|variant| variant_area(variant))
            .max()?;
        potential_variants.retain(|variant| variant_area(variant) >= largest_area);
        let selected = potential_variants.choose(&mut rand::rng())?;
        candidate.set_variant(selected);
        candidate.set_direction(direction);
        Some(candidate)
    }

    /// The painting's variant.
    #[must_use]
    pub fn variant(&self) -> PaintingVariantRef {
        self.entity_data.lock().painting_variant.get().value()
    }

    /// Vanilla `setVariant`; vanilla's synced data callback then recalculates the box.
    fn set_variant(&self, variant: PaintingVariantRef) {
        self.entity_data
            .lock()
            .painting_variant
            .set(RegistryReference::new(variant));
        self.recalculate_bounding_box_or_warn();
    }

    /// Vanilla `HangingEntity.setDirection`.
    fn set_direction(&self, direction: Direction) {
        debug_assert!(direction.is_horizontal());
        self.entity_data
            .lock()
            .hanging_entity
            .direction
            .set(direction);
        self.base.set_rotation((
            direction.get_2d_data_value() as f32 * DEGREE_90,
            self.rotation().1,
        ));
        self.base.set_old_rotation_to_current();
        self.recalculate_bounding_box_or_warn();
    }

    fn recalculate_bounding_box_or_warn(&self) {
        if let Err(error) = self.recalculate_bounding_box() {
            log::warn!(
                "failed to commit painting {} bounding box: {error}",
                self.base.id()
            );
        }
    }

    /// Vanilla `Painting.calculateBoundingBox`.
    fn painting_bounding_box(
        block_pos: BlockPos,
        direction: Direction,
        variant: PaintingVariantRef,
    ) -> WorldAabb {
        let width = variant.value().width;
        let height = variant.value().height;
        let attached_to_wall = block_pos.0.as_dvec3() + DVec3::splat(0.5)
            - direction.offset_vec().as_dvec3() * 0.46875;
        let left = direction
            .rotate_y_counter_clockwise()
            .offset_vec()
            .as_dvec3();
        let position = attached_to_wall
            + left * offset_for_painting_size(width)
            + DVec3::Y * offset_for_painting_size(height);
        let axis = direction.axis();
        let x_size = if axis == Axis::X {
            DEPTH
        } else {
            f64::from(width)
        };
        let z_size = if axis == Axis::Z {
            DEPTH
        } else {
            f64::from(width)
        };
        WorldAabb::of_size(position, x_size, f64::from(height), z_size)
    }
}

/// Vanilla `Painting.variantArea`.
const fn variant_area(variant: PaintingVariantRef) -> i32 {
    variant.value().width * variant.value().height
}

/// Vanilla `Painting.offsetForPaintingSize`: even sizes center on a block edge.
const fn offset_for_painting_size(size: i32) -> f64 {
    if size % 2 == 0 { 0.5 } else { 0.0 }
}

impl HangingEntity for PaintingEntity {
    fn direction(&self) -> Direction {
        *self.entity_data.lock().hanging_entity.direction.get()
    }
}

impl BlockAttachedEntity for PaintingEntity {
    fn block_attached_entity_base(&self) -> &BlockAttachedEntityBase {
        &self.block_attached_entity_base
    }

    fn survives(&self) -> bool {
        self.survives_hanging_entity()
    }

    /// Vanilla `Painting.dropItem`.
    fn drop_item(&self, caused_by: Option<&dyn Entity>) {
        let Some(world) = self.level() else {
            return;
        };
        if !world.get_game_rule(&ENTITY_DROPS) {
            return;
        }
        self.play_sound(&sound_events::ENTITY_PAINTING_BREAK, 1.0, 1.0);
        if caused_by
            .and_then(Entity::as_player)
            .is_some_and(Player::has_infinite_materials)
        {
            return;
        }
        let mut item = ItemStack::new(&vanilla_items::PAINTING);
        if let Some(name) = self.custom_name() {
            item.set(CUSTOM_NAME, name);
        }
        self.spawn_in_front(item);
    }

    /// Vanilla `HangingEntity.recalculateBoundingBox`.
    fn recalculate_bounding_box(&self) -> Result<(), EntityMoveError> {
        let aabb = Self::painting_bounding_box(
            self.block_attached_entity_base.pos(),
            self.direction(),
            self.variant(),
        );
        self.base.try_set_position(aabb.center())?;
        self.base.set_bounding_box(aabb);
        Ok(())
    }
}

impl Entity for PaintingEntity {
    /// Vanilla `BlockAttachedEntity.thunderHit` ignores lightning.
    fn thunder_hit(&self, _world: &World, _lightning: &LightningBoltEntity) {}

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `Painting.getAddEntityPacket`: the facing as its 3D data value.
    fn spawn_data(&self) -> i32 {
        self.direction().get_3d_data_value()
    }

    /// Vanilla `Painting.getAddEntityPacket`: the painting's block position.
    fn spawn_position(&self) -> DVec3 {
        self.block_attached_entity_base.pos().0.as_dvec3()
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn tick(self: Arc<Self>) {
        self.tick_block_attached_entity();
    }

    fn is_pickable(&self) -> bool {
        self.is_pickable_block_attached_entity()
    }

    fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
        self.ignore_explosion_block_attached_entity(explosion)
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.hurt_block_attached_entity(world, source, amount)
    }

    fn kill(&self, _world: &World) {
        self.kill_attributed(None);
    }

    fn skip_attack_interaction(&self, source: &SharedEntity) -> bool {
        self.skip_attack_interaction_block_attached_entity(source)
    }

    fn push_impulse(&self, impulse: DVec3) {
        self.push_impulse_block_attached_entity(impulse);
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        self.move_entity_block_attached_entity(mover_type, delta)
    }

    fn refresh_dimensions(&self) {
        self.refresh_dimensions_block_attached_entity();
    }

    fn try_set_position(&self, pos: DVec3) -> Result<(), EntityMoveError> {
        self.try_set_position_block_attached_entity(pos)
    }

    /// Vanilla `Painting.snapTo` keeps the rotation of the painting's direction.
    fn snap_to(&self, position: DVec3, _yaw: f32, _pitch: f32) {
        if let Err(error) = self.try_set_position(position) {
            panic!(
                "failed to commit painting {} snap position: {error}",
                self.id()
            );
        }
        self.set_old_position_to_current();
    }

    /// Vanilla `Painting.applyImplicitComponents`.
    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(PAINTING_VARIANT)
            && let Some(variant) = variant.variant().as_reference()
        {
            self.set_variant(variant);
        }
    }

    /// Vanilla `Painting.addAdditionalSaveData`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        nbt.insert("facing", self.direction().get_2d_data_value() as i8);
        self.save_block_attached_entity(nbt);
        nbt.insert("variant", self.variant().key.to_string());
    }

    /// Vanilla `Painting.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let direction = nbt.byte("facing").map_or(Direction::South, |facing| {
            Direction::from_2d_data_value(i32::from(facing))
        });
        self.load_block_attached_entity(nbt);
        self.set_direction(direction);
        if let Some(variant) = nbt
            .string("variant")
            .and_then(|variant| variant.to_str().parse::<Identifier>().ok())
            .and_then(|key| REGISTRY.painting_variants.by_key(&key))
        {
            self.set_variant(variant);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::items::item::BlockHitResult;
    use steel_registry::{
        init_vanilla_registry, vanilla_blocks, vanilla_damage_types, vanilla_painting_variants,
    };
    use steel_utils::types::{GameType, InteractionHand, UpdateFlags};
    use steel_utils::{ChunkPos, Downcast as _};

    use super::*;
    use crate::behavior::items::{HangingEntityItem, ItemFrameItem};
    use crate::behavior::{InteractionResult, ItemBehavior, UseOnContext, init_behaviors};
    use crate::entity::entities::ItemEntity;
    use crate::entity::next_entity_id;
    use crate::player::ResetReason;
    use crate::test_support::{
        TestPlayerBuilder, dropped_items, fresh_test_world, insert_ready_full_chunk,
    };

    /// A stone wall facing south, `width` blocks wide and `height` tall, with its
    /// lower west corner at `corner`.
    fn build_wall(world: &Arc<World>, corner: BlockPos, width: i32, height: i32) {
        for x in 0..width {
            for y in 0..height {
                assert!(world.set_block(
                    corner.offset(x, y, 0),
                    vanilla_blocks::STONE.default_state(),
                    UpdateFlags::UPDATE_ALL,
                ));
            }
        }
    }

    fn paintings_near(world: &World, pos: BlockPos) -> Vec<SharedEntity> {
        world.get_entities_in_aabb_matching(
            &WorldAabb::of_size(pos.0.as_dvec3(), 9.0, 9.0, 9.0),
            |entity| entity.downcast_ref::<PaintingEntity>().is_some(),
        )
    }

    fn hang(
        item: &dyn ItemBehavior,
        player: &Arc<Player>,
        world: &Arc<World>,
        wall: BlockPos,
    ) -> InteractionResult {
        let mut context = UseOnContext::new(
            player,
            InteractionHand::MainHand,
            BlockHitResult {
                location: wall.0.as_dvec3() + DVec3::splat(0.5),
                direction: Direction::South,
                block_pos: wall,
                miss: false,
                inside: false,
                world_border_hit: false,
            },
            world,
            Arc::clone(&player.inventory),
        );
        item.use_on(&mut context)
    }

    #[test]
    fn paintings_pick_the_largest_variant_that_fits() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("painting_lifecycle");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        // A wall one block wide and tall only fits 1x1 paintings.
        let small_wall = BlockPos::new(1, 64, 2);
        build_wall(world, small_wall, 1, 1);
        // A wide wall fits the 4x4 ones.
        let wall = BlockPos::new(4, 62, 2);
        build_wall(world, wall, 8, 8);
        let player = TestPlayerBuilder::new(Arc::clone(world), "Painter", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        player.inventory.lock().set_item_in_hand(
            InteractionHand::MainHand,
            ItemStack::with_count(&vanilla_items::PAINTING, 3),
        );
        let painting_item = HangingEntityItem::new(&vanilla_entities::PAINTING);

        assert_eq!(
            hang(&painting_item, &player, world, small_wall),
            InteractionResult::Success
        );
        let small = paintings_near(world, small_wall.south());
        assert_eq!(small.len(), 1);
        let small = small[0]
            .downcast_ref::<PaintingEntity>()
            .expect("a painting");
        assert_eq!(
            (
                small.variant().value().width,
                small.variant().value().height
            ),
            (1, 1)
        );
        assert_eq!(small.direction(), Direction::South);
        assert_eq!(small.rotation().0, 0.0);

        let center = wall.offset(4, 4, 0);
        assert_eq!(
            hang(&painting_item, &player, world, center),
            InteractionResult::Success
        );
        let large = paintings_near(world, center.south());
        let large = large
            .iter()
            .filter_map(|entity| entity.downcast_ref::<PaintingEntity>())
            .find(|painting| painting.block_attached_entity_base.pos() == center.south())
            .expect("the large painting");
        assert_eq!(
            (
                large.variant().value().width,
                large.variant().value().height
            ),
            (4, 4)
        );
        // A 4x4 painting reaches one block west and down of its block, and two east and up.
        let aabb = large.bounding_box();
        assert_eq!(
            (aabb.min_x(), aabb.min_y(), aabb.max_x(), aabb.max_y()),
            (7.0, 65.0, 11.0, 69.0)
        );
        assert_eq!(aabb.min_z(), 3.0);
        assert!((aabb.max_z() - (3.0 + DEPTH)).abs() < 1.0E-9);
        assert_eq!(
            player
                .inventory
                .lock()
                .get_item_in_hand(InteractionHand::MainHand)
                .count(),
            1
        );

        // Neither another painting nor an item frame facing the same way fits on top.
        assert_eq!(
            hang(&painting_item, &player, world, center),
            InteractionResult::Consume
        );
        player.inventory.lock().set_item_in_hand(
            InteractionHand::MainHand,
            ItemStack::new(&vanilla_items::ITEM_FRAME),
        );
        assert_eq!(
            hang(
                &ItemFrameItem::new(&vanilla_entities::ITEM_FRAME),
                &player,
                world,
                center
            ),
            InteractionResult::Consume
        );
    }

    #[test]
    fn breaking_a_painting_drops_it_unless_a_creative_player_broke_it() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("painting_drops");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let player = TestPlayerBuilder::new(Arc::clone(world), "Breaker", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        let source = DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK)
            .with_causing_entity(Arc::clone(&player) as SharedEntity);
        let hang_at = |wall: BlockPos| {
            build_wall(world, wall, 1, 1);
            player.inventory.lock().set_item_in_hand(
                InteractionHand::MainHand,
                ItemStack::new(&vanilla_items::PAINTING),
            );
            let item = HangingEntityItem::new(&vanilla_entities::PAINTING);
            assert_eq!(
                hang(&item, &player, world, wall),
                InteractionResult::Success
            );
            Arc::clone(&paintings_near(world, wall.south())[0])
        };
        let drops_at = |pos: DVec3| -> Vec<ItemStack> {
            dropped_items(world, pos)
                .iter()
                .filter_map(|entity| {
                    entity
                        .downcast_ref::<ItemEntity>()
                        .map(ItemEntity::get_item)
                })
                .collect()
        };

        let painting = hang_at(BlockPos::new(2, 64, 2));
        assert!(painting.hurt(world, &source, 1.0));
        assert!(painting.is_removed());
        let drops = drops_at(painting.position());
        assert_eq!(drops.len(), 1);
        assert!(drops[0].is(&vanilla_items::PAINTING));

        player.restore_game_modes(GameType::Creative, None);
        player
            .abilities
            .lock()
            .update_for_game_mode(GameType::Creative);
        let painting = hang_at(BlockPos::new(10, 64, 10));
        assert!(painting.hurt(world, &source, 1.0));
        assert!(painting.is_removed());
        assert_eq!(drops_at(painting.position()), Vec::<ItemStack>::new());
    }

    #[test]
    fn paintings_fall_off_without_their_wall_and_round_trip_through_nbt() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("painting_support");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let wall = BlockPos::new(4, 64, 4);
        build_wall(world, wall, 2, 1);
        let painting = PaintingEntity::new_attached(
            next_entity_id(),
            wall.east().relative(Direction::North),
            Direction::North,
            &vanilla_painting_variants::COURBET,
            Arc::downgrade(world),
        );
        assert!(painting.survives());
        assert_eq!(painting.rotation().0, 2.0 * DEGREE_90);

        let mut nbt = NbtCompound::new();
        painting.save_additional(&mut nbt);
        assert_eq!(nbt.byte("facing"), Some(2));
        assert_eq!(
            nbt.string("variant").map(ToString::to_string).as_deref(),
            Some("minecraft:courbet")
        );
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes)).expect("nbt");
        let loaded = PaintingEntity::new(
            &vanilla_entities::PAINTING,
            next_entity_id(),
            painting.position(),
            Arc::downgrade(world),
        );
        loaded.load_additional((&borrowed).into());
        assert_eq!(loaded.direction(), Direction::North);
        assert_eq!(loaded.variant().key, painting.variant().key);
        assert_eq!(loaded.bounding_box(), painting.bounding_box());

        // Courbet is two blocks wide: losing either block behind it breaks it.
        assert!(world.set_block(
            wall,
            vanilla_blocks::AIR.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        assert!(!painting.survives());
    }
}
