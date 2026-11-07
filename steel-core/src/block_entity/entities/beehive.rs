//! Vanilla `BeehiveBlockEntity`: up to three bees stored inside a beehive or
//! bee nest, released after their stay or when the hive is disturbed.

use std::collections::BTreeMap;
use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::RngExt as _;
use simdnbt::FromNbtTag as _;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{
    BlockStateProperties, Direction, EnumProperty, IntProperty,
};
use steel_registry::data_components::components::BlockItemStateProperties;
use steel_registry::data_components::components::{BeehiveOccupant, Bees, CustomData, EntityData};
use steel_registry::data_components::vanilla_components::BEES;
use steel_registry::data_components::vanilla_components::BLOCK_STATE;
use steel_registry::data_components::{DataComponentMap, DataComponentPatch};
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_block_entity_types, vanilla_blocks,
    vanilla_entities, vanilla_game_events,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};

use crate::behavior::blocks::CampfireBlock;
use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::block_entity::components::ImplicitComponentGetter;
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::entities::BeeEntity;
use crate::entity::{
    AgeableMob as _, Animal as _, Entity, EntitySpawnReason, Mob as _, RemovalReason, SharedEntity,
    load_entity_recursive_owned,
};
use crate::player::Player;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Maximum number of occupants in a vanilla beehive.
pub const BEEHIVE_MAX_OCCUPANTS: usize = 3;
/// Minimum occupation time for bees without nectar.
pub const BEEHIVE_MIN_OCCUPATION_TICKS_NECTARLESS: i32 = 600;
/// Minimum occupation time for bees carrying nectar.
const MIN_OCCUPATION_TICKS_NECTAR: i32 = 2400;
/// Vanilla `MIN_TICKS_BEFORE_REENTERING_HIVE`: a sedated hive keeps its bees
/// out this long instead of angering them.
const MIN_TICKS_BEFORE_REENTERING_HIVE: i32 = 400;
/// Released bees within this squared distance of the player target them.
const ANGER_PLAYER_DISTANCE_SQR: f64 = 16.0;
/// Highest honey level of a hive.
const MAX_HONEY_LEVEL: u8 = 5;
/// Chance per tick that an occupied hive plays its work sound.
const WORK_SOUND_CHANCE: f64 = 0.005;

const HORIZONTAL_FACING: &EnumProperty<Direction> = &BlockStateProperties::HORIZONTAL_FACING;
const LEVEL_HONEY: &IntProperty = &BlockStateProperties::LEVEL_HONEY;

/// Vanilla `IGNORED_BEE_TAGS`: entity data a bee loses while inside a hive.
const IGNORED_BEE_TAGS: [&str; 25] = [
    "Air",
    "drop_chances",
    "equipment",
    "Brain",
    "CanPickUpLoot",
    "DeathTime",
    "fall_distance",
    "FallFlying",
    "Fire",
    "HurtTime",
    "LeftHanded",
    "Motion",
    "NoGravity",
    "OnGround",
    "PortalCooldown",
    "Pos",
    "Rotation",
    "sleeping_pos",
    "CannotEnterHiveTicks",
    "TicksSincePollination",
    "CropsGrownSincePollination",
    "hive_pos",
    "Passengers",
    "leash",
    "UUID",
];

/// Vanilla `BeehiveBlockEntity.BeeReleaseStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeeReleaseStatus {
    /// The bee leaves after delivering nectar, raising the honey level.
    HoneyDelivered,
    /// The bee leaves after its stay without nectar.
    BeeReleased,
    /// The hive is disturbed: bees leave whatever the weather or blockage.
    Emergency,
}

/// Vanilla `BeehiveBlockEntity.BeeData`: an occupant plus its live stay counter.
#[derive(Debug, Clone)]
struct BeeData {
    entity_data: EntityData,
    ticks_in_hive: i32,
    min_ticks_in_hive: i32,
}

impl BeeData {
    fn new(occupant: &BeehiveOccupant) -> Self {
        Self {
            entity_data: occupant.entity_data().clone(),
            ticks_in_hive: occupant.ticks_in_hive(),
            min_ticks_in_hive: occupant.min_ticks_in_hive(),
        }
    }

