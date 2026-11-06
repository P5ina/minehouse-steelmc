//! Crossbow item behavior (vanilla `CrossbowItem`).
//!
//! Charging loads ammo into the `charged_projectiles` component; the next use
//! fires it. Mobs fire at a target through [`perform_shooting`].

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use glam::{DVec3, Vec3};
use steel_macros::item_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::data_components::components::ChargedProjectiles;
use steel_registry::data_components::vanilla_components::CHARGED_PROJECTILES;
use steel_registry::item_stack::ItemStack;
use steel_registry::item_stack_template::ItemStackTemplate;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::stat::vanilla_stat_types;
use steel_registry::{sound_events, vanilla_entities, vanilla_items};

use super::projectile_weapon::{
    as_abstract_arrow, create_arrow, draw_player_ammo, find_player_projectile,
    is_arrow_or_firework, spawn_arrow, use_ammo,
};
use crate::behavior::{InteractionResult, ItemBehavior, ItemUseAnimation, UseItemContext};
use crate::enchantment_helper;
use crate::entity::entities::FireworkRocketEntity;
use crate::entity::{
    Entity, LivingEntity, LivingEntityRef, Projectile as _, SharedEntity, next_entity_id,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;

/// Vanilla `CrossbowItem.MAX_CHARGE_DURATION`, in seconds.
const MAX_CHARGE_DURATION: f32 = 1.25;
/// Vanilla `CrossbowItem.getUseDuration`.
const USE_DURATION: i32 = 72000;
/// Vanilla `CrossbowItem.START_SOUND_PERCENT`.
const START_SOUND_PERCENT: f32 = 0.2;
/// Vanilla `CrossbowItem.MID_SOUND_PERCENT`.
const MID_SOUND_PERCENT: f32 = 0.5;
/// Vanilla `CrossbowItem.ARROW_POWER`.
const ARROW_POWER: f32 = 3.15;
/// Vanilla `CrossbowItem.FIREWORK_POWER`.
const FIREWORK_POWER: f32 = 1.6;
/// Vanilla `CrossbowItem.createProjectile` firework height below the eye.
const FIREWORK_EYE_OFFSET: f64 = 0.15;

/// Vanilla `CrossbowItem.startSoundPlayed`. VANILLA: the flag lives on the
/// item singleton, so everyone charging a crossbow shares it.
static START_SOUND_PLAYED: AtomicBool = AtomicBool::new(false);
/// Vanilla `CrossbowItem.midLoadSoundPlayed`, shared like [`START_SOUND_PLAYED`].
static MID_LOAD_SOUND_PLAYED: AtomicBool = AtomicBool::new(false);

/// The crossbow.
#[item_behavior(class = "CrossbowItem")]
pub struct CrossbowItem;

/// Vanilla `CrossbowItem.isCharged`.
#[must_use]
pub(crate) fn is_charged(stack: &ItemStack) -> bool {
    stack
        .get(CHARGED_PROJECTILES)
        .is_some_and(|projectiles| !projectiles.items().is_empty())
}

/// Vanilla `CrossbowItem.getChargeDuration`: ticks to fully charge.
#[must_use]
pub(crate) fn get_charge_duration(crossbow: &ItemStack) -> i32 {
    let duration = enchantment_helper::modify_crossbow_charging_time(crossbow, MAX_CHARGE_DURATION);
    (duration * 20.0).floor() as i32
}

/// Vanilla `CrossbowItem.getPowerForTime`.
fn get_power_for_time(time_held: i32, crossbow: &ItemStack) -> f32 {
    (time_held as f32 / get_charge_duration(crossbow) as f32).min(1.0)
}

/// Vanilla `CrossbowItem.getShootingPower`.
fn get_shooting_power(projectiles: &ChargedProjectiles) -> f32 {
    if contains_firework(projectiles) {
        FIREWORK_POWER
    } else {
        ARROW_POWER
    }
}

fn contains_firework(projectiles: &ChargedProjectiles) -> bool {
    projectiles
        .items()
        .iter()
        .any(|template| template.item() == &*vanilla_items::FIREWORK_ROCKET)
}

/// Vanilla `CrossbowItem.tryLoadProjectiles`: draws ammo for `shooter` and
/// stores it in `crossbow`. Returns whether anything was loaded.
pub(crate) fn try_load_projectiles(
    world: &Arc<World>,
    shooter: LivingEntityRef<'_>,
    crossbow: &mut ItemStack,
) -> bool {
    let drawn = draw_ammo(world, shooter, crossbow);
    if drawn.is_empty() {
        return false;
    }
    // Vanilla `ChargedProjectiles.ofNonEmpty` / `ItemStackTemplate.fromNonEmptyStack`.
    let templates = drawn
        .iter()
        .filter_map(|stack| ItemStackTemplate::from_stack(stack).ok())
        .collect();
    match ChargedProjectiles::new(templates) {
        Ok(projectiles) => {
            crossbow.set(CHARGED_PROJECTILES, projectiles);
            true
        }
        Err(error) => {
            log::warn!("failed to charge crossbow: {error}");
            false
        }
    }
}

/// Vanilla `ProjectileWeaponItem.draw(heldItem, shooter.getProjectile(heldItem), shooter)`.
fn draw_ammo(
    world: &Arc<World>,
    shooter: LivingEntityRef<'_>,
    crossbow: &ItemStack,
) -> Vec<ItemStack> {
    let entity = shooter.entity();
    if let Some(player) = world.players.get_by_entity_id(entity.id()) {
        let Some((slot, projectile)) = find_player_projectile(&player, is_arrow_or_firework) else {
            return Vec::new();
        };
        return draw_player_ammo(&player, crossbow, slot, &projectile);
    }
    draw_mob_ammo(shooter.living(), crossbow)
}

/// Vanilla `Monster.getProjectile` + `ProjectileWeaponItem.draw` for a mob:
/// a held arrow or firework (taken from the hand), else a fresh arrow.
fn draw_mob_ammo(shooter: &dyn LivingEntity, crossbow: &ItemStack) -> Vec<ItemStack> {
    let infinite = shooter.has_infinite_materials();
    let num_projectiles = enchantment_helper::process_projectile_count(crossbow, shooter, 1);
    let mut equipment = shooter.living_base().equipment().lock();
    let held_slot = [EquipmentSlot::OffHand, EquipmentSlot::MainHand]
        .into_iter()
        .find(|&slot| is_arrow_or_firework(equipment.get_ref(slot)));
    let mut fresh_arrow = ItemStack::new(&vanilla_items::ARROW);
    let projectile = match held_slot {
        Some(slot) => equipment.get_mut(slot),
        None => &mut fresh_arrow,
    };
    if projectile.is_empty() {
        return Vec::new();
    }
    let projectile_copy = projectile.clone();
    let mut drawn = Vec::with_capacity(usize::try_from(num_projectiles).unwrap_or(0));
    for index in 0..num_projectiles {
        let stack = if index == 0 {
            use_ammo(crossbow, projectile, false, infinite)
        } else {
            use_ammo(crossbow, &mut projectile_copy.clone(), true, infinite)
        };
        if !stack.is_empty() {
            drawn.push(stack);
        }
    }
    drawn
}

/// Vanilla `CrossbowItem.performShooting`: fires everything loaded in
/// `weapon` (at `target` when given, else along the shooter's view) and
/// empties it. Returns whether anything was fired.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla CrossbowItem.performShooting"
)]
pub(crate) fn perform_shooting(
    world: &Arc<World>,
    shooter: &SharedEntity,
    weapon: &mut ItemStack,
    power: f32,
    uncertainty: f32,
    target: Option<&SharedEntity>,
    is_player: bool,
    has_infinite_materials: bool,
) -> bool {
    let Some(charged) = weapon.get(CHARGED_PROJECTILES).cloned() else {
        return false;
    };
    weapon.set(CHARGED_PROJECTILES, ChargedProjectiles::empty());
    if charged.items().is_empty() {
        return false;
    }
    let projectiles: Vec<ItemStack> = charged
        .items()
        .iter()
        .map(ItemStackTemplate::create)
        .collect();
    shoot(
        world,
        shooter,
        weapon,
        &projectiles,
        power,
        uncertainty,
        is_player,
        target,
        has_infinite_materials,
    );
    true
}

