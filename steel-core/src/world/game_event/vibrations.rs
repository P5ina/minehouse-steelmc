//! Vanilla `VibrationSystem`: listeners that pick the closest vibration of a
//! tick, send the vibration particle and receive it after it travels.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::game_events::{GameEvent, GameEventRef};
use steel_registry::particle_type::{ParticleData, VibrationParticleOption};
use steel_registry::position_source::PositionSource;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_event_tags::GameEventTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, vanilla_game_events, vanilla_particle_types,
};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, Direction, Identifier, UuidExt as _};
use uuid::Uuid;

use super::{GameEventContext, GameEventListener};
use crate::entity::projectile::Projectile;
use crate::entity::{Entity, SharedEntity};
use crate::world::World;
use crate::world::raycast::RaytraceAction;

/// Vanilla `VibrationSystem.Data.NBT_TAG_KEY`.
pub const VIBRATION_LISTENER_TAG: &str = "listener";

/// Vanilla `VIBRATION_FREQUENCY_FOR_EVENT`. Events outside the table have
/// frequency 0.
#[must_use]
pub fn game_event_frequency(event: GameEventRef) -> i32 {
    use vanilla_game_events as e;
    const TABLE: &[(&GameEvent, i32)] = &[
        (&e::STEP, 1),
        (&e::SWIM, 1),
        (&e::FLAP, 1),
        (&e::PROJECTILE_LAND, 2),
        (&e::HIT_GROUND, 2),
        (&e::SPLASH, 2),
        (&e::BOUNCE, 2),
        (&e::ITEM_INTERACT_FINISH, 3),
        (&e::PROJECTILE_SHOOT, 3),
        (&e::INSTRUMENT_PLAY, 3),
        (&e::ENTITY_ACTION, 4),
        (&e::ELYTRA_GLIDE, 4),
        (&e::UNEQUIP, 4),
        (&e::ENTITY_DISMOUNT, 5),
        (&e::EQUIP, 5),
        (&e::ENTITY_INTERACT, 6),
        (&e::SHEAR, 6),
        (&e::ENTITY_MOUNT, 6),
        (&e::ENTITY_DAMAGE, 7),
        (&e::DRINK, 8),
        (&e::EAT, 8),
        (&e::CONTAINER_CLOSE, 9),
        (&e::BLOCK_CLOSE, 9),
        (&e::BLOCK_DEACTIVATE, 9),
        (&e::BLOCK_DETACH, 9),
        (&e::CONTAINER_OPEN, 10),
        (&e::BLOCK_OPEN, 10),
        (&e::BLOCK_ACTIVATE, 10),
        (&e::BLOCK_ATTACH, 10),
        (&e::PRIME_FUSE, 10),
        (&e::NOTE_BLOCK_PLAY, 10),
        (&e::BLOCK_CHANGE, 11),
        (&e::BLOCK_DESTROY, 12),
        (&e::FLUID_PICKUP, 12),
        (&e::BLOCK_PLACE, 13),
        (&e::FLUID_PLACE, 13),
        (&e::ENTITY_PLACE, 14),
        (&e::LIGHTNING_STRIKE, 14),
        (&e::TELEPORT, 14),
        (&e::ENTITY_DIE, 15),
        (&e::EXPLODE, 15),
        (&e::RESONATE_1, 1),
        (&e::RESONATE_2, 2),
        (&e::RESONATE_3, 3),
        (&e::RESONATE_4, 4),
        (&e::RESONATE_5, 5),
        (&e::RESONATE_6, 6),
        (&e::RESONATE_7, 7),
        (&e::RESONATE_8, 8),
        (&e::RESONATE_9, 9),
        (&e::RESONATE_10, 10),
        (&e::RESONATE_11, 11),
        (&e::RESONATE_12, 12),
        (&e::RESONATE_13, 13),
        (&e::RESONATE_14, 14),
        (&e::RESONATE_15, 15),
    ];
    TABLE
        .iter()
        .find(|(candidate, _)| candidate.key == event.key)
        .map_or(0, |(_, frequency)| *frequency)
}

/// Vanilla `getRedstoneStrengthForDistance`.
#[must_use]
pub fn redstone_strength_for_distance(distance: f32, listener_radius: i32) -> i32 {
    let power_scale = 15.0 / f64::from(listener_radius);
    (15 - (power_scale * f64::from(distance)).floor() as i32).max(1)
}

