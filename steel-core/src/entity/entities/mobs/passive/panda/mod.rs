//! Vanilla panda: a bamboo-eating animal whose two genes decide its
//! personality (lazy, worried, playful, aggressive, weak or brown).

mod goals;

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::{CTakeItemEntity, SoundSource};
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::loot_table::LootContext;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::PandaEntityData;
use steel_registry::vanilla_game_rules::MOB_DROPS;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_game_events,
    vanilla_items, vanilla_loot_tables,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Downcast as _, DowncastType, DowncastTypeKey};

use goals::{
    PandaAttackGoal, PandaAvoidGoal, PandaBreedGoal, PandaHurtByTargetGoal, PandaLieOnBackGoal,
    PandaLookAtPlayerGoal, PandaPanicGoal, PandaRollGoal, PandaSitGoal, PandaSneezeGoal,
};

use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    FloatGoal, FollowParentGoal, RandomLookAroundGoal, TemptGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::living_base::LivingTravelInput;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, DefaultEntityPush, Entity,
    EntityBase, EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, EquipmentSlot,
    LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob, RemovalReason, SharedEntity,
    SpawnGroupData, SpawnRule, entity_loot_ref, living_loot_equipment, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::World;

const PANDA_BABY_PASSENGER_ATTACHMENTS: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.5625, 0.0)];

const FLAG_SNEEZE: i8 = 2;
const FLAG_ROLL: i8 = 4;
const FLAG_SIT: i8 = 8;
const FLAG_ON_BACK: i8 = 16;
const EAT_TICK_INTERVAL: i32 = 5;
/// Vanilla `Panda.TOTAL_ROLL_STEPS`.
pub const TOTAL_ROLL_STEPS: i32 = 32;
/// Vanilla `Panda.TOTAL_UNHAPPY_TIME`.
const TOTAL_UNHAPPY_TIME: i32 = 32;
const SNEEZE_TICKS: i32 = 20;
const SNEEZE_JUMP_RANGE: f64 = 10.0;
const ROLL_JUMP: f64 = 0.27;
const WEAK_MAX_HEALTH: f64 = 10.0;
const LAZY_MOVEMENT_SPEED: f64 = 0.07;
const BABY_SPAWN_CHANCE: f32 = 0.2;
const MUTATION_CHANCE: u32 = 32;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;

/// Vanilla `Panda.Gene`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PandaGene {
    /// No special behavior.
    Normal,
    /// Moves slowly and lies on its back.
    Lazy,
    /// Runs from players and monsters and hides in thunderstorms.
    Worried,
    /// Rolls around.
    Playful,
    /// Recessive: brown fur.
    Brown,
    /// Recessive: low health and frequent sneezing.
    Weak,
    /// Fights back and joins fights of other pandas.
    Aggressive,
}

impl PandaGene {
    const ALL: [Self; 7] = [
        Self::Normal,
        Self::Lazy,
        Self::Worried,
        Self::Playful,
        Self::Brown,
        Self::Weak,
        Self::Aggressive,
    ];

    /// The gene's network id.
    #[must_use]
    pub const fn id(self) -> i8 {
        match self {
            Self::Normal => 0,
            Self::Lazy => 1,
            Self::Worried => 2,
            Self::Playful => 3,
            Self::Brown => 4,
            Self::Weak => 5,
            Self::Aggressive => 6,
        }
    }

