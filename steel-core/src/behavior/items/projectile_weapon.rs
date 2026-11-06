//! Vanilla `ProjectileWeaponItem` and `ArrowItem.createArrow`: finding,
//! drawing and firing ammo, shared by the bow, the crossbow and ranged mobs.

use std::sync::{Arc, Weak};

use glam::DVec3;
use steel_registry::data_components::vanilla_components::INTANGIBLE_PROJECTILE;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{vanilla_entities, vanilla_items};
use steel_utils::types::InteractionHand;

use crate::enchantment_helper;
use crate::entity::entities::{ArrowEntity, SpectralArrowEntity};
use crate::entity::{AbstractArrow, Projectile as _, SharedEntity, next_entity_id};
use crate::inventory::container::Container as _;
use crate::player::Player;
use crate::world::World;

/// Vanilla `AbstractArrow(LivingEntity mob, ...)` spawn offset below the eye.
const ARROW_SPAWN_EYE_OFFSET: f64 = 0.1;

/// Vanilla `ProjectileWeaponItem.ARROW_ONLY`.
#[must_use]
pub(crate) fn is_arrow(stack: &ItemStack) -> bool {
    !stack.is_empty() && stack.item().has_tag(&ItemTag::ARROWS)
}

/// Where [`Player`] ammo was found (vanilla `Player.getProjectile` returns
/// the live stack; Steel returns its slot so it can be drawn later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AmmoSlot {
    /// A held stack (`getHeldProjectile`).
    Hand(InteractionHand),
    /// A stack in the inventory container.
    Inventory(usize),
    /// No ammo, but the player has infinite materials: a fresh arrow.
    Creative,
}

/// Vanilla `Player.getProjectile` for an arrow-only weapon (the bow): the
/// off hand, then the main hand, then the inventory, then a creative arrow.
#[must_use]
pub(crate) fn find_player_arrow(player: &Player) -> Option<(AmmoSlot, ItemStack)> {
    let inventory = player.inventory.lock();
    for hand in [InteractionHand::OffHand, InteractionHand::MainHand] {
        let held = inventory.get_item_in_hand(hand);
        if is_arrow(held) {
            return Some((AmmoSlot::Hand(hand), held.clone()));
        }
    }
    for slot in 0..inventory.get_container_size() {
        let stack = inventory.get_item(slot);
        if is_arrow(stack) {
            return Some((AmmoSlot::Inventory(slot), stack.clone()));
        }
    }
    drop(inventory);
    player
        .has_infinite_materials()
        .then(|| (AmmoSlot::Creative, ItemStack::new(&vanilla_items::ARROW)))
}

/// Vanilla `ProjectileWeaponItem.draw` for a player: the stacks to fire,
/// taking the ammo from `slot` (Multishot copies are free).
pub(crate) fn draw_player_ammo(
    player: &Player,
    weapon: &ItemStack,
    slot: AmmoSlot,
    projectile: &ItemStack,
) -> Vec<ItemStack> {
    if projectile.is_empty() {
        return Vec::new();
    }
    let num_projectiles = enchantment_helper::process_projectile_count(weapon, player, 1);
    let mut drawn = Vec::with_capacity(usize::try_from(num_projectiles).unwrap_or(0));
    let projectile_copy = projectile.clone();
    for index in 0..num_projectiles {
        let stack = if index == 0 {
            use_player_ammo(player, weapon, slot, projectile)
        } else {
            use_ammo(
                weapon,
                &mut projectile_copy.clone(),
                true,
                player.has_infinite_materials(),
            )
        };
        if !stack.is_empty() {
            drawn.push(stack);
        }
    }
    drawn
}

fn use_player_ammo(
    player: &Player,
    weapon: &ItemStack,
    slot: AmmoSlot,
    projectile: &ItemStack,
) -> ItemStack {
    let infinite = player.has_infinite_materials();
    let mut inventory = player.inventory.lock();
    let stack = match slot {
        AmmoSlot::Hand(hand) => inventory.get_item_in_hand_mut(hand),
        AmmoSlot::Inventory(index) => inventory.get_item_mut(index),
        AmmoSlot::Creative => return use_ammo(weapon, &mut projectile.clone(), false, infinite),
    };
    let used = use_ammo(weapon, stack, false, infinite);
    // Vanilla `player.getInventory().removeItem(projectile)` once emptied.
    if stack.is_empty() {
        *stack = ItemStack::empty();
    }
    inventory.set_changed();
    used
}

/// Vanilla `ProjectileWeaponItem.useAmmo`.
pub(crate) fn use_ammo(
    weapon: &ItemStack,
    projectile: &mut ItemStack,
    force_infinite: bool,
    holder_has_infinite_materials: bool,
) -> ItemStack {
    let ammo_to_use = if !force_infinite && !holder_has_infinite_materials {
        enchantment_helper::process_ammo_use(weapon, projectile, 1)
    } else {
        0
    };
    if ammo_to_use > projectile.count() {
        return ItemStack::empty();
    }
    if ammo_to_use == 0 {
        let mut copy = projectile.copy_with_count(1);
        copy.set(INTANGIBLE_PROJECTILE, ());
        return copy;
    }
    projectile.split(ammo_to_use)
}