    /// Vanilla `Occupant.create(ticksInHive)`: a fresh bee placed by worldgen.
    fn worldgen(ticks_in_hive: i32) -> Self {
        Self {
            entity_data: EntityData::new(&vanilla_entities::BEE, empty_custom_data()),
            ticks_in_hive,
            min_ticks_in_hive: BEEHIVE_MIN_OCCUPATION_TICKS_NECTARLESS,
        }
    }

    /// Vanilla `Occupant.of(entity)`: saves the entity without the tags a hive drops.
    fn of(entity: &dyn Entity) -> Self {
        let mut entity_tag = entity.nbt_for_data_compare();
        for tag in IGNORED_BEE_TAGS {
            entity_tag.remove(tag);
        }
        let has_nectar = entity_tag.byte("HasNectar").is_some_and(|value| value != 0);
        Self {
            entity_data: EntityData::new(
                entity.entity_type(),
                CustomData::try_from_compound(entity_tag).unwrap_or_else(empty_custom_data),
            ),
            ticks_in_hive: 0,
            min_ticks_in_hive: if has_nectar {
                MIN_OCCUPATION_TICKS_NECTAR
            } else {
                BEEHIVE_MIN_OCCUPATION_TICKS_NECTARLESS
            },
        }
    }

    /// Vanilla `BeeData.tick`: counts the stay and reports whether it is over.
    const fn tick(&mut self) -> bool {
        let done = self.ticks_in_hive > self.min_ticks_in_hive;
        self.ticks_in_hive += 1;
        done
    }

    fn to_occupant(&self) -> BeehiveOccupant {
        BeehiveOccupant::new(
            self.entity_data.clone(),
            self.ticks_in_hive,
            self.min_ticks_in_hive,
        )
    }

    fn has_nectar(&self) -> bool {
        self.entity_data
            .data()
            .as_compound()
            .byte("HasNectar")
            .is_some_and(|value| value != 0)
    }

    /// Vanilla `Occupant.createEntity`: loads the stored entity next to the hive.
    fn create_entity(&self, world: &Arc<World>, hive_pos: BlockPos) -> Option<SharedEntity> {
        let mut entity_tag = self.entity_data.data().copy_tag();
        for tag in IGNORED_BEE_TAGS {
            entity_tag.remove(tag);
        }
        entity_tag.insert("id", self.entity_data.entity_type().key.to_string());
        let loaded =
            load_entity_recursive_owned(world, &entity_tag, EntitySpawnReason::Load, |_| {})?;
        let entity = Arc::clone(&loaded.root);
        if !REGISTRY
            .entity_types
            .is_in_tag(entity.entity_type(), &EntityTypeTag::BEEHIVE_INHABITORS)
        {
            return None;
        }
        entity.set_no_gravity(true);
        if let Some(bee) = entity.downcast_ref::<BeeEntity>() {
            bee.set_hive_pos(hive_pos);
            set_bee_release_data(self.ticks_in_hive, bee);
        }
        Some(entity)
    }
}

fn empty_custom_data() -> CustomData {
    CustomData::try_from_compound(NbtCompound::new())
        .unwrap_or_else(|| unreachable!("an empty compound is valid custom data"))
}

/// Vanilla `Occupant.setBeeReleaseData`: the stay counts toward growing up
/// and toward the end of love mode.
fn set_bee_release_data(ticks_in_hive: i32, bee: &BeeEntity) {
    if !bee.is_age_locked() {
        let age = bee.get_age();
        if age < 0 {
            bee.set_age((age + ticks_in_hive).min(0));
        } else if age > 0 {
            bee.set_age((age - ticks_in_hive).max(0));
        }
    }
    bee.set_in_love_time((bee.in_love_time() - ticks_in_hive).max(0));
}

struct BeehiveState {
    stored: Vec<BeeData>,
    saved_flower_pos: Option<BlockPos>,
}

/// Beehive and bee nest block entity.
pub struct BeehiveBlockEntity {
    base: BlockEntityBase,
    state: SyncMutex<BeehiveState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `BeehiveBlockEntity`.
unsafe impl DowncastType for BeehiveBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/beehive");
}

