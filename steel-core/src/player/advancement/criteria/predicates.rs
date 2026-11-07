//! Vanilla advancement predicates (`net.minecraft.advancements.predicates`)
//! and the loot conditions criteria use for their entity and location fields.
//!
//! The predicates are decoded from the datapack's JSON. Parts Steel cannot
//! evaluate yet decode as [`EntitySubPredicate::Unsupported`] and similar,
//! which never match, so an advancement is never granted for a condition
//! Steel did not check.

use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::sync::Arc;

use glam::DVec3;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use simdnbt::owned::NbtTag;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_predicate::{
    BlockPredicate, DoubleBounds, IntBounds, ItemPredicate, StatePropertiesPredicate,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::{REGISTRY, RegistryExt as _, TaggedRegistryExt as _};
use steel_utils::nbt::json_to_nbt;
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Identifier};

use crate::behavior::blocks::CampfireBlock;
use crate::command::execution::registered_item_predicate_matches;
use crate::entity::{Entity, LivingEntity};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;

/// Decodes a registry predicate through its NBT codec, vanilla
/// `JsonOps.convertTo(NbtOps.INSTANCE, ...)` followed by the codec.
fn decode_nbt<'de, D, T>(deserializer: D, decode: fn(&NbtTag) -> Option<T>) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    json_to_nbt(&value)
        .as_ref()
        .and_then(decode)
        .ok_or_else(|| D::Error::custom(format!("invalid predicate: {value}")))
}

/// Vanilla `MinMaxBounds.Ints`, decoded from a number or `{min, max}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ints(pub(crate) IntBounds);

impl Default for Ints {
    fn default() -> Self {
        Self(IntBounds::ANY)
    }
}

impl Ints {
    /// Vanilla `MinMaxBounds.Ints.matches`.
    pub(crate) fn matches(self, value: i32) -> bool {
        self.0.min().is_none_or(|min| min <= value) && self.0.max().is_none_or(|max| value <= max)
    }
}

impl<'de> Deserialize<'de> for Ints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_nbt(deserializer, IntBounds::from_owned_nbt).map(Self)
    }
}

/// Vanilla `MinMaxBounds.Doubles`, decoded from a number or `{min, max}`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Doubles(pub(crate) DoubleBounds);

impl Default for Doubles {
    fn default() -> Self {
        Self(DoubleBounds::ANY)
    }
}

impl Doubles {
    /// Vanilla `MinMaxBounds.Doubles.matches`.
    pub(crate) fn matches(self, value: f64) -> bool {
        self.0.min().is_none_or(|min| min <= value) && self.0.max().is_none_or(|max| value <= max)
    }

    /// Vanilla `MinMaxBounds.Doubles.matchesSqr`: tests a squared value
    /// against the squared bounds.
    pub(crate) fn matches_sqr(self, value_sqr: f64) -> bool {
        self.0.min().is_none_or(|min| min * min <= value_sqr)
            && self.0.max().is_none_or(|max| value_sqr <= max * max)
    }
}

impl<'de> Deserialize<'de> for Doubles {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_nbt(deserializer, DoubleBounds::from_owned_nbt).map(Self)
    }
}

/// Vanilla `ItemPredicate`, decoded and evaluated by the registry's
/// item predicate.
#[derive(Debug, Clone)]
pub(crate) struct ItemCondition(ItemPredicate);

impl ItemCondition {
    /// Vanilla `ItemPredicate.test`.
    pub(crate) fn test(&self, stack: &ItemStack) -> bool {
        registered_item_predicate_matches(&self.0, stack)
    }
}

impl<'de> Deserialize<'de> for ItemCondition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_nbt(deserializer, ItemPredicate::from_owned_nbt).map(Self)
    }
}

/// Vanilla `BlockPredicate`.
#[derive(Debug, Clone)]
pub(crate) struct BlockCondition(BlockPredicate);

