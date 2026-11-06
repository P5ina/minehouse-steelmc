//! Vanilla `TamableAnimal`: animals that players tame, own, order to sit and
//! that follow and defend their owner.
//!
//! The synced flags and owner live in each entity's own entity data, which the
//! entity exposes through the accessors at the top of [`TamableAnimal`].

use std::sync::atomic::{AtomicBool, Ordering};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::CSystemChat;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::damage_type::DeathMessageType;
use steel_registry::data_components::components::FoodProperties;
use steel_registry::data_components::vanilla_components::FOOD;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_rules::SHOW_DEATH_MESSAGES;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, translations};
use text_components::TextComponent;
use text_components::translation::TranslatedMessage;
use uuid::Uuid;

use crate::entity::ai::path::{PathType, PathfindingContext};
use crate::entity::ai::walk::WalkPathEvaluator;
use crate::entity::damage::DamageSource;
use crate::entity::{Animal, Entity, EntityReference, LivingEntity, SharedEntity};
use crate::physics::{WorldCollisionProvider, has_collision};
use crate::player::Player;

const TAG_OWNER: &str = "Owner";
const TAG_SITTING: &str = "Sitting";
/// Vanilla `DATA_FLAGS_ID` bit set while the animal sits.
const FLAG_SITTING: i8 = 1;
/// Vanilla `DATA_FLAGS_ID` bit set once the animal is tame.
const FLAG_TAME: i8 = 4;
/// Owners further than 12 blocks away make the animal teleport to them.
const TELEPORT_TO_OWNER_DISTANCE_SQR: f64 = 144.0;
const TELEPORT_ATTEMPTS: i32 = 10;

/// Non-synced `TamableAnimal` state.
#[derive(Default)]
pub struct TamableAnimalBase {
    ordered_to_sit: AtomicBool,
}

impl TamableAnimalBase {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ordered_to_sit: AtomicBool::new(false),
        }
    }
}

/// Vanilla `TamableAnimal`.
pub trait TamableAnimal: Animal {
    fn tamable_base(&self) -> &TamableAnimalBase;

    /// Vanilla `DATA_FLAGS_ID`.
    fn tamable_flags(&self) -> i8;

    fn set_tamable_flags(&self, flags: i8);

    /// Vanilla `DATA_OWNERUUID_ID`.
    fn owner_uuid(&self) -> Option<Uuid>;

    fn set_owner_uuid(&self, owner: Option<Uuid>);

    /// Vanilla `applyTamingSideEffects`, such as a tamed wolf's extra health.
    fn apply_taming_side_effects(&self) {}

    /// Vanilla `canFlyToOwner`: flying pets may teleport onto leaves.
    fn can_fly_to_owner(&self) -> bool {
        false
    }

    /// Vanilla `wantsToAttack`: whether the pet joins its owner's fight against `target`.
    fn wants_to_attack(&self, _target: &dyn LivingEntity, _owner: &dyn LivingEntity) -> bool {
        true
    }

    fn is_tame(&self) -> bool {
        self.tamable_flags() & FLAG_TAME != 0
    }

    /// Vanilla `setTame`.
    fn set_tame(&self, tame: bool, include_side_effects: bool) {
        let flags = self.tamable_flags();
        self.set_tamable_flags(if tame {
            flags | FLAG_TAME
        } else {
            flags & !FLAG_TAME
        });
        if include_side_effects {
            self.apply_taming_side_effects();
        }
    }

    fn is_in_sitting_pose(&self) -> bool {
        self.tamable_flags() & FLAG_SITTING != 0
    }

    fn set_in_sitting_pose(&self, sitting: bool) {
        let flags = self.tamable_flags();
        self.set_tamable_flags(if sitting {
            flags | FLAG_SITTING
        } else {
            flags & !FLAG_SITTING
        });
    }

    fn is_ordered_to_sit(&self) -> bool {
        self.tamable_base().ordered_to_sit.load(Ordering::Relaxed)
    }

