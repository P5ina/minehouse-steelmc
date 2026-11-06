//! Brain memories: typed slots that sensors fill and behaviors read.

use std::collections::BTreeMap;
use std::fmt::{self, Debug, Formatter};
use std::marker::PhantomData;

use simdnbt::borrow::{NbtCompound as BorrowedNbtCompound, NbtTag as BorrowedNbtTag};
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_utils::{Downcast as _, DowncastType, DowncastTypeKey, ErasedType};

const NEVER_EXPIRE: i64 = i64::MAX;

/// A value a memory can hold.
///
/// `KEY` names the concrete Rust type for keyed downcasting; it must be unique
/// per implementing type.
pub trait MemoryValue: Clone + Send + Sync + 'static {
    /// Downcast key of this value type, such as `steel:brain_memory/i32`.
    const KEY: DowncastTypeKey;

    /// Vanilla `Brain` stores an empty collection as an absent memory.
    fn is_empty_collection(&self) -> bool {
        false
    }
}

/// Wrapper that gives every [`MemoryValue`] its downcast key.
struct MemoryBox<T>(T);

// SAFETY: `MemoryValue::KEY` is required to be unique per implementing type,
// so it identifies exactly one concrete `MemoryBox<T>`.
unsafe impl<T: MemoryValue> DowncastType for MemoryBox<T> {
    const TYPE_KEY: DowncastTypeKey = T::KEY;
}

type ErasedMemory = Box<dyn ErasedType + Send + Sync>;

/// Saves a memory value to NBT and reads it back (vanilla memory codec).
pub struct MemoryCodec<T> {
    /// Writes the value.
    pub save: fn(&T) -> NbtTag,
    /// Reads the value, or `None` when the tag does not hold one.
    pub load: for<'a, 'tape> fn(BorrowedNbtTag<'a, 'tape>) -> Option<T>,
}

/// A kind of memory (vanilla `MemoryModuleType`).
pub struct MemoryModuleType<T> {
    /// Registry path in the `minecraft` namespace.
    pub name: &'static str,
    codec: Option<MemoryCodec<T>>,
    _value: PhantomData<fn() -> T>,
}

impl<T> MemoryModuleType<T> {
    /// A memory that is never saved.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            codec: None,
            _value: PhantomData,
        }
    }

    /// A memory that is saved with the entity.
    #[must_use]
    pub const fn with_codec(name: &'static str, codec: MemoryCodec<T>) -> Self {
        Self {
            name,
            codec: Some(codec),
            _value: PhantomData,
        }
    }

    /// Whether this memory is saved with the entity.
    #[must_use]
    pub const fn can_serialize(&self) -> bool {
        self.codec.is_some()
    }
}

impl<T> Debug for MemoryModuleType<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "minecraft:{}", self.name)
    }
}

/// Type-erased identity of a [`MemoryModuleType`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryId(&'static str);

/// What a behavior or activity requires of a memory (vanilla `MemoryStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryStatus {
    /// The memory holds a value.
    ValuePresent,
    /// The memory is empty.
    ValueAbsent,
    /// The memory is registered, with or without a value.
    Registered,
}

/// A memory type with its value type erased.
pub trait AnyMemoryModuleType: Sync {
    /// The memory's identity.
    fn id(&self) -> MemoryId;

    /// Registers the memory in `memories`.
    fn register_into(&'static self, memories: &mut Memories);
}

impl<T: MemoryValue> AnyMemoryModuleType for MemoryModuleType<T> {
    fn id(&self) -> MemoryId {
        MemoryId(self.name)
    }

    fn register_into(&'static self, memories: &mut Memories) {
        memories.register(self);
    }
}

/// A memory requirement: the memory and the status it must have.
#[derive(Clone, Copy)]
pub struct MemoryRequirement {
    memory: &'static dyn AnyMemoryModuleType,
    status: MemoryStatus,
}

impl MemoryRequirement {
    /// Requires `memory` to have `status`.
    #[must_use]
    pub const fn new(memory: &'static dyn AnyMemoryModuleType, status: MemoryStatus) -> Self {
        Self { memory, status }
    }