impl BlockCondition {
    /// Vanilla `BlockPredicate.matchesState`.
    pub(crate) fn matches_state(&self, state: BlockStateId) -> bool {
        self.0.matches_state(state)
    }

    /// Vanilla `BlockPredicate.matches(level, pos)`. Block entity data and
    /// components are not checked yet, so a predicate on them never matches.
    pub(crate) fn matches(&self, world: &World, pos: BlockPos) -> bool {
        is_loaded(world, pos)
            && self.matches_state(world.get_block_state(pos))
            && self.0.nbt().is_none()
            && self.0.components().is_empty()
    }
}

impl<'de> Deserialize<'de> for BlockCondition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_nbt(deserializer, BlockPredicate::from_owned_nbt).map(Self)
    }
}

/// Vanilla `StatePropertiesPredicate`.
#[derive(Debug, Clone)]
pub(crate) struct StateCondition(StatePropertiesPredicate);

impl StateCondition {
    /// Vanilla `StatePropertiesPredicate.matches(BlockState)`.
    pub(crate) fn matches(&self, state: BlockStateId) -> bool {
        self.0.matches_block_state(state)
    }
}

impl<'de> Deserialize<'de> for StateCondition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        decode_nbt(deserializer, StatePropertiesPredicate::from_owned_nbt).map(Self)
    }
}

/// A registry holder set as data packs write it: an id, a `#tag`, or a list
/// of ids (vanilla `RegistryCodecs.holderSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdSet {
    /// `#namespace:path`
    Tag(Identifier),
    /// One or more ids.
    Direct(Vec<Identifier>),
}

impl IdSet {
    /// Whether `key` is in the set, with `in_tag` answering tag membership.
    pub(crate) fn contains(
        &self,
        key: &Identifier,
        in_tag: impl FnOnce(&Identifier) -> bool,
    ) -> bool {
        match self {
            Self::Tag(tag) => in_tag(tag),
            Self::Direct(keys) => keys.contains(key),
        }
    }
}

impl<'de> Deserialize<'de> for IdSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<Identifier>),
        }
        match Raw::deserialize(deserializer)? {
            Raw::One(value) => {
                if let Some(tag) = value.strip_prefix('#') {
                    Identifier::from_str(tag)
                        .map(Self::Tag)
                        .map_err(D::Error::custom)
                } else {
                    Identifier::from_str(&value)
                        .map(|key| Self::Direct(vec![key]))
                        .map_err(D::Error::custom)
                }
            }
            Raw::Many(keys) => Ok(Self::Direct(keys)),
        }
    }
}

/// Vanilla `Level.isLoaded`.
fn is_loaded(world: &World, pos: BlockPos) -> bool {
    world.has_full_chunk(ChunkPos::from_block_pos(pos))
}

/// Vanilla `BlockPos.containing`.
const fn block_pos_containing(x: f64, y: f64, z: f64) -> BlockPos {
    BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
}

/// Vanilla `LocationPredicate.PositionPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PositionPredicate {
    #[serde(default)]
    x: Doubles,
    #[serde(default)]
    y: Doubles,
    #[serde(default)]
    z: Doubles,
}

/// Vanilla `LightPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LightPredicate {
    #[serde(default)]
    light: Ints,
}

/// Vanilla `FluidPredicate`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FluidPredicate {
    fluids: Option<IdSet>,
    state: Option<Value>,
}

/// Vanilla `LocationPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocationPredicate {
    position: Option<PositionPredicate>,
    biomes: Option<IdSet>,
    structures: Option<IdSet>,
    dimension: Option<Identifier>,
    smokey: Option<bool>,
    light: Option<LightPredicate>,
    block: Option<BlockCondition>,
    fluid: Option<FluidPredicate>,
    can_see_sky: Option<bool>,
}

