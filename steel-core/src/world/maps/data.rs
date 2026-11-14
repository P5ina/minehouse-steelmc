//! Vanilla `MapItemSavedData`.

use std::io::{self, Cursor};
use std::str::FromStr as _;
use std::sync::{Arc, Weak};
use std::{mem, ptr};

use simdnbt::borrow::read_compound;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_protocol::packets::game::{CMapItemData, MapDecorationData, MapPatch};
use steel_registry::data_components::components::{MapDecorations, MapId};
use steel_registry::data_components::vanilla_components::MAP_ID;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::map_decoration_type::MapDecorationType;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{RegistryReference, vanilla_items, vanilla_map_decoration_types};
use steel_utils::{BlockPos, Identifier};

use crate::entity::{Entity as _, LivingEntity as _};
use crate::inventory::container::Container as _;
use crate::player::Player;
use crate::server::worlds::NETHER_WORLD_NAME;
use crate::world::{LevelReader, World};

use super::MapBanner;

/// Vanilla `MapItem.IMAGE_WIDTH` and `IMAGE_HEIGHT`.
pub const MAP_SIZE: usize = 128;
/// Vanilla `MapItemSavedData.MAX_SCALE`.
pub const MAX_SCALE: u8 = 4;
/// Vanilla `MapItemSavedData.TRACKED_DECORATION_LIMIT`.
pub const TRACKED_DECORATION_LIMIT: i32 = 256;
const COLOR_COUNT: usize = MAP_SIZE * MAP_SIZE;

type DecorationTypeRef = RegistryReference<MapDecorationType>;

const fn type_ref(value: &'static MapDecorationType) -> DecorationTypeRef {
    RegistryReference::new(value)
}

/// Vanilla `MapDecoration`: the rotation is stored as `rot & 15`.
const fn make_decoration(
    decoration_type: DecorationTypeRef,
    x: i8,
    y: i8,
    rot: i8,
    name: Option<text_components::TextComponent>,
) -> MapDecorationData {
    MapDecorationData {
        decoration_type,
        x,
        y,
        rot: rot & 15,
        name,
    }
}

/// Vanilla `MapItemSavedData.HoldingPlayer`.
pub struct HoldingPlayer {
    player: Weak<Player>,
    /// Vanilla `Player.getPlainTextName`, kept for the decoration key after the player is gone.
    name: String,
    dirty_data: bool,
    min_dirty_x: usize,
    min_dirty_y: usize,
    max_dirty_x: usize,
    max_dirty_y: usize,
    dirty_decorations: bool,
    tick: i32,
    /// Vanilla `HoldingPlayer.step`: which map column stripe `MapItem.update` redraws.
    pub step: i32,
}

impl HoldingPlayer {
    fn new(player: &Arc<Player>) -> Self {
        Self {
            player: Arc::downgrade(player),
            name: player.gameprofile.name().to_owned(),
            dirty_data: true,
            min_dirty_x: 0,
            min_dirty_y: 0,
            max_dirty_x: MAP_SIZE - 1,
            max_dirty_y: MAP_SIZE - 1,
            dirty_decorations: true,
            tick: 0,
            step: 0,
        }
    }

    fn is(&self, player: &Arc<Player>) -> bool {
        ptr::eq(self.player.as_ptr(), Arc::as_ptr(player))
    }

    fn mark_colors_dirty(&mut self, x: usize, y: usize) {
        if self.dirty_data {
            self.min_dirty_x = self.min_dirty_x.min(x);
            self.min_dirty_y = self.min_dirty_y.min(y);
            self.max_dirty_x = self.max_dirty_x.max(x);
            self.max_dirty_y = self.max_dirty_y.max(y);
        } else {
            self.dirty_data = true;
            self.min_dirty_x = x;
            self.min_dirty_y = y;
            self.max_dirty_x = x;
            self.max_dirty_y = y;
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "dirty bounds are map pixel coordinates below 128"
    )]
    fn create_patch(&self, colors: &[u8; COLOR_COUNT]) -> MapPatch {
        let width = self.max_dirty_x + 1 - self.min_dirty_x;
        let height = self.max_dirty_y + 1 - self.min_dirty_y;
        let mut patch = vec![0; width * height];
        for x in 0..width {
            for y in 0..height {
                patch[x + y * width] =
                    colors[self.min_dirty_x + x + (self.min_dirty_y + y) * MAP_SIZE];
            }
        }
        MapPatch {
            start_x: self.min_dirty_x as u8,
            start_y: self.min_dirty_y as u8,
            width: width as u8,
            height: height as u8,
            colors: patch,
        }
    }
}

