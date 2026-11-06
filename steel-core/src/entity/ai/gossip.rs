//! Vanilla `GossipContainer` and `GossipType`: what a villager has heard about
//! other entities, which sets its prices and whether it calls an iron golem.
//!
//! Vanilla keeps the gossip in hash maps; Steel keeps insertion order so the
//! weighted pick in [`GossipContainer::transfer_from`] is deterministic.

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_utils::random::Random;
use uuid::Uuid;

use steel_utils::UuidExt as _;

/// Vanilla `GossipContainer.DISCARD_THRESHOLD`: values below this are forgotten.
pub const DISCARD_THRESHOLD: i32 = 2;

/// Vanilla `GossipType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GossipType {
    /// Someone killed a villager.
    MajorNegative,
    /// Someone hurt a villager.
    MinorNegative,
    /// Someone cured a zombie villager (fades).
    MinorPositive,
    /// Someone cured a zombie villager (never fades on its own).
    MajorPositive,
    /// Someone traded with a villager.
    Trading,
}

impl GossipType {
    /// Every gossip type, in vanilla declaration order.
    pub const ALL: [Self; 5] = [
        Self::MajorNegative,
        Self::MinorNegative,
        Self::MinorPositive,
        Self::MajorPositive,
        Self::Trading,
    ];

    /// Vanilla `GossipType.id`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::MajorNegative => "major_negative",
            Self::MinorNegative => "minor_negative",
            Self::MinorPositive => "minor_positive",
            Self::MajorPositive => "major_positive",
            Self::Trading => "trading",
        }
    }

    /// Parses a vanilla `GossipType` id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// Vanilla `GossipType.weight`: how much one point moves the reputation.
    #[must_use]
    pub const fn weight(self) -> i32 {
        match self {
            Self::MajorNegative => -5,
            Self::MinorNegative => -1,
            Self::MinorPositive | Self::Trading => 1,
            Self::MajorPositive => 5,
        }
    }

    /// Vanilla `GossipType.max`.
    #[must_use]
    pub const fn max(self) -> i32 {
        match self {
            Self::MajorNegative => 100,
            Self::MinorNegative => 200,
            Self::MinorPositive | Self::Trading => 25,
            Self::MajorPositive => 20,
        }
    }

    /// Vanilla `GossipType.decayPerDay`.
    #[must_use]
    pub const fn decay_per_day(self) -> i32 {
        match self {
            Self::MajorNegative => 10,
            Self::MinorNegative => 20,
            Self::MinorPositive => 1,
            Self::MajorPositive => 0,
            Self::Trading => 2,
        }
    }

    /// Vanilla `GossipType.decayPerTransfer`.
    #[must_use]
    pub const fn decay_per_transfer(self) -> i32 {
        match self {
            Self::MajorNegative => 10,
            Self::MinorNegative | Self::MajorPositive | Self::Trading => 20,
            Self::MinorPositive => 5,
        }
    }
}

/// Vanilla `ReputationEventType`: what a villager saw an entity do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReputationEventType {
    /// The entity cured a zombie villager.
    ZombieVillagerCured,
    /// The entity killed an iron golem.
    GolemKilled,
    /// The entity hurt a villager.
    VillagerHurt,
    /// The entity killed a villager.
    VillagerKilled,
    /// The entity traded with a villager.
    Trade,
}

/// Vanilla `GossipContainer.EntityGossips`: the gossip about one entity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EntityGossips {
    entries: Vec<(GossipType, i32)>,
}

impl EntityGossips {
    fn get(&self, kind: GossipType) -> Option<i32> {
        self.entries
            .iter()
            .find_map(|&(entry_kind, value)| (entry_kind == kind).then_some(value))
    }

    fn put(&mut self, kind: GossipType, value: i32) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.0 == kind) {
            entry.1 = value;
        } else {
            self.entries.push((kind, value));
        }
    }

    /// Java `Object2IntMap.mergeInt`.
    fn merge(&mut self, kind: GossipType, value: i32, merge: impl FnOnce(i32, i32) -> i32) {
        let merged = self.get(kind).map_or(value, |old| merge(old, value));
        self.put(kind, merged);
    }

    fn remove(&mut self, kind: GossipType) {
        self.entries.retain(|entry| entry.0 != kind);
    }

    fn weighted_value(&self, types: impl Fn(GossipType) -> bool) -> i32 {
        self.entries
            .iter()
            .filter(|(kind, _)| types(*kind))
            .map(|&(kind, value)| value * kind.weight())
            .sum()
    }

    fn decay(&mut self) {
        self.entries.retain_mut(|(kind, value)| {
            let new_value = *value - kind.decay_per_day();
            if new_value < DISCARD_THRESHOLD {
                false
            } else {
                *value = new_value;
                true
            }
        });
    }

    const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn make_sure_value_isnt_too_low_or_too_high(&mut self, kind: GossipType) {
        let value = self.get(kind).unwrap_or(0);
        if value > kind.max() {
            self.put(kind, kind.max());
        }
        if value < DISCARD_THRESHOLD {
            self.remove(kind);
        }
    }
}