impl BeehiveBlockEntity {
    /// Creates a new beehive block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: BlockEntityBase::new(&vanilla_block_entity_types::BEEHIVE, level, pos, state),
            state: SyncMutex::new(BeehiveState {
                stored: Vec::new(),
                saved_flower_pos: None,
            }),
        }
    }

    /// Stores a vanilla worldgen bee occupant.
    ///
    /// Mirrors `storeBee(BeehiveBlockEntity.Occupant.create(ticksInHive))`.
    pub fn store_worldgen_bee(&self, ticks_in_hive: i32) {
        self.state
            .lock()
            .stored
            .push(BeeData::worldgen(ticks_in_hive));
    }

    /// Returns the number of stored occupants.
    #[must_use]
    pub fn occupant_count(&self) -> usize {
        self.state.lock().stored.len()
    }

    /// Returns whether the hive currently stores no occupants.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.lock().stored.is_empty()
    }

    /// Vanilla `isFull`.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.state.lock().stored.len() == BEEHIVE_MAX_OCCUPANTS
    }

    /// Vanilla `isFireNearby`: fire in the 3x3x3 cube around the hive.
    #[must_use]
    pub fn is_fire_nearby(&self) -> bool {
        let Some(world) = self.base.level() else {
            return false;
        };
        let pos = self.base.pos;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if world.get_block_state(pos.offset(dx, dy, dz)).get_block()
                        == &vanilla_blocks::FIRE
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Vanilla `isSedated`: a campfire smokes the hive from below.
    #[must_use]
    pub fn is_sedated(&self) -> bool {
        self.base
            .level()
            .is_some_and(|world| CampfireBlock::is_smokey_pos(world.as_ref(), self.base.pos))
    }

    /// Vanilla `emptyAllLivingFromHive`: releases every bee; bees near the
    /// player attack them unless the hive is smoked.
    pub fn empty_all_living_from_hive(
        &self,
        player: Option<&Player>,
        state: BlockStateId,
        release_status: BeeReleaseStatus,
    ) {
        let released = self.release_all_occupants(state, release_status);
        let Some(player) = player else {
            return;
        };
        let Some(world) = self.base.level() else {
            return;
        };
        let player_entity = world.get_entity_by_id(player.id());
        for entity in released {
            let Some(bee) = entity.downcast_ref::<BeeEntity>() else {
                continue;
            };
            if player.position().distance_squared(entity.position()) > ANGER_PLAYER_DISTANCE_SQR {
                continue;
            }
            if self.is_sedated() {
                bee.set_stay_out_of_hive_countdown(MIN_TICKS_BEFORE_REENTERING_HIVE);
            } else if let Some(player_entity) = &player_entity {
                let _ = bee.set_target(Some(player_entity));
            }
        }
    }

    /// Vanilla `releaseAllOccupants`.
    fn release_all_occupants(
        &self,
        state: BlockStateId,
        release_status: BeeReleaseStatus,
    ) -> Vec<SharedEntity> {
        let Some(world) = self.base.level() else {
            return Vec::new();
        };
        let (occupants, saved_flower_pos) = {
            let mut hive = self.state.lock();
            (mem::take(&mut hive.stored), hive.saved_flower_pos)
        };
        let mut spawned = Vec::new();
        let mut kept = Vec::new();
        for occupant in occupants {
            if !release_occupant(
                &world,
                self.base.pos,
                state,
                &occupant,
                Some(&mut spawned),
                release_status,
                saved_flower_pos,
            ) {
                kept.push(occupant);
            }
        }
        {
            let mut hive = self.state.lock();
            kept.append(&mut hive.stored);
            hive.stored = kept;
        }
        if !spawned.is_empty() {
            self.base.set_changed();
        }
        spawned
    }

    /// Vanilla `addOccupant`: the bee enters the hive and leaves the world.
    pub fn add_occupant(&self, bee: &BeeEntity) {
        {
            let hive = self.state.lock();
            if hive.stored.len() >= BEEHIVE_MAX_OCCUPANTS {
                return;
            }
        }
        bee.stop_riding();
        for passenger in bee.passengers() {
            passenger.stop_riding();
        }
        if let Some(leashable) = bee.as_leashable() {
            leashable.drop_leash();
        }
        let occupant = BeeData::of(bee);
        let saved_flower_pos = {
            let mut hive = self.state.lock();
            hive.stored.push(occupant);
            hive.saved_flower_pos
        };
        if let Some(world) = self.base.level() {
            if let Some(bee_flower_pos) = bee.saved_flower_pos()
                && (saved_flower_pos.is_none() || rand::rng().random_bool(0.5))
            {
                self.state.lock().saved_flower_pos = Some(bee_flower_pos);
            }
            let pos = self.base.pos;
            world.play_sound_at(
                &sound_events::BLOCK_BEEHIVE_ENTER,
                SoundSource::Blocks,
                DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z())),
                1.0,
                1.0,
                None,
            );
            world.game_event(
                &vanilla_game_events::BLOCK_CHANGE,
                pos,
                &GameEventContext::new(Some(bee), Some(self.get_block_state())),
            );
        }
        bee.set_removed(RemovalReason::Discarded);
        self.base.set_changed();
    }

    /// Vanilla `getBees`.
    fn bees(&self) -> Bees {
        Bees::new(
            self.state
                .lock()
                .stored
                .iter()
                .map(BeeData::to_occupant)
                .collect(),
        )
    }

    /// The beehive as an item carrying its bees and honey level, as vanilla
    /// `BeehiveBlock.playerWillDestroy` drops it in creative mode.
    #[must_use]
    pub fn beehive_as_item(&self, state: BlockStateId) -> ItemStack {
        let mut properties = BTreeMap::new();
        properties.insert(
            LEVEL_HONEY.name.to_owned(),
            state.get_value(LEVEL_HONEY).to_string(),
        );
        let mut patch = DataComponentPatch::new();
        patch.set(BEES, self.bees());
        patch.set(BLOCK_STATE, BlockItemStateProperties::new(properties));
        ItemStack::with_count_and_patch(REGISTRY.items.by_block(state.get_block()), 1, patch)
    }
}