/// Vanilla `MapItemSavedData`: the colors and decorations of one map id.
pub struct MapItemSavedData {
    /// Vanilla `centerX`: the world X at the image center.
    pub center_x: i32,
    /// Vanilla `centerZ`: the world Z at the image center.
    pub center_z: i32,
    /// The loaded world this map draws.
    pub dimension: Identifier,
    tracking_position: bool,
    unlimited_tracking: bool,
    /// Zoom level 0 to 4; each pixel covers `2^scale` blocks.
    pub scale: u8,
    colors: Box<[u8; COLOR_COUNT]>,
    /// Locked maps are never redrawn.
    pub locked: bool,
    carried_by: Vec<HoldingPlayer>,
    /// Keyed by [`MapBanner::id`], in the order they were marked.
    banner_markers: Vec<(String, MapBanner)>,
    /// Vanilla keeps a `LinkedHashMap`; the order is the packet order.
    decorations: Vec<(String, MapDecorationData)>,
    tracked_decoration_count: i32,
    dirty: bool,
}

impl MapItemSavedData {
    fn new(
        center_x: i32,
        center_z: i32,
        scale: u8,
        tracking_position: bool,
        unlimited_tracking: bool,
        locked: bool,
        dimension: Identifier,
    ) -> Self {
        Self {
            center_x,
            center_z,
            dimension,
            tracking_position,
            unlimited_tracking,
            scale,
            colors: Box::new([0; COLOR_COUNT]),
            locked,
            carried_by: Vec::new(),
            banner_markers: Vec::new(),
            decorations: Vec::new(),
            tracked_decoration_count: 0,
            dirty: false,
        }
    }

    /// Vanilla `MapItemSavedData.createFresh`: snaps the origin to the map grid of this scale.
    #[must_use]
    pub fn create_fresh(
        origin_x: f64,
        origin_z: f64,
        scale: u8,
        tracking_position: bool,
        unlimited_tracking: bool,
        dimension: Identifier,
    ) -> Self {
        let size = 128 * (1 << scale);
        let area_x = java_floor((origin_x + 64.0) / f64::from(size));
        let area_z = java_floor((origin_z + 64.0) / f64::from(size));
        let x = area_x * size + size / 2 - 64;
        let z = area_z * size + size / 2 - 64;
        Self::new(
            x,
            z,
            scale,
            tracking_position,
            unlimited_tracking,
            false,
            dimension,
        )
    }

    /// Vanilla `MapItemSavedData.locked`: a locked copy with the same colors and decorations.
    #[must_use]
    pub fn locked(&self) -> Self {
        let mut result = Self::new(
            self.center_x,
            self.center_z,
            self.scale,
            self.tracking_position,
            self.unlimited_tracking,
            true,
            self.dimension.clone(),
        );
        result.banner_markers.clone_from(&self.banner_markers);
        result.decorations.clone_from(&self.decorations);
        result.tracked_decoration_count = self.tracked_decoration_count;
        result.colors.copy_from_slice(&self.colors[..]);
        result
    }

    /// Vanilla `MapItemSavedData.scaled`: a fresh map one zoom level out.
    #[must_use]
    pub fn scaled(&self) -> Self {
        Self::create_fresh(
            f64::from(self.center_x),
            f64::from(self.center_z),
            (self.scale + 1).min(MAX_SCALE),
            self.tracking_position,
            self.unlimited_tracking,
            self.dimension.clone(),
        )
    }

    /// Vanilla `SavedData.isDirty`.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Vanilla `SavedData.setDirty`.
    pub const fn set_dirty(&mut self) {
        self.dirty = true;
    }

    /// Marks this map as written to storage.
    pub(super) const fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// The packed color of a map pixel.
    #[must_use]
    pub fn color(&self, x: usize, y: usize) -> u8 {
        self.colors[x + y * MAP_SIZE]
    }

