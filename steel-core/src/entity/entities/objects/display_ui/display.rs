//! Vanilla `Display`: the state block, item, and text displays share.
//!
//! Displays are purely visual: they never collide, take no damage, and only
//! check their vehicle each tick. The rendering itself (interpolation,
//! culling, billboarding) happens on the client from the synced data.

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::vanilla_entity_data::DisplayEntityData;
use steel_utils::nbt::NbtNumeric;

use super::transformation::Transformation;
use crate::entity::Entity;

/// Vanilla `Display.TAG_TRANSFORMATION`.
const TAG_TRANSFORMATION: &str = "transformation";
/// Vanilla `Display.TAG_TRANSFORMATION_INTERPOLATION_DURATION`.
const TAG_INTERPOLATION_DURATION: &str = "interpolation_duration";
/// Vanilla `Display.TAG_TRANSFORMATION_START_INTERPOLATION`, read but never saved.
const TAG_START_INTERPOLATION: &str = "start_interpolation";
/// Vanilla `Display.TAG_POS_ROT_INTERPOLATION_DURATION`.
const TAG_TELEPORT_DURATION: &str = "teleport_duration";
const TAG_BILLBOARD: &str = "billboard";
const TAG_BRIGHTNESS: &str = "brightness";
const TAG_VIEW_RANGE: &str = "view_range";
const TAG_SHADOW_RADIUS: &str = "shadow_radius";
const TAG_SHADOW_STRENGTH: &str = "shadow_strength";
const TAG_WIDTH: &str = "width";
const TAG_HEIGHT: &str = "height";
const TAG_GLOW_COLOR_OVERRIDE: &str = "glow_color_override";

/// Vanilla `Display.NO_BRIGHTNESS_OVERRIDE`.
const NO_BRIGHTNESS_OVERRIDE: i32 = -1;
/// Vanilla `Display.NO_GLOW_COLOR_OVERRIDE`.
const NO_GLOW_COLOR_OVERRIDE: i32 = -1;
/// The longest teleport interpolation vanilla accepts from saved data.
const MAX_TELEPORT_DURATION: i32 = 59;

/// Vanilla `Display.BillboardConstraints`: which axes turn to face the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillboardConstraints {
    /// Never turns.
    Fixed,
    /// Turns around the vertical axis.
    Vertical,
    /// Tilts around the horizontal axis.
    Horizontal,
    /// Always faces the camera.
    Center,
}

impl BillboardConstraints {
    const ALL: [Self; 4] = [Self::Fixed, Self::Vertical, Self::Horizontal, Self::Center];

    /// The synced byte id.
    #[must_use]
    pub const fn id(self) -> i8 {
        self as i8
    }

    /// Vanilla `BY_ID`: out-of-range ids fall back to `FIXED`.
    #[must_use]
    pub fn by_id(id: i8) -> Self {
        usize::try_from(id)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
            .unwrap_or(Self::Fixed)
    }

    /// The serialized name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
            Self::Center => "center",
        }
    }

    fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.name() == name)
    }
}

/// Vanilla `Brightness`: a fixed block and sky light level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brightness {
    /// Block light, `0..=15`.
    pub block: i32,
    /// Sky light, `0..=15`.
    pub sky: i32,
}

impl Brightness {
    /// Vanilla `Brightness.pack` (`LightCoordsUtil.pack`).
    #[must_use]
    pub const fn pack(self) -> i32 {
        self.block << 4 | self.sky << 20
    }

    /// Vanilla `Brightness.unpack`.
    #[must_use]
    pub const fn unpack(packed: i32) -> Self {
        Self {
            block: packed >> 4 & 15,
            sky: packed >> 20 & 15,
        }
    }

    /// Vanilla `Brightness.CODEC`: both levels required and within `0..=15`.
    fn decode(tag: &NbtTag) -> Option<Self> {
        let NbtTag::Compound(compound) = tag else {
            return None;
        };
        let level = |key| {
            compound
                .get(key)
                .and_then(NbtNumeric::codec_i32)
                .filter(|value| (0..=15).contains(value))
        };
        Some(Self {
            block: level("block")?,
            sky: level("sky")?,
        })
    }

    fn encode(self) -> NbtCompound {
        let mut compound = NbtCompound::new();
        compound.insert("block", self.block);
        compound.insert("sky", self.sky);
        compound
    }
}