/// Vanilla `VibrationInfo`.
#[derive(Clone)]
pub struct VibrationInfo {
    game_event: GameEventRef,
    distance: f32,
    pos: DVec3,
    uuid: Option<Uuid>,
    projectile_owner_uuid: Option<Uuid>,
    entity: Option<Weak<dyn Entity>>,
}

impl VibrationInfo {
    /// Vanilla `VibrationInfo(event, distance, pos, entity)`.
    #[must_use]
    pub fn new(
        game_event: GameEventRef,
        distance: f32,
        pos: DVec3,
        entity: Option<&SharedEntity>,
    ) -> Self {
        let projectile_owner_uuid = entity
            .and_then(|entity| entity.as_projectile())
            .and_then(Projectile::get_owner)
            .map(|owner| owner.uuid());
        Self {
            game_event,
            distance,
            pos,
            uuid: entity.map(|entity| entity.uuid()),
            projectile_owner_uuid,
            entity: entity.map(Arc::downgrade),
        }
    }

    /// The game event that caused this vibration.
    #[must_use]
    pub const fn game_event(&self) -> GameEventRef {
        self.game_event
    }

    /// Distance from the source to the listener.
    #[must_use]
    pub const fn distance(&self) -> f32 {
        self.distance
    }

    /// The vibration's source position.
    #[must_use]
    pub const fn pos(&self) -> DVec3 {
        self.pos
    }

    /// Vanilla `getEntity`: the cached entity, else a lookup by UUID.
    #[must_use]
    pub fn entity(&self, world: &World) -> Option<SharedEntity> {
        self.entity
            .as_ref()
            .and_then(Weak::upgrade)
            .or_else(|| self.uuid.and_then(|uuid| world.get_entity_by_uuid(&uuid)))
    }

    /// Vanilla `getProjectileOwner`.
    #[must_use]
    pub fn projectile_owner(&self, world: &World) -> Option<SharedEntity> {
        self.entity(world)
            .and_then(|entity| entity.as_projectile().and_then(Projectile::get_owner))
            .or_else(|| {
                self.projectile_owner_uuid
                    .and_then(|uuid| world.get_entity_by_uuid(&uuid))
            })
    }

    /// Vanilla `VibrationInfo.CODEC` encode.
    #[must_use]
    pub fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert("game_event", self.game_event.key.to_string());
        nbt.insert("distance", self.distance);
        nbt.insert(
            "pos",
            NbtList::Double(vec![self.pos.x, self.pos.y, self.pos.z]),
        );
        if let Some(uuid) = self.uuid {
            nbt.insert("source", NbtTag::IntArray(uuid.to_int_array().to_vec()));
        }
        if let Some(uuid) = self.projectile_owner_uuid {
            nbt.insert(
                "projectile_owner",
                NbtTag::IntArray(uuid.to_int_array().to_vec()),
            );
        }
        nbt
    }

    /// Vanilla `VibrationInfo.CODEC` decode.
    #[must_use]
    pub fn load(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let key = nbt
            .string("game_event")?
            .to_str()
            .parse::<Identifier>()
            .ok()?;
        let game_event = REGISTRY.game_events.by_key(&key)?;
        let distance = nbt.float("distance")?;
        if !(0.0..=f32::MAX).contains(&distance) {
            return None;
        }
        let pos = match nbt.list("pos")?.doubles()?.as_slice() {
            [x, y, z] => DVec3::new(*x, *y, *z),
            _ => return None,
        };
        let uuid_field = |name: &str| {
            nbt.int_array(name)
                .and_then(|array| Uuid::from_int_array(&array))
        };
        Some(Self {
            game_event,
            distance,
            pos,
            uuid: uuid_field("source"),
            projectile_owner_uuid: uuid_field("projectile_owner"),
            entity: None,
        })
    }
}

/// Vanilla `VibrationSelector`: keeps the best candidate of one tick.
#[derive(Clone, Default)]
pub struct VibrationSelector {
    current: Option<(VibrationInfo, i64)>,
}

impl VibrationSelector {
    /// Vanilla `addCandidate`.
    pub fn add_candidate(&mut self, vibration: VibrationInfo, tick: i64) {
        if self.should_replace_vibration(&vibration, tick) {
            self.current = Some((vibration, tick));
        }
    }

    fn should_replace_vibration(&self, vibration: &VibrationInfo, tick: i64) -> bool {
        let Some((previous, previous_tick)) = &self.current else {
            return true;
        };
        if tick != *previous_tick {
            return false;
        }
        if vibration.distance < previous.distance {
            return true;
        }
        if vibration.distance > previous.distance {
            return false;
        }
        game_event_frequency(vibration.game_event) > game_event_frequency(previous.game_event)
    }