/// Vanilla `GossipContainer.GossipEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GossipEntry {
    /// Who the gossip is about.
    pub target: Uuid,
    /// What kind of gossip it is.
    pub kind: GossipType,
    /// How strong it is.
    pub value: i32,
}

impl GossipEntry {
    const fn weighted_value(self) -> i32 {
        self.value * self.kind.weight()
    }
}

/// Vanilla `GossipContainer`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GossipContainer {
    gossips: Vec<(Uuid, EntityGossips)>,
}

impl GossipContainer {
    /// An empty container.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gossips: Vec::new(),
        }
    }

    fn get(&self, target: Uuid) -> Option<&EntityGossips> {
        self.gossips
            .iter()
            .find_map(|(uuid, gossips)| (*uuid == target).then_some(gossips))
    }

    fn get_or_create(&mut self, target: Uuid) -> &mut EntityGossips {
        let index = if let Some(index) = self.gossips.iter().position(|(uuid, _)| *uuid == target) {
            index
        } else {
            self.gossips.push((target, EntityGossips::default()));
            self.gossips.len() - 1
        };
        &mut self.gossips[index].1
    }

    fn remove_target(&mut self, target: Uuid) {
        self.gossips.retain(|(uuid, _)| *uuid != target);
    }

    /// Vanilla `unpack`: every gossip entry.
    #[must_use]
    pub fn entries(&self) -> Vec<GossipEntry> {
        self.gossips
            .iter()
            .flat_map(|(target, gossips)| {
                gossips.entries.iter().map(|&(kind, value)| GossipEntry {
                    target: *target,
                    kind,
                    value,
                })
            })
            .collect()
    }

    /// Vanilla `decay`: a day passes.
    pub fn decay(&mut self) {
        self.gossips.retain_mut(|(_, gossips)| {
            gossips.decay();
            !gossips.is_empty()
        });
    }

    /// Vanilla `selectGossipsForTransfer`: `max_count` weighted picks, each
    /// entry at most once.
    fn select_gossips_for_transfer(
        &self,
        random: &mut impl Random,
        max_count: i32,
    ) -> Vec<GossipEntry> {
        let entries = self.entries();
        if entries.is_empty() {
            return Vec::new();
        }
        let mut ranges = Vec::with_capacity(entries.len());
        let mut ranges_end = 0;
        for gossip in &entries {
            ranges_end += gossip.weighted_value().abs();
            ranges.push(ranges_end - 1);
        }

        let mut picked = vec![false; entries.len()];
        let mut results = Vec::new();
        for _ in 0..max_count {
            let choice = random.next_i32_bounded(ranges_end);
            // Vanilla `Arrays.binarySearch`: the first range that ends at or past `choice`.
            let index = ranges.partition_point(|&range_end| range_end < choice);
            if !picked[index] {
                picked[index] = true;
                results.push(entries[index]);
            }
        }
        results
    }

    /// Vanilla `transferFrom`: hears up to `max_count` pieces of `source`'s
    /// gossip, each weakened by its type's transfer decay. Returns how many
    /// pieces were picked.
    pub fn transfer_from(
        &mut self,
        source: &Self,
        random: &mut impl Random,
        max_count: i32,
    ) -> usize {
        let new_gossips = source.select_gossips_for_transfer(random, max_count);
        for gossip in &new_gossips {
            let decayed_value = gossip.value - gossip.kind.decay_per_transfer();
            if decayed_value >= DISCARD_THRESHOLD {
                self.get_or_create(gossip.target)
                    .merge(gossip.kind, decayed_value, i32::max);
            }
        }
        new_gossips.len()
    }

    /// Vanilla `getReputation`.
    #[must_use]
    pub fn reputation(&self, entity: Uuid, types: impl Fn(GossipType) -> bool) -> i32 {
        self.get(entity)
            .map_or(0, |gossips| gossips.weighted_value(types))
    }

    /// Vanilla `getCountForType`: how many entities have gossip of `kind`
    /// whose weighted value passes `value_test`.
    #[must_use]
    pub fn count_for_type(&self, kind: GossipType, value_test: impl Fn(f64) -> bool) -> usize {
        self.gossips
            .iter()
            .filter(|(_, gossips)| {
                value_test(f64::from(gossips.get(kind).unwrap_or(0) * kind.weight()))
            })
            .count()
    }

    /// Vanilla `add`.
    pub fn add(&mut self, target: Uuid, kind: GossipType, amount_to_add: i32) {
        let gossips = self.get_or_create(target);
        gossips.merge(kind, amount_to_add, |old, new| {
            let sum = old + new;
            if sum > kind.max() {
                kind.max().max(old)
            } else {
                sum
            }
        });
        gossips.make_sure_value_isnt_too_low_or_too_high(kind);
        if gossips.is_empty() {
            self.remove_target(target);
        }
    }

    /// Vanilla `remove(UUID, GossipType, int)`.
    pub fn remove_amount(&mut self, target: Uuid, kind: GossipType, amount_to_remove: i32) {
        self.add(target, kind, -amount_to_remove);
    }

    /// Vanilla `remove(UUID, GossipType)`.
    pub fn remove_target_type(&mut self, target: Uuid, kind: GossipType) {
        let Some(index) = self.gossips.iter().position(|(uuid, _)| *uuid == target) else {
            return;
        };
        let gossips = &mut self.gossips[index].1;
        gossips.remove(kind);
        if gossips.is_empty() {
            self.gossips.remove(index);
        }
    }

    /// Vanilla `remove(GossipType)`.
    pub fn remove_type(&mut self, kind: GossipType) {
        self.gossips.retain_mut(|(_, gossips)| {
            gossips.remove(kind);
            !gossips.is_empty()
        });
    }

    /// Vanilla `clear`.
    pub fn clear(&mut self) {
        self.gossips.clear();
    }

    /// Vanilla `putAll`.
    pub fn put_all(&mut self, container: &Self) {
        for (target, gossips) in &container.gossips {
            let own = self.get_or_create(*target);
            for &(kind, value) in &gossips.entries {
                own.put(kind, value);
            }
        }
    }

    /// Vanilla `CODEC` encode: a list of `{Target, Type, Value}`.
    #[must_use]
    pub fn to_nbt(&self) -> NbtList {
        NbtList::Compound(
            self.entries()
                .into_iter()
                .map(|entry| {
                    let mut compound = NbtCompound::new();
                    compound.insert(
                        "Target",
                        NbtTag::IntArray(entry.target.to_int_array().to_vec()),
                    );
                    compound.insert("Type", entry.kind.id());
                    compound.insert("Value", entry.value);
                    compound
                })
                .collect(),
        )
    }

    /// Vanilla `CODEC` decode. Like vanilla's list codec, a malformed entry
    /// fails the whole list.
    #[must_use]
    pub fn from_nbt<'a, 'tape>(
        entries: impl IntoIterator<Item = BorrowedNbtCompoundView<'a, 'tape>>,
    ) -> Option<Self>
    where
        'a: 'tape,
    {
        let mut container = Self::new();
        for entry in entries {
            let target = Uuid::from_int_array(&entry.int_array("Target")?)?;
            let kind = GossipType::from_id(entry.string("Type")?.to_str().as_ref())?;
            // Vanilla `ExtraCodecs.POSITIVE_INT`.
            let value = entry.int("Value").filter(|value| *value > 0)?;
            container.get_or_create(target).put(kind, value);
        }
        Some(container)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_utils::random::legacy_random::LegacyRandom;

    use super::*;

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    #[test]
    fn add_caps_at_max_and_drops_low_values() {
        let mut gossips = GossipContainer::new();
        gossips.add(A, GossipType::MinorNegative, 25);
        gossips.add(A, GossipType::MinorNegative, 190);
        // Vanilla: a sum over max keeps max(max, old).
        assert_eq!(gossips.reputation(A, |_| true), -200);

        gossips.add(B, GossipType::Trading, 1);
        assert!(gossips.entries().iter().all(|entry| entry.target != B));
    }

    #[test]
    fn reputation_weights_each_type() {
        let mut gossips = GossipContainer::new();
        gossips.add(A, GossipType::MajorPositive, 20);
        gossips.add(A, GossipType::MinorPositive, 25);
        gossips.add(A, GossipType::MinorNegative, 25);
        assert_eq!(gossips.reputation(A, |_| true), 100 + 25 - 25);
        assert_eq!(
            gossips.reputation(A, |kind| kind == GossipType::MajorPositive),
            100
        );
    }

    #[test]
    fn decay_forgets_faded_gossip() {
        let mut gossips = GossipContainer::new();
        gossips.add(A, GossipType::MajorPositive, 20);
        gossips.add(B, GossipType::MinorNegative, 21);
        gossips.decay();
        assert_eq!(gossips.reputation(A, |_| true), 100);
        assert!(gossips.entries().iter().all(|entry| entry.target != B));
    }

    #[test]
    fn transfer_weakens_gossip() {
        let mut source = GossipContainer::new();
        source.add(A, GossipType::MajorNegative, 25);
        let mut listener = GossipContainer::new();
        let mut random = LegacyRandom::from_seed(0);
        assert_eq!(listener.transfer_from(&source, &mut random, 10), 1);
        assert_eq!(listener.reputation(A, |_| true), -5 * 15);
    }

    #[test]
    fn nbt_round_trips() {
        let mut gossips = GossipContainer::new();
        gossips.add(A, GossipType::Trading, 6);
        gossips.add(B, GossipType::MajorNegative, 25);
        let mut nbt = NbtCompound::new();
        nbt.insert("Gossips", gossips.to_nbt());
        let bytes = {
            let mut buf = Vec::new();
            nbt.write(&mut buf);
            buf
        };
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let view: BorrowedNbtCompoundView<'_, '_> = (&borrowed).into();
        let list = view
            .list("Gossips")
            .and_then(|list| list.compounds())
            .unwrap_or_else(|| panic!("gossip list missing"));
        assert_eq!(GossipContainer::from_nbt(list), Some(gossips));
    }
}