impl LocationPredicate {
    /// Vanilla `LocationPredicate.matches(level, x, y, z)`.
    pub(crate) fn matches(&self, world: &World, x: f64, y: f64, z: f64) -> bool {
        if let Some(position) = &self.position
            && !(position.x.matches(x) && position.y.matches(y) && position.z.matches(z))
        {
            return false;
        }
        if let Some(dimension) = &self.dimension
            && *dimension != world.key
        {
            return false;
        }
        let pos = block_pos_containing(x, y, z);
        let loaded = is_loaded(world, pos);
        if let Some(biomes) = &self.biomes {
            let in_biomes = loaded
                && world
                    .biome_at(pos)
                    .is_some_and(|biome| biomes.contains(&biome.key, |tag| biome.has_tag(tag)));
            if !in_biomes {
                return false;
            }
        }
        if let Some(structures) = &self.structures {
            let in_structure = loaded
                && match structures {
                    IdSet::Tag(tag) => world.structure_with_piece_at(&format!("#{tag}"), pos),
                    IdSet::Direct(keys) => keys
                        .iter()
                        .any(|key| world.structure_with_piece_at(&key.to_string(), pos)),
                };
            if !in_structure {
                return false;
            }
        }
        if let Some(smokey) = self.smokey
            && !(loaded && smokey == CampfireBlock::is_smokey_pos(world, pos))
        {
            return false;
        }
        // Light and fluid checks are not used by vanilla advancements and are
        // not evaluated yet: a predicate on them never matches.
        if self.light.is_some() || self.fluid.is_some() {
            return false;
        }
        if let Some(block) = &self.block
            && !block.matches(world, pos)
        {
            return false;
        }
        self.can_see_sky
            .is_none_or(|can_see_sky| can_see_sky == world.can_see_sky(pos))
    }
}

/// Vanilla `DistancePredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DistancePredicate {
    #[serde(default)]
    x: Doubles,
    #[serde(default)]
    y: Doubles,
    #[serde(default)]
    z: Doubles,
    #[serde(default)]
    horizontal: Doubles,
    #[serde(default)]
    absolute: Doubles,
}

impl DistancePredicate {
    /// Vanilla `DistancePredicate.matches`, which measures in `float`s.
    pub(crate) fn matches(&self, from: DVec3, to: DVec3) -> bool {
        let dx = (from.x - to.x) as f32;
        let dy = (from.y - to.y) as f32;
        let dz = (from.z - to.z) as f32;
        self.x.matches(f64::from(dx.abs()))
            && self.y.matches(f64::from(dy.abs()))
            && self.z.matches(f64::from(dz.abs()))
            && self.horizontal.matches_sqr(f64::from(dx * dx + dz * dz))
            && self
                .absolute
                .matches_sqr(f64::from(dx * dx + dy * dy + dz * dz))
    }
}

/// Vanilla `MobEffectsPredicate.MobEffectInstancePredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MobEffectInstancePredicate {
    #[serde(default)]
    amplifier: Ints,
    #[serde(default)]
    duration: Ints,
    ambient: Option<bool>,
    visible: Option<bool>,
}

/// Vanilla `MobEffectsPredicate`: every listed effect must be active and
/// match its instance predicate.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct MobEffectsPredicate(BTreeMap<Identifier, MobEffectInstancePredicate>);

impl MobEffectsPredicate {
    /// Vanilla `MobEffectsPredicate.matches(Entity)`.
    pub(crate) fn matches(&self, entity: &dyn Entity) -> bool {
        let Some(living) = entity.as_living_entity() else {
            return false;
        };
        self.0.iter().all(|(key, predicate)| {
            let Some(effect) = REGISTRY.mob_effects.by_key(key) else {
                return false;
            };
            living.mob_effect(effect).is_some_and(|instance| {
                predicate.amplifier.matches(instance.amplifier())
                    && predicate.duration.matches(instance.duration())
                    && predicate
                        .ambient
                        .is_none_or(|ambient| ambient == instance.is_ambient())
                    && predicate
                        .visible
                        .is_none_or(|visible| visible == instance.is_visible())
            })
        })
    }
}

