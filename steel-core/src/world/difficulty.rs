//! Vanilla regional difficulty (`DifficultyInstance`).

use steel_registry::vanilla_world_clocks;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, ChunkPos};

use super::World;

const DIFFICULTY_TIME_GLOBAL_OFFSET: f32 = -72000.0;
const MAX_DIFFICULTY_TIME_GLOBAL: f32 = 1_440_000.0;
const MAX_DIFFICULTY_TIME_LOCAL: f32 = 3_600_000.0;

/// Vanilla `DifficultyInstance`: the world difficulty scaled by world age, how long
/// players stayed in the chunk, and the moon phase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifficultyInstance {
    base: Difficulty,
    effective_difficulty: f32,
}

impl DifficultyInstance {
    /// Computes the regional difficulty from its vanilla inputs.
    #[must_use]
    pub fn new(
        base: Difficulty,
        total_game_time: i64,
        local_game_time: i64,
        moon_brightness: f32,
    ) -> Self {
        Self {
            base,
            effective_difficulty: Self::calculate_difficulty(
                base,
                total_game_time,
                local_game_time,
                moon_brightness,
            ),
        }
    }

    /// Returns the world difficulty this instance was computed from.
    #[must_use]
    pub const fn difficulty(self) -> Difficulty {
        self.base
    }

    /// Returns the regional ("effective") difficulty, from 0 to 6.75.
    #[must_use]
    pub const fn effective_difficulty(self) -> f32 {
        self.effective_difficulty
    }

    /// Vanilla `isHard`: the regional difficulty reaches the `HARD` ordinal.
    #[must_use]
    pub fn is_hard(self) -> bool {
        self.effective_difficulty >= f32::from(Difficulty::Hard as u8)
    }

    /// Vanilla `isHarderThan`.
    #[must_use]
    pub fn is_harder_than(self, required_difficulty: f32) -> bool {
        self.effective_difficulty > required_difficulty
    }

    /// Vanilla `getSpecialMultiplier` ("clamped regional difficulty"), from 0 to 1.
    #[must_use]
    pub fn special_multiplier(self) -> f32 {
        if self.effective_difficulty < 2.0 {
            0.0
        } else if self.effective_difficulty > 4.0 {
            1.0
        } else {
            (self.effective_difficulty - 2.0) / 2.0
        }
    }

    fn calculate_difficulty(
        base: Difficulty,
        total_game_time: i64,
        local_game_time: i64,
        moon_brightness: f32,
    ) -> f32 {
        if base == Difficulty::Peaceful {
            return 0.0;
        }
        let is_hard = base == Difficulty::Hard;
        let mut scale = 0.75_f32;
        let global_scale = ((total_game_time as f32 + DIFFICULTY_TIME_GLOBAL_OFFSET)
            / MAX_DIFFICULTY_TIME_GLOBAL)
            .clamp(0.0, 1.0)
            * 0.25;
        scale += global_scale;
        let mut local_scale = 0.0_f32;
        local_scale += (local_game_time as f32 / MAX_DIFFICULTY_TIME_LOCAL).clamp(0.0, 1.0)
            * if is_hard { 1.0 } else { 0.75 };
        local_scale += (moon_brightness * 0.25).clamp(0.0, global_scale);
        if base == Difficulty::Easy {
            local_scale *= 0.5;
        }
        scale += local_scale;
        f32::from(base as u8) * scale
    }
}

impl World {
    /// Vanilla `ServerLevel.getCurrentDifficultyAt`. Unloaded chunks count as never
    /// inhabited, under a new moon.
    #[must_use]
    pub fn current_difficulty_at(&self, pos: BlockPos) -> DifficultyInstance {
        let chunk_pos = ChunkPos::from_block_pos(pos);
        let (local_time, moon_brightness) = self
            .chunk_map
            .with_full_chunk(chunk_pos, |chunk| chunk.common().inhabited_time())
            .map_or((0, 0.0), |inhabited| (inhabited, self.moon_brightness()));
        let overworld_time = self
            .clock_total_ticks(&vanilla_world_clocks::OVERWORLD)
            .unwrap_or(0);
        DifficultyInstance::new(
            self.difficulty(),
            overworld_time,
            local_time,
            moon_brightness,
        )
    }
}

#[cfg(test)]
mod tests {
    use steel_utils::types::Difficulty;

    use super::DifficultyInstance;

    #[test]
    fn peaceful_is_always_zero() {
        let instance = DifficultyInstance::new(Difficulty::Peaceful, 10_000_000, 10_000_000, 1.0);
        assert!(instance.effective_difficulty().abs() < f32::EPSILON);
        assert!(instance.special_multiplier().abs() < f32::EPSILON);
    }

    #[test]
    fn fresh_world_uses_the_base_scale() {
        // 0.75 × difficulty id with no world age, inhabited time, or moon.
        let hard = DifficultyInstance::new(Difficulty::Hard, 0, 0, 1.0);
        assert!((hard.effective_difficulty() - 2.25).abs() < 1e-6);
        assert!((hard.special_multiplier() - 0.125).abs() < 1e-6);
        let normal = DifficultyInstance::new(Difficulty::Normal, 0, 0, 1.0);
        assert!((normal.effective_difficulty() - 1.5).abs() < 1e-6);
        assert!(normal.special_multiplier().abs() < f32::EPSILON);
    }

    #[test]
    fn old_inhabited_hard_chunk_under_full_moon_reaches_the_cap() {
        let instance = DifficultyInstance::new(Difficulty::Hard, 2_000_000, 4_000_000, 1.0);
        assert!((instance.effective_difficulty() - 6.75).abs() < 1e-5);
        assert!(instance.is_hard());
        assert!((instance.special_multiplier() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn easy_halves_the_local_part() {
        // global 0.25, local (0.75 + 0.25) * 0.5 = 0.5 → 1 × 1.5.
        let instance = DifficultyInstance::new(Difficulty::Easy, 2_000_000, 4_000_000, 1.0);
        assert!((instance.effective_difficulty() - 1.5).abs() < 1e-5);
    }
}