    /// The gene's saved name.
    #[must_use]
    pub const fn serialized_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Lazy => "lazy",
            Self::Worried => "worried",
            Self::Playful => "playful",
            Self::Brown => "brown",
            Self::Weak => "weak",
            Self::Aggressive => "aggressive",
        }
    }

    /// Whether the gene only shows when both genes carry it.
    #[must_use]
    pub const fn is_recessive(self) -> bool {
        matches!(self, Self::Brown | Self::Weak)
    }

    /// Vanilla `Gene.byId`: out-of-range ids fall back to normal.
    #[must_use]
    pub fn by_id(id: i8) -> Self {
        Self::ALL
            .into_iter()
            .find(|gene| gene.id() == id)
            .unwrap_or(Self::Normal)
    }

    /// The gene with this saved name.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|gene| gene.serialized_name() == name)
    }

    /// Vanilla `Gene.getVariantFromGenes`.
    #[must_use]
    pub fn variant_from_genes(main_gene: Self, hidden_gene: Self) -> Self {
        if !main_gene.is_recessive() || main_gene == hidden_gene {
            main_gene
        } else {
            Self::Normal
        }
    }

    /// Vanilla `Gene.getRandom`, weighted out of 16.
    #[must_use]
    pub fn random() -> Self {
        Self::from_roll(rand::random_range(0..16))
    }

    const fn from_roll(roll: u32) -> Self {
        match roll {
            0 => Self::Lazy,
            1 => Self::Worried,
            2 => Self::Playful,
            4 => Self::Aggressive,
            3 | 5..=8 => Self::Weak,
            9 | 10 => Self::Brown,
            _ => Self::Normal,
        }
    }
}

#[entity_behavior(class = "Panda", spawn_rule)]
/// Vanilla panda.
pub struct PandaEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    got_bamboo: AtomicBool,
    did_bite: AtomicBool,
    roll_counter: AtomicI32,
    roll_delta: SyncMutex<DVec3>,
    /// The entity vanilla `PandaLookAtPlayerGoal.lookAt` holds; the breed goal
    /// points it at the nearest player when there is no bamboo around.
    look_at: SyncMutex<Option<SharedEntity>>,
    entity_data: SyncMutex<PandaEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `PandaEntity`.
unsafe impl DowncastType for PandaEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/panda");
}

/// Vanilla `Panda.canPickUpAndEat`.
fn can_pick_up_and_eat(item: &ItemEntity) -> bool {
    is_ground_food(&item.get_item()) && Entity::is_alive(item) && !item.has_pickup_delay()
}

fn is_ground_food(item_stack: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item_stack.item(), &ItemTag::PANDA_EATS_FROM_GROUND)
}

