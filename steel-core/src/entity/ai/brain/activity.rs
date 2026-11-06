//! Activities group brain behaviors (vanilla `Activity`).

/// A brain activity, such as idling or fighting.
///
/// Vanilla registers these in code; Steel mirrors them as named constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Activity(&'static str);

impl Activity {
    pub const CORE: Self = Self("core");
    pub const IDLE: Self = Self("idle");
    pub const WORK: Self = Self("work");
    pub const PLAY: Self = Self("play");
    pub const REST: Self = Self("rest");
    pub const MEET: Self = Self("meet");
    pub const PANIC: Self = Self("panic");
    pub const RAID: Self = Self("raid");
    pub const PRE_RAID: Self = Self("pre_raid");
    pub const HIDE: Self = Self("hide");
    pub const FIGHT: Self = Self("fight");
    pub const CELEBRATE: Self = Self("celebrate");
    pub const ADMIRE_ITEM: Self = Self("admire_item");
    pub const AVOID: Self = Self("avoid");
    pub const RIDE: Self = Self("ride");
    pub const PLAY_DEAD: Self = Self("play_dead");
    pub const LONG_JUMP: Self = Self("long_jump");
    pub const RAM: Self = Self("ram");
    pub const TONGUE: Self = Self("tongue");
    pub const SWIM: Self = Self("swim");
    pub const LAY_SPAWN: Self = Self("lay_spawn");
    pub const SNIFF: Self = Self("sniff");
    pub const INVESTIGATE: Self = Self("investigate");
    pub const ROAR: Self = Self("roar");
    pub const EMERGE: Self = Self("emerge");
    pub const DIG: Self = Self("dig");

    /// Registry path in the `minecraft` namespace.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.0
    }
}
