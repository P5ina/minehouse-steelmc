//! Vanilla `NearestVisibleLivingEntities`: nearby living entities with a
//! lazily cached "can the body see it" test.

use std::sync::{Arc, Weak};

use rustc_hash::{FxBuildHasher, FxHashMap};

use steel_registry::vanilla_attributes;
use steel_utils::DowncastTypeKey;
use steel_utils::locks::SyncMutex;

use super::memory::MemoryValue;
use super::memory_types;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{LivingEntity, SharedEntity, WeakEntity};
use crate::world::World;

struct Inner {
    entities: Vec<SharedEntity>,
    world: Weak<World>,
    body: Option<WeakEntity>,
    visible: SyncMutex<FxHashMap<i32, bool>>,
}

/// Nearby living entities, closest first, filtered by visibility on demand.
#[derive(Clone)]
pub struct NearestVisibleLivingEntities(Arc<Inner>);

impl NearestVisibleLivingEntities {
    /// No entities (vanilla `NearestVisibleLivingEntities.empty`).
    #[must_use]
    pub fn empty() -> Self {
        Self(Arc::new(Inner {
            entities: Vec::new(),
            world: Weak::new(),
            body: None,
            visible: SyncMutex::new(FxHashMap::default()),
        }))
    }

    /// `entities` as seen by `body`, sorted closest first by the caller.
    #[must_use]
    pub fn new(world: &Arc<World>, body: &SharedEntity, entities: Vec<SharedEntity>) -> Self {
        Self(Arc::new(Inner {
            visible: SyncMutex::new(FxHashMap::with_capacity_and_hasher(
                entities.len(),
                FxBuildHasher,
            )),
            entities,
            world: Arc::downgrade(world),
            body: Some(Arc::downgrade(body)),
        }))
    }

    /// The entities without the visibility test.
    #[must_use]
    pub fn nearby_entities(&self) -> &[SharedEntity] {
        &self.0.entities
    }

    fn is_visible(&self, entity: &SharedEntity) -> bool {
        if let Some(visible) = self.0.visible.lock().get(&entity.id()) {
            return *visible;
        }
        let visible = self.test_visible(entity);
        self.0.visible.lock().insert(entity.id(), visible);
        visible
    }

    fn test_visible(&self, entity: &SharedEntity) -> bool {
        let (Some(world), Some(body)) = (
            self.0.world.upgrade(),
            self.0.body.as_ref().and_then(Weak::upgrade),
        ) else {
            return false;
        };
        let (Some(body), Some(target)) = (body.as_living_entity(), entity.as_living_entity())
        else {
            return false;
        };
        is_entity_targetable(&world, body, target)
    }

    /// The closest visible entity matching `filter`.
    #[must_use]
    pub fn find_closest(
        &self,
        mut filter: impl FnMut(&SharedEntity) -> bool,
    ) -> Option<SharedEntity> {
        self.0
            .entities
            .iter()
            .find(|entity| filter(entity) && self.is_visible(entity))
            .cloned()
    }

    /// Every visible entity matching `filter`, closest first.
    pub fn find_all<'a>(
        &'a self,
        mut filter: impl FnMut(&SharedEntity) -> bool + 'a,
    ) -> impl Iterator<Item = &'a SharedEntity> + 'a {
        self.0
            .entities
            .iter()
            .filter(move |entity| filter(entity) && self.is_visible(entity))
    }

    /// Whether `entity` is among the visible entities.
    #[must_use]
    pub fn contains(&self, entity_id: i32) -> bool {
        self.0
            .entities
            .iter()
            .find(|entity| entity.id() == entity_id)
            .is_some_and(|entity| self.is_visible(entity))
    }

    /// Whether any visible entity matches `filter`.
    #[must_use]
    pub fn contains_matching(&self, filter: impl FnMut(&SharedEntity) -> bool) -> bool {
        self.find_closest(filter).is_some()
    }
}

impl MemoryValue for NearestVisibleLivingEntities {
    const KEY: DowncastTypeKey =
        DowncastTypeKey::new("steel:brain_memory/nearest_visible_living_entities");
}

fn follow_range(body: &dyn LivingEntity) -> f64 {
    body.attributes()
        .lock()
        .required_value(vanilla_attributes::FOLLOW_RANGE)
}

/// Vanilla `Sensor.isEntityTargetable`: non-combat targeting within the body's
/// follow range, ignoring invisibility for its current attack target.
#[must_use]
pub fn is_entity_targetable(
    world: &World,
    body: &dyn LivingEntity,
    target: &dyn LivingEntity,
) -> bool {
    let mut conditions = TargetingConditions::for_non_combat().range(follow_range(body));
    if memory_types::is_entity_memory(body, &memory_types::ATTACK_TARGET, target.id()) {
        conditions = conditions.ignore_invisibility_testing();
    }
    conditions.test(world, Some(body), target)
}

/// Vanilla `Sensor.isEntityAttackable`.
#[must_use]
pub fn is_entity_attackable(
    world: &World,
    body: &dyn LivingEntity,
    target: &dyn LivingEntity,
) -> bool {
    let mut conditions = TargetingConditions::for_combat().range(follow_range(body));
    if memory_types::is_entity_memory(body, &memory_types::ATTACK_TARGET, target.id()) {
        conditions = conditions.ignore_invisibility_testing();
    }
    conditions.test(world, Some(body), target)
}

/// Vanilla `Sensor.isEntityAttackableIgnoringLineOfSight`.
#[must_use]
pub fn is_entity_attackable_ignoring_line_of_sight(
    world: &World,
    body: &dyn LivingEntity,
    target: &dyn LivingEntity,
) -> bool {
    let mut conditions = TargetingConditions::for_combat()
        .range(follow_range(body))
        .ignore_line_of_sight();
    if memory_types::is_entity_memory(body, &memory_types::ATTACK_TARGET, target.id()) {
        conditions = conditions.ignore_invisibility_testing();
    }
    conditions.test(world, Some(body), target)
}
