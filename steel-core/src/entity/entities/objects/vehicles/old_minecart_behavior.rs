//! Vanilla `OldMinecartBehavior`: the default minecart rail movement.
//!
//! Only the server half is ported; the client-side interpolation hooks have nothing to
//! do on a server.

use std::f64::consts::PI;
use std::sync::Arc;

use glam::DVec3;
use steel_math::wrap_degrees;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, RailShape};
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{vanilla_blocks, vanilla_entities};
use steel_utils::BlockPos;

use super::abstract_minecart::{AbstractMinecart, come_off_track, exits, is_rail};
use crate::entity::selector::EntitySelector;
use crate::entity::{Entity, SharedEntity};
use crate::physics::MoverType;
use crate::player::Player;

/// Speed under which a halting powered rail stops the cart outright.
const HALT_SPEED: f64 = 0.03;
/// Speed a powered rail adds per tick.
const POWERED_RAIL_BOOST: f64 = 0.06;
/// Push a powered rail gives a stopped cart away from a solid block.
const POWERED_RAIL_KICK: f64 = 0.02;
/// Horizontal speed a rideable cart needs before it scoops up mobs.
const RIDABLE_THRESHOLD: f64 = 0.01;

/// Vanilla `OldMinecartBehavior.tick` (server branch).
pub(super) fn tick<M: AbstractMinecart>(minecart: &Arc<M>) {
    let Some(world) = minecart.level() else {
        return;
    };
    minecart.apply_gravity();
    let rail_pos = minecart.current_block_pos_or_rail_below();
    let state = world.get_block_state(rail_pos);
    let on_rails = is_rail(state);
    minecart.set_on_rails(on_rails);
    if on_rails {
        move_along_track(minecart);
        if state.get_block() == &vanilla_blocks::ACTIVATOR_RAIL {
            minecart.activate_minecart(
                &world,
                rail_pos,
                state.get_value(&BlockStateProperties::POWERED),
            );
        }
    } else {
        come_off_track(minecart);
    }
    drop(world);

    minecart.apply_effects_from_blocks();
    let mut yaw = minecart.rotation().0;
    let pitch: f32 = 0.0;
    let old_position = minecart.old_position();
    let position = minecart.position();
    let x_diff = old_position.x - position.x;
    let z_diff = old_position.z - position.z;
    if x_diff * x_diff + z_diff * z_diff > 0.001 {
        yaw = (z_diff.atan2(x_diff) * 180.0 / PI) as f32;
        if minecart.is_flipped() {
            yaw += 180.0;
        }
    }

    let rotation_diff = f64::from(wrap_degrees(yaw - minecart.base().old_rotation().0));
    if !(-170.0..170.0).contains(&rotation_diff) {
        yaw += 180.0;
        minecart.set_flipped(!minecart.is_flipped());
    }

    minecart.set_rotation((yaw % 360.0, pitch % 360.0));
    push_and_pickup_entities(minecart);
}