    /// Vanilla `MapItemSavedData.tickCarriedBy` for a map in a player's inventory.
    ///
    /// Item frames do not carry maps yet, so the framed-map branch is not reachable.
    pub fn tick_carried_by(
        &mut self,
        ticking_player: &Arc<Player>,
        map_id: Option<MapId>,
        static_decorations: Option<&MapDecorations>,
    ) {
        if !self
            .carried_by
            .iter()
            .any(|holding| holding.is(ticking_player))
        {
            self.carried_by.push(HoldingPlayer::new(ticking_player));
        }

        let ticking_name = ticking_player.gameprofile.name().to_owned();
        if !inventory_contains_map(ticking_player, map_id) {
            self.remove_decoration(&ticking_name);
        }

        // Vanilla iterates by index and removes in place, so the holder after a
        // removed one is skipped this tick.
        let mut i = 0;
        while i < self.carried_by.len() {
            let other = self.carried_by[i].player.upgrade();
            let other_name = self.carried_by[i].name.clone();
            let carried = other.as_ref().is_some_and(|player| {
                !player.is_removed() && inventory_contains_map(player, map_id)
            });
            if carried {
                if let Some(other) = &other {
                    let world = other.get_world();
                    if world.key == self.dimension && self.tracking_position {
                        let position = other.position();
                        self.add_decoration(
                            type_ref(&vanilla_map_decoration_types::PLAYER),
                            Some(&world),
                            &other_name,
                            position.x,
                            position.z,
                            f64::from(other.rotation().0),
                            None,
                        );
                    }
                }
            } else {
                self.carried_by.remove(i);
                self.remove_decoration(&other_name);
            }

            if let Some(other) = &other
                && !Arc::ptr_eq(other, ticking_player)
                && has_map_invisibility_item_equipped(other)
            {
                self.remove_decoration(&other_name);
            }
            i += 1;
        }

        if let Some(static_decorations) = static_decorations
            && !static_decorations
                .decorations()
                .keys()
                .all(|id| self.decoration_index(id).is_some())
        {
            let world = ticking_player.get_world();
            for (id, entry) in static_decorations.decorations() {
                if self.decoration_index(id).is_none() {
                    self.add_decoration(
                        entry.decoration_type(),
                        Some(&world),
                        id,
                        entry.x(),
                        entry.z(),
                        f64::from(entry.rotation()),
                        None,
                    );
                }
            }
        }
    }

    fn decoration_index(&self, key: &str) -> Option<usize> {
        self.decorations.iter().position(|(id, _)| id == key)
    }