    fn set_ordered_to_sit(&self, ordered_to_sit: bool) {
        self.tamable_base()
            .ordered_to_sit
            .store(ordered_to_sit, Ordering::Relaxed);
    }

    /// Vanilla `getOwner`: the owner when it is loaded in this level.
    fn owner(&self) -> Option<SharedEntity> {
        let uuid = self.owner_uuid()?;
        let world = self.level()?;
        EntityReference::from_uuid(uuid)
            .get_entity(&world)
            .filter(|owner| owner.as_living_entity().is_some())
    }

    /// Vanilla `isOwnedBy`.
    fn is_owned_by(&self, entity: &dyn Entity) -> bool {
        self.owner_uuid() == Some(entity.uuid())
    }

    /// Vanilla `getRootOwner`: follows owners of owners, such as a pet owned by a pet.
    fn root_owner(&self) -> Option<SharedEntity> {
        let mut owner = self.owner()?;
        for _ in 0..16 {
            let Some(next) = owner.as_tamable_animal().and_then(TamableAnimal::owner) else {
                break;
            };
            owner = next;
        }
        Some(owner)
    }

    /// Vanilla `tame`: makes `player` the owner.
    fn tame(&self, player: &Player) {
        self.set_tame(true, true);
        self.set_owner_uuid(Some(player.uuid()));
    }

    /// Vanilla `TamableAnimal.canAttack`, for `Mob::can_attack` overrides.
    fn can_attack_tamable(&self, target: &dyn LivingEntity) -> bool {
        !self.is_owned_by(target) && self.default_can_attack_mob(target)
    }

    /// Vanilla `TamableAnimal.considersEntityAsAlly`: a tame animal is allied to
    /// its owner and to everything its owner counts as an ally.
    fn considers_entity_as_ally_tamable(&self, other: &dyn Entity) -> bool {
        if !self.is_tame() {
            return false;
        }
        let Some(owner) = self.root_owner() else {
            return false;
        };
        owner.uuid() == other.uuid() || owner.considers_entity_as_ally(other)
    }

    /// Vanilla `unableToMoveToOwner`.
    fn unable_to_move_to_owner(&self) -> bool {
        self.is_ordered_to_sit()
            || self.is_passenger()
            || self.may_be_leashed()
            || self.owner().is_some_and(|owner| owner.is_spectator())
    }

    /// Vanilla `shouldTryTeleportToOwner`: the owner is 12 or more blocks away.
    fn should_try_teleport_to_owner(&self) -> bool {
        self.owner().is_some_and(|owner| {
            self.position().distance_squared(owner.position()) >= TELEPORT_TO_OWNER_DISTANCE_SQR
        })
    }

    /// Vanilla `tryToTeleportToOwner`: up to ten tries at a free spot two or
    /// three blocks from the owner.
    fn try_to_teleport_to_owner(&self) {
        let Some(owner) = self.owner() else {
            return;
        };
        let target = owner.block_position();
        for _ in 0..TELEPORT_ATTEMPTS {
            let dx: i32 = rand::random_range(-3..=3);
            let dz: i32 = rand::random_range(-3..=3);
            if dx.abs() < 2 && dz.abs() < 2 {
                continue;
            }
            let dy = rand::random_range(-1..=1);
            if self.maybe_teleport_to(target.offset(dx, dy, dz)) {
                return;
            }
        }
    }

    /// Vanilla `maybeTeleportTo`.
    fn maybe_teleport_to(&self, pos: BlockPos) -> bool {
        if !self.can_teleport_to(pos) {
            return false;
        }
        let (yaw, pitch) = self.rotation();
        self.snap_to(
            DVec3::new(
                f64::from(pos.x()) + 0.5,
                f64::from(pos.y()),
                f64::from(pos.z()) + 0.5,
            ),
            yaw,
            pitch,
        );
        self.mob_base().navigation().lock().stop();
        true
    }

