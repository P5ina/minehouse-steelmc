//! Arrow and spectral arrow entities (vanilla `Arrow`, `SpectralArrow`).
//!
//! Both extend vanilla `AbstractArrow`, ported in
//! [`crate::entity::projectile::AbstractArrow`]. The arrow carries the potion
//! of a tipped arrow and applies it on hit; the spectral arrow makes its
//! target glow.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::data_components::PotionContents;
use steel_registry::data_components::vanilla_components::{POTION_CONTENTS, POTION_DURATION_SCALE};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::mob_effect_instance::MobEffectInstance as RegistryMobEffectInstance;
use steel_registry::vanilla_entity_data::{ArrowEntityData, SpectralArrowEntityData};
use steel_registry::{vanilla_items, vanilla_mob_effects};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::entity::potion_contents::to_runtime_instance;
use crate::entity::{
    ARROW_GRAVITY, AbstractArrow, ArrowBase, Entity, EntityBase, EntityBaseLoad, EntitySyncedData,
    LivingEntity, MobEffectInstance, Projectile, ProjectileBase, SharedEntity,
};
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::world::{ClipHitResult, World};

/// Vanilla `Arrow.tick`: a tipped arrow loses its potion after this long in
/// the ground.
const POTION_FADE_TICKS: i32 = 600;
/// Vanilla `SpectralArrow.DEFAULT_DURATION` of the glowing effect.
const SPECTRAL_DEFAULT_DURATION: i32 = 200;

/// The `Entity` / `Projectile` / `AbstractArrow` plumbing every arrow shares.
macro_rules! arrow_entity_common {
    () => {
        fn base(&self) -> &EntityBase {
            &self.base
        }

        fn entity_type(&self) -> EntityTypeRef {
            self.entity_type
        }

        fn get_default_gravity(&self) -> f64 {
            ARROW_GRAVITY
        }

        fn spawn_data(&self) -> i32 {
            self.get_owner().map_or(0, |owner| owner.id())
        }

        fn restore_owner_reference(&self, owner: &SharedEntity) {
            self.cache_owner_entity(owner);
        }

        fn projectile_owner_uuid(&self) -> Option<uuid::Uuid> {
            self.owner_uuid()
        }

        fn projectile_owner(&self) -> Option<SharedEntity> {
            self.get_owner()
        }

        fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
            Some(&self.entity_data)
        }

        fn player_touch(self: Arc<Self>, player: &Arc<Player>) {
            self.arrow_player_touch(player);
        }

        fn push_impulse(&self, impulse: DVec3) {
            // Vanilla `AbstractArrow.push`: stuck arrows ignore pushes.
            if self.is_in_ground() || !impulse.is_finite() {
                return;
            }
            self.set_velocity(self.velocity() + impulse);
            self.mark_velocity_sync();
        }

        fn on_above_bubble_column(&self, drag_down: bool, pos: BlockPos) {
            if !self.is_in_ground() {
                self.on_above_bubble_column_projectile(drag_down, pos);
            }
        }

        fn on_inside_bubble_column(&self, drag_down: bool) {
            if !self.is_in_ground() {
                self.on_inside_bubble_column_projectile(drag_down);
            }
        }

        fn is_pushed_by_fluid(&self) -> bool {
            !self.is_in_ground()
        }

        fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
            let result = Arc::clone(&self).default_move_entity(mover_type, delta);
            self.arrow_after_move(mover_type);
            result
        }
    };
}

/// Projectile overrides shared by every arrow.
macro_rules! arrow_projectile_common {
    () => {
        fn projectile_base(&self) -> &ProjectileBase {
            &self.projectile_base
        }

        fn shoot(&self, direction: DVec3, power: f32, uncertainty: f32) {
            self.arrow_shoot(direction, power, uncertainty);
        }

        fn set_owner_entity(&self, owner: Option<&SharedEntity>) {
            self.projectile_base_set_owner_entity(owner);
            self.arrow_owner_changed(owner);
        }

        fn can_hit_entity(&self, entity: &dyn Entity) -> bool {
            self.arrow_can_hit_entity(entity)
        }

        fn should_bounce_on_world_border(&self) -> bool {
            true
        }

        fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
            self.arrow_on_hit_entity(entity);
        }

        fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
            self.arrow_on_hit_block(hit);
        }
    };
}