impl PandaEntity {
    /// Vanilla `SpawnPlacements` predicate: `Animal.checkAnimalSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Animal>::check_animal_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new panda at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a panda from persisted base entity state.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        let ageable_base = AgeableMobBase::new();
        let animal_base = AnimalBase::new();
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        let mut entity_data = PandaEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(2, PandaPanicGoal::new());
            goal_selector.add_goal(2, PandaBreedGoal::new());
            goal_selector.add_goal(3, PandaAttackGoal::new());
            goal_selector.add_goal(
                4,
                TemptGoal::new(
                    1.0,
                    |item_stack| {
                        REGISTRY
                            .items
                            .is_in_tag(item_stack.item(), &ItemTag::PANDA_FOOD)
                    },
                    false,
                ),
            );
            goal_selector.add_goal(6, PandaAvoidGoal::players());
            goal_selector.add_goal(6, PandaAvoidGoal::monsters());
            goal_selector.add_goal(7, PandaSitGoal::new());
            goal_selector.add_goal(8, PandaLieOnBackGoal::new());
            goal_selector.add_goal(8, PandaSneezeGoal);
            goal_selector.add_goal(9, PandaLookAtPlayerGoal::new());
            goal_selector.add_goal(10, RandomLookAroundGoal::new());
            goal_selector.add_goal(12, PandaRollGoal);
            goal_selector.add_goal(13, FollowParentGoal::new(1.25));
            goal_selector.add_goal(14, WaterAvoidingRandomStrollGoal::new(1.0));

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, PandaHurtByTargetGoal::new());
        }

        let panda = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            got_bamboo: AtomicBool::new(false),
            did_bite: AtomicBool::new(false),
            roll_counter: AtomicI32::new(0),
            roll_delta: SyncMutex::new(DVec3::ZERO),
            look_at: SyncMutex::new(None),
            entity_data: SyncMutex::new(entity_data),
        };
        // Vanilla: a panda that is not a baby when constructed picks up loot.
        if !AgeableMob::is_baby(&panda) {
            panda.set_can_pick_up_loot(true);
        }
        panda
    }

    fn flag(&self, flag: i8) -> bool {
        *self.entity_data.lock().id_flags.get() & flag != 0
    }

    fn set_flag(&self, flag: i8, value: bool) {
        let mut data = self.entity_data.lock();
        let current = *data.id_flags.get();
        data.id_flags.set(if value {
            current | flag
        } else {
            current & !flag
        });
    }

    /// Ticks left of the unhappy head shake.
    #[must_use]
    pub fn unhappy_counter(&self) -> i32 {
        *self.entity_data.lock().unhappy_counter.get()
    }

    /// Sets the ticks left of the unhappy head shake.
    pub fn set_unhappy_counter(&self, value: i32) {
        self.entity_data.lock().unhappy_counter.set(value);
    }

    /// Whether the panda is sneezing.
    #[must_use]
    pub fn is_sneezing(&self) -> bool {
        self.flag(FLAG_SNEEZE)
    }

    /// Vanilla `Panda.sneeze`.
    pub fn sneeze(&self, value: bool) {
        self.set_flag(FLAG_SNEEZE, value);
        if !value {
            self.set_sneeze_counter(0);
        }
    }

    /// Ticks into the current sneeze.
    #[must_use]
    pub fn sneeze_counter(&self) -> i32 {
        *self.entity_data.lock().sneeze_counter.get()
    }

    fn set_sneeze_counter(&self, value: i32) {
        self.entity_data.lock().sneeze_counter.set(value);
    }

    /// Whether the panda is sitting.
    #[must_use]
    pub fn is_sitting(&self) -> bool {
        self.flag(FLAG_SIT)
    }

    /// Makes the panda sit or stand.
    pub fn sit(&self, value: bool) {
        self.set_flag(FLAG_SIT, value);
    }

    /// Whether the panda lies on its back.
    #[must_use]
    pub fn is_on_back(&self) -> bool {
        self.flag(FLAG_ON_BACK)
    }

    /// Makes the panda lie on its back or get up.
    pub fn set_on_back(&self, value: bool) {
        self.set_flag(FLAG_ON_BACK, value);
    }

    /// Whether the panda is rolling.
    #[must_use]
    pub fn is_rolling(&self) -> bool {
        self.flag(FLAG_ROLL)
    }

    /// Starts or stops a roll.
    pub fn roll(&self, value: bool) {
        self.set_flag(FLAG_ROLL, value);
    }

    /// Whether the panda is eating.
    #[must_use]
    pub fn is_eating(&self) -> bool {
        self.eat_counter() > 0
    }

    /// Vanilla `Panda.eat`: starts or stops eating.
    pub fn eat(&self, value: bool) {
        self.set_eat_counter(i32::from(value));
    }

    fn eat_counter(&self) -> i32 {
        *self.entity_data.lock().eat_counter.get()
    }

    fn set_eat_counter(&self, value: i32) {
        self.entity_data.lock().eat_counter.set(value);
    }

    /// The gene that shows unless it is recessive.
    #[must_use]
    pub fn main_gene(&self) -> PandaGene {
        PandaGene::by_id(*self.entity_data.lock().main_gene.get())
    }

    /// Sets the main gene.
    pub fn set_main_gene(&self, gene: PandaGene) {
        self.entity_data.lock().main_gene.set(gene.id());
    }

    /// The second, hidden gene.
    #[must_use]
    pub fn hidden_gene(&self) -> PandaGene {
        PandaGene::by_id(*self.entity_data.lock().hidden_gene.get())
    }

    /// Sets the hidden gene.
    pub fn set_hidden_gene(&self, gene: PandaGene) {
        self.entity_data.lock().hidden_gene.set(gene.id());
    }

    /// Vanilla `Panda.getVariant`: the personality the genes show.
    #[must_use]
    pub fn variant(&self) -> PandaGene {
        PandaGene::variant_from_genes(self.main_gene(), self.hidden_gene())
    }

    /// Whether the panda is lazy.
    #[must_use]
    pub fn is_lazy(&self) -> bool {
        self.variant() == PandaGene::Lazy
    }

    /// Whether the panda is worried.
    #[must_use]
    pub fn is_worried(&self) -> bool {
        self.variant() == PandaGene::Worried
    }

    /// Whether the panda is playful.
    #[must_use]
    pub fn is_playful(&self) -> bool {
        self.variant() == PandaGene::Playful
    }

    /// Whether the panda is brown.
    #[must_use]
    pub fn is_brown(&self) -> bool {
        self.variant() == PandaGene::Brown
    }

    /// Whether the panda is weak.
    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.variant() == PandaGene::Weak
    }

    /// Vanilla `Panda.isScared`: a worried panda during a thunderstorm.
    #[must_use]
    pub fn is_scared(&self) -> bool {
        self.is_worried() && self.level().is_some_and(|world| world.is_thundering())
    }

    /// Vanilla `Panda.canPerformAction`.
    #[must_use]
    pub fn can_perform_action(&self) -> bool {
        !self.is_on_back()
            && !self.is_scared()
            && !self.is_eating()
            && !self.is_rolling()
            && !self.is_sitting()
    }

    /// Vanilla `Panda.setGeneFromParents`.
    pub fn set_gene_from_parents(&self, parent1: &Self, parent2: Option<&Self>) {
        if let Some(parent2) = parent2 {
            if rand::random::<bool>() {
                self.set_main_gene(parent1.one_of_genes_randomly());
                self.set_hidden_gene(parent2.one_of_genes_randomly());
            } else {
                self.set_main_gene(parent2.one_of_genes_randomly());
                self.set_hidden_gene(parent1.one_of_genes_randomly());
            }
        } else if rand::random::<bool>() {
            self.set_main_gene(parent1.one_of_genes_randomly());
            self.set_hidden_gene(PandaGene::random());
        } else {
            self.set_main_gene(PandaGene::random());
            self.set_hidden_gene(parent1.one_of_genes_randomly());
        }

        if rand::random_range(0..MUTATION_CHANCE) == 0 {
            self.set_main_gene(PandaGene::random());
        }
        if rand::random_range(0..MUTATION_CHANCE) == 0 {
            self.set_hidden_gene(PandaGene::random());
        }
    }

    fn one_of_genes_randomly(&self) -> PandaGene {
        if rand::random::<bool>() {
            self.main_gene()
        } else {
            self.hidden_gene()
        }
    }

    /// Vanilla `Panda.setAttributes`: weak pandas have less health and lazy
    /// pandas move slower.
    pub fn set_gene_attributes(&self) {
        if self.is_weak() {
            self.attributes()
                .lock()
                .set_base_value(vanilla_attributes::MAX_HEALTH, WEAK_MAX_HEALTH);
        }
        if self.is_lazy() {
            self.attributes()
                .lock()
                .set_base_value(vanilla_attributes::MOVEMENT_SPEED, LAZY_MOVEMENT_SPEED);
        }
    }

    fn main_hand_is_empty(&self) -> bool {
        self.equipment_in_slot(EquipmentSlot::MainHand).is_empty()
    }

    fn set_main_hand(&self, item_stack: ItemStack) {
        self.living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, item_stack);
    }

    /// Vanilla `Panda.tryToSit`.
    pub(crate) fn try_to_sit(&self) {
        if !self.is_in_water() {
            let input = self.travel_input();
            self.set_travel_input(LivingTravelInput::new(
                input.sideways(),
                input.vertical(),
                0.0,
            ));
            self.mob_base.navigation().lock().stop();
            self.sit(true);
        }
    }

    pub(crate) fn set_look_at(&self, entity: Option<SharedEntity>) {
        *self.look_at.lock() = entity;
    }

    pub(crate) fn look_at_target(&self) -> Option<SharedEntity> {
        self.look_at.lock().clone()
    }

    pub(crate) fn got_bamboo_or_bit(&self) -> bool {
        self.got_bamboo.load(Ordering::Relaxed) || self.did_bite.load(Ordering::Relaxed)
    }

    /// The panda-specific half of vanilla `Panda.tick`.
    fn tick_panda(&self) {
        let world = self.level();
        if self.is_worried() {
            let thundering = world.as_ref().is_some_and(|world| world.is_thundering());
            if thundering && !self.is_in_water() {
                self.sit(true);
                self.eat(false);
            } else if !self.is_eating() {
                self.sit(false);
            }
        }

        let target = self.target();
        if target.is_none() {
            self.got_bamboo.store(false, Ordering::Relaxed);
            self.did_bite.store(false, Ordering::Relaxed);
        }

        let unhappy = self.unhappy_counter();
        if unhappy > 0 {
            if let Some(target) = &target {
                self.mob_look_at(target.as_ref(), 90.0, 90.0);
            }
            if unhappy == 29 || unhappy == 14 {
                self.play_sound(&sound_events::ENTITY_PANDA_CANT_BREED, 1.0, 1.0);
            }
            self.set_unhappy_counter(unhappy - 1);
        }

        if self.is_sneezing() {
            let counter = self.sneeze_counter() + 1;
            self.set_sneeze_counter(counter);
            if counter > SNEEZE_TICKS {
                self.sneeze(false);
                if let Some(world) = &world {
                    self.after_sneeze(world);
                }
            } else if counter == 1 {
                self.play_sound(&sound_events::ENTITY_PANDA_PRE_SNEEZE, 1.0, 1.0);
            }
        }

        if self.is_rolling() {
            self.handle_roll();
        } else {
            self.roll_counter.store(0, Ordering::Relaxed);
        }

        if self.is_sitting() {
            let (y_rot, _) = self.rotation();
            self.set_rotation((y_rot, 0.0));
        }

        self.handle_eating();
    }

    /// Vanilla `Panda.handleEating`. The crumb particles are client-side.
    fn handle_eating(&self) {
        let has_food = !self.main_hand_is_empty();
        if !self.is_eating()
            && self.is_sitting()
            && !self.is_scared()
            && has_food
            && rand::random_range(0..80) == 1
        {
            self.eat(true);
        } else if !has_food || !self.is_sitting() {
            self.eat(false);
        }

        if !self.is_eating() {
            return;
        }
        let counter = self.eat_counter();
        if counter % EAT_TICK_INTERVAL == 0 {
            let volume = 0.5 + 0.5 * rand::random_range(0..2) as f32;
            let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0;
            self.play_sound(&sound_events::ENTITY_PANDA_EAT, volume, pitch);
        }
        if counter > 80 && rand::random_range(0..20) == 1 {
            if counter > 100 && is_ground_food(&self.equipment_in_slot(EquipmentSlot::MainHand)) {
                self.set_main_hand(ItemStack::empty());
                self.game_event(&vanilla_game_events::EAT);
                self.sit(false);
            }
            self.eat(false);
            return;
        }
        self.set_eat_counter(counter + 1);
    }

    /// Vanilla `Panda.handleRoll`.
    fn handle_roll(&self) {
        let counter = self.roll_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if counter > TOTAL_ROLL_STEPS {
            self.roll(false);
            return;
        }
        let movement = self.velocity();
        if counter == 1 {
            let angle = self.rotation().0.to_radians();
            let multiplier = if AgeableMob::is_baby(self) { 0.1 } else { 0.2 };
            let delta = DVec3::new(
                movement.x + f64::from(-angle.sin() * multiplier),
                0.0,
                movement.z + f64::from(angle.cos() * multiplier),
            );
            *self.roll_delta.lock() = delta;
            self.set_velocity(delta + DVec3::new(0.0, ROLL_JUMP, 0.0));
        } else if counter != 7 && counter != 15 && counter != 23 {
            let delta = *self.roll_delta.lock();
            self.set_velocity(DVec3::new(delta.x, movement.y, delta.z));
        } else {
            let y = if self.on_ground() {
                ROLL_JUMP
            } else {
                movement.y
            };
            self.set_velocity(DVec3::new(0.0, y, 0.0));
        }
    }

    /// Vanilla `Panda.afterSneeze`: nearby adults jump and the sneeze may
    /// drop slime. The sneeze particle is client-side.
    fn after_sneeze(&self, world: &Arc<World>) {
        self.play_sound(&sound_events::ENTITY_PANDA_SNEEZE, 1.0, 1.0);

        let area = self.bounding_box().inflate(SNEEZE_JUMP_RANGE);
        for entity in world
            .get_entities_in_aabb_matching(&area, |entity| entity.downcast_ref::<Self>().is_some())
        {
            if let Some(panda) = entity.downcast_ref::<Self>()
                && !AgeableMob::is_baby(panda)
                && panda.on_ground()
                && !panda.is_in_water()
                && panda.can_perform_action()
            {
                panda.jump_from_ground();
            }
        }

        if world.get_game_rule(&MOB_DROPS) {
            self.drop_sneeze_gift();
        }
    }

    /// Vanilla `dropFromGiftLootTable(level, PANDA_SNEEZE, spawnAtLocation)`.
    fn drop_sneeze_gift(&self) {
        let position = self.position();
        let mut rng = rand::rng();
        let equipment = living_loot_equipment(self);
        let mut context = LootContext::new(&mut rng)
            .with_origin(position.x, position.y, position.z)
            .with_this_entity(entity_loot_ref(self, Some(&equipment)));
        for item in vanilla_loot_tables::GAMEPLAY_PANDA_SNEEZE.get_random_items(&mut context) {
            self.spawn_at_location(item, 0.0);
        }
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }
}

