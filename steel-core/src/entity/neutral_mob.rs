//! Mobs that stay calm until provoked (Vanilla `NeutralMob`).
//!
//! The anger state lives in [`NeutralMobState`]; mobs implement
//! [`NeutralMob::start_persistent_anger_timer`] with their own anger duration.

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::vanilla_game_rules::{FORGIVE_DEAD_PLAYERS, UNIVERSAL_ANGER};
use steel_utils::locks::SyncMutex;
use steel_utils::types::{Difficulty, GameType};

use crate::entity::{Entity, EntityReference, LivingEntity, Mob, SharedEntity};
use crate::player::Player;
use crate::world::World;

const TAG_ANGER_END_TIME: &str = "anger_end_time";
const TAG_ANGRY_AT: &str = "angry_at";
const TAG_LEGACY_ANGER_TIME: &str = "AngerTime";
/// Vanilla `NeutralMob.NO_ANGER_END_TIME`.
pub(crate) const NO_ANGER_END_TIME: i64 = -1;

/// Persistent anger fields of a neutral mob.
pub struct NeutralMobState {
    end_time: SyncMutex<i64>,
    target: SyncMutex<Option<EntityReference>>,
}

impl NeutralMobState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            end_time: SyncMutex::new(NO_ANGER_END_TIME),
            target: SyncMutex::new(None),
        }
    }
}

impl Default for NeutralMobState {
    fn default() -> Self {
        Self::new()
    }
}

fn is_valid_player_target(target: &dyn LivingEntity, world: &World) -> bool {
    target.as_player().is_some_and(|player| {
        player.game_mode() != GameType::Creative
            && !player.is_spectator()
            && world.difficulty() != Difficulty::Peaceful
    })
}

fn is_creative_or_spectator(player: &Player) -> bool {
    player.game_mode() == GameType::Creative || player.is_spectator()
}

/// Vanilla `NeutralMob`.
pub trait NeutralMob: Mob {
    fn neutral_mob_state(&self) -> &NeutralMobState;

    /// Vanilla `startPersistentAngerTimer`: each mob picks its own anger duration.
    fn start_persistent_anger_timer(&self);

    fn persistent_anger_end_time(&self) -> i64 {
        *self.neutral_mob_state().end_time.lock()
    }

    fn set_persistent_anger_end_time(&self, end_time: i64) {
        *self.neutral_mob_state().end_time.lock() = end_time;
    }

    fn set_time_to_remain_angry(&self, remaining_time: i64) {
        let Some(world) = self.level() else {
            return;
        };
        self.set_persistent_anger_end_time(world.game_time() + remaining_time);
    }

    fn persistent_anger_target(&self) -> Option<EntityReference> {
        self.neutral_mob_state().target.lock().clone()
    }

    fn set_persistent_anger_target(&self, target: Option<EntityReference>) {
        *self.neutral_mob_state().target.lock() = target;
    }

    fn add_persistent_anger_save_data(&self, nbt: &mut NbtCompound) {
        nbt.insert(TAG_ANGER_END_TIME, self.persistent_anger_end_time());
        if let Some(target) = self.persistent_anger_target() {
            target.store(nbt, TAG_ANGRY_AT);
        }
    }

    fn read_persistent_anger_save_data(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        if let Some(end_time) = nbt.long(TAG_ANGER_END_TIME) {
            self.set_persistent_anger_end_time(end_time);
        } else if let Some(anger_time) = nbt.int(TAG_LEGACY_ANGER_TIME) {
            self.set_time_to_remain_angry(i64::from(anger_time));
        } else {
            self.set_persistent_anger_end_time(NO_ANGER_END_TIME);
        }

        let target = EntityReference::read(&nbt, TAG_ANGRY_AT);
        let resolved = self
            .level()
            .zip(target.as_ref())
            .and_then(|(world, target)| target.get_entity(&world))
            .filter(|entity| entity.as_living_entity().is_some());
        self.set_persistent_anger_target(target);
        let _ = self.set_target(resolved.as_ref());
    }

    /// Vanilla `updatePersistentAnger`, run from the mob's `aiStep`.
    fn update_persistent_anger(&self, world: &World, stay_angry_if_target_present: bool) {
        let previous_target = self.mob_base().target(|_| true);
        let anger_target = self.persistent_anger_target();
        if let (Some(previous), Some(anger)) = (&previous_target, &anger_target)
            && previous
                .as_living_entity()
                .is_some_and(LivingEntity::is_dead_or_dying)
            && anger.uuid() == previous.uuid()
            && previous.as_mob().is_some()
        {
            self.stop_being_angry();
            return;
        }

        let target = self.target();
        if let Some(target) = &target {
            let new_target = anger_target
                .as_ref()
                .is_none_or(|anger| anger.uuid() != target.uuid());
            if new_target {
                self.set_persistent_anger_target(Some(EntityReference::from_entity(target)));
            }
            if new_target || stay_angry_if_target_present {
                self.start_persistent_anger_timer();
            }
        }

        if anger_target.is_some()
            && !self.is_angry()
            && (target.as_ref().is_none_or(|target| {
                !target
                    .as_living_entity()
                    .is_some_and(|living| is_valid_player_target(living, world))
            }) || !stay_angry_if_target_present)
        {
            self.stop_being_angry();
        }

        let anger_player = anger_target.and_then(|anger| anger.get_entity(world));
        if anger_player
            .as_ref()
            .and_then(|entity| entity.as_player())
            .is_some_and(|player| {
                is_creative_or_spectator(player) || world.difficulty() == Difficulty::Peaceful
            })
        {
            self.stop_being_angry();
        }
    }

    fn is_angry_at(&self, entity: &dyn LivingEntity, world: &World) -> bool {
        if !Mob::can_attack(self, entity) {
            return false;
        }
        if is_valid_player_target(entity, world) && self.is_angry_at_all_players(world) {
            return true;
        }
        self.persistent_anger_target()
            .is_some_and(|anger| anger.uuid() == entity.uuid())
    }

    fn is_angry_at_all_players(&self, world: &World) -> bool {
        world.get_game_rule(&UNIVERSAL_ANGER)
            && self.is_angry()
            && self.persistent_anger_target().is_none()
    }

    fn is_angry(&self) -> bool {
        let end_time = self.persistent_anger_end_time();
        if end_time <= 0 {
            return false;
        }
        self.level()
            .is_some_and(|world| end_time - world.game_time() > 0)
    }

    /// Vanilla `playerDied`.
    fn player_died(&self, world: &World, player: &Player) {
        if !world.get_game_rule(&FORGIVE_DEAD_PLAYERS) {
            return;
        }
        if self
            .persistent_anger_target()
            .is_some_and(|anger| anger.uuid() == player.uuid())
        {
            self.stop_being_angry();
        }
    }

    fn forget_current_target_and_refresh_universal_anger(&self) {
        self.stop_being_angry();
        self.start_persistent_anger_timer();
    }

    fn stop_being_angry(&self) {
        self.set_last_hurt_by_mob(None::<&SharedEntity>);
        self.set_persistent_anger_target(None);
        let _ = self.set_target(None);
        self.set_persistent_anger_end_time(NO_ANGER_END_TIME);
    }
}
