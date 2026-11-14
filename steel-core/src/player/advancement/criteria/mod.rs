//! Advancement criteria and their triggers (vanilla `CriteriaTriggers`,
//! `SimpleCriterionTrigger` and `PlayerAdvancements` listeners).
//!
//! Every criterion of every advancement is decoded once from its datapack
//! JSON into [`Conditions`] and grouped by trigger type. A trigger fires for a
//! player by testing the criteria of that type the player still listens to:
//! the advancement is not done and the criterion is not done, as vanilla
//! `PlayerAdvancements.registerListeners` and `unregisterListeners` keep it.
#![expect(
    dead_code,
    reason = "conditions of every trigger are decoded now; the triggers that read them are wired one group at a time"
)]

pub(crate) mod predicates;
mod triggers;

use std::sync::LazyLock;

use rustc_hash::FxHashMap;
use serde::Deserialize;
use steel_registry::REGISTRY;
use steel_registry::advancement::registry::AdvancementRef;
use steel_utils::Identifier;

use crate::entity::Entity as _;
use crate::player::Player;
use predicates::{
    ConditionContext, ContextPredicate, DistancePredicate, Doubles, EntityPredicate, IdSet, Ints,
    ItemCondition, LocationPredicate, MobEffectsPredicate, StateCondition,
};

pub(crate) use triggers::*;

/// Vanilla `TagPredicate<DamageType>`: the tag is written `#namespace:path`
/// (vanilla `TagKey.hashedCodec`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DamageTagPredicate {
    #[serde(deserialize_with = "hashed_tag")]
    pub(crate) id: Identifier,
    pub(crate) expected: bool,
}

/// Vanilla `TagKey.hashedCodec`: a tag id with its leading `#`.
fn hashed_tag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Identifier, D::Error> {
    use serde::de::Error as _;
    let value = String::deserialize(deserializer)?;
    let tag = value
        .strip_prefix('#')
        .ok_or_else(|| D::Error::custom(format!("not a tag: {value}")))?;
    tag.parse().map_err(D::Error::custom)
}

/// Vanilla `DamageSourcePredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DamageSourcePredicate {
    #[serde(default)]
    pub(crate) tags: Vec<DamageTagPredicate>,
    pub(crate) direct_entity: Option<EntityPredicate>,
    pub(crate) source_entity: Option<EntityPredicate>,
    pub(crate) is_direct: Option<bool>,
}

/// Vanilla `DamagePredicate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DamagePredicate {
    #[serde(default)]
    pub(crate) dealt: Doubles,
    #[serde(default)]
    pub(crate) taken: Doubles,
    pub(crate) source_entity: Option<EntityPredicate>,
    pub(crate) blocked: Option<bool>,
    #[serde(rename = "type")]
    pub(crate) damage_type: Option<DamageSourcePredicate>,
}

/// Vanilla `PotionsPredicate`. No vanilla advancement narrows by `effects`,
/// so only the potion holder set is modeled.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PotionsPredicate {
    pub(crate) potions: Option<IdSet>,
}

/// Vanilla `InventoryChangeTrigger.TriggerInstance.Slots`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlotsPredicate {
    #[serde(default)]
    pub(crate) occupied: Ints,
    #[serde(default)]
    pub(crate) full: Ints,
    #[serde(default)]
    pub(crate) empty: Ints,
}

/// `distance`: a [`DistancePredicate`] on most triggers, plain bounds on
/// `used_ender_eye`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum DistanceField {
    Bounds(Doubles),
    Distance(DistancePredicate),
}

/// A list of ids, written as one id or an array (vanilla
/// `Recipe.LIST_CODEC`, `LootTable.LIST_CODEC`).
#[derive(Debug, Clone, Default)]
pub(crate) struct IdList(pub(crate) Vec<Identifier>);

impl<'de> Deserialize<'de> for IdList {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(Identifier),
            Many(Vec<Identifier>),
        }
        Ok(Self(match Raw::deserialize(deserializer)? {
            Raw::One(key) => vec![key],
            Raw::Many(keys) => keys,
        }))
    }
}