impl DefaultEntityPush for PandaEntity {}

impl Entity for PandaEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        if !self.is_removed() {
            self.tick_panda();
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        let dimensions = if AgeableMob::is_baby(self) {
            let adult = self.entity_type.dimensions;
            EntityDimensions::new_with_attachments(
                adult.width * 0.5,
                adult.height * 0.5,
                0.281_25,
                EntityAttachments::new(&PANDA_BABY_PASSENGER_ATTACHMENTS, &[], &[], &[]),
            )
        } else {
            self.entity_type.dimensions
        };
        if self.entity_type.fixed {
            dimensions
        } else {
            dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn max_up_step(&self) -> f32 {
        self.attributes()
            .lock()
            .get_value(vanilla_attributes::STEP_HEIGHT)
            .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_PANDA_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("MainGene", self.main_gene().serialized_name());
        nbt.insert("HiddenGene", self.hidden_gene().serialized_name());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        let gene = |key: &str| {
            nbt.string(key)
                .and_then(|name| PandaGene::by_name(name.to_str().as_ref()))
                .unwrap_or(PandaGene::Normal)
        };
        self.set_main_gene(gene("MainGene"));
        self.set_hidden_gene(gene("HiddenGene"));
    }
}

impl LivingEntity for PandaEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PANDA_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_PANDA_DEATH)
    }

    /// Vanilla `Panda.hurtServer`: getting hurt makes it stand up.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.sit(false);
        self.default_hurt_server(world, source, amount)
    }

    fn can_dispenser_equip_into_slot(&self, slot: EquipmentSlot) -> bool {
        slot == EquipmentSlot::MainHand && self.can_pick_up_loot()
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for PandaEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob().age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob_mut()
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob_mut().baby.set(baby);
    }

    fn age_boundary_changed(&self, _baby: bool) {
        self.refresh_dimensions();
    }

    /// Vanilla `Panda.getBreedOffspring`: the cub takes a gene from each parent.
    fn get_breed_offspring(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        let offspring = self.create_breed_offspring(world)?;
        if let Some(baby) = offspring.downcast_ref::<Self>() {
            if let Some(partner) = partner.as_entity_event_source().downcast_ref::<Self>() {
                baby.set_gene_from_parents(self, Some(partner));
            }
            baby.set_gene_attributes();
        }
        Some(offspring)
    }
}

