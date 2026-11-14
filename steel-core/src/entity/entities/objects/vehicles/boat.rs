//! Boats, rafts and their chest variants (vanilla `Boat`, `Raft`, `ChestBoat`
//! and `ChestRaft`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::{AbstractBoatEntityData, VehicleEntityData};
use steel_registry::vanilla_game_events;
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, Direction, DowncastType, DowncastTypeKey, WorldAabb};

use super::abstract_boat::{AbstractBoat, BoatCore, boat_gravity, tick_boat};
use super::vehicle_container::{ContainerVehicleMenuKind, VehicleContainer};
use super::vehicle_entity::VehicleEntity;
use crate::behavior::InteractionResult;
use crate::behavior::blocks::anger_nearby_piglins;
use crate::entity::damage::DamageSource;
use crate::entity::leash::{LeashData, Leashable};
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, RemovalReason,
    SharedEntity, reset_forward_direction_of_relative_portal_position,
};
use crate::inventory::lock::ContainerRef;
use crate::inventory::menu::kinds::chest_with_kind;
use crate::player::Player;
use crate::portal::portal_shape::PortalShape;
use crate::world::{Explosion, World};

/// Slots in a chest boat (vanilla `AbstractChestBoat.CONTAINER_SIZE`).
pub const CHEST_BOAT_SLOTS: usize = 27;

/// Vanilla `Boat.rideHeight`.
fn boat_ride_height(dimensions: &EntityDimensions) -> f64 {
    f64::from(dimensions.height / 3.0)
}

/// Vanilla `Raft.rideHeight`.
fn raft_ride_height(dimensions: &EntityDimensions) -> f64 {
    f64::from(dimensions.height * 0.888_888_9)
}

/// A wooden boat.
#[entity_behavior(class = "Boat")]
pub struct BoatEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<AbstractBoatEntityData>,
    boat: BoatCore,
    #[json_arg(vanilla_items, json = "drop_item")]
    drop_item: ItemRef,
}

/// A bamboo raft.
#[entity_behavior(class = "Raft")]
pub struct RaftEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<AbstractBoatEntityData>,
    boat: BoatCore,
    #[json_arg(vanilla_items, json = "drop_item")]
    drop_item: ItemRef,
}

/// A wooden boat carrying a chest.
#[entity_behavior(class = "ChestBoat")]
pub struct ChestBoatEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<AbstractBoatEntityData>,
    boat: BoatCore,
    container: VehicleContainer,
    #[json_arg(vanilla_items, json = "drop_item")]
    drop_item: ItemRef,
}

/// A bamboo raft carrying a chest.
#[entity_behavior(class = "ChestRaft")]
pub struct ChestRaftEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<AbstractBoatEntityData>,
    boat: BoatCore,
    container: VehicleContainer,
    #[json_arg(vanilla_items, json = "drop_item")]
    drop_item: ItemRef,
}

/// The chest half shared by chest boats and chest rafts (vanilla `AbstractChestBoat`).
trait ChestBoat: AbstractBoat {
    fn chest(&self) -> &VehicleContainer;