    /// Vanilla `chosenCandidate`: a candidate from an earlier tick.
    #[must_use]
    pub fn chosen_candidate(&self, time: i64) -> Option<VibrationInfo> {
        self.current
            .as_ref()
            .filter(|(_, tick)| *tick < time)
            .map(|(vibration, _)| vibration.clone())
    }

    /// Vanilla `startOver`.
    pub fn start_over(&mut self) {
        self.current = None;
    }

    fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if let Some((vibration, _)) = &self.current {
            nbt.insert("event", vibration.save());
        }
        nbt.insert("tick", self.current.as_ref().map_or(-1, |(_, tick)| *tick));
        nbt
    }

    fn load(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let tick = nbt.long("tick")?;
        let vibration = nbt.compound("event").and_then(VibrationInfo::load);
        Some(Self {
            current: vibration.map(|vibration| (vibration, tick)),
        })
    }
}

/// Vanilla `VibrationSystem.Data`.
#[derive(Clone, Default)]
pub struct VibrationData {
    current_vibration: Option<VibrationInfo>,
    travel_time_in_ticks: i32,
    selection_strategy: VibrationSelector,
    reload_vibration_particle: bool,
}

impl VibrationData {
    /// The vibration currently traveling to the listener.
    #[must_use]
    pub const fn current_vibration(&self) -> Option<&VibrationInfo> {
        self.current_vibration.as_ref()
    }

    /// Ticks left before the current vibration arrives.
    #[must_use]
    pub const fn travel_time_in_ticks(&self) -> i32 {
        self.travel_time_in_ticks
    }

    /// Vanilla `getSelectionStrategy`.
    #[must_use]
    pub const fn selection_strategy(&self) -> &VibrationSelector {
        &self.selection_strategy
    }

    /// Vanilla `setCurrentVibration`.
    pub fn set_current_vibration(&mut self, vibration: Option<VibrationInfo>) {
        self.current_vibration = vibration;
    }

    /// Vanilla `setTravelTimeInTicks`.
    pub const fn set_travel_time_in_ticks(&mut self, travel_time_in_ticks: i32) {
        self.travel_time_in_ticks = travel_time_in_ticks;
    }

    /// Vanilla `Data.CODEC` encode, stored under [`VIBRATION_LISTENER_TAG`].
    #[must_use]
    pub fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if let Some(vibration) = &self.current_vibration {
            nbt.insert("event", vibration.save());
        }
        nbt.insert("selector", self.selection_strategy.save());
        nbt.insert("event_delay", self.travel_time_in_ticks);
        nbt
    }

    /// Vanilla `Data.CODEC` decode. A loaded vibration resends its particle.
    #[must_use]
    pub fn load(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let selection_strategy = VibrationSelector::load(nbt.compound("selector")?)?;
        let travel_time_in_ticks = match nbt.int("event_delay") {
            Some(delay) if delay < 0 => return None,
            Some(delay) => delay,
            None => 0,
        };
        Some(Self {
            current_vibration: nbt.compound("event").and_then(VibrationInfo::load),
            travel_time_in_ticks,
            selection_strategy,
            reload_vibration_particle: true,
        })
    }
}

/// Vanilla `VibrationSystem.User`.
pub trait VibrationUser: Send + Sync {
    /// Vanilla `getListenerRadius`.
    fn vibration_listener_radius(&self) -> i32;

    /// Vanilla `getPositionSource`, as sent in the vibration particle.
    fn vibration_position_source(&self) -> PositionSource;

    /// Vanilla `getPositionSource().getPosition(level)`.
    fn vibration_listener_pos(&self) -> Option<DVec3>;

    /// Vanilla `canReceiveVibration`.
    fn can_receive_vibration(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        event: GameEventRef,
        context: &GameEventContext<'_>,
    ) -> bool;

    /// Vanilla `onReceiveVibration`.
    fn on_receive_vibration(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        event: GameEventRef,
        source_entity: Option<&SharedEntity>,
        projectile_owner: Option<&SharedEntity>,
        receiving_distance: f32,
    );

    /// Vanilla `getListenableEvents`.
    fn listenable_events(&self) -> Identifier {
        GameEventTag::VIBRATIONS
    }

    /// Vanilla `canTriggerAvoidVibration`.
    fn can_trigger_avoid_vibration(&self) -> bool {
        false
    }

    /// Vanilla `calculateTravelTimeInTicks`.
    fn calculate_travel_time_in_ticks(&self, distance_to_destination: f32) -> i32 {
        distance_to_destination.floor() as i32
    }

