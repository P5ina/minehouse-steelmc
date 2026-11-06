//! Vanilla shoulder riding: tamed parrots perch on their owner's shoulders.
//! A perched parrot leaves the world and lives in the player's data as its
//! saved entity tag until it hops back off.

use std::io::Cursor;
use std::str::FromStr as _;

use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_variant::ParrotVariant;
use steel_registry::vanilla_entities;
use steel_utils::Identifier;

use crate::entity::entities::ParrotEntity;
use crate::entity::{Entity, EntitySpawnReason, LivingEntity, load_entity_recursive_owned};
use crate::player::Player;

/// Ticks after a parrot lands during which it cannot be shaken off.
const SHOULDER_SETTLE_TICKS: i64 = 20;
/// Vanilla `fallDistance > 0.5` throws the parrots off.
const SHOULDER_DROP_FALL_DISTANCE: f64 = 0.5;
/// Height above the player's feet at which a parrot reappears.
const SHOULDER_RESPAWN_HEIGHT: f64 = 0.7_f32 as f64;
/// One perched parrot in this many makes a sound each tick.
const SHOULDER_AMBIENT_CHANCE: u32 = 200;

/// The entities on a player's shoulders, as vanilla `CompoundTag`s.
#[derive(Debug, Default, Clone)]
pub(crate) struct ShoulderEntities {
    left: NbtCompound,
    right: NbtCompound,
    /// Vanilla `timeEntitySatOnShoulder`.
    time_sat: i64,
}

/// Vanilla `extractParrotVariant`: the variant id of a parrot tag.
fn parrot_variant_of(tag: &NbtCompound) -> Option<u32> {
    if !is_parrot_tag(tag) {
        return None;
    }
    let variant = tag.int("Variant")?;
    Some(ParrotVariant::by_id(variant).id() as u32)
}

fn is_parrot_tag(tag: &NbtCompound) -> bool {
    tag.string("id")
        .and_then(|id| Identifier::from_str(id.to_str().as_ref()).ok())
        .is_some_and(|id| id == vanilla_entities::PARROT.key)
}

/// Encodes a shoulder tag for player storage; an empty tag stores nothing.
pub(crate) fn shoulder_tag_to_bytes(tag: &NbtCompound) -> Vec<u8> {
    if tag.is_empty() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    tag.write(&mut bytes);
    bytes
}

/// Decodes a stored shoulder tag; unreadable bytes give an empty shoulder.
pub(crate) fn shoulder_tag_from_bytes(bytes: &[u8]) -> NbtCompound {
    if bytes.is_empty() {
        return NbtCompound::new();
    }
    read_borrowed_compound(&mut Cursor::new(bytes)).map_or_else(
        |_| NbtCompound::new(),
        |compound| simdnbt::borrow::NbtCompound::from(&compound).to_owned(),
    )
}

impl Player {
    /// Vanilla `getShoulderEntityLeft`.
    #[must_use]
    pub fn shoulder_entity_left(&self) -> NbtCompound {
        self.shoulder_entities.lock().left.clone()
    }

    /// Vanilla `getShoulderEntityRight`.
    #[must_use]
    pub fn shoulder_entity_right(&self) -> NbtCompound {
        self.shoulder_entities.lock().right.clone()
    }

    /// Vanilla `setShoulderEntityLeft`: also shows the parrot to clients.
    pub(crate) fn set_shoulder_entity_left(&self, tag: NbtCompound) {
        let variant = parrot_variant_of(&tag);
        self.shoulder_entities.lock().left = tag;
        self.entity_data.lock().shoulder_parrot_left.set(variant);
    }

    /// Vanilla `setShoulderEntityRight`: also shows the parrot to clients.
    pub(crate) fn set_shoulder_entity_right(&self, tag: NbtCompound) {
        let variant = parrot_variant_of(&tag);
        self.shoulder_entities.lock().right = tag;
        self.entity_data.lock().shoulder_parrot_right.set(variant);
    }

    /// Vanilla `setEntityOnShoulder`: perches the entity on the free shoulder,
    /// left first. Returns `false` when the player cannot carry it now.
    pub fn set_entity_on_shoulder(&self, tag: NbtCompound) -> bool {
        if self.is_passenger()
            || !self.on_ground()
            || self.is_in_water()
            || self.is_in_powder_snow()
        {
            return false;
        }
        let game_time = self.get_world().game_time();
        let mut shoulders = self.shoulder_entities.lock();
        if shoulders.left.is_empty() {
            shoulders.time_sat = game_time;
            drop(shoulders);
            self.set_shoulder_entity_left(tag);
            true
        } else if shoulders.right.is_empty() {
            shoulders.time_sat = game_time;
            drop(shoulders);
            self.set_shoulder_entity_right(tag);
            true
        } else {
            false
        }
    }