/// Vanilla `ProjectileWeaponItem.shoot` with the crossbow's projectile and
/// aim overrides.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla ProjectileWeaponItem.shoot"
)]
fn shoot(
    world: &Arc<World>,
    shooter: &SharedEntity,
    weapon: &mut ItemStack,
    projectiles: &[ItemStack],
    power: f32,
    uncertainty: f32,
    is_crit: bool,
    target: Option<&SharedEntity>,
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
        let projectile_entity = create_projectile(world, shooter, weapon, projectile, is_crit);
        shoot_projectile(
            world,
            shooter,
            &projectile_entity,
            index,
            power,
            uncertainty,
            angle,
            target,
        );
        spawn_projectile(world, &projectile_entity, projectile, shooter);
        weapon.hurt_and_break(get_durability_use(projectile), has_infinite_materials);
        if weapon.is_empty() {
            break;
        }
    }
}

/// Vanilla `CrossbowItem.getDurabilityUse`.
fn get_durability_use(projectile: &ItemStack) -> i32 {
    if projectile.is(&vanilla_items::FIREWORK_ROCKET) {
        3
    } else {
        1
    }
}

/// Vanilla `CrossbowItem.createProjectile`.
fn create_projectile(
    world: &Arc<World>,
    shooter: &SharedEntity,
    weapon: &ItemStack,
    projectile: &ItemStack,
    is_crit: bool,
) -> SharedEntity {
    if projectile.is(&vanilla_items::FIREWORK_ROCKET) {
        let position = shooter.position();
        let position = DVec3::new(
            position.x,
            shooter.get_eye_y() - FIREWORK_EYE_OFFSET,
            position.z,
        );
        let world_ref: Weak<World> = Arc::downgrade(world);
        let rocket = FireworkRocketEntity::launched(
            &vanilla_entities::FIREWORK_ROCKET,
            next_entity_id(),
            position,
            world_ref,
            projectile.clone(),
        );
        rocket.set_owner_entity(Some(shooter));
        rocket.set_shot_at_angle(true);
        return Arc::new(rocket);
    }
    let arrow = create_arrow(world, projectile, shooter, Some(weapon));
    if let Some(abstract_arrow) = as_abstract_arrow(&arrow) {
        if is_crit {
            abstract_arrow.set_crit_arrow(true);
        }
        abstract_arrow.set_sound_event(&sound_events::ITEM_CROSSBOW_HIT);
    }
    arrow
}