/// Vanilla `BeehiveBlockEntity.releaseOccupant`.
fn release_occupant(
    world: &Arc<World>,
    pos: BlockPos,
    state: BlockStateId,
    occupant: &BeeData,
    spawned: Option<&mut Vec<SharedEntity>>,
    release_status: BeeReleaseStatus,
    saved_flower_pos: Option<BlockPos>,
) -> bool {
    if world.bees_stay_in_hive() && release_status != BeeReleaseStatus::Emergency {
        return false;
    }
    let facing = state.get_value(HORIZONTAL_FACING);
    let facing_pos = pos.relative(facing);
    let facing_state = world.get_block_state(facing_pos);
    let front_blocked = !BLOCK_BEHAVIORS
        .get_behavior(facing_state.get_block())
        .get_collision_shape(
            facing_state,
            world.as_ref(),
            facing_pos,
            BlockCollisionContext::empty(),
        )
        .is_empty();
    if front_blocked && release_status != BeeReleaseStatus::Emergency {
        return false;
    }
    let Some(entity) = occupant.create_entity(world, pos) else {
        return false;
    };
    if let Some(bee) = entity.downcast_ref::<BeeEntity>() {
        let mut random = rand::rng();
        if let Some(saved_flower_pos) = saved_flower_pos
            && !bee.has_saved_flower_pos()
            && random.random::<f32>() < 0.9
        {
            bee.set_saved_flower_pos(saved_flower_pos);
        }

        if release_status == BeeReleaseStatus::HoneyDelivered {
            bee.drop_off_nectar();
            if state.get_block().has_tag(&BlockTag::BEEHIVES)
                && let Some(honey_level) = state.try_get_value(LEVEL_HONEY)
                && honey_level < MAX_HONEY_LEVEL
            {
                let mut level_increase = if random.random_range(0..100) == 0 {
                    2
                } else {
                    1
                };
                if honey_level + level_increase > MAX_HONEY_LEVEL {
                    level_increase -= 1;
                }
                world.set_block(
                    pos,
                    state.set_value(LEVEL_HONEY, honey_level + level_increase),
                    UpdateFlags::UPDATE_ALL,
                );
            }
        }

        if let Some(spawned) = spawned {
            spawned.push(Arc::clone(&entity));
        }

        let dimensions = entity.base().dimensions();
        let delta = if front_blocked {
            0.0
        } else {
            0.55 + f64::from(dimensions.width / 2.0)
        };
        let (step_x, _, step_z) = facing.offset();
        let spawn_x = f64::from(pos.x()) + 0.5 + delta * f64::from(step_x);
        let spawn_y = f64::from(pos.y()) + 0.5 - f64::from(dimensions.height / 2.0);
        let spawn_z = f64::from(pos.z()) + 0.5 + delta * f64::from(step_z);
        let (yaw, pitch) = entity.rotation();
        entity.snap_to(DVec3::new(spawn_x, spawn_y, spawn_z), yaw, pitch);
    }

    world.play_sound(
        &sound_events::BLOCK_BEEHIVE_EXIT,
        SoundSource::Blocks,
        pos,
        1.0,
        1.0,
        None,
    );
    world.game_event(
        &vanilla_game_events::BLOCK_CHANGE,
        pos,
        &GameEventContext::new(Some(entity.as_ref()), Some(world.get_block_state(pos))),
    );
    world
        .try_add_fresh_entity_with_passengers(Arc::clone(&entity))
        .is_ok()
}