    /// The memory this requirement checks.
    #[must_use]
    pub fn memory(&self) -> &'static dyn AnyMemoryModuleType {
        self.memory
    }
}

impl Debug for MemoryRequirement {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {:?}", self.memory.id(), self.status)
    }
}

type SaveFn = Box<dyn Fn(&ErasedMemory) -> Option<NbtTag> + Send + Sync>;

/// Type-erased save hooks of a memory type.
struct ErasedCodec {
    save: SaveFn,
    load:
        Box<dyn for<'a, 'tape> Fn(BorrowedNbtTag<'a, 'tape>) -> Option<ErasedMemory> + Send + Sync>,
}

struct MemorySlot {
    value: Option<ErasedMemory>,
    time_to_live: i64,
    codec: Option<ErasedCodec>,
}

impl MemorySlot {
    const fn new(codec: Option<ErasedCodec>) -> Self {
        Self {
            value: None,
            time_to_live: NEVER_EXPIRE,
            codec,
        }
    }

    fn tick(&mut self) {
        if self.value.is_none() || self.time_to_live == NEVER_EXPIRE {
            return;
        }
        if self.time_to_live <= 0 {
            self.clear();
        } else {
            self.time_to_live -= 1;
        }
    }

    fn set(&mut self, value: ErasedMemory, time_to_live: i64) {
        self.value = Some(value);
        self.time_to_live = time_to_live;
    }

    fn clear(&mut self) {
        self.value = None;
        self.time_to_live = NEVER_EXPIRE;
    }
}

/// All memories of one brain (vanilla `Brain.memories`).
///
/// Only registered memories can hold values; setting an unregistered memory
/// does nothing, as in vanilla.
#[derive(Default)]
pub struct Memories {
    slots: BTreeMap<MemoryId, MemorySlot>,
}

impl Memories {
    /// Registers `memory` so it can hold a value.
    pub fn register<T: MemoryValue>(&mut self, memory: &'static MemoryModuleType<T>) {
        self.slots
            .entry(AnyMemoryModuleType::id(memory))
            .or_insert_with(|| {
                MemorySlot::new(memory.codec.as_ref().map(|codec| {
                    let save = codec.save;
                    let load = codec.load;
                    ErasedCodec {
                        save: Box::new(move |value: &ErasedMemory| {
                            value
                                .downcast_ref::<MemoryBox<T>>()
                                .map(|value| save(&value.0))
                        }),
                        load: Box::new(move |tag| {
                            load(tag).map(|value| Box::new(MemoryBox(value)) as ErasedMemory)
                        }),
                    }
                }))
            });
    }

    /// Returns whether no memory is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Returns a copy of the memory's value.
    #[must_use]
    pub fn get<T: MemoryValue>(&self, memory: &MemoryModuleType<T>) -> Option<T> {
        self.get_ref(memory).cloned()
    }

    /// Returns the memory's value by reference.
    #[must_use]
    pub fn get_ref<T: MemoryValue>(&self, memory: &MemoryModuleType<T>) -> Option<&T> {
        self.slots
            .get(&AnyMemoryModuleType::id(memory))?
            .value
            .as_ref()?
            .downcast_ref::<MemoryBox<T>>()
            .map(|value| &value.0)
    }

    /// Returns whether the memory holds a value.
    #[must_use]
    pub fn has_value(&self, memory: &dyn AnyMemoryModuleType) -> bool {
        self.check(memory.id(), MemoryStatus::ValuePresent)
    }

    /// Returns whether the memory holds a value equal to `value`.
    #[must_use]
    pub fn is_value<T: MemoryValue + PartialEq>(
        &self,
        memory: &MemoryModuleType<T>,
        value: &T,
    ) -> bool {
        self.get_ref(memory) == Some(value)
    }

    /// Returns whether a memory has the given status (vanilla `Brain.checkMemory`).
    #[must_use]
    pub fn check(&self, id: MemoryId, status: MemoryStatus) -> bool {
        let Some(slot) = self.slots.get(&id) else {
            return false;
        };
        match status {
            MemoryStatus::Registered => true,
            MemoryStatus::ValuePresent => slot.value.is_some(),
            MemoryStatus::ValueAbsent => slot.value.is_none(),
        }
    }