/// The conditions of a criterion: the union of every vanilla trigger
/// instance's fields. Each trigger reads only its own fields; a field no
/// trigger knows fails decoding, so the criterion is never granted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conditions {
    pub(crate) player: Option<ContextPredicate>,
    pub(crate) entity: Option<ContextPredicate>,
    pub(crate) location: Option<ContextPredicate>,
    pub(crate) parent: Option<ContextPredicate>,
    pub(crate) partner: Option<ContextPredicate>,
    pub(crate) child: Option<ContextPredicate>,
    pub(crate) zombie: Option<ContextPredicate>,
    pub(crate) villager: Option<ContextPredicate>,
    pub(crate) lightning: Option<ContextPredicate>,
    pub(crate) bystander: Option<ContextPredicate>,
    pub(crate) cause: Option<ContextPredicate>,
    pub(crate) source: Option<ContextPredicate>,
    pub(crate) projectile: Option<ContextPredicate>,
    #[serde(default)]
    pub(crate) victims: Vec<ContextPredicate>,
    pub(crate) item: Option<ItemCondition>,
    pub(crate) rod: Option<ItemCondition>,
    pub(crate) fired_from_weapon: Option<ItemCondition>,
    #[serde(default)]
    pub(crate) items: Vec<ItemCondition>,
    #[serde(default)]
    pub(crate) ingredients: Vec<ItemCondition>,
    #[serde(default)]
    pub(crate) slots: SlotsPredicate,
    pub(crate) recipes: Option<IdList>,
    pub(crate) loot_tables: Option<IdList>,
    pub(crate) blocks: Option<IdSet>,
    pub(crate) state: Option<StateCondition>,
    pub(crate) damage: Option<DamagePredicate>,
    pub(crate) killing_blow: Option<DamageSourcePredicate>,
    pub(crate) distance: Option<DistanceField>,
    pub(crate) start_position: Option<LocationPredicate>,
    pub(crate) effects: Option<MobEffectsPredicate>,
    pub(crate) potion: Option<PotionsPredicate>,
    pub(crate) from: Option<Identifier>,
    pub(crate) to: Option<Identifier>,
    pub(crate) count: Option<i32>,
    #[serde(default)]
    pub(crate) level: Ints,
    #[serde(default)]
    pub(crate) levels: Ints,
    #[serde(default)]
    pub(crate) num_bees_inside: Ints,
    #[serde(default)]
    pub(crate) signal_strength: Ints,
    #[serde(default)]
    pub(crate) duration: Ints,
    #[serde(default)]
    pub(crate) durability: Ints,
    #[serde(default)]
    pub(crate) delta: Ints,
    #[serde(default)]
    pub(crate) unique_entity_types: Ints,
}

/// One decoded criterion and where it belongs (vanilla
/// `PlayerAdvancements.TriggerInstanceKey` with its instance).
pub(crate) struct CriterionListener {
    pub(crate) advancement: AdvancementRef,
    pub(crate) criterion: &'static str,
    pub(crate) conditions: Conditions,
}

/// Every criterion, grouped by trigger type.
static CRITERIA: LazyLock<FxHashMap<Identifier, Vec<CriterionListener>>> = LazyLock::new(|| {
    let mut by_trigger: FxHashMap<Identifier, Vec<CriterionListener>> = FxHashMap::default();
    for &advancement in REGISTRY.advancements.advancements() {
        for (criterion, data) in advancement.criteria() {
            match serde_json::from_str::<Conditions>(data.conditions) {
                Ok(conditions) => {
                    by_trigger
                        .entry(data.trigger.clone())
                        .or_default()
                        .push(CriterionListener {
                            advancement,
                            criterion,
                            conditions,
                        });
                }
                Err(error) => log::warn!(
                    "Advancement {} criterion '{criterion}' ({}) can't be checked and will never be granted: {error}",
                    advancement.key(),
                    data.trigger
                ),
            }
        }
    }
    by_trigger
});

/// Decodes every criterion now instead of on the first trigger.
pub(crate) fn init_criteria() {
    LazyLock::force(&CRITERIA);
}

/// Vanilla `SimpleCriterionTrigger.trigger(player, matcher)`: awards every
/// criterion of `trigger` the player listens to whose conditions pass
/// `matcher` and whose `player` predicate holds.
///
/// The matching criteria are collected first and awarded after, as vanilla
/// does, and no advancement lock is held while conditions are tested.
pub(crate) fn trigger(
    player: &Player,
    trigger: &Identifier,
    mut matcher: impl FnMut(&Conditions) -> bool,
) {
    let Some(listeners) = CRITERIA.get(trigger) else {
        return;
    };
    let listening: Vec<&CriterionListener> = {
        let advancements = player.advancements.lock();
        listeners
            .iter()
            .filter(|listener| advancements.is_listening(listener.advancement, listener.criterion))
            .collect()
    };
    if listening.is_empty() {
        return;
    }

    let world = player.get_world();
    let player_context = ConditionContext::entity(&world, player.position(), player);
    let passed: Vec<&CriterionListener> = listening
        .into_iter()
        .filter(|listener| {
            matcher(&listener.conditions)
                && listener
                    .conditions
                    .player
                    .as_ref()
                    .is_none_or(|predicate| predicate.test(&player_context))
        })
        .collect();

    for listener in passed {
        player
            .advancements
            .lock()
            .award(player, listener.advancement, listener.criterion);
    }
}

#[cfg(test)]
mod tests;