/// Vanilla `Display.tick`: displays drop off a vehicle that was removed.
/// Unlike other entities they do not run the base entity tick.
pub(super) fn tick_display(display: &dyn Entity) {
    if display
        .vehicle()
        .is_some_and(|vehicle| vehicle.is_removed())
    {
        display.stop_riding();
    }
}

/// Vanilla `Display.createTransformation` from the synced parts.
#[must_use]
pub(super) const fn transformation(data: &DisplayEntityData) -> Transformation {
    Transformation {
        translation: *data.translation.get(),
        left_rotation: *data.left_rotation.get(),
        scale: *data.scale.get(),
        right_rotation: *data.right_rotation.get(),
    }
}

/// Vanilla `Display.setTransformation`.
pub(super) fn set_transformation(data: &mut DisplayEntityData, transformation: Transformation) {
    data.translation.set(transformation.translation);
    data.left_rotation.set(transformation.left_rotation);
    data.scale.set(transformation.scale);
    data.right_rotation.set(transformation.right_rotation);
}

/// Vanilla `Display.getBrightnessOverride`.
#[must_use]
pub(super) fn brightness_override(data: &DisplayEntityData) -> Option<Brightness> {
    let packed = *data.brightness_override.get();
    (packed != NO_BRIGHTNESS_OVERRIDE).then(|| Brightness::unpack(packed))
}

/// Vanilla `Display.addAdditionalSaveData`.
pub(super) fn save_display(data: &DisplayEntityData, nbt: &mut NbtCompound) {
    nbt.insert(TAG_TRANSFORMATION, transformation(data).encode());
    nbt.insert(
        TAG_BILLBOARD,
        BillboardConstraints::by_id(*data.billboard_render_constraints.get()).name(),
    );
    nbt.insert(
        TAG_INTERPOLATION_DURATION,
        *data.transformation_interpolation_duration.get(),
    );
    nbt.insert(
        TAG_TELEPORT_DURATION,
        *data.pos_rot_interpolation_duration.get(),
    );
    nbt.insert(TAG_VIEW_RANGE, *data.view_range.get());
    nbt.insert(TAG_SHADOW_RADIUS, *data.shadow_radius.get());
    nbt.insert(TAG_SHADOW_STRENGTH, *data.shadow_strength.get());
    nbt.insert(TAG_WIDTH, *data.width.get());
    nbt.insert(TAG_HEIGHT, *data.height.get());
    nbt.insert(TAG_GLOW_COLOR_OVERRIDE, *data.glow_color_override.get());
    if let Some(brightness) = brightness_override(data) {
        nbt.insert(TAG_BRIGHTNESS, brightness.encode());
    }
}