    /// Returns whether every requirement holds.
    #[must_use]
    pub fn meets(&self, requirements: &[MemoryRequirement]) -> bool {
        requirements
            .iter()
            .all(|requirement| self.check(requirement.memory.id(), requirement.status))
    }

    /// Sets the memory forever, or clears it for `None` or an empty collection.
    pub fn set<T: MemoryValue>(&mut self, memory: &MemoryModuleType<T>, value: Option<T>) {
        self.set_internal(memory, value, NEVER_EXPIRE);
    }

    /// Sets the memory for `time_to_live` ticks.
    pub fn set_with_expiry<T: MemoryValue>(
        &mut self,
        memory: &MemoryModuleType<T>,
        value: T,
        time_to_live: i64,
    ) {
        self.set_internal(memory, Some(value), time_to_live);
    }

    fn set_internal<T: MemoryValue>(
        &mut self,
        memory: &MemoryModuleType<T>,
        value: Option<T>,
        time_to_live: i64,
    ) {
        let Some(slot) = self.slots.get_mut(&AnyMemoryModuleType::id(memory)) else {
            return;
        };
        match value.filter(|value| !value.is_empty_collection()) {
            Some(value) => slot.set(Box::new(MemoryBox(value)), time_to_live),
            None => slot.clear(),
        }
    }

    /// Clears the memory.
    pub fn erase(&mut self, memory: &dyn AnyMemoryModuleType) {
        if let Some(slot) = self.slots.get_mut(&memory.id()) {
            slot.clear();
        }
    }

    /// Ticks until the memory expires, `i64::MAX` if it never does, or `None`
    /// if it is not registered.
    #[must_use]
    pub fn time_until_expiry<T: MemoryValue>(&self, memory: &MemoryModuleType<T>) -> Option<i64> {
        self.slots
            .get(&AnyMemoryModuleType::id(memory))
            .map(|slot| slot.time_to_live)
    }

    /// Clears every memory.
    pub fn clear(&mut self) {
        self.slots.values_mut().for_each(MemorySlot::clear);
    }

    /// Counts down expiring memories (vanilla `Brain.forgetOutdatedMemories`).
    pub fn forget_outdated(&mut self) {
        self.slots.values_mut().for_each(MemorySlot::tick);
    }

    /// Writes the saved memories as vanilla `Brain.Packed`: `{memories: {...}}`.
    #[must_use]
    pub fn save(&self) -> NbtCompound {
        let mut memories = NbtCompound::new();
        for (id, slot) in &self.slots {
            let (Some(codec), Some(value)) = (&slot.codec, &slot.value) else {
                continue;
            };
            let Some(tag) = (codec.save)(value) else {
                continue;
            };
            let mut entry = NbtCompound::new();
            entry.insert("value", tag);
            if slot.time_to_live != NEVER_EXPIRE {
                entry.insert("ttl", slot.time_to_live);
            }
            memories.insert(format!("minecraft:{}", id.0), entry);
        }
        let mut packed = NbtCompound::new();
        packed.insert("memories", memories);
        packed
    }

    /// Restores saved memories from vanilla `Brain.Packed`.
    pub fn load(&mut self, packed: BorrowedNbtCompound<'_, '_>) {
        let Some(memories) = packed.compound("memories") else {
            return;
        };
        for (id, slot) in &mut self.slots {
            let Some(codec) = &slot.codec else {
                continue;
            };
            let Some(entry) = memories.compound(&format!("minecraft:{}", id.0)) else {
                continue;
            };
            let Some(value) = entry.get("value").and_then(|tag| (codec.load)(tag)) else {
                continue;
            };
            let time_to_live = entry.long("ttl").unwrap_or(NEVER_EXPIRE);
            slot.set(value, time_to_live);
        }
    }
}

impl Debug for Memories {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.slots
                    .iter()
                    .map(|(id, slot)| (id.0, (slot.value.is_some(), slot.time_to_live))),
            )
            .finish()
    }
}