/// Vanilla `OldMinecartBehavior.moveAlongTrack`.
#[expect(
    clippy::too_many_lines,
    reason = "keeps vanilla's single rail-movement pass in order"
)]
pub(super) fn move_along_track<M: AbstractMinecart>(minecart: &Arc<M>) {
    let Some(world) = minecart.level() else {
        return;
    };
    let pos = minecart.current_block_pos_or_rail_below();
    let state = world.get_block_state(pos);
    minecart.reset_fall_distance();
    let start = minecart.position();
    let old_track_pos = track_pos(minecart.as_ref(), start);
    let mut y = f64::from(pos.y());
    let mut power_track = false;
    let mut halt_track = false;
    if state.get_block() == &vanilla_blocks::POWERED_RAIL {
        power_track = state.get_value(&BlockStateProperties::POWERED);
        halt_track = !power_track;
    }

    let mut slide_speed = 0.007_812_5;
    if minecart.is_in_water() {
        slide_speed *= 0.2;
    }

    let shape = state.get_value(&BlockStateProperties::RAIL_SHAPE);
    let slope_push = match shape {
        RailShape::AscendingEast => Some(DVec3::new(-slide_speed, 0.0, 0.0)),
        RailShape::AscendingWest => Some(DVec3::new(slide_speed, 0.0, 0.0)),
        RailShape::AscendingNorth => Some(DVec3::new(0.0, 0.0, slide_speed)),
        RailShape::AscendingSouth => Some(DVec3::new(0.0, 0.0, -slide_speed)),
        _ => None,
    };
    if let Some(push) = slope_push {
        minecart.set_velocity(minecart.velocity() + push);
        y += 1.0;
    }

    let movement = minecart.velocity();
    let (exit0, exit1) = exits(shape);
    let mut x_dir = f64::from(exit1.x - exit0.x);
    let mut z_dir = f64::from(exit1.z - exit0.z);
    let length = (x_dir * x_dir + z_dir * z_dir).sqrt();
    if movement.x * x_dir + movement.z * z_dir < 0.0 {
        x_dir = -x_dir;
        z_dir = -z_dir;
    }

    let pow = horizontal_distance(movement).min(2.0);
    minecart.set_velocity(DVec3::new(
        pow * x_dir / length,
        movement.y,
        pow * z_dir / length,
    ));

    let rider = minecart.first_passenger();
    let move_intent = rider
        .as_deref()
        .and_then(Entity::as_player)
        .map_or(DVec3::ZERO, Player::last_client_move_intent);
    if rider.as_deref().and_then(Entity::as_player).is_some() && move_intent.length_squared() > 0.0
    {
        let rider_movement = move_intent.normalize();
        let own_distance = horizontal_distance_sqr(minecart.velocity());
        if rider_movement.length_squared() > 0.0 && own_distance < 0.01 {
            minecart.set_velocity(
                minecart.velocity() + DVec3::new(move_intent.x * 0.001, 0.0, move_intent.z * 0.001),
            );
            halt_track = false;
        }
    }

    if halt_track {
        if horizontal_distance(minecart.velocity()) < HALT_SPEED {
            minecart.set_velocity(DVec3::ZERO);
        } else {
            minecart.set_velocity(minecart.velocity() * DVec3::new(0.5, 0.0, 0.5));
        }
    }

    let block_x = f64::from(pos.x());
    let block_z = f64::from(pos.z());
    let x0 = block_x + 0.5 + f64::from(exit0.x) * 0.5;
    let z0 = block_z + 0.5 + f64::from(exit0.z) * 0.5;
    let x1 = block_x + 0.5 + f64::from(exit1.x) * 0.5;
    let z1 = block_z + 0.5 + f64::from(exit1.z) * 0.5;
    let x_span = x1 - x0;
    let z_span = z1 - z0;
    let progress = if x_span == 0.0 {
        start.z - block_z
    } else if z_span == 0.0 {
        start.x - block_x
    } else {
        ((start.x - x0) * x_span + (start.z - z0) * z_span) * 2.0
    };
    set_pos(
        minecart.as_ref(),
        DVec3::new(x0 + x_span * progress, y, z0 + z_span * progress),
    );

    let scale = if minecart.is_vehicle() { 0.75 } else { 1.0 };
    let max_speed = minecart.max_speed();
    let movement = minecart.velocity();
    Arc::clone(minecart).move_entity(
        MoverType::SelfMovement,
        DVec3::new(
            (scale * movement.x).clamp(-max_speed, max_speed),
            0.0,
            (scale * movement.z).clamp(-max_speed, max_speed),
        ),
    );

    let position = minecart.position();
    let block_offset_x = position.x.floor() as i32 - pos.x();
    let block_offset_z = position.z.floor() as i32 - pos.z();
    if exit0.y != 0 && block_offset_x == exit0.x && block_offset_z == exit0.z {
        set_pos(
            minecart.as_ref(),
            position + DVec3::new(0.0, f64::from(exit0.y), 0.0),
        );
    } else if exit1.y != 0 && block_offset_x == exit1.x && block_offset_z == exit1.z {
        set_pos(
            minecart.as_ref(),
            position + DVec3::new(0.0, f64::from(exit1.y), 0.0),
        );
    }

    minecart.set_velocity(minecart.apply_natural_slowdown(minecart.velocity()));
    let position = minecart.position();
    if let (Some(new_track_pos), Some(old_track_pos)) =
        (track_pos(minecart.as_ref(), position), old_track_pos)
    {
        let speed = (old_track_pos.y - new_track_pos.y) * 0.05;
        let velocity = minecart.velocity();
        let other_pow = horizontal_distance(velocity);
        if other_pow > 0.0 {
            let factor = (other_pow + speed) / other_pow;
            minecart.set_velocity(velocity * DVec3::new(factor, 1.0, factor));
        }
        set_pos(
            minecart.as_ref(),
            DVec3::new(position.x, new_track_pos.y, position.z),
        );
    }

    let position = minecart.position();
    let new_x = position.x.floor() as i32;
    let new_z = position.z.floor() as i32;
    if new_x != pos.x() || new_z != pos.z() {
        let velocity = minecart.velocity();
        let other_pow = horizontal_distance(velocity);
        minecart.set_velocity(DVec3::new(
            other_pow * f64::from(new_x - pos.x()),
            velocity.y,
            other_pow * f64::from(new_z - pos.z()),
        ));
    }

    if power_track {
        apply_powered_rail(minecart.as_ref(), pos, shape);
    }
}