/// Vanilla `Projectile.spawnProjectile` tail: arrows also run their weapon's
/// spawn effects, anything else only the ammo's.
fn spawn_projectile(
    world: &Arc<World>,
    projectile_entity: &SharedEntity,
    ammo: &ItemStack,
    owner: &SharedEntity,
) {
    if as_abstract_arrow(projectile_entity).is_some() {
        spawn_arrow(world, projectile_entity, ammo, Some(owner));
        return;
    }
    if let Err(error) = world.try_add_entity(Arc::clone(projectile_entity)) {
        log::debug!("failed to spawn crossbow projectile: {error}");
        return;
    }
    let mut ammo = ammo.clone();
    enchantment_helper::on_projectile_spawned(
        world,
        &mut ammo,
        projectile_entity.as_ref(),
        Some(owner),
    );
}

/// Vanilla `CrossbowItem.shootProjectile`.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla CrossbowItem.shootProjectile"
)]
fn shoot_projectile(
    world: &Arc<World>,
    shooter: &SharedEntity,
    projectile_entity: &SharedEntity,
    index: usize,
    power: f32,
    uncertainty: f32,
    angle: f32,
    target: Option<&SharedEntity>,
) {
    let shot_vector = if let Some(target) = target {
        let shooter_position = shooter.position();
        let target_position = target.position();
        let xd = target_position.x - shooter_position.x;
        let zd = target_position.z - shooter_position.z;
        let distance_to_target = (xd * xd + zd * zd).sqrt();
        // Vanilla `targetOverride.getY(1/3)`.
        let target_y = target_position.y
            + f64::from(target.bounding_box().height() as f32) * 0.333_333_333_333_333_3;
        let yd =
            target_y - projectile_entity.position().y + distance_to_target * f64::from(0.2_f32);
        get_projectile_shot_vector(shooter.as_ref(), DVec3::new(xd, yd, zd), angle)
    } else {
        let up_vector = up_vector(shooter.as_ref()).as_vec3();
        let view_vector = shooter.look_angle().as_vec3();
        // Vanilla `new Quaternionf().setAngleAxis(angle, up)` then `rotate`.
        quaternion_rotate(view_vector, angle.to_radians(), up_vector)
    };
    if let Some(projectile) = projectile_entity.as_projectile() {
        projectile.shoot(shot_vector.as_dvec3(), power, uncertainty);
    }
    let pitch = get_shot_pitch(index);
    world.play_sound_at(
        &sound_events::ITEM_CROSSBOW_SHOOT,
        shooter.sound_source(),
        shooter.position(),
        1.0,
        pitch,
        None,
    );
}

