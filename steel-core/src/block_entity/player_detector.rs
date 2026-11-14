//! Vanilla `PlayerDetector`: which players a vault or trial spawner sees.

use std::sync::Arc;

use glam::DVec3;
use steel_utils::BlockPos;
use steel_utils::types::GameType;
use uuid::Uuid;

use crate::entity::Entity as _;
use crate::player::Player;
use crate::world::World;
use crate::world::{ClipBlockShape, ClipFluid};

/// Vanilla `PlayerDetector` presets used outside debug builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerDetector {
    /// Vanilla `NO_CREATIVE_PLAYERS`: skips creative and spectator players.
    NoCreativePlayers,
    /// Vanilla `INCLUDING_CREATIVE_PLAYERS`: skips only spectators.
    IncludingCreativePlayers,
}

/// Vanilla `PlayerDetector.EntitySelector`.
#[derive(Clone, Copy)]
pub enum EntitySelector<'a> {
    /// Vanilla `SELECT_FROM_LEVEL`: the players in the level.
    SelectFromLevel,
    /// Vanilla `onlySelectPlayers`: a fixed list of players.
    OnlySelectPlayers(&'a [Arc<Player>]),
}

impl PlayerDetector {
    /// Vanilla `detect`: the UUIDs of matching players whose block position is
    /// closer than `range` to `pos`, in selector order.
    #[must_use]
    pub fn detect(
        self,
        world: &World,
        selector: EntitySelector<'_>,
        pos: BlockPos,
        range: f64,
        require_line_of_sight: bool,
    ) -> Vec<Uuid> {
        let mut found = Vec::new();
        let mut consider = |player: &Arc<Player>| {
            if self.matches(player, pos, range)
                && (!require_line_of_sight
                    || in_line_of_sight(world, block_center(pos), eye_position(player)))
            {
                found.push(player.uuid());
            }
        };
        match selector {
            EntitySelector::SelectFromLevel => world.players.iter_players(|_uuid, player| {
                consider(player);
                true
            }),
            EntitySelector::OnlySelectPlayers(players) => players.iter().for_each(consider),
        }
        found
    }

    fn matches(self, player: &Player, pos: BlockPos, range: f64) -> bool {
        if !player.block_position().closer_than(pos, range) || player.is_spectator() {
            return false;
        }
        match self {
            Self::NoCreativePlayers => player.game_mode() != GameType::Creative,
            Self::IncludingCreativePlayers => true,
        }
    }
}

pub(crate) fn block_center(pos: BlockPos) -> DVec3 {
    let (x, y, z) = pos.get_center();
    DVec3::new(x, y, z)
}

fn eye_position(player: &Player) -> DVec3 {
    let position = player.position();
    DVec3::new(position.x, player.get_eye_y(), position.z)
}

/// Vanilla `inLineOfSight`: a visual clip from the eye back to the block
/// either misses or stops in the block itself.
pub(crate) fn in_line_of_sight(world: &World, origin: DVec3, dest: DVec3) -> bool {
    let hit = world.clip(dest, origin, ClipBlockShape::Visual, ClipFluid::None);
    hit.block_pos == BlockPos::containing(origin.x, origin.y, origin.z) || hit.miss
}
