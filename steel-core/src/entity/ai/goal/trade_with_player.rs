use super::selector::{Goal, GoalControls};
use crate::entity::entities::mobs::villagers::trading_player_of;
use crate::entity::{Entity as _, LivingEntity, PathfinderMob};

/// Vanilla `TradeWithPlayerGoal` reach: the trading player must stay within 4 blocks.
const MAX_TRADING_DISTANCE_SQR: f64 = 16.0;

/// Vanilla `TradeWithPlayerGoal`: a merchant stands still while a player trades
/// with it, and stops trading when it no longer can.
pub struct TradeWithPlayerGoal;

impl Goal for TradeWithPlayerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if !LivingEntity::is_alive(mob)
            || mob.is_in_water()
            || !mob.on_ground()
            || mob.hurt_time() > 0
        {
            return false;
        }
        let Some(merchant) = mob.as_merchant() else {
            return false;
        };
        trading_player_of(merchant).is_some_and(|player| {
            mob.position().distance_squared(player.position()) <= MAX_TRADING_DISTANCE_SQR
        })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(merchant) = mob.as_merchant() {
            merchant.set_trading_player(None);
        }
    }
}