/// Vanilla `Entity.getUpVector(1.0F)`.
fn up_vector(entity: &dyn Entity) -> DVec3 {
    let (y_rot, x_rot) = entity.rotation();
    entity.calculate_view_vector(x_rot - 90.0, y_rot)
}

/// Vanilla `CrossbowItem.getProjectileShotVector`.
fn get_projectile_shot_vector(body: &dyn Entity, original_vector: DVec3, angle: f32) -> Vec3 {
    let view_vec = original_vector.as_vec3().normalize();
    let mut right_vector_pre_rot = view_vec.cross(Vec3::Y);
    if right_vector_pre_rot.length_squared() <= 1.0E-7 {
        right_vector_pre_rot = view_vec.cross(up_vector(body).as_vec3());
    }
    let view_vec_up = joml_rotate_axis(view_vec, FRAC_PI_2, right_vector_pre_rot);
    joml_rotate_axis(view_vec, angle.to_radians(), view_vec_up)
}

/// JOML `Math.cosFromSin(sin, angle)`.
fn cos_from_sin(sin: f32, angle: f32) -> f32 {
    let cos = (1.0 - sin * sin).sqrt();
    let tau = TAU;
    let a = angle + FRAC_PI_2;
    let mut b = a - (a / tau).trunc() * tau;
    if b < 0.0 {
        b += tau;
    }
    if b >= PI { -cos } else { cos }
}

/// JOML `Vector3f.rotateAxis(angle, x, y, z)`. The axis is not normalized,
/// exactly like JOML, which matters for the unnormalized cross product
/// vanilla passes in.
fn joml_rotate_axis(vector: Vec3, angle: f32, axis: Vec3) -> Vec3 {
    let half_angle = angle * 0.5;
    let sin_angle = half_angle.sin();
    let qx = axis.x * sin_angle;
    let qy = axis.y * sin_angle;
    let qz = axis.z * sin_angle;
    let qw = cos_from_sin(sin_angle, half_angle);
    let w2 = qw * qw;
    let x2 = qx * qx;
    let y2 = qy * qy;
    let z2 = qz * qz;
    let zw = qz * qw;
    let xy = qx * qy;
    let xz = qx * qz;
    let yw = qy * qw;
    let yz = qy * qz;
    let xw = qx * qw;
    let (x, y, z) = (vector.x, vector.y, vector.z);
    Vec3::new(
        (w2 + x2 - z2 - y2) * x + (-zw + xy - zw + xy) * y + (yw + xz + xz + yw) * z,
        (xy + zw + zw + xy) * x + (y2 - z2 + w2 - x2) * y + (yz + yz - xw - xw) * z,
        (xz - yw + xz - yw) * x + (yz + yz + xw + xw) * y + (z2 - y2 - x2 + w2) * z,
    )
}