    /// Vanilla `isValidVibration`.
    fn is_valid_vibration(&self, event: GameEventRef, context: &GameEventContext<'_>) -> bool {
        if !REGISTRY
            .game_events
            .is_in_tag(event, &self.listenable_events())
        {
            return false;
        }
        if let Some(source) = context.source_entity() {
            if source.is_spectator() {
                return false;
            }
            if source.is_stepping_carefully()
                && REGISTRY
                    .game_events
                    .is_in_tag(event, &GameEventTag::IGNORE_VIBRATIONS_SNEAKING)
            {
                // TODO(advancements): vanilla triggers AVOID_VIBRATION here
                // when `can_trigger_avoid_vibration` and the source is a player.
                return false;
            }
            if source.dampens_vibrations() {
                return false;
            }
        }
        context.affected_state().is_none_or(|state| {
            !REGISTRY
                .blocks
                .is_in_tag(state.get_block(), &BlockTag::DAMPENS_VIBRATIONS)
        })
    }

    /// Vanilla `onDataChanged`.
    fn on_vibration_data_changed(&self) {}
}

/// Vanilla `VibrationSystem`: something that owns vibration data and a user.
pub trait VibrationSystem: VibrationUser {
    /// Vanilla `getVibrationData`.
    fn vibration_data(&self) -> &SyncMutex<VibrationData>;
}

/// Vanilla `VibrationSystem.Listener`, attached to an entity that is a
/// [`VibrationSystem`].
pub struct VibrationListener {
    owner: Weak<dyn Entity>,
}

impl VibrationListener {
    /// Creates a listener for `owner`, which must return itself from
    /// [`Entity::as_vibration_system`].
    #[must_use]
    pub fn new(owner: &SharedEntity) -> Self {
        Self {
            owner: Arc::downgrade(owner),
        }
    }

    fn with_system<R>(&self, f: impl FnOnce(&dyn VibrationSystem) -> R) -> Option<R> {
        let owner = self.owner.upgrade()?;
        owner.as_vibration_system().map(f)
    }

    /// Vanilla `forceScheduleVibration`.
    pub fn force_schedule_vibration(
        &self,
        world: &World,
        event: GameEventRef,
        context: &GameEventContext<'_>,
        origin: DVec3,
    ) {
        self.with_system(|system| {
            if let Some(destination) = system.vibration_listener_pos() {
                schedule_vibration(world, system, event, context, origin, destination);
            }
        });
    }
}

fn schedule_vibration(
    world: &World,
    system: &dyn VibrationSystem,
    event: GameEventRef,
    context: &GameEventContext<'_>,
    origin: DVec3,
    destination: DVec3,
) {
    let source = context
        .source_entity()
        .and_then(|entity| world.get_entity_by_id(entity.id()));
    let vibration = VibrationInfo::new(
        event,
        origin.distance(destination) as f32,
        origin,
        source.as_ref(),
    );
    system
        .vibration_data()
        .lock()
        .selection_strategy
        .add_candidate(vibration, world.game_time());
}

/// Vanilla `Listener.distanceBetweenInBlocks`.
#[must_use]
pub fn distance_between_in_blocks(origin: BlockPos, destination: BlockPos) -> f32 {
    let dx = f64::from(origin.x() - destination.x());
    let dy = f64::from(origin.y() - destination.y());
    let dz = f64::from(origin.z() - destination.z());
    (dx * dx + dy * dy + dz * dz).sqrt() as f32
}

/// Vanilla `Listener.isOccluded`: blocked when every nudged ray hits a block
/// that occludes vibration signals.
fn is_occluded(world: &World, origin: DVec3, destination: DVec3) -> bool {
    let center = |pos: DVec3| pos.floor() + DVec3::splat(0.5);
    let from = center(origin);
    let to = center(destination);
    Direction::ALL.iter().all(|direction| {
        let (x, y, z) = direction.offset();
        let nudged =
            from + DVec3::new(f64::from(x), f64::from(y), f64::from(z)) * f64::from(1.0e-5f32);
        let (hit, _) = world.raytrace(nudged, to, |pos, world| {
            if REGISTRY.blocks.is_in_tag(
                world.get_block_state(pos).get_block(),
                &BlockTag::OCCLUDES_VIBRATION_SIGNALS,
            ) {
                RaytraceAction::ImmediateHit
            } else {
                RaytraceAction::Pass
            }
        });
        hit.is_some()
    })
}