    /// Vanilla `AbstractChestBoat.interact`.
    fn chest_boat_interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        location: DVec3,
    ) -> InteractionResult {
        let super_interaction = self.boat_interact(player, hand, location);
        if super_interaction != InteractionResult::Pass {
            return super_interaction;
        }
        if self.boat_can_add_passenger() && !player.is_secondary_use_active() {
            return InteractionResult::Pass;
        }
        if !self.open_chest_menu(player) {
            return InteractionResult::Pass;
        }
        self.game_event_with_source_entity(
            &vanilla_game_events::CONTAINER_OPEN,
            Some(player as &dyn Entity),
        );
        if let Some(world) = self.level() {
            anger_nearby_piglins(&world, player, true);
        }
        InteractionResult::Success
    }

    /// Vanilla `ContainerEntity.interactWithContainerVehicle` with
    /// `AbstractChestBoat.createMenu`; returns whether a menu opened.
    fn open_chest_menu(&self, player: &Player) -> bool {
        if !self
            .chest()
            .prepare_open(self.as_entity_event_source(), player)
        {
            return false;
        }
        let Some(boat) = self
            .level()
            .and_then(|world| world.get_entity_by_id(self.id()))
        else {
            return false;
        };
        let container = ContainerRef::from(self.chest().raw());
        let inventory = Arc::clone(&player.inventory);
        let title = self.display_name();
        player.open_menu(title, move |context| {
            chest_with_kind(
                inventory,
                context.container_id,
                container.clone(),
                3,
                ContainerVehicleMenuKind::new(Arc::downgrade(&boat), container.clone(), true),
            )
        });
        true
    }

    /// Vanilla `AbstractChestBoat.openCustomInventoryScreen`.
    fn open_chest_screen(&self, player: &Player) {
        if !self.open_chest_menu(player) {
            return;
        }
        self.game_event_with_source_entity(
            &vanilla_game_events::CONTAINER_OPEN,
            Some(player as &dyn Entity),
        );
        if let Some(world) = self.level() {
            anger_nearby_piglins(&world, player, true);
        }
    }
}