/// JOML `Quaternionf.setAngleAxis(angle, axis)` followed by
/// `Vector3f.rotate(quaternion)` (which normalizes by the quaternion length).
fn quaternion_rotate(vector: Vec3, angle: f32, axis: Vec3) -> Vec3 {
    let sin = (angle * 0.5).sin();
    let qx = axis.x * sin;
    let qy = axis.y * sin;
    let qz = axis.z * sin;
    let qw = cos_from_sin(sin, angle * 0.5);
    let xx = qx * qx;
    let yy = qy * qy;
    let zz = qz * qz;
    let ww = qw * qw;
    let xy = qx * qy;
    let xz = qx * qz;
    let yz = qy * qz;
    let xw = qx * qw;
    let zw = qz * qw;
    let yw = qy * qw;
    let k = 1.0 / (xx + yy + zz + ww);
    let (x, y, z) = (vector.x, vector.y, vector.z);
    // JOML `Math.fma` is a plain multiply-add unless `joml.useMathFma` is set.
    Vec3::new(
        (xx - yy - zz + ww) * k * x + (2.0 * (xy - zw) * k * y + 2.0 * (xz + yw) * k * z),
        2.0 * (xy + zw) * k * x + ((yy - xx - zz + ww) * k * y + 2.0 * (yz - xw) * k * z),
        2.0 * (xz - yw) * k * x + (2.0 * (yz + xw) * k * y + (zz - xx - yy + ww) * k * z),
    )
}

/// Vanilla `CrossbowItem.getShotPitch`.
fn get_shot_pitch(index: usize) -> f32 {
    if index == 0 {
        1.0
    } else {
        let range_decider = if index & 1 == 1 { 0.63 } else { 0.43 };
        1.0 / (rand::random::<f32>() * 0.5 + 1.8) + range_decider
    }
}

fn play_charging_sound(
    world: &World,
    entity: &dyn Entity,
    sound: SoundEventRef,
    source: SoundSource,
    pitch: f32,
    volume: f32,
) {
    world.play_sound_at(sound, source, entity.position(), volume, pitch, None);
}

impl ItemBehavior for CrossbowItem {
    fn use_item(&self, context: &mut UseItemContext) -> InteractionResult {
        let charged = context
            .inv
            .with_item(|stack| stack.get(CHARGED_PROJECTILES).cloned());
        if let Some(charged) = charged
            && !charged.items().is_empty()
        {
            let power = get_shooting_power(&charged);
            let shooter: SharedEntity = Arc::clone(context.player) as SharedEntity;
            let infinite = context.player.has_infinite_materials();
            let mut weapon = context.inv.with_item(|stack| stack.clone());
            let fired = perform_shooting(
                context.world,
                &shooter,
                &mut weapon,
                power,
                1.0,
                None,
                true,
                infinite,
            );
            let item = weapon.item();
            context.inv.with_item(|stack| *stack = weapon);
            if fired {
                // TODO: `CriteriaTriggers.SHOT_CROSSBOW` once advancement triggers exist.
                context
                    .player
                    .award_stat(&vanilla_stat_types::ITEM_USED, item);
            }
            return InteractionResult::Consume;
        }
        if find_player_projectile(context.player, is_arrow_or_firework).is_some() {
            START_SOUND_PLAYED.store(false, Ordering::Relaxed);
            MID_LOAD_SOUND_PLAYED.store(false, Ordering::Relaxed);
            context.player.start_using_item(context.hand);
            return InteractionResult::Consume;
        }
        InteractionResult::Fail
    }

    fn get_use_animation(&self, _stack: &ItemStack) -> ItemUseAnimation {
        ItemUseAnimation::Crossbow
    }

    fn get_use_duration(&self, _stack: &ItemStack, _user: &dyn LivingEntity) -> i32 {
        USE_DURATION
    }