impl GameEventListener for VibrationListener {
    fn listener_pos(&self) -> Option<DVec3> {
        self.with_system(|system| system.vibration_listener_pos())
            .flatten()
    }

    fn listener_radius(&self) -> i32 {
        self.with_system(|system| system.vibration_listener_radius())
            .unwrap_or(0)
    }

    fn handle_game_event(
        &self,
        world: &Arc<World>,
        event: GameEventRef,
        context: &GameEventContext<'_>,
        source_pos: DVec3,
    ) -> bool {
        self.with_system(|system| {
            if system.vibration_data().lock().current_vibration.is_some() {
                return false;
            }
            if !system.is_valid_vibration(event, context) {
                return false;
            }
            let Some(destination) = system.vibration_listener_pos() else {
                return false;
            };
            if !system.can_receive_vibration(world, BlockPos::from(source_pos), event, context) {
                return false;
            }
            if is_occluded(world, source_pos, destination) {
                return false;
            }
            schedule_vibration(world, system, event, context, source_pos, destination);
            true
        })
        .unwrap_or(false)
    }
}

/// Vanilla `VibrationSystem.Ticker.tick`.
pub fn tick_vibrations(world: &Arc<World>, system: &dyn VibrationSystem) {
    if system.vibration_data().lock().current_vibration.is_none() {
        try_select_and_schedule_vibration(world, system);
    }
    let Some(current) = system.vibration_data().lock().current_vibration.clone() else {
        return;
    };
    let mut has_changed = system.vibration_data().lock().travel_time_in_ticks > 0;
    try_reload_vibration_particle(world, system);
    let arrived = {
        let mut data = system.vibration_data().lock();
        data.travel_time_in_ticks = (data.travel_time_in_ticks - 1).max(0);
        data.travel_time_in_ticks <= 0
    };
    if arrived {
        has_changed = receive_vibration(world, system, &current);
    }
    if has_changed {
        system.on_vibration_data_changed();
    }
}

fn send_vibration_particle(
    world: &World,
    system: &dyn VibrationSystem,
    position: DVec3,
    travel_time_in_ticks: i32,
) -> i32 {
    world.send_particles(
        ParticleData::new(
            &vanilla_particle_types::VIBRATION,
            VibrationParticleOption::new(system.vibration_position_source(), travel_time_in_ticks),
        ),
        position,
        1,
        DVec3::ZERO,
        0.0,
    )
}

fn try_select_and_schedule_vibration(world: &World, system: &dyn VibrationSystem) {
    let Some(vibration) = system
        .vibration_data()
        .lock()
        .selection_strategy
        .chosen_candidate(world.game_time())
    else {
        return;
    };
    let origin = vibration.pos;
    let travel_time = system.calculate_travel_time_in_ticks(vibration.distance);
    {
        let mut data = system.vibration_data().lock();
        data.current_vibration = Some(vibration);
        data.travel_time_in_ticks = travel_time;
    }
    send_vibration_particle(world, system, origin, travel_time);
    system.on_vibration_data_changed();
    system
        .vibration_data()
        .lock()
        .selection_strategy
        .start_over();
}

fn try_reload_vibration_particle(world: &World, system: &dyn VibrationSystem) {
    let (current, travel_time) = {
        let mut data = system.vibration_data().lock();
        if !data.reload_vibration_particle {
            return;
        }
        let Some(current) = data.current_vibration.clone() else {
            data.reload_vibration_particle = false;
            return;
        };
        (current, data.travel_time_in_ticks)
    };
    let origin = current.pos;
    let destination = system.vibration_listener_pos().unwrap_or(origin);
    let initial_travel_time = system.calculate_travel_time_in_ticks(current.distance);
    let alpha = 1.0 - f64::from(travel_time) / f64::from(initial_travel_time);
    let start = origin.lerp(destination, alpha);
    if send_vibration_particle(world, system, start, travel_time) > 0 {
        system.vibration_data().lock().reload_vibration_particle = false;
    }
}

fn receive_vibration(
    world: &Arc<World>,
    system: &dyn VibrationSystem,
    current: &VibrationInfo,
) -> bool {
    let origin = BlockPos::from(current.pos);
    let destination = system
        .vibration_listener_pos()
        .map_or(origin, BlockPos::from);
    let source = current.entity(world);
    let owner = current.projectile_owner(world);
    system.on_receive_vibration(
        world,
        origin,
        current.game_event,
        source.as_ref(),
        owner.as_ref(),
        distance_between_in_blocks(origin, destination),
    );
    system.vibration_data().lock().current_vibration = None;
    true
}

#[cfg(test)]
mod tests;