    /// Vanilla `canTeleportTo`: a walkable, free spot, and not onto leaves
    /// unless the pet flies.
    fn can_teleport_to(&self, pos: BlockPos) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        let mut context = PathfindingContext::new(world.as_ref(), self.block_position());
        if WalkPathEvaluator::path_type_static(&mut context, pos) != PathType::Walkable {
            return false;
        }
        if !self.can_fly_to_owner()
            && world
                .get_block_state(pos.below())
                .get_block()
                .has_tag(&BlockTag::LEAVES)
        {
            return false;
        }
        let here = self.block_position();
        let delta = DVec3::new(
            f64::from(pos.x() - here.x()),
            f64::from(pos.y() - here.y()),
            f64::from(pos.z() - here.z()),
        );
        !has_collision(
            &WorldCollisionProvider::for_entity(&world, self.as_entity_event_source()),
            self.bounding_box().translate(delta),
        )
    }

    /// Vanilla `feed`: eats the held food and heals by its nutrition.
    fn feed(&self, player: &Player, hand: InteractionHand, healing_factor: f32, default_heal: f32) {
        let nutrition = {
            let inventory = player.inventory.lock();
            inventory
                .get_item_in_hand(hand)
                .get(FOOD)
                .map(FoodProperties::nutrition)
        };
        self.use_player_item(player, hand);
        self.heal(nutrition.map_or(default_heal, |nutrition| healing_factor * nutrition as f32));
        self.play_eating_sound();
    }

    /// Vanilla `TamableAnimal.die`: tells the owner how the pet died.
    fn announce_death_to_owner(&self, source: &DamageSource) {
        let Some(world) = self.level() else {
            return;
        };
        if !world.get_game_rule(&SHOW_DEATH_MESSAGES) {
            return;
        }
        let Some(owner) = self.owner() else {
            return;
        };
        let Some(player) = owner.as_player() else {
            return;
        };
        player.send_packet(CSystemChat {
            content: death_message(self.display_name(), source),
            overlay: false,
        });
    }

    fn save_tamable(&self, nbt: &mut NbtCompound) {
        if let Some(owner) = self.owner_uuid() {
            EntityReference::from_uuid(owner).store(nbt, TAG_OWNER);
        }
        nbt.insert(TAG_SITTING, i8::from(self.is_ordered_to_sit()));
    }

    /// Vanilla `readAdditionalSaveData`, including the old string owner format.
    fn load_tamable(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let owner = EntityReference::read(&nbt, TAG_OWNER)
            .map(|owner| owner.uuid())
            .or_else(|| {
                nbt.string(TAG_OWNER)
                    .and_then(|owner| Uuid::parse_str(owner.to_str().as_ref()).ok())
            });
        self.set_owner_uuid(owner);
        self.set_tame(owner.is_some(), owner.is_none());
        let sitting = nbt.byte(TAG_SITTING).is_some_and(|sitting| sitting != 0);
        self.set_ordered_to_sit(sitting);
        self.set_in_sitting_pose(sitting);
    }
}

/// Vanilla `CombatTracker.getDeathMessage` for a victim without a combat log.
fn death_message(name: TextComponent, source: &DamageSource) -> TextComponent {
    let key = format!("death.attack.{}", source.damage_type.message_id);
    match source.damage_type.death_message_type {
        DeathMessageType::IntentionalGameDesign => {
            let link = translations::CHAT_SQUARE_BRACKETS
                .message([TranslatedMessage {
                    key: format!("{key}.link").into(),
                    fallback: None,
                    args: None,
                }
                .component()])
                .component();
            TranslatedMessage {
                key: format!("{key}.message").into(),
                fallback: None,
                args: Some(Box::new([name, link])),
            }
            .component()
        }
        DeathMessageType::Default | DeathMessageType::FallVariants => TranslatedMessage {
            key: key.into(),
            fallback: None,
            args: Some(Box::new([name])),
        }
        .component(),
    }
}