    fn on_use_tick(
        &self,
        world: &Arc<World>,
        user: LivingEntityRef<'_>,
        stack: &mut ItemStack,
        ticks_remaining: i32,
    ) {
        let entity = user.entity().as_ref();
        let sounds = enchantment_helper::pick_crossbow_charging_sounds(stack);
        let start = sounds.map_or(Some(&sound_events::ITEM_CROSSBOW_LOADING_START), |s| {
            s.start
        });
        let mid = sounds.map_or(Some(&sound_events::ITEM_CROSSBOW_LOADING_MIDDLE), |s| s.mid);
        let end = sounds.map_or(Some(&sound_events::ITEM_CROSSBOW_LOADING_END), |s| s.end);
        let tick_percent = (self.get_use_duration(stack, user.living()) - ticks_remaining) as f32
            / get_charge_duration(stack) as f32;
        if tick_percent < START_SOUND_PERCENT {
            START_SOUND_PLAYED.store(false, Ordering::Relaxed);
            MID_LOAD_SOUND_PLAYED.store(false, Ordering::Relaxed);
        }
        if tick_percent >= START_SOUND_PERCENT
            && !START_SOUND_PLAYED.swap(true, Ordering::Relaxed)
            && let Some(sound) = start
        {
            play_charging_sound(world, entity, sound, SoundSource::Players, 1.0, 0.5);
        }
        if tick_percent >= MID_SOUND_PERCENT
            && !MID_LOAD_SOUND_PLAYED.swap(true, Ordering::Relaxed)
            && let Some(sound) = mid
        {
            play_charging_sound(world, entity, sound, SoundSource::Players, 1.0, 0.5);
        }
        if tick_percent >= 1.0
            && !is_charged(stack)
            && try_load_projectiles(world, user, stack)
            && let Some(sound) = end
        {
            let pitch = 1.0 / (rand::random::<f32>() * 0.5 + 1.0) + 0.2;
            play_charging_sound(world, entity, sound, entity.sound_source(), pitch, 1.0);
        }
    }

    fn release_using(
        &self,
        stack: &mut ItemStack,
        _world: &Arc<World>,
        user: &dyn LivingEntity,
        time_left: i32,
    ) -> bool {
        let time_held = self.get_use_duration(stack, user) - time_left;
        get_power_for_time(time_held, stack) >= 1.0 && is_charged(stack)
    }

    fn use_on_release(&self, stack: &ItemStack) -> bool {
        stack.is(&vanilla_items::CROSSBOW)
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;
    use std::sync::Arc;

    use glam::Vec3;
    use steel_registry::data_components::vanilla_components::CHARGED_PROJECTILES;
    use steel_registry::item_stack::ItemStack;
    use steel_registry::{
        init_vanilla_registry, vanilla_enchantments, vanilla_entities, vanilla_items,
    };
    use steel_utils::ChunkPos;

    use super::{
        ARROW_POWER, get_charge_duration, is_charged, joml_rotate_axis, perform_shooting,
        quaternion_rotate, try_load_projectiles,
    };
    use crate::behavior::{ITEM_BEHAVIORS, init_behaviors};
    use crate::entity::{Entity as _, LivingEntityRef, SharedEntity, next_entity_id};
    use crate::inventory::container::Container as _;
    use crate::player::{Player, ResetReason};
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, enchanted_item, fresh_test_world, insert_ready_full_chunk,
    };
    use crate::world::World;

    fn world(name: &'static str) -> TestWorld {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world(name);
        insert_ready_full_chunk(&fixture.world, ChunkPos::new(0, 0));
        fixture
    }