    /// Vanilla `removeEntitiesOnShoulder`: puts both perched entities back in
    /// the world, unless they only just landed.
    pub(crate) fn remove_entities_on_shoulder(&self) {
        let world = self.get_world();
        if self.shoulder_entities.lock().time_sat + SHOULDER_SETTLE_TICKS >= world.game_time() {
            return;
        }
        self.respawn_entity_on_shoulder(&self.shoulder_entity_left());
        self.set_shoulder_entity_left(NbtCompound::new());
        self.respawn_entity_on_shoulder(&self.shoulder_entity_right());
        self.set_shoulder_entity_right(NbtCompound::new());
    }

    /// Vanilla `respawnEntityOnShoulder`.
    fn respawn_entity_on_shoulder(&self, tag: &NbtCompound) {
        if tag.is_empty() {
            return;
        }
        let world = self.get_world();
        let Some(loaded) =
            load_entity_recursive_owned(&world, tag, EntitySpawnReason::Load, |_| {})
        else {
            return;
        };
        let entity = loaded.root;
        if let Some(tamable) = entity.as_tamable_animal() {
            tamable.set_owner_uuid(Some(self.uuid()));
        }
        let position = self.position();
        let (yaw, pitch) = entity.rotation();
        entity.snap_to(
            glam::DVec3::new(position.x, position.y + SHOULDER_RESPAWN_HEIGHT, position.z),
            yaw,
            pitch,
        );
        if let Err(error) = world.try_add_entity(entity) {
            tracing::debug!(?error, "Could not put a shoulder entity back in the world");
        }
    }

    /// Vanilla `handleShoulderEntities`, run at the end of the player's AI step.
    pub(crate) fn handle_shoulder_entities(&self) {
        self.play_shoulder_entity_ambient_sound(&self.shoulder_entity_left());
        self.play_shoulder_entity_ambient_sound(&self.shoulder_entity_right());
        if self.fall_distance() > SHOULDER_DROP_FALL_DISTANCE
            || self.is_in_water()
            || self.is_flying()
            || self.is_sleeping()
            || self.is_in_powder_snow()
        {
            self.remove_entities_on_shoulder();
        }
    }

    /// Vanilla `playShoulderEntityAmbientSound`: a perched parrot now and then
    /// imitates a nearby mob, or else makes its own sound.
    fn play_shoulder_entity_ambient_sound(&self, tag: &NbtCompound) {
        if tag.is_empty() || tag.byte("Silent").is_some_and(|silent| silent != 0) {
            return;
        }
        if rand::random_range(0..SHOULDER_AMBIENT_CHANCE) != 0 || !is_parrot_tag(tag) {
            return;
        }
        let world = self.get_world();
        if !ParrotEntity::imitate_nearby_mobs(&world, self) {
            world.play_sound_at(
                ParrotEntity::ambient_sound_for(&world),
                self.sound_source(),
                self.position(),
                1.0,
                ParrotEntity::random_pitch(),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use simdnbt::owned::NbtCompound;

    use super::{parrot_variant_of, shoulder_tag_from_bytes, shoulder_tag_to_bytes};

    fn parrot_tag(variant: i32) -> NbtCompound {
        let mut tag = NbtCompound::new();
        tag.insert("id", "minecraft:parrot");
        tag.insert("Variant", variant);
        tag
    }

    #[test]
    fn only_parrot_tags_show_a_shoulder_parrot() {
        assert_eq!(parrot_variant_of(&parrot_tag(2)), Some(2));
        assert_eq!(parrot_variant_of(&parrot_tag(99)), Some(4));
        let mut wolf = NbtCompound::new();
        wolf.insert("id", "minecraft:wolf");
        wolf.insert("Variant", 1);
        assert_eq!(parrot_variant_of(&wolf), None);
        assert_eq!(parrot_variant_of(&NbtCompound::new()), None);
    }

    #[test]
    fn shoulder_tags_round_trip_through_storage() {
        assert_eq!(shoulder_tag_to_bytes(&NbtCompound::new()), Vec::<u8>::new());
        let tag = parrot_tag(3);
        let restored = shoulder_tag_from_bytes(&shoulder_tag_to_bytes(&tag));
        assert_eq!(parrot_variant_of(&restored), Some(3));
        assert!(shoulder_tag_from_bytes(&[1, 2, 3]).is_empty());
    }
}
