//! The world the current thread is ticking, so entities can reach it without touching
//! its reference count.
//!
//! `Entity::level()` used to upgrade the entity's `Weak<World>` on every call. That is
//! a compare-and-swap on the world's shared strong count, and a decrement when the
//! `Arc` drops. Mob code calls it hundreds of times per entity tick, so with islands
//! ticking in parallel every worker hammered the same cache line: on an M4 Pro, eight
//! tick threads spent 3.3 times the CPU of four for the same work.
//!
//! A world tick thread and each island worker bind the `Arc<World>` they tick here.
//! While it is bound, `level()` hands out a borrow of it for entities of that world.

use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr;
use std::sync::{Arc, Weak};

use super::World;

thread_local! {
    static TICKING_WORLD: Cell<*const Arc<World>> = const { Cell::new(ptr::null()) };
}

/// Binds `world` as the current thread's ticking world until dropped.
///
/// Scopes nest: dropping one restores the binding that was active before it.
#[must_use = "the world is only bound while the scope is alive"]
pub(crate) struct TickingWorldScope<'a> {
    previous: *const Arc<World>,
    _world: PhantomData<&'a Arc<World>>,
}

impl<'a> TickingWorldScope<'a> {
    /// Binds `world` to the current thread.
    pub(crate) fn enter(world: &'a Arc<World>) -> Self {
        let previous = TICKING_WORLD.with(|current| current.replace(ptr::from_ref(world)));
        Self {
            previous,
            _world: PhantomData,
        }
    }
}

impl Drop for TickingWorldScope<'_> {
    fn drop(&mut self) {
        TICKING_WORLD.with(|current| current.set(self.previous));
    }
}

/// A world handed out by `level()`: a borrow of the thread's ticking world, or an owned
/// `Arc` when the entity's world is not the one this thread ticks.
///
/// It dereferences to `Arc<World>`, so `&level` works where `&Arc<World>` is expected
/// and `Arc::clone(&level)` keeps an owned handle. It is not `Send`: a borrow is only
/// valid on the thread whose scope bound it.
#[derive(Clone)]
pub enum LevelRef<'a> {
    /// The thread's ticking world, borrowed from the scope that bound it.
    Ticking(&'a Arc<World>, PhantomData<*const ()>),
    /// An upgraded handle to another world.
    Owned(Arc<World>),
}

impl LevelRef<'_> {
    /// Resolves `world` without touching its reference count when it is the world this
    /// thread is ticking.
    pub(crate) fn resolve(world: &Weak<World>) -> Option<Self> {
        let ticking = TICKING_WORLD.with(Cell::get);
        if !ticking.is_null() {
            // SAFETY: a non-null binding points at the `Arc` a live `TickingWorldScope`
            // borrows, which outlives everything the scope's tick calls on this thread.
            let ticking = unsafe { &*ticking };
            if ptr::eq(Arc::as_ptr(ticking), world.as_ptr()) {
                return Some(Self::Ticking(ticking, PhantomData));
            }
        }
        world.upgrade().map(Self::Owned)
    }

    /// An owned handle to the world.
    #[must_use]
    pub fn into_arc(self) -> Arc<World> {
        match self {
            Self::Ticking(world, _) => Arc::clone(world),
            Self::Owned(world) => world,
        }
    }
}

impl Deref for LevelRef<'_> {
    type Target = Arc<World>;

    fn deref(&self) -> &Arc<World> {
        match self {
            Self::Ticking(world, _) => world,
            Self::Owned(world) => world,
        }
    }
}