    fn joined_player(world: &Arc<World>) -> Arc<Player> {
        let player =
            TestPlayerBuilder::new(Arc::clone(world), "Crossbowman", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        player
    }

    fn arrows_near(world: &Arc<World>, player: &Arc<Player>) -> Vec<SharedEntity> {
        world.get_entities_in_aabb_matching(&player.bounding_box().inflate(10.0), |entity| {
            entity.entity_type() == &vanilla_entities::ARROW
        })
    }

    #[test]
    fn plain_crossbow_charges_in_25_ticks_and_starts_empty() {
        init_vanilla_registry();
        let crossbow = ItemStack::new(&vanilla_items::CROSSBOW);
        assert_eq!(get_charge_duration(&crossbow), 25);
        assert!(!is_charged(&crossbow));
    }

    #[test]
    fn quick_charge_three_charges_in_ten_ticks() {
        init_vanilla_registry();
        let crossbow = enchanted_item(
            &vanilla_items::CROSSBOW,
            vanilla_enchantments::QUICK_CHARGE.key.clone(),
            3,
        );
        assert_eq!(get_charge_duration(&crossbow), 10);
    }

    #[test]
    fn charging_loads_one_arrow_and_shooting_fires_it() {
        let fixture = world("crossbow_charge_and_shoot");
        let world = &fixture.world;
        let player = joined_player(world);
        player
            .inventory
            .lock()
            .set_item(5, ItemStack::with_count(&vanilla_items::ARROW, 3));
        let entity: SharedEntity = Arc::clone(&player) as SharedEntity;
        let user = LivingEntityRef::new(&entity).expect("players are living");
        let behavior = ITEM_BEHAVIORS.get_behavior(&vanilla_items::CROSSBOW);
        let mut crossbow = ItemStack::new(&vanilla_items::CROSSBOW);

        behavior.on_use_tick(world, user, &mut crossbow, 72000 - 24);
        assert!(!is_charged(&crossbow), "not loaded before the charge time");
        behavior.on_use_tick(world, user, &mut crossbow, 72000 - 25);
        assert!(is_charged(&crossbow));
        assert_eq!(player.inventory.lock().get_item(5).count(), 2);
        assert!(behavior.release_using(&mut crossbow, world, player.as_ref(), 72000 - 25));

        assert!(perform_shooting(
            world,
            &entity,
            &mut crossbow,
            ARROW_POWER,
            1.0,
            None,
            true,
            false,
        ));
        assert!(!is_charged(&crossbow));
        let arrows = arrows_near(world, &player);
        assert_eq!(arrows.len(), 1);
        assert!((arrows[0].velocity().length() - f64::from(ARROW_POWER)).abs() < 0.2);
        assert_eq!(crossbow.get_damage_value(), 1);
    }

    #[test]
    fn multishot_loads_three_but_uses_one_arrow() {
        let fixture = world("crossbow_multishot");
        let world = &fixture.world;
        let player = joined_player(world);
        player
            .inventory
            .lock()
            .set_item(5, ItemStack::with_count(&vanilla_items::ARROW, 3));
        let entity: SharedEntity = Arc::clone(&player) as SharedEntity;
        let user = LivingEntityRef::new(&entity).expect("players are living");
        let mut crossbow = enchanted_item(
            &vanilla_items::CROSSBOW,
            vanilla_enchantments::MULTISHOT.key.clone(),
            1,
        );

        assert!(try_load_projectiles(world, user, &mut crossbow));
        let loaded = crossbow
            .get(CHARGED_PROJECTILES)
            .map_or(0, |projectiles| projectiles.items().len());
        assert_eq!(loaded, 3);
        assert_eq!(player.inventory.lock().get_item(5).count(), 2);

        assert!(perform_shooting(
            world,
            &entity,
            &mut crossbow,
            ARROW_POWER,
            1.0,
            None,
            true,
            false,
        ));
        assert_eq!(arrows_near(world, &player).len(), 3);
    }

    #[test]
    fn unit_axis_rotations_match_a_plain_rotation() {
        let rotated = joml_rotate_axis(Vec3::Z, FRAC_PI_2, Vec3::Y);
        assert!((rotated - Vec3::X).length() < 1.0E-6, "{rotated:?}");
        let rotated = quaternion_rotate(Vec3::Z, FRAC_PI_2, Vec3::Y);
        assert!((rotated - Vec3::X).length() < 1.0E-6, "{rotated:?}");
    }
}