/// Vanilla `Display.readAdditionalSaveData`.
pub(super) fn load_display(data: &mut DisplayEntityData, nbt: BorrowedNbtCompoundView<'_, '_>) {
    let int_or = |key, default| {
        nbt.get(key)
            .and_then(|tag| tag.int_value())
            .unwrap_or(default)
    };
    let float_or = |key, default| {
        nbt.get(key)
            .and_then(|tag| tag.codec_f32())
            .unwrap_or(default)
    };

    set_transformation(
        data,
        nbt.get(TAG_TRANSFORMATION)
            .and_then(|tag| Transformation::decode(&tag.to_owned()))
            .unwrap_or(Transformation::IDENTITY),
    );
    data.transformation_interpolation_duration
        .set(int_or(TAG_INTERPOLATION_DURATION, 0));
    data.transformation_interpolation_start_delta_ticks
        .set_forced(int_or(TAG_START_INTERPOLATION, 0));
    data.pos_rot_interpolation_duration
        .set(int_or(TAG_TELEPORT_DURATION, 0).clamp(0, MAX_TELEPORT_DURATION));
    data.billboard_render_constraints.set(
        nbt.string(TAG_BILLBOARD)
            .and_then(|name| BillboardConstraints::by_name(&name.to_str()))
            .unwrap_or(BillboardConstraints::Fixed)
            .id(),
    );
    data.view_range.set(float_or(TAG_VIEW_RANGE, 1.0));
    data.shadow_radius.set(float_or(TAG_SHADOW_RADIUS, 0.0));
    data.shadow_strength.set(float_or(TAG_SHADOW_STRENGTH, 1.0));
    data.width.set(float_or(TAG_WIDTH, 0.0));
    data.height.set(float_or(TAG_HEIGHT, 0.0));
    data.glow_color_override
        .set(int_or(TAG_GLOW_COLOR_OVERRIDE, NO_GLOW_COLOR_OVERRIDE));
    data.brightness_override.set(
        nbt.get(TAG_BRIGHTNESS)
            .and_then(|tag| Brightness::decode(&tag.to_owned()))
            .map_or(NO_BRIGHTNESS_OVERRIDE, Brightness::pack),
    );
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::entity_data::{Quaternionf, Vector3f};

    use super::*;

    /// Writes `nbt` and reads it back as the borrowed view loaders take.
    pub(in super::super) fn with_borrowed<T>(
        nbt: &NbtCompound,
        read: impl FnOnce(BorrowedNbtCompoundView<'_, '_>) -> T,
    ) -> T {
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed =
            read_compound(&mut Cursor::new(bytes.as_slice())).expect("test NBT should reborrow");
        read((&borrowed).into())
    }

    #[test]
    fn defaults_save_the_vanilla_shape_without_brightness() {
        let mut nbt = NbtCompound::new();
        save_display(&DisplayEntityData::new(), &mut nbt);

        assert_eq!(
            nbt.string("billboard").map(|name| name.to_str()),
            Some("fixed".into())
        );
        assert_eq!(nbt.float("view_range"), Some(1.0));
        assert_eq!(nbt.float("shadow_strength"), Some(1.0));
        assert_eq!(nbt.int("glow_color_override"), Some(-1));
        assert!(nbt.compound("transformation").is_some());
        assert!(!nbt.contains("brightness"));
        assert!(!nbt.contains("start_interpolation"));
    }

    #[test]
    fn saved_state_round_trips_and_clamps_teleport_duration() {
        let mut data = DisplayEntityData::new();
        set_transformation(
            &mut data,
            Transformation {
                translation: Vector3f::new(0.5, 1.0, -0.5),
                left_rotation: Quaternionf::new(0.0, 0.6, 0.0, 0.8),
                scale: Vector3f::new(2.0, 3.0, 4.0),
                right_rotation: Quaternionf::IDENTITY,
            },
        );
        data.billboard_render_constraints
            .set(BillboardConstraints::Center.id());
        data.brightness_override
            .set(Brightness { block: 12, sky: 3 }.pack());
        data.view_range.set(2.5);
        data.glow_color_override.set(0x00FF_00FF);
        data.pos_rot_interpolation_duration.set(90);
        let mut nbt = NbtCompound::new();
        save_display(&data, &mut nbt);

        let mut loaded = DisplayEntityData::new();
        with_borrowed(&nbt, |view| load_display(&mut loaded, view));

        assert_eq!(transformation(&loaded), transformation(&data));
        assert_eq!(
            BillboardConstraints::by_id(*loaded.billboard_render_constraints.get()),
            BillboardConstraints::Center
        );
        assert_eq!(
            brightness_override(&loaded),
            Some(Brightness { block: 12, sky: 3 })
        );
        assert_eq!(*loaded.view_range.get(), 2.5);
        assert_eq!(*loaded.glow_color_override.get(), 0x00FF_00FF);
        assert_eq!(*loaded.pos_rot_interpolation_duration.get(), 59);
    }

    #[test]
    fn invalid_brightness_and_billboard_fall_back_to_defaults() {
        let mut nbt = NbtCompound::new();
        let mut brightness = NbtCompound::new();
        brightness.insert("block", 16);
        brightness.insert("sky", 0);
        nbt.insert("brightness", brightness);
        nbt.insert("billboard", "sideways");

        let mut loaded = DisplayEntityData::new();
        loaded
            .brightness_override
            .set(Brightness { block: 1, sky: 1 }.pack());
        with_borrowed(&nbt, |view| load_display(&mut loaded, view));

        assert_eq!(brightness_override(&loaded), None);
        assert_eq!(*loaded.billboard_render_constraints.get(), 0);
    }

    #[test]
    fn start_interpolation_is_resent_even_when_unchanged() {
        let mut nbt = NbtCompound::new();
        nbt.insert("start_interpolation", 0);
        let mut data = DisplayEntityData::new();
        with_borrowed(&nbt, |view| load_display(&mut data, view));

        assert!(
            data.transformation_interpolation_start_delta_ticks
                .is_dirty(),
            "vanilla forces the start delay so clients restart the interpolation"
        );
    }
}