/// Everything a boat or raft has in common; `$chest` adds the chest half.
macro_rules! boat_entity {
    ($name:ident, $key:literal, $ride_height:path, plain) => {
        boat_entity!(@common $name, $key, $ride_height);

        impl $name {
            /// Creates a new boat entity.
            #[must_use]
            pub fn new(
                entity_type: EntityTypeRef,
                id: i32,
                position: DVec3,
                world: Weak<World>,
                drop_item: ItemRef,
            ) -> Self {
                Self::with_base(
                    EntityBase::new(id, position, entity_type.dimensions, world),
                    entity_type,
                    drop_item,
                )
            }

            /// Creates a boat entity from saved data.
            #[must_use]
            pub fn from_saved(
                entity_type: EntityTypeRef,
                load: EntityBaseLoad,
                drop_item: ItemRef,
            ) -> Self {
                Self::with_base(
                    EntityBase::from_load(load, entity_type.dimensions),
                    entity_type,
                    drop_item,
                )
            }

            fn with_base(base: EntityBase, entity_type: EntityTypeRef, drop_item: ItemRef) -> Self {
                Self {
                    base,
                    entity_type,
                    entity_data: SyncMutex::new(AbstractBoatEntityData::new()),
                    boat: BoatCore::new(),
                    drop_item,
                }
            }
        }

        impl VehicleEntity for $name {
            fn with_vehicle_data(&self, f: &mut dyn FnMut(&mut VehicleEntityData)) {
                f(&mut self.entity_data.lock().vehicle_entity);
            }

            fn drop_item(&self) -> ItemRef {
                self.drop_item
            }
        }

        impl AbstractBoat for $name {
            fn boat_core(&self) -> &BoatCore {
                &self.boat
            }

            fn with_boat_data(&self, f: &mut dyn FnMut(&mut AbstractBoatEntityData)) {
                f(&mut self.entity_data.lock());
            }

            fn ride_height(&self, dimensions: &EntityDimensions) -> f64 {
                $ride_height(dimensions)
            }
        }

        impl Entity for $name {
            boat_entity!(@entity_methods);

            fn interact(
                &self,
                player: &Player,
                hand: InteractionHand,
                location: DVec3,
            ) -> InteractionResult {
                self.boat_interact(player, hand, location)
            }

            fn set_removed(&self, reason: RemovalReason) {
                self.boat_removed(reason);
                self.default_set_removed(reason);
            }

            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_boat(nbt);
            }

            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_boat(nbt);
            }
        }
    };
    ($name:ident, $key:literal, $ride_height:path, chest) => {
        boat_entity!(@common $name, $key, $ride_height);

        impl $name {
            /// Creates a new chest boat entity.
            #[must_use]
            pub fn new(
                entity_type: EntityTypeRef,
                id: i32,
                position: DVec3,
                world: Weak<World>,
                drop_item: ItemRef,
            ) -> Self {
                Self::with_base(
                    EntityBase::new(id, position, entity_type.dimensions, world),
                    entity_type,
                    drop_item,
                )
            }

            /// Creates a chest boat entity from saved data.
            #[must_use]
            pub fn from_saved(
                entity_type: EntityTypeRef,
                load: EntityBaseLoad,
                drop_item: ItemRef,
            ) -> Self {
                Self::with_base(
                    EntityBase::from_load(load, entity_type.dimensions),
                    entity_type,
                    drop_item,
                )
            }

            fn with_base(base: EntityBase, entity_type: EntityTypeRef, drop_item: ItemRef) -> Self {
                Self {
                    base,
                    entity_type,
                    entity_data: SyncMutex::new(AbstractBoatEntityData::new()),
                    boat: BoatCore::new(),
                    container: VehicleContainer::new(CHEST_BOAT_SLOTS),
                    drop_item,
                }
            }

            /// The chest's inventory, rolling a pending loot table first.
            #[must_use]
            pub fn container_ref(&self) -> ContainerRef {
                self.container.container_ref(self)
            }
        }

        impl ChestBoat for $name {
            fn chest(&self) -> &VehicleContainer {
                &self.container
            }
        }

        impl VehicleEntity for $name {
            fn with_vehicle_data(&self, f: &mut dyn FnMut(&mut VehicleEntityData)) {
                f(&mut self.entity_data.lock().vehicle_entity);
            }

            fn drop_item(&self) -> ItemRef {
                self.drop_item
            }

            /// Vanilla `AbstractChestBoat.destroy`.
            fn destroy(&self, world: &World, source: &DamageSource) {
                self.default_destroy(world);
                self.container.destroyed(self, world, source);
            }
        }

        impl AbstractBoat for $name {
            fn boat_core(&self) -> &BoatCore {
                &self.boat
            }

            fn with_boat_data(&self, f: &mut dyn FnMut(&mut AbstractBoatEntityData)) {
                f(&mut self.entity_data.lock());
            }

            fn ride_height(&self, dimensions: &EntityDimensions) -> f64 {
                $ride_height(dimensions)
            }

            /// Vanilla `AbstractChestBoat.getSinglePassengerXOffset`.
            fn single_passenger_x_offset(&self) -> f32 {
                0.15
            }

            /// Vanilla `AbstractChestBoat.getMaxPassengers`.
            fn max_passengers(&self) -> usize {
                1
            }

            fn open_custom_inventory_screen(&self, player: &Player) {
                self.open_chest_screen(player);
            }
        }

        impl Entity for $name {
            boat_entity!(@entity_methods);

            fn interact(
                &self,
                player: &Player,
                hand: InteractionHand,
                location: DVec3,
            ) -> InteractionResult {
                self.chest_boat_interact(player, hand, location)
            }

            /// Vanilla `AbstractChestBoat.remove`: a destroyed boat spills its chest.
            fn set_removed(&self, reason: RemovalReason) {
                if matches!(reason, RemovalReason::Killed | RemovalReason::Discarded)
                    && let Some(world) = self.level()
                {
                    self.container.drop_contents(self, &world);
                }
                self.boat_removed(reason);
                self.default_set_removed(reason);
            }

            /// Vanilla `AbstractChestBoat.addAdditionalSaveData`.
            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_boat(nbt);
                self.container.save(nbt);
            }

            /// Vanilla `AbstractChestBoat.readAdditionalSaveData`.
            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_boat(nbt);
                self.container.load(nbt);
            }

            fn container_entity(&self) -> Option<ContainerRef> {
                self.is_alive().then(|| self.container_ref())
            }
        }
    };
    (@common $name:ident, $key:literal, $ride_height:path) => {
        // SAFETY: This key is owned by Steel and uniquely identifies the boat struct.
        unsafe impl DowncastType for $name {
            const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new($key);
        }

        impl Leashable for $name {
            fn leash_data(&self) -> &SyncMutex<Option<LeashData>> {
                self.boat.leash_data()
            }
        }
    };
    (@entity_methods) => {
        fn base(&self) -> &EntityBase {
            &self.base
        }

        fn entity_type(&self) -> EntityTypeRef {
            self.entity_type
        }

        fn tick(self: Arc<Self>) {
            tick_boat(&self);
        }

        fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
            self.hurt_vehicle(world, source, amount)
        }

        fn movement_emission(&self) -> EntityMovementEmission {
            EntityMovementEmission::Events
        }

        fn is_pickable(&self) -> bool {
            !self.is_removed()
        }

        fn is_pushable(&self) -> bool {
            true
        }

        fn blocks_building(&self) -> bool {
            true
        }

        fn can_be_collided_with(&self, _other: Option<&dyn Entity>) -> bool {
            true
        }

        /// Vanilla `AbstractBoat.canVehicleCollide`.
        fn can_collide_with(&self, other: &dyn Entity) -> bool {
            (other.can_be_collided_with(Some(self)) || other.is_pushable())
                && !self.is_passenger_of_same_vehicle(other)
        }

        fn push_entity(&self, entity: &dyn Entity) {
            self.boat_push_entity(entity);
        }

        fn on_above_bubble_column(&self, drag_down: bool, _pos: BlockPos) {
            self.boat_on_above_bubble_column(drag_down);
        }

        fn passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
            self.boat_passenger_attachment_point(passenger)
        }

        fn position_rider(&self, passenger: &dyn Entity) {
            self.boat_position_rider(passenger);
        }

        fn dismount_location_for_passenger(&self, passenger: &dyn Entity) -> DVec3 {
            self.boat_dismount_location(passenger)
        }

        fn can_add_passenger(&self, _passenger: &dyn Entity) -> bool {
            self.boat_can_add_passenger()
        }

        fn controlling_passenger(&self) -> Option<SharedEntity> {
            self.boat_controlling_passenger()
        }

        fn motion_direction(&self) -> Direction {
            self.boat_motion_direction()
        }

        fn is_under_water(&self) -> bool {
            self.boat_is_under_water()
        }

        fn get_default_gravity(&self) -> f64 {
            boat_gravity()
        }

        /// Vanilla `AbstractBoat.getAirDrag`.
        fn get_air_drag(&self) -> f64 {
            1.0
        }

        fn check_fall_damage(
            self: Arc<Self>,
            vertical_movement: f64,
            on_ground: bool,
            _on_state: BlockStateId,
            _pos: BlockPos,
            world: &Arc<World>,
        ) {
            self.boat_check_fall_damage(vertical_movement, on_ground, world);
        }

        fn modify_passenger_fluid_interaction_box(
            &self,
            passenger_box: WorldAabb,
        ) -> Option<WorldAabb> {
            self.boat_modify_passenger_fluid_interaction_box(passenger_box)
        }

        fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
            self.vehicle_ignores_explosion(explosion)
        }

        /// Vanilla `AbstractBoat.getRelativePortalPosition`.
        fn get_relative_portal_position(&self, axis: Axis, portal_area: FoundRectangle) -> DVec3 {
            reset_forward_direction_of_relative_portal_position(PortalShape::get_relative_position(
                portal_area,
                axis,
                self.position(),
                self.dimensions_for_pose(self.pose()),
            ))
        }

        fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
            Some(&self.entity_data)
        }
    };
}

boat_entity!(BoatEntity, "steel:entity/boat", boat_ride_height, plain);
boat_entity!(RaftEntity, "steel:entity/raft", raft_ride_height, plain);
boat_entity!(
    ChestBoatEntity,
    "steel:entity/chest_boat",
    boat_ride_height,
    chest
);
boat_entity!(
    ChestRaftEntity,
    "steel:entity/chest_raft",
    raft_ride_height,
    chest
);