/// The synced-data accessors over an `abstract_arrow` data layer.
macro_rules! arrow_synced_accessors {
    () => {
        fn arrow_base(&self) -> &ArrowBase {
            &self.arrow_base
        }

        fn arrow_flags(&self) -> i8 {
            *self.entity_data.lock().abstract_arrow.id_flags.get()
        }

        fn set_arrow_flags(&self, flags: i8) {
            self.entity_data.lock().abstract_arrow.id_flags.set(flags);
        }

        fn pierce_level(&self) -> i8 {
            *self.entity_data.lock().abstract_arrow.pierce_level.get()
        }

        fn set_pierce_level(&self, pierce_level: i8) {
            self.entity_data
                .lock()
                .abstract_arrow
                .pierce_level
                .set(pierce_level);
        }

        fn is_in_ground(&self) -> bool {
            *self.entity_data.lock().abstract_arrow.in_ground.get()
        }

        fn set_in_ground(&self, in_ground: bool) {
            self.entity_data
                .lock()
                .abstract_arrow
                .in_ground
                .set(in_ground);
        }
    };
}

/// A fired arrow, plain or tipped.
#[entity_behavior(class = "Arrow")]
pub struct ArrowEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ArrowEntityData>,
    projectile_base: ProjectileBase,
    arrow_base: ArrowBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ArrowEntity`.
unsafe impl DowncastType for ArrowEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/arrow");
}

impl ArrowEntity {
    /// Creates an arrow with no owner carrying a plain arrow item.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(ArrowEntityData::new()),
            projectile_base: ProjectileBase::new(),
            arrow_base: ArrowBase::new(ItemStack::new(&vanilla_items::ARROW)),
        }
    }

    /// Creates an arrow from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(ArrowEntityData::new()),
            projectile_base: ProjectileBase::new(),
            arrow_base: ArrowBase::new(ItemStack::new(&vanilla_items::ARROW)),
        }
    }

    /// Vanilla `Arrow(level, x, y, z, pickupItemStack, firedFromWeapon)`.
    #[must_use]
    pub fn with_items(
        entity_type: EntityTypeRef,
        id: i32,
        position: DVec3,
        world: Weak<World>,
        pickup_item_stack: &ItemStack,
        fired_from_weapon: Option<&ItemStack>,
    ) -> Self {
        let arrow = Self::new(entity_type, id, position, world);
        arrow.init_from_items(pickup_item_stack, fired_from_weapon);
        arrow
    }

    fn potion_contents(&self) -> PotionContents {
        self.pickup_item_stack()
            .get_or_default(POTION_CONTENTS, PotionContents::empty())
    }

    /// Vanilla `Arrow.updateColor`.
    fn update_color(&self) {
        let contents = self.potion_contents();
        let color = if contents == PotionContents::empty() {
            -1
        } else {
            contents.get_color()
        };
        self.entity_data.lock().id_effect_color.set(color);
    }

    /// Vanilla `Arrow.setPotionContents`: stores the contents on the pickup
    /// stack and refreshes the color.
    fn set_potion_contents(&self, contents: PotionContents) {
        let mut stack = self.pickup_item_stack();
        stack.set(POTION_CONTENTS, contents);
        self.set_pickup_item_stack(stack);
    }

    /// Vanilla `Arrow.addEffect`.
    pub fn add_effect(&self, effect: RegistryMobEffectInstance) {
        self.set_potion_contents(self.potion_contents().with_effect_added(effect));
    }

    /// Vanilla `Arrow.getColor`: the synced potion color, `-1` for none.
    #[must_use]
    pub fn color(&self) -> i32 {
        *self.entity_data.lock().id_effect_color.get()
    }
}

impl Entity for ArrowEntity {
    arrow_entity_common!();

    fn tick(self: Arc<Self>) {
        Arc::clone(&self).abstract_arrow_tick();
        // VANILLA CLIENT-LOCAL: the potion particle trail.
        if self.is_in_ground()
            && self.in_ground_time() != 0
            && self.potion_contents() != PotionContents::empty()
            && self.in_ground_time() >= POTION_FADE_TICKS
        {
            self.broadcast_entity_event(EntityStatus::TippedArrow);
            self.set_pickup_item_stack(ItemStack::new(&vanilla_items::ARROW));
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_arrow(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_arrow(nbt);
    }
}

impl Projectile for ArrowEntity {
    arrow_projectile_common!();
}

impl AbstractArrow for ArrowEntity {
    arrow_synced_accessors!();

    fn get_default_pickup_item(&self) -> ItemStack {
        ItemStack::new(&vanilla_items::ARROW)
    }

    fn set_pickup_item_stack(&self, item_stack: ItemStack) {
        self.default_set_pickup_item_stack(item_stack);
        self.update_color();
    }

    fn do_post_hurt_effects(&self, mob: &dyn LivingEntity) {
        let pickup = self.pickup_item_stack();
        let contents = pickup.get_or_default(POTION_CONTENTS, PotionContents::empty());
        let duration_scale = pickup.get_or_default(POTION_DURATION_SCALE, 1.0);
        let owner = self.get_owner();
        let effect_source: &dyn Entity = owner.as_deref().unwrap_or(self);
        for effect in contents.all_effects() {
            let scaled = effect.with_scaled_duration(duration_scale);
            mob.add_mob_effect_with_source(
                to_runtime_instance(&scaled, scaled.duration()),
                Some(effect_source),
            );
        }
    }
}

/// A fired spectral arrow.
#[entity_behavior(class = "SpectralArrow")]
pub struct SpectralArrowEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<SpectralArrowEntityData>,
    projectile_base: ProjectileBase,
    arrow_base: ArrowBase,
    duration: SyncMutex<i32>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SpectralArrowEntity`.