/// Vanilla `EntityFlagsPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntityFlagsPredicate {
    is_on_ground: Option<bool>,
    is_on_fire: Option<bool>,
    is_sneaking: Option<bool>,
    is_sprinting: Option<bool>,
    is_swimming: Option<bool>,
    is_flying: Option<bool>,
    is_baby: Option<bool>,
    is_in_water: Option<bool>,
    is_fall_flying: Option<bool>,
}

impl EntityFlagsPredicate {
    /// Vanilla `EntityFlagsPredicate.matches(Entity)`.
    fn matches(&self, entity: &dyn Entity) -> bool {
        let living = entity.as_living_entity();
        self.is_on_ground.is_none_or(|v| v == entity.on_ground())
            && self.is_on_fire.is_none_or(|v| v == entity.is_on_fire())
            && self.is_sneaking.is_none_or(|v| v == entity.is_crouching())
            && self
                .is_sprinting
                .is_none_or(|v| living.is_some_and(LivingEntity::is_sprinting) == v)
            && self.is_swimming.is_none_or(|v| v == entity.is_swimming())
            && self.is_flying.is_none_or(|v| {
                let flying = living.is_some_and(|living| {
                    living.is_fall_flying()
                        || entity
                            .as_player()
                            .is_some_and(|player| player.abilities.lock().flying)
                });
                flying == v
            })
            && self.is_in_water.is_none_or(|v| v == entity.is_in_water())
            && self
                .is_fall_flying
                .is_none_or(|v| living.is_none_or(|living| living.is_fall_flying() == v))
            && self
                .is_baby
                .is_none_or(|v| living.is_none_or(|living| living.is_baby() == v))
    }
}

/// Vanilla `EntityEquipmentPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntityEquipmentPredicate {
    head: Option<ItemCondition>,
    chest: Option<ItemCondition>,
    legs: Option<ItemCondition>,
    feet: Option<ItemCondition>,
    body: Option<ItemCondition>,
    mainhand: Option<ItemCondition>,
    offhand: Option<ItemCondition>,
}

impl EntityEquipmentPredicate {
    /// Vanilla `EntityEquipmentPredicate.matches(Entity)`: only living
    /// entities have equipment.
    fn matches(&self, entity: &dyn Entity) -> bool {
        let Some(living) = entity.as_living_entity() else {
            return false;
        };
        [
            (&self.head, EquipmentSlot::Head),
            (&self.chest, EquipmentSlot::Chest),
            (&self.legs, EquipmentSlot::Legs),
            (&self.feet, EquipmentSlot::Feet),
            (&self.body, EquipmentSlot::Body),
            (&self.mainhand, EquipmentSlot::MainHand),
            (&self.offhand, EquipmentSlot::OffHand),
        ]
        .into_iter()
        .all(|(predicate, slot)| {
            predicate.as_ref().is_none_or(|predicate| {
                let mut matched = false;
                living.with_equipment_slot(slot, &mut |stack| matched = predicate.test(stack));
                matched
            })
        })
    }
}

/// Vanilla `PlayerPredicate`, limited to the fields Steel can check.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayerPredicate {
    #[serde(default)]
    level: Ints,
    gamemode: Option<Value>,
    food: Option<Value>,
    stats: Option<Value>,
    recipes: Option<Value>,
    advancements: Option<Value>,
    looking_at: Option<Box<EntityPredicate>>,
    input: Option<Value>,
}

impl PlayerPredicate {
    /// Vanilla `PlayerPredicate.matches`. Fields Steel does not check yet
    /// make the predicate fail.
    fn matches(&self, entity: &dyn Entity) -> bool {
        let Some(player) = entity.as_player() else {
            return false;
        };
        self.gamemode.is_none()
            && self.food.is_none()
            && self.stats.is_none()
            && self.recipes.is_none()
            && self.advancements.is_none()
            && self.looking_at.is_none()
            && self.input.is_none()
            && self.level.matches(player.experience.lock().level())
    }
}

