//! Vanilla `SelectableSlotContainer`: which slot of a block's front face a
//! click lands on.

use steel_registry::items::item::BlockHitResult;
use steel_utils::Direction;

const PIXELS_PER_BLOCK_EDGE: f32 = 16.0;

/// Vanilla `SelectableSlotContainer.getHitSlot`: the slot under `hit` on a
/// face split into `rows` × `columns`, or `None` when the click is not on
/// the `facing` face.
#[must_use]
pub fn hit_slot(hit: &BlockHitResult, facing: Direction, rows: u32, columns: u32) -> Option<usize> {
    let (x, y) = relative_hit_coordinates(hit, facing)?;
    let row = section(1.0 - y, rows);
    let column = section(x, columns);
    Some((column + row * columns) as usize)
}

/// Vanilla `getRelativeHitCoordinatesForBlockFace`: the hit on the face, as
/// floats seen from the front.
fn relative_hit_coordinates(hit: &BlockHitResult, facing: Direction) -> Option<(f32, f32)> {
    if hit.direction != facing {
        return None;
    }
    let origin = hit.block_pos.relative(hit.direction);
    let relative_x = hit.location.x - f64::from(origin.x());
    let relative_y = hit.location.y - f64::from(origin.y());
    let relative_z = hit.location.z - f64::from(origin.z());
    match facing {
        Direction::North => Some(((1.0 - relative_x) as f32, relative_y as f32)),
        Direction::South => Some((relative_x as f32, relative_y as f32)),
        Direction::West => Some((relative_z as f32, relative_y as f32)),
        Direction::East => Some(((1.0 - relative_z) as f32, relative_y as f32)),
        Direction::Down | Direction::Up => None,
    }
}

/// Vanilla `getSection`.
fn section(relative_coordinate: f32, max_sections: u32) -> u32 {
    let targeted_pixel = relative_coordinate * PIXELS_PER_BLOCK_EDGE;
    let section_size = PIXELS_PER_BLOCK_EDGE / max_sections as f32;
    ((targeted_pixel / section_size).floor() as i32).clamp(0, max_sections as i32 - 1) as u32
}

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use steel_utils::BlockPos;

    use super::*;

    fn hit_on(direction: Direction, location: DVec3) -> BlockHitResult {
        BlockHitResult {
            location,
            direction,
            block_pos: BlockPos::new(0, 0, 0),
            miss: false,
            inside: false,
            world_border_hit: false,
        }
    }

    #[test]
    fn a_north_face_counts_columns_from_the_viewers_left() {
        // Looking at a north face from the north, +x is on the viewer's right.
        let left = hit_on(Direction::North, DVec3::new(0.9, 0.5, 0.0));
        let right = hit_on(Direction::North, DVec3::new(0.1, 0.5, 0.0));
        assert_eq!(hit_slot(&left, Direction::North, 1, 3), Some(0));
        assert_eq!(hit_slot(&right, Direction::North, 1, 3), Some(2));
    }

    #[test]
    fn rows_count_from_the_top() {
        let top = hit_on(Direction::South, DVec3::new(0.5, 0.9, 1.0));
        let bottom = hit_on(Direction::South, DVec3::new(0.5, 0.1, 1.0));
        assert_eq!(hit_slot(&top, Direction::South, 2, 3), Some(1));
        assert_eq!(hit_slot(&bottom, Direction::South, 2, 3), Some(4));
    }

    #[test]
    fn other_faces_select_nothing() {
        let side = hit_on(Direction::East, DVec3::new(1.0, 0.5, 0.5));
        assert_eq!(hit_slot(&side, Direction::North, 1, 3), None);
    }
}
