//! Evoker fangs (Vanilla `EvokerFangs`): jaws that snap shut out of the ground.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_damage_types;
use steel_registry::vanilla_entity_data::EvokerFangsEntityData;
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityReference, EntitySyncedData, RemovalReason,
    SharedEntity,
};
use crate::world::World;

const TAG_WARMUP: &str = "Warmup";
const TAG_OWNER: &str = "Owner";
/// Vanilla `lifeTicks` start: 20 ticks of attack plus `LIFE_OFFSET`.
const START_LIFE_TICKS: i32 = 22;
/// Vanilla bites on the eighth tick after the warmup runs out.
const BITE_TICK: i32 = -8;
const BITE_DAMAGE: f32 = 6.0;
const BITE_REACH: f64 = 0.2;

struct EvokerFangsState {
    warmup_delay_ticks: i32,
    sent_spike_event: bool,
    life_ticks: i32,
    owner: Option<EntityReference>,
}

/// Evoker fangs: bite every living thing standing on them once.
#[entity_behavior(class = "EvokerFangs")]
pub struct EvokerFangsEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<EvokerFangsEntityData>,
    state: SyncMutex<EvokerFangsState>,
}

// SAFETY: The owner-scoped type key uniquely identifies EvokerFangsEntity.
unsafe impl DowncastType for EvokerFangsEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/evoker_fangs");
}

impl EvokerFangsEntity {
    /// Creates fangs with no warmup and no owner.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads saved fangs.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(EvokerFangsEntityData::new()),
            state: SyncMutex::new(EvokerFangsState {
                warmup_delay_ticks: 0,
                sent_spike_event: false,
                life_ticks: START_LIFE_TICKS,
                owner: None,
            }),
        }
    }

    /// Vanilla `new EvokerFangs(level, x, y, z, rotationRadians, warmupDelayTicks, owner)`.
    pub fn configure(&self, rotation_radians: f32, warmup_delay_ticks: i32, owner: &SharedEntity) {
        {
            let mut state = self.state.lock();
            state.warmup_delay_ticks = warmup_delay_ticks;
            state.owner = Some(EntityReference::from_entity(owner));
        }
        self.set_rotation((rotation_radians.to_degrees(), self.rotation().1));
    }

    /// Vanilla `getOwner`.
    #[must_use]
    pub fn owner(&self) -> Option<SharedEntity> {
        let owner = self.state.lock().owner.clone()?;
        let world = self.level()?;
        owner
            .get_entity(&world)
            .filter(|entity| entity.as_living_entity().is_some())
    }

    /// Vanilla `getWarmupDelayTicks`-style accessor for tests and commands.
    #[must_use]
    pub fn warmup_delay_ticks(&self) -> i32 {
        self.state.lock().warmup_delay_ticks
    }

    fn server_tick(self: &Arc<Self>, world: &Arc<World>) {
        let (bite, send_spike, discard) = {
            let mut state = self.state.lock();
            state.warmup_delay_ticks -= 1;
            if state.warmup_delay_ticks >= 0 {
                return;
            }
            let bite = state.warmup_delay_ticks == BITE_TICK;
            let send_spike = !state.sent_spike_event;
            state.sent_spike_event = true;
            state.life_ticks -= 1;
            (bite, send_spike, state.life_ticks < 0)
        };

        if bite {
            let search_box = self.bounding_box().inflate_xyz(BITE_REACH, 0.0, BITE_REACH);
            let victims = world.get_entities_in_aabb_matching(&search_box, |entity| {
                entity.as_living_entity().is_some()
            });
            let owner = self.owner();
            let fangs: SharedEntity = Arc::<Self>::clone(self);
            for victim in victims {
                deal_damage_to(world, &fangs, owner.as_ref(), &victim);
            }
        }

        if send_spike {
            self.broadcast_entity_event(EntityStatus::StartAttacking);
        }

        if discard {
            self.set_removed(RemovalReason::Discarded);
        }
    }
}

/// Vanilla `EvokerFangs.dealDamageTo`.
fn deal_damage_to(
    world: &World,
    fangs: &SharedEntity,
    owner: Option<&SharedEntity>,
    victim: &SharedEntity,
) {
    if !victim.is_alive()
        || victim.is_invulnerable()
        || owner.is_some_and(|owner| owner.uuid() == victim.uuid())
    {
        return;
    }
    let Some(owner) = owner else {
        let source = DamageSource::environment(&vanilla_damage_types::MAGIC);
        let _ = victim.hurt(world, &source, BITE_DAMAGE);
        return;
    };
    if owner.is_allied_to(victim.as_ref()) {
        return;
    }
    let source = DamageSource::direct(&vanilla_damage_types::INDIRECT_MAGIC, Arc::clone(fangs))
        .with_causing_entity(Arc::clone(owner));
    if victim.hurt(world, &source, BITE_DAMAGE) {
        let context = EnchantmentPostAttackContext::new(victim, &source);
        enchantment_helper::do_post_attack_effects_with_item_source(
            world,
            victim.as_ref(),
            &ItemStack::empty(),
            &context,
        );
    }
}

impl Entity for EvokerFangsEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.default_tick();
        if let Some(world) = self.level() {
            self.server_tick(&world);
        }
    }

    fn hurt(&self, _world: &World, _source: &DamageSource, _amount: f32) -> bool {
        false
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let state = self.state.lock();
        nbt.insert(TAG_WARMUP, state.warmup_delay_ticks);
        if let Some(owner) = state.owner.as_ref() {
            owner.store(nbt, TAG_OWNER);
        }
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let mut state = self.state.lock();
        state.warmup_delay_ticks = nbt.int(TAG_WARMUP).unwrap_or(0);
        state.owner = EntityReference::read(&nbt, TAG_OWNER);
    }
}