/// Vanilla `RaiderPredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RaiderPredicate {
    #[serde(default)]
    has_raid: bool,
    #[serde(default)]
    is_captain: bool,
}

/// One entry of a vanilla `EntityPredicate`'s sub-predicate map.
#[derive(Debug, Clone)]
pub(crate) enum EntitySubPredicate {
    EntityType(IdSet),
    Location(Box<LocationPredicate>),
    SteppingOn(Box<LocationPredicate>),
    MovementAffectedBy(Box<LocationPredicate>),
    Distance(Box<DistancePredicate>),
    Effects(MobEffectsPredicate),
    Flags(Box<EntityFlagsPredicate>),
    Equipment(Box<EntityEquipmentPredicate>),
    PeriodicTick(i32),
    Vehicle(Box<EntityPredicate>),
    Passenger(Box<EntityPredicate>),
    Player(PlayerPredicate),
    Raider(RaiderPredicate),
    /// A sub-predicate Steel does not evaluate yet; it never matches.
    Unsupported(String),
}

impl EntitySubPredicate {
    fn decode(key: &str, value: Value) -> Result<Self, serde_json::Error> {
        let name = key.strip_prefix("minecraft:").unwrap_or(key);
        Ok(match name {
            "entity_type" => Self::EntityType(serde_json::from_value(value)?),
            "location" => Self::Location(serde_json::from_value(value)?),
            "stepping_on" => Self::SteppingOn(serde_json::from_value(value)?),
            "movement_affected_by" => Self::MovementAffectedBy(serde_json::from_value(value)?),
            "distance" => Self::Distance(serde_json::from_value(value)?),
            "effects" => Self::Effects(serde_json::from_value(value)?),
            "flags" => Self::Flags(serde_json::from_value(value)?),
            "equipment" => Self::Equipment(serde_json::from_value(value)?),
            "periodic_tick" => Self::PeriodicTick(serde_json::from_value(value)?),
            "vehicle" => Self::Vehicle(Box::new(serde_json::from_value(value)?)),
            "passenger" => Self::Passenger(Box::new(serde_json::from_value(value)?)),
            "type_specific/player" => Self::Player(serde_json::from_value(value)?),
            "type_specific/raider" => Self::Raider(serde_json::from_value(value)?),
            "movement"
            | "nbt"
            | "targeted_entity"
            | "team"
            | "slots"
            | "components"
            | "predicates"
            | "entity_tags"
            | "type_specific/lightning"
            | "type_specific/fishing_hook"
            | "type_specific/cube_mob"
            | "type_specific/sheep" => Self::Unsupported(name.to_owned()),
            _ => {
                return Err(serde_json::Error::custom(format!(
                    "unknown entity sub-predicate {key}"
                )));
            }
        })
    }