/// Vanilla `ArrowItem.createArrow` (`ProjectileWeaponItem.createProjectile`
/// without the crit flag): spectral ammo makes a spectral arrow, anything
/// else a plain or tipped arrow, at the shooter's eye.
#[must_use]
pub(crate) fn create_arrow(
    world: &Arc<World>,
    projectile: &ItemStack,
    owner: &SharedEntity,
    fired_from_weapon: Option<&ItemStack>,
) -> SharedEntity {
    let position = owner.position();
    let position = DVec3::new(
        position.x,
        owner.get_eye_y() - ARROW_SPAWN_EYE_OFFSET,
        position.z,
    );
    let ammo = projectile.copy_with_count(1);
    let world_ref: Weak<World> = Arc::downgrade(world);
    if projectile.is(&vanilla_items::SPECTRAL_ARROW) {
        let arrow = SpectralArrowEntity::with_items(
            &vanilla_entities::SPECTRAL_ARROW,
            next_entity_id(),
            position,
            world_ref,
            &ammo,
            fired_from_weapon,
        );
        arrow.set_owner_entity(Some(owner));
        Arc::new(arrow)
    } else {
        let arrow = ArrowEntity::with_items(
            &vanilla_entities::ARROW,
            next_entity_id(),
            position,
            world_ref,
            &ammo,
            fired_from_weapon,
        );
        arrow.set_owner_entity(Some(owner));
        Arc::new(arrow)
    }
}

/// Returns `entity` as an arrow created by [`create_arrow`].
#[must_use]
pub(crate) fn as_abstract_arrow(entity: &SharedEntity) -> Option<&dyn AbstractArrow> {
    use steel_utils::Downcast as _;
    if let Some(arrow) = entity.downcast_ref::<ArrowEntity>() {
        return Some(arrow);
    }
    entity
        .downcast_ref::<SpectralArrowEntity>()
        .map(|arrow| arrow as &dyn AbstractArrow)
}

/// Vanilla `Projectile.spawnProjectile` tail for arrows: adds the arrow to
/// the world and runs `applyOnProjectileSpawned` for the ammo and, when it
/// differs, the weapon. Returns `false` when the world rejected it.
pub(crate) fn spawn_arrow(
    world: &Arc<World>,
    arrow: &SharedEntity,
    ammo: &ItemStack,
    owner: Option<&SharedEntity>,
) -> bool {
    if let Err(error) = world.try_add_entity(Arc::clone(arrow)) {
        log::debug!("failed to spawn arrow: {error}");
        return false;
    }
    let mut ammo = ammo.clone();
    enchantment_helper::on_projectile_spawned(world, &mut ammo, arrow.as_ref(), owner);
    if let Some(abstract_arrow) = as_abstract_arrow(arrow)
        && let Some(mut weapon) = abstract_arrow.get_weapon_item()
        && !weapon.is_empty()
        && !ammo.is(weapon.item())
    {
        enchantment_helper::on_projectile_spawned(world, &mut weapon, arrow.as_ref(), owner);
        if weapon.is_empty() {
            abstract_arrow.on_item_break();
        }
    }
    true
}

/// Vanilla `ProjectileWeaponItem.shoot` for a shooter firing along its own
/// rotation (the bow's `shootProjectile`): fans Multishot copies out by the
/// spread angle and wears the weapon once per arrow.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla ProjectileWeaponItem.shoot"
)]
pub(crate) fn shoot_from_rotation(
    world: &Arc<World>,
    shooter: &SharedEntity,
    weapon: &mut ItemStack,
    projectiles: &[ItemStack],
    power: f32,
    uncertainty: f32,
    is_crit: bool,
    has_infinite_materials: bool,
) {
    let max_angle = enchantment_helper::process_projectile_spread(weapon, shooter.as_ref(), 0.0);
    let count = projectiles.len();
    let angle_step = if count == 1 {
        0.0
    } else {
        2.0 * max_angle / (count - 1) as f32
    };
    let angle_offset = ((count as f32 - 1.0) % 2.0) * angle_step / 2.0;
    let mut direction = 1.0_f32;
    for (index, projectile) in projectiles.iter().enumerate() {
        if projectile.is_empty() {
            continue;
        }
        let angle = angle_offset + direction * index.div_ceil(2) as f32 * angle_step;
        direction = -direction;
        let arrow = create_arrow(world, projectile, shooter, Some(weapon));
        let Some(abstract_arrow) = as_abstract_arrow(&arrow) else {
            continue;
        };
        if is_crit {
            abstract_arrow.set_crit_arrow(true);
        }
        let (y_rot, x_rot) = shooter.rotation();
        abstract_arrow.shoot_from_rotation(
            shooter.as_ref(),
            x_rot,
            y_rot + angle,
            0.0,
            power,
            uncertainty,
        );
        spawn_arrow(world, &arrow, projectile, Some(shooter));
        // VANILLA: `getDurabilityUse` is 1 for arrows.
        weapon.hurt_and_break(1, has_infinite_materials);
        if weapon.is_empty() {
            break;
        }
    }
}