    fn remove_decoration(&mut self, key: &str) {
        if let Some(index) = self.decoration_index(key) {
            let (_, removed) = self.decorations.remove(index);
            if removed.decoration_type.value().track_count {
                self.tracked_decoration_count -= 1;
            }
        }
        self.set_decorations_dirty();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors vanilla MapItemSavedData.addDecoration"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla narrows the world offset to a float"
    )]
    fn add_decoration(
        &mut self,
        decoration_type: DecorationTypeRef,
        world: Option<&World>,
        key: &str,
        x_pos: f64,
        z_pos: f64,
        y_rot: f64,
        name: Option<text_components::TextComponent>,
    ) {
        let scaling = (1 << self.scale) as f32;
        let x_delta = (x_pos - f64::from(self.center_x)) as f32 / scaling;
        let y_delta = (z_pos - f64::from(self.center_z)) as f32 / scaling;
        let Some((decoration_type, x, y, rot)) = self.calculate_decoration_location_and_type(
            decoration_type,
            world,
            y_rot,
            x_delta,
            y_delta,
        ) else {
            self.remove_decoration(key);
            return;
        };

        let new_decoration = make_decoration(decoration_type, x, y, rot, name);
        let previous = if let Some(index) = self.decoration_index(key) {
            Some(mem::replace(
                &mut self.decorations[index].1,
                new_decoration.clone(),
            ))
        } else {
            self.decorations
                .push((key.to_owned(), new_decoration.clone()));
            None
        };
        if previous.as_ref() != Some(&new_decoration) {
            if previous.is_some_and(|previous| previous.decoration_type.value().track_count) {
                self.tracked_decoration_count -= 1;
            }
            if decoration_type.value().track_count {
                self.tracked_decoration_count += 1;
            }
            self.set_decorations_dirty();
        }
    }

    fn calculate_decoration_location_and_type(
        &self,
        decoration_type: DecorationTypeRef,
        world: Option<&World>,
        y_rot: f64,
        x_delta: f32,
        y_delta: f32,
    ) -> Option<(DecorationTypeRef, i8, i8, i8)> {
        let x = clamp_map_coordinate(x_delta);
        let y = clamp_map_coordinate(y_delta);
        if decoration_type == type_ref(&vanilla_map_decoration_types::PLAYER) {
            let decoration_type = if is_inside_map(x_delta, y_delta) {
                decoration_type
            } else {
                self.decoration_type_for_player_outside_map(x_delta, y_delta)?
            };
            Some((decoration_type, x, y, self.calculate_rotation(world, y_rot)))
        } else if !is_inside_map(x_delta, y_delta) && !self.unlimited_tracking {
            None
        } else {
            Some((decoration_type, x, y, self.calculate_rotation(world, y_rot)))
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla narrows the rotation through int to byte"
    )]
    fn calculate_rotation(&self, world: Option<&World>, y_rot: f64) -> i8 {
        if let Some(world) = world
            && self.dimension.path == NETHER_WORLD_NAME
        {
            let s = (world.game_time() / 10) as i32;
            return ((s
                .wrapping_mul(s)
                .wrapping_mul(34_187_121)
                .wrapping_add(s.wrapping_mul(121))
                >> 15)
                & 15) as i8;
        }
        let adjusted = if y_rot < 0.0 {
            y_rot - 8.0
        } else {
            y_rot + 8.0
        };
        (adjusted * 16.0 / 360.0) as i32 as i8
    }

    fn decoration_type_for_player_outside_map(
        &self,
        x_delta: f32,
        y_delta: f32,
    ) -> Option<DecorationTypeRef> {
        if x_delta.abs() < 320.0 && y_delta.abs() < 320.0 {
            Some(type_ref(&vanilla_map_decoration_types::PLAYER_OFF_MAP))
        } else if self.unlimited_tracking {
            Some(type_ref(&vanilla_map_decoration_types::PLAYER_OFF_LIMITS))
        } else {
            None
        }
    }

    /// Vanilla `MapItemSavedData.getUpdatePacket`.
    pub fn update_packet(&mut self, id: MapId, player: &Arc<Player>) -> Option<CMapItemData> {
        let index = self
            .carried_by
            .iter()
            .position(|holding| holding.is(player))?;
        let holding = &mut self.carried_by[index];
        let patch = holding.dirty_data.then(|| {
            holding.dirty_data = false;
            holding.create_patch(&self.colors)
        });
        let decorations = if holding.dirty_decorations && {
            let due = holding.tick % 5 == 0;
            holding.tick = holding.tick.wrapping_add(1);
            due
        } {
            holding.dirty_decorations = false;
            Some(
                self.decorations
                    .iter()
                    .map(|(_, decoration)| decoration.clone())
                    .collect(),
            )
        } else {
            None
        };
        if decorations.is_none() && patch.is_none() {
            return None;
        }
        Some(CMapItemData {
            map_id: id.id(),
            scale: self.scale.cast_signed(),
            locked: self.locked,
            decorations,
            color_patch: patch,
        })
    }

    fn set_colors_dirty(&mut self, x: usize, y: usize) {
        self.set_dirty();
        for holding in &mut self.carried_by {
            holding.mark_colors_dirty(x, y);
        }
    }

    fn set_decorations_dirty(&mut self) {
        for holding in &mut self.carried_by {
            holding.dirty_decorations = true;
        }
    }

    /// Vanilla `MapItemSavedData.getHoldingPlayer`.
    pub fn holding_player(&mut self, player: &Arc<Player>) -> &mut HoldingPlayer {
        let index = if let Some(index) = self
            .carried_by
            .iter()
            .position(|holding| holding.is(player))
        {
            index
        } else {
            self.carried_by.push(HoldingPlayer::new(player));
            self.carried_by.len() - 1
        };
        &mut self.carried_by[index]
    }

    /// Vanilla `MapItemSavedData.toggleBanner`: marks the banner at `pos`, or unmarks it when it
    /// is already marked. Returns `false` when nothing changed.
    pub fn toggle_banner(&mut self, world: &World, pos: BlockPos) -> bool {
        let x_pos = f64::from(pos.x()) + 0.5;
        let z_pos = f64::from(pos.z()) + 0.5;
        let scale = f64::from(1 << self.scale);
        let xd = (x_pos - f64::from(self.center_x)) / scale;
        let yd = (z_pos - f64::from(self.center_z)) / scale;
        if !(-63.0..=63.0).contains(&xd) || !(-63.0..=63.0).contains(&yd) {
            return false;
        }
        let Some(banner) = MapBanner::from_world(world, pos) else {
            return false;
        };
        let id = banner.id();
        if let Some(index) = self
            .banner_markers
            .iter()
            .position(|(key, marked)| *key == id && *marked == banner)
        {
            self.banner_markers.remove(index);
            self.remove_decoration(&id);
            self.set_dirty();
            return true;
        }
        if self.is_tracked_count_over_limit(TRACKED_DECORATION_LIMIT) {
            return false;
        }
        self.add_banner_decoration(Some(world), &banner, x_pos, z_pos);
        self.put_banner_marker(id, banner);
        self.set_dirty();
        true
    }

    /// Vanilla `MapItemSavedData.checkBanners`: unmarks banners in this column that were broken or
    /// changed.
    pub fn check_banners(&mut self, world: &dyn LevelReader, x: i32, z: i32) {
        let mut i = 0;
        while i < self.banner_markers.len() {
            let expected = &self.banner_markers[i].1;
            if expected.pos.x() == x
                && expected.pos.z() == z
                && MapBanner::from_world(world, expected.pos).as_ref() != Some(expected)
            {
                let (id, _) = self.banner_markers.remove(i);
                self.remove_decoration(&id);
                self.set_dirty();
            } else {
                i += 1;
            }
        }
    }

    /// The banners marked on this map.
    pub fn banners(&self) -> impl Iterator<Item = &MapBanner> {
        self.banner_markers.iter().map(|(_, banner)| banner)
    }

    fn put_banner_marker(&mut self, id: String, banner: MapBanner) {
        if let Some((_, marked)) = self.banner_markers.iter_mut().find(|(key, _)| *key == id) {
            *marked = banner;
        } else {
            self.banner_markers.push((id, banner));
        }
    }

    fn add_banner_decoration(
        &mut self,
        world: Option<&World>,
        banner: &MapBanner,
        x_pos: f64,
        z_pos: f64,
    ) {
        self.add_decoration(
            type_ref(banner.decoration()),
            world,
            &banner.id(),
            x_pos,
            z_pos,
            180.0,
            banner.name.clone(),
        );
    }

    /// Vanilla `MapItemSavedData.updateColor`: returns whether the pixel changed.
    pub fn update_color(&mut self, x: usize, y: usize, color: u8) -> bool {
        if self.colors[x + y * MAP_SIZE] == color {
            false
        } else {
            self.set_color(x, y, color);
            true
        }
    }

    /// Vanilla `MapItemSavedData.setColor`.
    pub fn set_color(&mut self, x: usize, y: usize, color: u8) {
        self.colors[x + y * MAP_SIZE] = color;
        self.set_colors_dirty(x, y);
    }

    /// Vanilla `MapItemSavedData.isTrackedCountOverLimit`.
    #[must_use]
    pub const fn is_tracked_count_over_limit(&self, limit: i32) -> bool {
        self.tracked_decoration_count > limit
    }

    /// The current decorations in packet order.
    pub fn decorations(&self) -> impl Iterator<Item = &MapDecorationData> {
        self.decorations.iter().map(|(_, decoration)| decoration)
    }

    /// Encodes vanilla `MapItemSavedData.CODEC`.
    #[must_use]
    pub fn to_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert("dimension", self.dimension.to_string());
        nbt.insert("xCenter", self.center_x);
        nbt.insert("zCenter", self.center_z);
        nbt.insert("scale", self.scale.cast_signed());
        nbt.insert("colors", NbtTag::ByteArray(self.colors.to_vec()));
        nbt.insert("trackingPosition", i8::from(self.tracking_position));
        nbt.insert("unlimitedTracking", i8::from(self.unlimited_tracking));
        nbt.insert("locked", i8::from(self.locked));
        if !self.banner_markers.is_empty() {
            nbt.insert(
                "banners",
                NbtList::Compound(self.banners().map(MapBanner::to_nbt).collect()),
            );
        }
        nbt
    }

    /// Decodes vanilla `MapItemSavedData.CODEC`.
    pub fn from_nbt_bytes(bytes: &[u8]) -> io::Result<Self> {
        let invalid = |reason: &str| io::Error::new(io::ErrorKind::InvalidData, reason.to_owned());
        let nbt = read_compound(&mut Cursor::new(bytes))
            .map_err(|error| invalid(&format!("invalid map NBT: {error}")))?;
        let nbt = simdnbt::borrow::NbtCompound::from(&nbt);
        let dimension = nbt
            .string("dimension")
            .and_then(|value| Identifier::from_str(&value.to_str()).ok())
            .ok_or_else(|| invalid("map without a dimension"))?;
        let center_x = nbt
            .int("xCenter")
            .ok_or_else(|| invalid("map without xCenter"))?;
        let center_z = nbt
            .int("zCenter")
            .ok_or_else(|| invalid("map without zCenter"))?;
        let scale = nbt
            .byte("scale")
            .unwrap_or(0)
            .clamp(0, MAX_SCALE.cast_signed())
            .cast_unsigned();
        let flag = |key: &str, default: bool| nbt.byte(key).map_or(default, |value| value != 0);
        let mut data = Self::new(
            center_x,
            center_z,
            scale,
            flag("trackingPosition", true),
            flag("unlimitedTracking", false),
            flag("locked", false),
            dimension,
        );
        if let Some(colors) = nbt.byte_array("colors")
            && colors.len() == COLOR_COUNT
        {
            data.colors.copy_from_slice(colors);
        }
        for banner in nbt
            .list("banners")
            .and_then(|list| list.compounds())
            .into_iter()
            .flatten()
            .filter_map(MapBanner::from_nbt)
        {
            let id = banner.id();
            data.add_banner_decoration(
                None,
                &banner,
                f64::from(banner.pos.x()),
                f64::from(banner.pos.z()),
            );
            data.put_banner_marker(id, banner);
        }
        Ok(data)
    }

    /// Encodes this map for the domain's map storage.
    #[must_use]
    pub fn to_nbt_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.to_nbt().write(&mut bytes);
        bytes
    }
}