    /// Vanilla `EntitySubPredicate.matches(entity, level, position)`.
    fn matches(&self, entity: &dyn Entity, world: &Arc<World>, origin: Option<DVec3>) -> bool {
        match self {
            Self::EntityType(types) => {
                let entity_type = entity.entity_type();
                types.contains(&entity_type.key, |tag| {
                    REGISTRY.entity_types.is_in_tag(entity_type, tag)
                })
            }
            Self::Location(location) => {
                let position = entity.position();
                location.matches(world, position.x, position.y, position.z)
            }
            Self::SteppingOn(location) => {
                // Vanilla `SteppingOnPredicate`: the center of `getOnPos()`.
                entity.on_ground()
                    && entity.on_pos(1.0e-5).is_some_and(|pos| {
                        let (x, y, z) = pos.get_center();
                        location.matches(world, x, y, z)
                    })
            }
            Self::MovementAffectedBy(location) => entity
                .block_pos_below_that_affects_movement()
                .is_some_and(|pos| {
                    let (x, y, z) = pos.get_center();
                    location.matches(world, x, y, z)
                }),
            Self::Distance(distance) => {
                origin.is_some_and(|origin| distance.matches(origin, entity.position()))
            }
            Self::Effects(effects) => effects.matches(entity),
            Self::Flags(flags) => flags.matches(entity),
            Self::Equipment(equipment) => equipment.matches(entity),
            Self::PeriodicTick(period) => *period > 0 && entity.tick_count() % period == 0,
            Self::Vehicle(vehicle) => entity
                .vehicle()
                .is_some_and(|vehicle_entity| vehicle.matches(world, origin, &*vehicle_entity)),
            Self::Passenger(passenger) => entity
                .passengers()
                .iter()
                .any(|passenger_entity| passenger.matches(world, origin, &**passenger_entity)),
            Self::Player(player) => player.matches(entity),
            Self::Raider(raider) => entity.as_raider().is_some_and(|entity_raider| {
                entity_raider.has_raid() == raider.has_raid
                    && entity_raider.is_captain() == raider.is_captain
            }),
            Self::Unsupported(_) => false,
        }
    }
}

/// Vanilla `EntityPredicate`: a map of sub-predicates that must all match.
#[derive(Debug, Clone, Default)]
pub(crate) struct EntityPredicate(Vec<EntitySubPredicate>);

impl EntityPredicate {
    /// Vanilla `EntityPredicate.matches(level, position, entity)`.
    pub(crate) fn matches(
        &self,
        world: &Arc<World>,
        origin: Option<DVec3>,
        entity: &dyn Entity,
    ) -> bool {
        self.0
            .iter()
            .all(|predicate| predicate.matches(entity, world, origin))
    }
}

impl<'de> Deserialize<'de> for EntityPredicate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let parts = serde_json::Map::<String, Value>::deserialize(deserializer)?;
        let mut predicates = parts
            .into_iter()
            .map(|(key, value)| EntitySubPredicate::decode(&key, value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(D::Error::custom)?;
        // Vanilla `EntityPredicate.PREDICATE_TYPE_ORDER`: the cheap type check
        // first and checks on other entities last. Only the cost differs.
        predicates.sort_by_key(|predicate| match predicate {
            EntitySubPredicate::EntityType(_) => 0,
            EntitySubPredicate::Vehicle(_)
            | EntitySubPredicate::Passenger(_)
            | EntitySubPredicate::Player(_) => 2,
            _ => 1,
        });
        Ok(Self(predicates))
    }
}

/// Vanilla `LootContext.EntityTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntityTarget {
    This,
    Attacker,
    DirectAttacker,
    AttackingPlayer,
    TargetEntity,
    InteractingEntity,
}

/// The loot context a criterion's conditions are tested in (vanilla
/// `LootContextParamSets.ADVANCEMENT_*`).
pub(crate) struct ConditionContext<'a> {
    pub(crate) world: &'a Arc<World>,
    pub(crate) origin: Option<DVec3>,
    pub(crate) this: Option<&'a dyn Entity>,
    pub(crate) tool: Option<&'a ItemStack>,
    pub(crate) block_state: Option<BlockStateId>,
}

impl<'a> ConditionContext<'a> {
    /// Vanilla `EntityPredicate.createContext(player, entity)`: `this` is the
    /// entity, the origin is the player's position.
    pub(crate) fn entity(world: &'a Arc<World>, origin: DVec3, entity: &'a dyn Entity) -> Self {
        Self {
            world,
            origin: Some(origin),
            this: Some(entity),
            tool: None,
            block_state: None,
        }
    }

    fn entity_for(&self, target: EntityTarget) -> Option<&'a dyn Entity> {
        match target {
            EntityTarget::This => self.this,
            _ => None,
        }
    }
}