impl BlockEntity for BeehiveBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    /// Vanilla `setChanged`: fire next to the hive drives every bee out first.
    fn set_changed(&self) {
        if self.is_fire_nearby()
            && let Some(world) = self.base.level()
        {
            self.empty_all_living_from_hive(
                None,
                world.get_block_state(self.base.pos),
                BeeReleaseStatus::Emergency,
            );
        }
        self.base.set_changed();
    }

    /// Vanilla `BeehiveBlockEntity.serverTick`.
    fn tick(&self, world: &Arc<World>) {
        let pos = self.base.pos;
        let state = self.get_block_state();
        let (due, saved_flower_pos) = {
            let mut hive = self.state.lock();
            let due = hive
                .stored
                .iter_mut()
                .enumerate()
                .filter_map(|(index, data)| data.tick().then_some(index))
                .collect::<Vec<_>>();
            let due = due
                .into_iter()
                .filter_map(|index| hive.stored.get(index).cloned().map(|data| (index, data)))
                .collect::<Vec<_>>();
            (due, hive.saved_flower_pos)
        };

        let mut released = Vec::new();
        for (index, data) in &due {
            let release_status = if data.has_nectar() {
                BeeReleaseStatus::HoneyDelivered
            } else {
                BeeReleaseStatus::BeeReleased
            };
            if release_occupant(
                world,
                pos,
                state,
                data,
                None,
                release_status,
                saved_flower_pos,
            ) {
                released.push(*index);
            }
        }
        let has_occupants = {
            let mut hive = self.state.lock();
            for index in released.iter().rev() {
                if *index < hive.stored.len() {
                    hive.stored.remove(*index);
                }
            }
            !hive.stored.is_empty()
        };
        if !released.is_empty() {
            // Vanilla `BlockEntity.setChanged(level, pos, state)` skips the
            // fire check of the instance method.
            self.base.set_changed();
        }

        if has_occupants && rand::rng().random::<f64>() < WORK_SOUND_CHANCE {
            world.play_sound_at(
                &sound_events::BLOCK_BEEHIVE_WORK,
                SoundSource::Blocks,
                DVec3::new(
                    f64::from(pos.x()) + 0.5,
                    f64::from(pos.y()),
                    f64::from(pos.z()) + 0.5,
                ),
                1.0,
                1.0,
                None,
            );
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let stored = nbt
            .get("bees")
            .and_then(Bees::from_nbt_tag)
            .map_or_else(Vec::new, |bees| {
                bees.bees().iter().map(BeeData::new).collect()
            });
        let saved_flower_pos = nbt
            .int_array("flower_pos")
            .and_then(|pos| (pos.len() == 3).then(|| BlockPos::new(pos[0], pos[1], pos[2])));
        let mut hive = self.state.lock();
        hive.stored = stored;
        hive.saved_flower_pos = saved_flower_pos;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        nbt.insert("bees", self.bees().to_nbt_tag());
        let saved_flower_pos = self.state.lock().saved_flower_pos;
        if let Some(pos) = saved_flower_pos {
            nbt.insert(
                "flower_pos",
                NbtTag::IntArray(vec![pos.x(), pos.y(), pos.z()]),
            );
        }
    }

    fn apply_implicit_components(&self, components: &mut ImplicitComponentGetter<'_>) {
        let bees = components.get_or_default(BEES, Bees::empty());
        self.state.lock().stored = bees.bees().iter().map(BeeData::new).collect();
    }

    fn collect_implicit_components(&self, components: &mut DataComponentMap) {
        components.set(BEES, Some(self.bees()));
    }
}

#[cfg(test)]
mod tests;