impl Animal for PandaEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::PANDA_FOOD)
    }
}

impl Mob for PandaEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `PandaMoveControl`: only moves while it is free to act.
    fn tick_move_control(&self) {
        if self.can_perform_action() {
            self.default_tick_move_control();
        }
    }

    fn custom_server_ai_step(&self) {
        Animal::custom_server_ai_step_animal(self);
    }

    /// Vanilla `Panda.isAggressive`: the aggressive gene, not the attack flag.
    fn is_aggressive(&self) -> bool {
        self.variant() == PandaGene::Aggressive
    }

    fn mob_can_be_leashed(&self) -> bool {
        false
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        if !Mob::is_aggressive(self) {
            self.did_bite.store(true, Ordering::Relaxed);
        }
        self.default_do_hurt_target(entity, world, target)
    }

    fn play_attack_sound(&self) {
        self.play_sound(&sound_events::ENTITY_PANDA_BITE, 1.0, 1.0);
    }

    /// Vanilla `Panda.pickUpItem`: only bamboo and cake, into an empty paw.
    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        if !self.main_hand_is_empty() || !can_pick_up_and_eat(item_entity) {
            return;
        }
        let item_stack = item_entity.get_item();
        let count = item_stack.count();
        self.set_main_hand(item_stack);
        self.set_guaranteed_drop(EquipmentSlot::MainHand);
        world.broadcast_to_nearby(
            ChunkPos::from_entity_pos(item_entity.position()),
            CTakeItemEntity::new(item_entity.id(), self.id(), count),
            None,
        );
        item_entity.set_removed(RemovalReason::Discarded);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        if Mob::is_aggressive(self) {
            Some(&sound_events::ENTITY_PANDA_AGGRESSIVE_AMBIENT)
        } else if self.is_worried() {
            Some(&sound_events::ENTITY_PANDA_WORRIED_AMBIENT)
        } else {
            Some(&sound_events::ENTITY_PANDA_AMBIENT)
        }
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_main_gene(PandaGene::random());
        self.set_hidden_gene(PandaGene::random());
        self.set_gene_attributes();
        let group_data = group_data.unwrap_or(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_baby_spawn_chance(BABY_SPAWN_CHANCE),
        ));
        self.finalize_spawn_ageable_mob(world, spawn_reason, Some(group_data))
    }

    /// Vanilla `Panda.mobInteract`.
    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if self.is_scared() {
            return InteractionResult::Pass;
        }
        if self.is_on_back() {
            self.set_on_back(false);
            return InteractionResult::Success;
        }
        if !self.is_food(&item_stack) {
            let holds_dandelion = item_stack.is(&vanilla_items::GOLDEN_DANDELION);
            return if AgeableMob::is_baby(self) && holds_dandelion {
                self.mob_interact_animal(player, hand)
            } else {
                InteractionResult::Pass
            };
        }

        if self.target().is_some() {
            self.got_bamboo.store(true, Ordering::Relaxed);
        }
        if self.can_age_up() {
            self.use_player_item(player, hand);
            self.age_up(
                AgeableMobBase::get_speed_up_seconds_when_feeding(-self.get_age()),
                true,
            );
        } else if AgeableMob::is_baby(self) {
            return InteractionResult::Pass;
        } else if self.get_age() == 0 && self.can_fall_in_love() {
            self.use_player_item(player, hand);
            self.set_in_love(Some(player));
        } else {
            if self.level().is_none() || self.is_sitting() || self.is_in_water() {
                return InteractionResult::Pass;
            }
            self.try_to_sit();
            self.eat(true);
            let current = self.equipment_in_slot(EquipmentSlot::MainHand);
            if !current.is_empty() && !player.has_infinite_materials() {
                self.spawn_at_location(current, 0.0);
            }
            self.set_main_hand(ItemStack::new(item_stack.item()));
            self.use_player_item(player, hand);
        }
        InteractionResult::SuccessServer
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for PandaEntity {}

/// Whether `entity` is a panda whose genes make it aggressive.
fn is_aggressive_panda(entity: &dyn Entity) -> bool {
    entity
        .downcast_ref::<PandaEntity>()
        .is_some_and(Mob::is_aggressive)
}

#[cfg(test)]
mod tests;