/// A vanilla loot condition (`LootItemCondition`) as used by criteria.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum LootCondition {
    #[serde(rename = "minecraft:entity_properties", alias = "entity_properties")]
    EntityProperties {
        entity: EntityTarget,
        predicate: Option<EntityPredicate>,
    },
    #[serde(rename = "minecraft:location_check", alias = "location_check")]
    LocationCheck {
        #[serde(default, rename = "offsetX")]
        offset_x: i32,
        #[serde(default, rename = "offsetY")]
        offset_y: i32,
        #[serde(default, rename = "offsetZ")]
        offset_z: i32,
        predicate: Option<Box<LocationPredicate>>,
    },
    #[serde(rename = "minecraft:match_block", alias = "match_block")]
    MatchBlock(Box<BlockCondition>),
    #[serde(rename = "minecraft:match_tool", alias = "match_tool")]
    MatchTool {
        predicate: Option<Box<ItemCondition>>,
    },
    #[serde(rename = "minecraft:all_of", alias = "all_of")]
    AllOf { terms: Vec<LootCondition> },
    #[serde(rename = "minecraft:any_of", alias = "any_of")]
    AnyOf { terms: Vec<LootCondition> },
    #[serde(rename = "minecraft:inverted", alias = "inverted")]
    Inverted { term: Box<LootCondition> },
}

impl LootCondition {
    /// Vanilla `LootItemCondition.test(context)`.
    pub(crate) fn test(&self, ctx: &ConditionContext<'_>) -> bool {
        match self {
            Self::EntityProperties { entity, predicate } => {
                // Vanilla `LootItemEntityPropertyCondition`: an empty predicate
                // passes even without the entity.
                let Some(predicate) = predicate else {
                    return true;
                };
                ctx.entity_for(*entity)
                    .is_some_and(|entity| predicate.matches(ctx.world, ctx.origin, entity))
            }
            Self::LocationCheck {
                offset_x,
                offset_y,
                offset_z,
                predicate,
            } => ctx.origin.is_some_and(|origin| {
                predicate.as_ref().is_none_or(|predicate| {
                    predicate.matches(
                        ctx.world,
                        origin.x + f64::from(*offset_x),
                        origin.y + f64::from(*offset_y),
                        origin.z + f64::from(*offset_z),
                    )
                })
            }),
            Self::MatchBlock(block) => ctx
                .block_state
                .is_some_and(|state| block.matches_state(state)),
            Self::MatchTool { predicate } => ctx.tool.is_some_and(|tool| {
                predicate
                    .as_ref()
                    .is_none_or(|predicate| predicate.test(tool))
            }),
            Self::AllOf { terms } => terms.iter().all(|term| term.test(ctx)),
            Self::AnyOf { terms } => terms.iter().any(|term| term.test(ctx)),
            Self::Inverted { term } => !term.test(ctx),
        }
    }
}

/// A criterion field holding a loot condition: one condition, or a list that
/// must all pass (vanilla `LootItemCondition.CODEC` with the inline all-of
/// form).
#[derive(Debug, Clone)]
pub(crate) struct ContextPredicate(Vec<LootCondition>);

impl ContextPredicate {
    /// Tests every condition in the context.
    pub(crate) fn test(&self, ctx: &ConditionContext<'_>) -> bool {
        self.0.iter().all(|condition| condition.test(ctx))
    }
}

impl<'de> Deserialize<'de> for ContextPredicate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Many(Vec<LootCondition>),
            One(Box<LootCondition>),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Many(conditions) => Self(conditions),
            Raw::One(condition) => Self(vec![*condition]),
        })
    }
}

/// `state` written beside `blocks` on block triggers.
pub(crate) fn block_matches(
    blocks: Option<&IdSet>,
    state: Option<&StateCondition>,
    block_state: BlockStateId,
) -> bool {
    let block = block_state.get_block();
    blocks.is_none_or(|blocks| blocks.contains(&block.key, |tag| block.has_tag(tag)))
        && state.is_none_or(|state| state.matches(block_state))
}