unsafe impl DowncastType for SpectralArrowEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/spectral_arrow");
}

impl SpectralArrowEntity {
    /// Creates a spectral arrow with no owner.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(SpectralArrowEntityData::new()),
            projectile_base: ProjectileBase::new(),
            arrow_base: ArrowBase::new(ItemStack::new(&vanilla_items::SPECTRAL_ARROW)),
            duration: SyncMutex::new(SPECTRAL_DEFAULT_DURATION),
        }
    }

    /// Creates a spectral arrow from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(SpectralArrowEntityData::new()),
            projectile_base: ProjectileBase::new(),
            arrow_base: ArrowBase::new(ItemStack::new(&vanilla_items::SPECTRAL_ARROW)),
            duration: SyncMutex::new(SPECTRAL_DEFAULT_DURATION),
        }
    }

    /// Vanilla `SpectralArrow(level, x, y, z, pickupItemStack, firedFromWeapon)`.
    #[must_use]
    pub fn with_items(
        entity_type: EntityTypeRef,
        id: i32,
        position: DVec3,
        world: Weak<World>,
        pickup_item_stack: &ItemStack,
        fired_from_weapon: Option<&ItemStack>,
    ) -> Self {
        let arrow = Self::new(entity_type, id, position, world);
        arrow.init_from_items(pickup_item_stack, fired_from_weapon);
        arrow
    }
}

impl Entity for SpectralArrowEntity {
    arrow_entity_common!();

    fn tick(self: Arc<Self>) {
        // VANILLA CLIENT-LOCAL: the in-flight effect particles.
        self.abstract_arrow_tick();
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_arrow(nbt);
        nbt.insert("Duration", *self.duration.lock());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_arrow(nbt);
        *self.duration.lock() = nbt.int("Duration").unwrap_or(SPECTRAL_DEFAULT_DURATION);
    }
}

impl Projectile for SpectralArrowEntity {
    arrow_projectile_common!();
}

impl AbstractArrow for SpectralArrowEntity {
    arrow_synced_accessors!();

    fn get_default_pickup_item(&self) -> ItemStack {
        ItemStack::new(&vanilla_items::SPECTRAL_ARROW)
    }

    fn do_post_hurt_effects(&self, mob: &dyn LivingEntity) {
        let owner = self.get_owner();
        let effect_source: &dyn Entity = owner.as_deref().unwrap_or(self);
        mob.add_mob_effect_with_source(
            MobEffectInstance::with_duration(
                vanilla_mob_effects::GLOWING,
                *self.duration.lock(),
                0,
            ),
            Some(effect_source),
        );
    }
}

#[cfg(test)]
mod tests;