/// The powered-rail tail of vanilla `moveAlongTrack`.
fn apply_powered_rail<M: AbstractMinecart>(minecart: &M, pos: BlockPos, shape: RailShape) {
    let velocity = minecart.velocity();
    let speed = horizontal_distance(velocity);
    if speed > 0.01 {
        minecart.set_velocity(
            velocity
                + DVec3::new(
                    velocity.x / speed * POWERED_RAIL_BOOST,
                    0.0,
                    velocity.z / speed * POWERED_RAIL_BOOST,
                ),
        );
        return;
    }

    let mut x = velocity.x;
    let mut z = velocity.z;
    match shape {
        RailShape::EastWest => {
            if minecart.is_redstone_conductor_at(pos.west()) {
                x = POWERED_RAIL_KICK;
            } else if minecart.is_redstone_conductor_at(pos.east()) {
                x = -POWERED_RAIL_KICK;
            }
        }
        RailShape::NorthSouth => {
            if minecart.is_redstone_conductor_at(pos.north()) {
                z = POWERED_RAIL_KICK;
            } else if minecart.is_redstone_conductor_at(pos.south()) {
                z = -POWERED_RAIL_KICK;
            }
        }
        _ => return,
    }
    minecart.set_velocity(DVec3::new(x, velocity.y, z));
}

/// Vanilla `OldMinecartBehavior.getPos`: the point on the rail under `position`.
fn track_pos<M: AbstractMinecart + ?Sized>(minecart: &M, position: DVec3) -> Option<DVec3> {
    let world = minecart.level()?;
    let x_block = position.x.floor() as i32;
    let mut y_block = position.y.floor() as i32;
    let z_block = position.z.floor() as i32;
    if world
        .get_block_state(BlockPos::new(x_block, y_block - 1, z_block))
        .get_block()
        .has_tag(&BlockTag::RAILS)
    {
        y_block -= 1;
    }

    let state = world.get_block_state(BlockPos::new(x_block, y_block, z_block));
    if !is_rail(state) {
        return None;
    }

    let (exit0, exit1) = exits(state.get_value(&BlockStateProperties::RAIL_SHAPE));
    let (xt, yt, zt) = (f64::from(x_block), f64::from(y_block), f64::from(z_block));
    let x0 = xt + 0.5 + f64::from(exit0.x) * 0.5;
    let y0 = yt + 0.0625 + f64::from(exit0.y) * 0.5;
    let z0 = zt + 0.5 + f64::from(exit0.z) * 0.5;
    let x1 = xt + 0.5 + f64::from(exit1.x) * 0.5;
    let y1 = yt + 0.0625 + f64::from(exit1.y) * 0.5;
    let z1 = zt + 0.5 + f64::from(exit1.z) * 0.5;
    let x_span = x1 - x0;
    let y_span = (y1 - y0) * 2.0;
    let z_span = z1 - z0;
    let progress = if x_span == 0.0 {
        position.z - zt
    } else if z_span == 0.0 {
        position.x - xt
    } else {
        ((position.x - x0) * x_span + (position.z - z0) * z_span) * 2.0
    };

    let mut y = y0 + y_span * progress;
    if y_span < 0.0 {
        y += 1.0;
    } else if y_span > 0.0 {
        y += 0.5;
    }
    Some(DVec3::new(
        x0 + x_span * progress,
        y,
        z0 + z_span * progress,
    ))
}

/// Vanilla `OldMinecartBehavior.pushAndPickupEntities`.
fn push_and_pickup_entities<M: AbstractMinecart>(minecart: &Arc<M>) {
    let Some(world) = minecart.level() else {
        return;
    };
    let hitbox = minecart
        .bounding_box()
        .inflate_xyz(f64::from(0.2_f32), 0.0, f64::from(0.2_f32));
    let nearby: Vec<SharedEntity> = world
        .get_entities_in_aabb(&hitbox)
        .into_iter()
        .filter(|entity| entity.id() != minecart.id())
        .collect();

    if minecart.is_rideable() && horizontal_distance_sqr(minecart.velocity()) >= RIDABLE_THRESHOLD {
        let Some(vehicle) = world.get_entity_by_id(minecart.id()) else {
            return;
        };
        let pushable = EntitySelector::pushable_by(minecart.as_ref());
        for entity in nearby.iter().filter(|entity| pushable(entity.as_ref())) {
            let stays_out = entity.as_player().is_some()
                || entity.entity_type() == &vanilla_entities::IRON_GOLEM
                || entity.as_abstract_minecart().is_some()
                || minecart.is_vehicle()
                || entity.is_passenger();
            if stays_out {
                entity.push_entity(minecart.as_ref());
            } else {
                entity.start_riding(&vehicle);
            }
        }
        return;
    }

    for entity in &nearby {
        if !minecart.has_passenger(entity.as_ref())
            && entity.is_pushable()
            && entity.as_abstract_minecart().is_some()
        {
            entity.push_entity(minecart.as_ref());
        }
    }
}

/// Vanilla `MinecartBehavior.setPos`, which commits the position immediately.
fn set_pos<M: AbstractMinecart + ?Sized>(minecart: &M, position: DVec3) {
    if let Err(error) = minecart.try_set_position(position) {
        log::debug!(
            "failed to move minecart {} along its track: {error}",
            minecart.id()
        );
    }
}

fn horizontal_distance(vector: DVec3) -> f64 {
    horizontal_distance_sqr(vector).sqrt()
}

const fn horizontal_distance_sqr(vector: DVec3) -> f64 {
    vector.x * vector.x + vector.z * vector.z
}