/// Vanilla `MapItemSavedData.isInsideMap`.
fn is_inside_map(x: f32, y: f32) -> bool {
    (-63.0..=63.0).contains(&x) && (-63.0..=63.0).contains(&y)
}

/// Vanilla `MapItemSavedData.clampMapCoordinate`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "vanilla narrows the doubled offset through int to byte"
)]
fn clamp_map_coordinate(delta: f32) -> i8 {
    if delta <= -63.0 {
        -128
    } else if delta >= 63.0 {
        127
    } else {
        (f64::from(delta * 2.0) + 0.5) as i32 as i8
    }
}

/// Vanilla `MapItemSavedData.mapMatcher` over `Inventory.contains`.
fn inventory_contains_map(player: &Player, map_id: Option<MapId>) -> bool {
    player
        .inventory
        .lock()
        .items()
        .iter()
        .any(|stack| stack.is(&vanilla_items::FILLED_MAP) && stack.get(MAP_ID).copied() == map_id)
}

/// Vanilla `MapItemSavedData.hasMapInvisibilityItemEquipped`.
fn has_map_invisibility_item_equipped(player: &Player) -> bool {
    EquipmentSlot::ALL
        .into_iter()
        .filter(|slot| !matches!(slot, EquipmentSlot::MainHand | EquipmentSlot::OffHand))
        .any(|slot| {
            let mut hidden = false;
            player.with_equipment_slot(slot, &mut |stack: &ItemStack| {
                hidden = stack.item().has_tag(&ItemTag::MAP_INVISIBILITY_EQUIPMENT);
            });
            hidden
        })
}

/// Vanilla `Mth.floor(double)`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "vanilla saturates the floored double into an int"
)]
pub(crate) const fn java_floor(value: f64) -> i32 {
    value.floor() as i32
}
