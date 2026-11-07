use std::sync::Arc;

use super::{
    ATTACK_RANGE_BUFFER, CSetEntityMotion, ClipBlockShape, ClipFluid, DVec3, DamageSource,
    DamageType, ENTITY_INTERACTION_RANGE_BUFFER, EnchantmentDamageContext,
    EnchantmentPostAttackContext, Entity, EntityTypeRef, GameType, ITEM_BEHAVIORS, InteractionHand,
    InteractionResult, InventoryAccess, ItemStack, LivingEntity, PiercingWeapon, Player, SAttack,
    SInteract, SharedEntity, SoundEventHolder, SoundEventRef, TextComponent, TranslatedMessage,
    World, WorldAabb, enchantment_helper, piercing_ray_hit_t, vanilla_attributes,
    vanilla_damage_types, vanilla_entities,
};

use crate::entity::{EntityReference, ProjectileDeflection};
use crate::player::food_data::food_constants;
use steel_math::trig;
use steel_protocol::packets::game::CAnimate;
use steel_registry::particle_type::ParticleData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_custom_stats, vanilla_mob_effects,
    vanilla_particle_types,
};

const fn sound_holder_ref(holder: &SoundEventHolder) -> Option<SoundEventRef> {
    match holder {
        SoundEventHolder::Registry(sound) => Some(*sound),
        SoundEventHolder::Direct { .. } => {
            // TODO: Support direct sound holders when entity sound playback can send them.
            None
        }
    }
}
impl Player {
    fn invalid_entity_attacked_message() -> TextComponent {
        TranslatedMessage {
            key: "multiplayer.disconnect.invalid_entity_attacked".into(),
            fallback: None,
            args: None,
        }
        .component()
    }

    fn eye_position(&self) -> DVec3 {
        let position = self.position();
        DVec3::new(position.x, self.get_eye_y(), position.z)
    }

    fn damage_source_for_attack_type(
        self: &Arc<Self>,
        damage_type: &'static DamageType,
    ) -> DamageSource {
        DamageSource::direct(damage_type, Arc::<Player>::clone(self))
    }

    fn attack_damage_source(self: &Arc<Self>, attacking_item: &ItemStack) -> DamageSource {
        if let Some(damage_type) = attacking_item.get_damage_type() {
            return self.damage_source_for_attack_type(damage_type);
        }
        let attacker: SharedEntity = Arc::<Player>::clone(self);
        if let Some(source) = ITEM_BEHAVIORS
            .get_behavior(attacking_item.item())
            .get_item_damage_source(&attacker)
        {
            return source;
        }
        self.damage_source_for_attack_type(&vanilla_damage_types::PLAYER_ATTACK)
    }

    /// Ticks vanilla attack-strength recovery and resets it on main-hand item changes.
    pub(in crate::player) fn tick_attack_strength(&self) {
        self.tick_state.lock().advance_attack_strength_ticker();

        let main_hand_item = {
            let inventory = self.inventory.lock();
            let stack = inventory.get_item_in_hand(InteractionHand::MainHand);
            stack.copy_with_count(stack.count())
        };

        let mut last_item = self.last_item_in_main_hand.lock();
        if ItemStack::matches(&last_item, &main_hand_item) {
            return;
        }

        if !ItemStack::is_same_item(&last_item, &main_hand_item) {
            self.reset_attack_strength_ticker();
        }

        *last_item = main_hand_item;
    }

    pub(crate) fn reset_attack_strength_ticker(&self) {
        self.tick_state.lock().reset_attack_strength_ticker();
    }

    fn current_item_attack_strength_delay(&self) -> f32 {
        let attack_speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_SPEED);
        Self::attack_strength_delay_from_speed(attack_speed)
    }

    fn attack_strength_delay_from_speed(attack_speed: f64) -> f32 {
        (1.0 / attack_speed * 20.0) as f32
    }

    /// Returns how much of the current weapon's attack cooldown has
    /// recovered, in [0.0, 1.0], for scaling attack damage.
    #[must_use]
    pub fn attack_strength_scale(&self, partial_tick: f32) -> f32 {
        let attack_strength_delay = self.current_item_attack_strength_delay();
        self.attack_strength_scale_for_delay(partial_tick, attack_strength_delay)
    }

    fn attack_strength_scale_for_delay(
        &self,
        partial_tick: f32,
        attack_strength_delay: f32,
    ) -> f32 {
        let ticker = self.tick_state.lock().attack_strength_ticker() as f32;
        ((ticker + partial_tick) / attack_strength_delay).clamp(0.0, 1.0)
    }

    fn base_damage_scale_factor(attack_strength_scale: f32) -> f32 {
        0.2 + attack_strength_scale * attack_strength_scale * 0.8
    }

    fn get_knockback(
        attack_knockback: f64,
        weapon: &ItemStack,
        enchantment_context: &EnchantmentDamageContext<'_>,
    ) -> f64 {
        let modified = enchantment_helper::modify_knockback(
            weapon,
            enchantment_context,
            attack_knockback as f32,
        );
        f64::from(modified) / 2.0
    }

    fn cause_extra_knockback(
        &self,
        entity: &dyn Entity,
        knockback_amount: f64,
        old_movement: DVec3,
    ) {
        if knockback_amount > 0.0 {
            let yaw_radians = self.rotation().0.to_radians();
            let yaw_sin = f64::from(trig::sin(yaw_radians));
            let yaw_cos = f64::from(trig::cos(yaw_radians));
            if let Some(living_target) = entity.as_living_entity() {
                living_target.knockback(knockback_amount, yaw_sin, -yaw_cos);
            } else {
                entity.push_impulse(DVec3::new(
                    -yaw_sin * knockback_amount,
                    0.1,
                    yaw_cos * knockback_amount,
                ));
            }

            let velocity = self.velocity();
            self.set_velocity(DVec3::new(velocity.x * 0.6, velocity.y, velocity.z * 0.6));
            self.set_sprinting(false);
        }

        if entity.entity_type() == &vanilla_entities::PLAYER
            && entity.hurt_marked()
            && let Some(player) = self.get_world().players.get_by_entity_id(entity.id())
        {
            let velocity = entity.velocity();
            player.send_packet(CSetEntityMotion::new(entity.id(), velocity));
            entity.clear_hurt_mark();
            entity.set_velocity(old_movement);
        }
    }

    fn entity_interaction_range(&self) -> f64 {
        self.attributes()
            .lock()
            .required_value(vanilla_attributes::ENTITY_INTERACTION_RANGE)
    }

    /// Returns true if the target box is within the player's attack range for `item_stack`.
    #[must_use]
    pub fn is_within_attack_range_with_buffer(
        &self,
        item_stack: &ItemStack,
        aabb: WorldAabb,
        buffer: f64,
    ) -> bool {
        let distance = aabb.distance_to_sqr(self.eye_position()).sqrt();
        let (min_reach, max_reach, hitbox_margin) =
            if let Some(attack_range) = item_stack.get_attack_range() {
                if self.game_mode() == GameType::Creative {
                    (
                        attack_range.min_creative_reach,
                        attack_range.max_creative_reach,
                        attack_range.hitbox_margin,
                    )
                } else {
                    (
                        attack_range.min_reach,
                        attack_range.max_reach,
                        attack_range.hitbox_margin,
                    )
                }
            } else {
                (0.0, self.entity_interaction_range() as f32, 0.0)
            };
        let min_reach = f64::from(min_reach) - f64::from(hitbox_margin) - buffer;
        let max_reach = f64::from(max_reach) + f64::from(hitbox_margin) + buffer;
        distance >= min_reach && distance <= max_reach
    }

    /// Returns true if the target box is within the player's entity interaction range.
    #[must_use]
    pub fn is_within_entity_interaction_range_with_buffer(
        &self,
        aabb: WorldAabb,
        buffer: f64,
    ) -> bool {
        let max_range = self.entity_interaction_range() + buffer;
        aabb.distance_to_sqr(self.eye_position()) <= max_range * max_range
    }

    fn attack_range_for_item(&self, item_stack: &ItemStack) -> (f64, f64, f64) {
        let Some(attack_range) = item_stack.get_attack_range() else {
            return (0.0, self.entity_interaction_range(), 0.0);
        };

        let (min_reach, max_reach) = if self.game_mode() == GameType::Creative {
            (
                attack_range.min_creative_reach,
                attack_range.max_creative_reach,
            )
        } else {
            (attack_range.min_reach, attack_range.max_reach)
        };
        (
            f64::from(min_reach),
            f64::from(max_reach),
            f64::from(attack_range.hitbox_margin),
        )
    }

    fn piercing_hit_entities(&self, item_stack: &ItemStack, world: &World) -> Vec<SharedEntity> {
        let look = self.look_angle();
        if look.length_squared() <= f64::EPSILON {
            return Vec::new();
        }

        let (min_reach, max_reach, hitbox_margin) = self.attack_range_for_item(item_stack);
        let eye_position = self.eye_position();
        let from = eye_position + look * min_reach;
        let movement_extension = self.known_movement().dot(look).max(0.0);
        let mut to = eye_position + look * (max_reach + movement_extension);

        let block_hit = world.clip(eye_position, to, ClipBlockShape::Collider, ClipFluid::None);
        if !block_hit.is_miss() {
            to = block_hit.location;
            if eye_position.distance_squared(to) < eye_position.distance_squared(from) {
                return Vec::new();
            }
        }

        let search_area = WorldAabb::new(from.x, from.y, from.z, from.x, from.y, from.z)
            .inflate_xyz(hitbox_margin, hitbox_margin, hitbox_margin)
            .expand_towards(to - from)
            .inflate(1.0);
        let mut hits = world
            .get_entities_in_aabb_matching(&search_area, |entity| {
                self.can_piercing_hit_entity(entity)
            })
            .into_iter()
            .filter_map(|entity| {
                piercing_ray_hit_t(world, entity.bounding_box(), from, to, hitbox_margin)
                    .map(|hit_t| (hit_t, entity))
            })
            .collect::<Vec<_>>();
        hits.sort_by(|(left, _), (right, _)| left.total_cmp(right));
        hits.into_iter().map(|(_, entity)| entity).collect()
    }

    fn can_piercing_hit_entity(&self, target: &dyn Entity) -> bool {
        target.id() != self.id()
            && !target.is_invulnerable()
            && target.is_alive()
            && target.can_be_hit_by_projectile()
            && !self.is_passenger_of_same_vehicle(target)
    }

    pub(super) fn piercing_attack(
        self: &Arc<Self>,
        item_stack: &ItemStack,
        piercing_weapon: &PiercingWeapon,
    ) {
        let world = self.get_world();
        let base_damage = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as f32;
        let mut hit_something = false;
        for target in self.piercing_hit_entities(item_stack, &world) {
            hit_something |= self.stab_attack(
                &target,
                base_damage,
                true,
                piercing_weapon.deals_knockback,
                piercing_weapon.dismounts,
            );
        }

        self.reset_attack_strength_ticker();
        enchantment_helper::do_post_piercing_attack_effects(&world, self.as_ref());
        if hit_something {
            self.play_sound_holder(piercing_weapon.hit_sound.as_ref());
        }
        self.play_sound_holder(piercing_weapon.sound.as_ref());
        self.swing(
            InteractionHand::MainHand,
            self.attack_animation(InteractionHand::MainHand),
            false,
        );
    }

    fn stab_attack(
        self: &Arc<Self>,
        target: &SharedEntity,
        base_damage: f32,
        deals_damage: bool,
        deals_knockback: bool,
        dismounts: bool,
    ) -> bool {
        let entity = target.as_ref();
        if self.cannot_attack(entity) {
            return false;
        }

        let attacking_item = {
            let inventory = self.inventory.lock();
            let stack = inventory.get_item_in_hand(InteractionHand::MainHand);
            stack.copy_with_count(stack.count())
        };
        let damage_source = self.attack_damage_source(&attacking_item);
        let enchantment_context = EnchantmentDamageContext::new(
            entity.entity_type(),
            Some(self.entity_type()),
            Some(self.entity_type()),
            &damage_source,
        );
        let enchanted_damage =
            enchantment_helper::modify_damage(&attacking_item, &enchantment_context, base_damage);
        let mut magic_boost = enchanted_damage - base_damage;
        let mut base_damage = base_damage;
        // Vanilla scales by attack strength unless the hit comes from the
        // item being used in this hand (a kinetic charge).
        let using_main_hand = self
            .living_base()
            .active_item_use()
            .is_some_and(|active| active.hand() == InteractionHand::MainHand);
        if !using_main_hand {
            let attack_strength_scale = self.attack_strength_scale(0.5);
            magic_boost *= attack_strength_scale;
            base_damage *= Self::base_damage_scale_factor(attack_strength_scale);
        }
        if deals_knockback && self.deflect_projectile(target) {
            return true;
        }

        let total_damage = if deals_damage {
            base_damage + magic_boost
        } else {
            0.0
        };
        let old_entity_living_health = entity
            .as_living_entity()
            .map_or(0.0, LivingEntity::get_health);
        let old_movement = entity.velocity();
        let was_hurt = deals_damage
            && entity
                .level()
                .is_some_and(|world| entity.hurt(&world, &damage_source, total_damage));
        if deals_knockback {
            self.cause_extra_knockback(entity, 0.4, old_movement);
            self.cause_extra_knockback(
                entity,
                Self::get_knockback(0.0, &attacking_item, &enchantment_context),
                old_movement,
            );
        }
        let mut dismounted = false;
        if dismounts
            && entity.is_passenger()
            && !REGISTRY.entity_types.is_in_tag(
                entity.entity_type(),
                &EntityTypeTag::CANNOT_BE_DISMOUNTED_BY_ITEM_USAGE,
            )
        {
            dismounted = true;
            entity.stop_riding();
        }

        if !was_hurt && !deals_knockback && !dismounted {
            return false;
        }

        self.attack_visual_effects(entity, false, false, deals_damage, true, magic_boost);
        self.set_last_hurt_mob(Some(target));
        self.item_attack_interaction(target, &damage_source, was_hurt);
        self.damage_stats_and_hearts(entity, old_entity_living_health);
        self.cause_food_exhaustion(food_constants::EXHAUSTION_ATTACK);
        true
    }

    fn play_sound_holder(&self, holder: Option<&SoundEventHolder>) {
        let Some(sound) = holder.and_then(sound_holder_ref) else {
            return;
        };
        self.play_sound(sound, 1.0, 1.0);
    }

    fn cannot_attack(self: &Arc<Self>, entity: &dyn Entity) -> bool {
        !entity.attackable()
            || entity.skip_attack_interaction(&(Arc::clone(self) as Arc<dyn Entity>))
    }

    /// Vanilla `Player.deflectProjectile`: hitting a redirectable projectile
    /// (fireballs, wind charges) sends it back where the player looks.
    fn deflect_projectile(self: &Arc<Self>, target: &SharedEntity) -> bool {
        if !REGISTRY.entity_types.is_in_tag(
            target.entity_type(),
            &EntityTypeTag::REDIRECTABLE_PROJECTILE,
        ) {
            return false;
        }
        let Some(projectile) = target.as_projectile() else {
            return false;
        };
        let player: SharedEntity = Arc::<Player>::clone(self);
        if !projectile.deflect(
            ProjectileDeflection::AimDeflect,
            Some(player.as_ref()),
            Some(EntityReference::from_entity(&player)),
            true,
        ) {
            return false;
        }
        let world = self.get_world();
        world.play_sound_at(
            &sound_events::ENTITY_PLAYER_ATTACK_NODAMAGE,
            self.sound_source(),
            self.position(),
            1.0,
            1.0,
            None,
        );
        true
    }

    /// Vanilla `Player.attack`: a melee hit with the main-hand weapon,
    /// including critical hits, sweep attacks and the attack sounds.
    ///
    /// Returns `true` if the target accepted damage.
    #[must_use]
    pub fn attack(self: &Arc<Self>, target: &SharedEntity) -> bool {
        let entity = target.as_ref();
        if self.cannot_attack(entity) {
            return false;
        }

        // TODO: Riptide auto spin attacks use `autoSpinAttackDmg` once they exist.
        let attacking_item = {
            let inventory = self.inventory.lock();
            let stack = inventory.get_item_in_hand(InteractionHand::MainHand);
            stack.copy_with_count(stack.count())
        };
        let (attack_damage, attack_speed, attack_knockback) = {
            let attributes = self.attributes().lock();
            (
                attributes.required_value(vanilla_attributes::ATTACK_DAMAGE) as f32,
                attributes.required_value(vanilla_attributes::ATTACK_SPEED),
                attributes.required_value(vanilla_attributes::ATTACK_KNOCKBACK),
            )
        };
        let damage_source = self.attack_damage_source(&attacking_item);
        let attack_strength_delay = Self::attack_strength_delay_from_speed(attack_speed);
        let attack_strength_scale =
            self.attack_strength_scale_for_delay(0.5, attack_strength_delay);
        let enchantment_context = EnchantmentDamageContext::new(
            entity.entity_type(),
            Some(self.entity_type()),
            Some(self.entity_type()),
            &damage_source,
        );
        let enchanted_damage =
            enchantment_helper::modify_damage(&attacking_item, &enchantment_context, attack_damage);
        let magic_boost = attack_strength_scale * (enchanted_damage - attack_damage);
        let mut base_damage = attack_damage * Self::base_damage_scale_factor(attack_strength_scale);
        self.reset_attack_strength_ticker();
        if self.deflect_projectile(target) {
            return false;
        }

        let world = self.get_world();
        let mut was_hurt = false;
        if base_damage > 0.0 || magic_boost > 0.0 {
            let full_strength_attack = attack_strength_scale > 0.9;
            let knockback_attack = self.is_sprinting() && full_strength_attack;
            if knockback_attack {
                self.play_server_side_sound(&sound_events::ENTITY_PLAYER_ATTACK_KNOCKBACK);
            }

            base_damage += ITEM_BEHAVIORS
                .get_behavior(attacking_item.item())
                .get_attack_damage_bonus(self.as_ref(), entity, base_damage, &damage_source);
            let critical_attack = full_strength_attack && self.can_critical_attack(entity);
            if critical_attack {
                base_damage *= 1.5;
            }
            let total_damage = base_damage + magic_boost;
            let sweep_attack =
                self.is_sweep_attack(full_strength_attack, critical_attack, knockback_attack);
            let old_entity_living_health = entity
                .as_living_entity()
                .map_or(0.0, LivingEntity::get_health);

            let old_movement = entity.velocity();
            was_hurt = entity.level().is_some_and(|target_world| {
                entity.hurt(&target_world, &damage_source, total_damage)
            });
            if was_hurt {
                let sprint_knockback = if knockback_attack { 0.5 } else { 0.0 };
                self.cause_extra_knockback(
                    entity,
                    Self::get_knockback(attack_knockback, &attacking_item, &enchantment_context)
                        + sprint_knockback,
                    old_movement,
                );
                if sweep_attack {
                    self.do_sweep_attack(
                        &world,
                        entity,
                        &attacking_item,
                        base_damage,
                        &damage_source,
                        attack_strength_scale,
                    );
                }
                self.attack_visual_effects(
                    entity,
                    critical_attack,
                    sweep_attack,
                    full_strength_attack,
                    false,
                    magic_boost,
                );
                self.set_last_hurt_mob(Some(target));
                self.item_attack_interaction(target, &damage_source, true);
                self.damage_stats_and_hearts(entity, old_entity_living_health);
                self.cause_food_exhaustion(food_constants::EXHAUSTION_ATTACK);
            } else {
                self.play_server_side_sound(&sound_events::ENTITY_PLAYER_ATTACK_NODAMAGE);
            }
        }

        enchantment_helper::do_post_piercing_attack_effects(&world, self.as_ref());
        was_hurt
    }

    /// Vanilla `Player.playServerSideSound`: a sound at the player heard by
    /// everyone, the player included.
    fn play_server_side_sound(&self, sound: SoundEventRef) {
        self.get_world()
            .play_sound_at(sound, self.sound_source(), self.position(), 1.0, 1.0, None);
    }

    /// Vanilla `Player.canCriticalAttack`.
    fn can_critical_attack(&self, entity: &dyn Entity) -> bool {
        self.fall_distance() > 0.0
            && !self.on_ground()
            && !self.on_climbable()
            && !self.is_in_water()
            && self.mob_effect(vanilla_mob_effects::BLINDNESS).is_none()
            && !self.is_passenger()
            && entity.as_living_entity().is_some()
            && !self.is_sprinting()
    }

    /// Vanilla `Player.isSweepAttack`: a full-strength, grounded, slow sword
    /// hit that is neither critical nor a sprint knockback.
    fn is_sweep_attack(
        &self,
        full_strength_attack: bool,
        critical_attack: bool,
        knockback_attack: bool,
    ) -> bool {
        if !full_strength_attack || critical_attack || knockback_attack || !self.on_ground() {
            return false;
        }
        let movement = self.known_movement();
        let approximate_speed_sq = movement.x * movement.x + movement.z * movement.z;
        // Vanilla `Player.getSpeed` reads the movement speed attribute.
        let speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED) as f32;
        let max_speed_for_sweep_attack = f64::from(speed * 2.5);
        if approximate_speed_sq >= max_speed_for_sweep_attack * max_speed_for_sweep_attack {
            return false;
        }
        let inventory = self.inventory.lock();
        inventory
            .get_item_in_hand(InteractionHand::MainHand)
            .item()
            .has_tag(&ItemTag::SWORDS)
    }

    /// Vanilla `Player.doSweepAttack`: damages and knocks back the living
    /// entities next to the main target.
    fn do_sweep_attack(
        &self,
        world: &Arc<World>,
        entity: &dyn Entity,
        attacking_item: &ItemStack,
        base_damage: f32,
        damage_source: &DamageSource,
        attack_strength_scale: f32,
    ) {
        self.play_server_side_sound(&sound_events::ENTITY_PLAYER_ATTACK_SWEEP);
        let sweeping_damage_ratio =
            self.attributes()
                .lock()
                .required_value(vanilla_attributes::SWEEPING_DAMAGE_RATIO) as f32;
        let sweep_damage = 1.0 + sweeping_damage_ratio * base_damage;
        let (yaw, _) = self.rotation();
        let yaw_radians = yaw.to_radians();
        let yaw_sin = f64::from(trig::sin(yaw_radians));
        let yaw_cos = f64::from(trig::cos(yaw_radians));
        let area = entity.bounding_box().inflate_xyz(1.0, 0.25, 1.0);
        // Vanilla `getEntitiesOfClass` skips spectators.
        let nearby_entities = world.get_entities_in_aabb_matching(&area, |nearby| {
            !nearby.is_spectator() && nearby.as_living_entity().is_some()
        });
        for nearby in nearby_entities {
            let Some(living) = nearby.as_living_entity() else {
                continue;
            };
            if nearby.id() == self.id()
                || nearby.id() == entity.id()
                || self.is_allied_to(nearby.as_ref())
                || nearby.is_marker_armor_stand()
                || self.distance_to_sqr(nearby.position()) >= 9.0
            {
                continue;
            }
            let context = EnchantmentDamageContext::new(
                nearby.entity_type(),
                Some(self.entity_type()),
                Some(self.entity_type()),
                damage_source,
            );
            let enchanted_damage =
                enchantment_helper::modify_damage(attacking_item, &context, sweep_damage)
                    * attack_strength_scale;
            if nearby.hurt(world, damage_source, enchanted_damage) {
                living.knockback(0.4, yaw_sin, -yaw_cos);
                let post_attack_context = EnchantmentPostAttackContext::new(&nearby, damage_source);
                enchantment_helper::do_post_attack_effects_with_item_source(
                    world,
                    nearby.as_ref(),
                    attacking_item,
                    &post_attack_context,
                );
            }
        }

        let dx = -yaw_sin;
        let dz = yaw_cos;
        let position = self.position();
        world.send_particles(
            ParticleData::simple(&vanilla_particle_types::SWEEP_ATTACK),
            DVec3::new(
                position.x + dx,
                position.y + f64::from(self.base().dimensions().height) * 0.5,
                position.z + dz,
            ),
            0,
            DVec3::new(dx, 0.0, dz),
            0.0,
        );
    }

    /// Vanilla `Player.attackVisualEffects`.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "mirrors Vanilla's attackVisualEffects parameters"
    )]
    fn attack_visual_effects(
        &self,
        entity: &dyn Entity,
        critical_attack: bool,
        sweep_attack: bool,
        full_strength_attack: bool,
        stab_attack: bool,
        magic_boost: f32,
    ) {
        if critical_attack {
            self.play_server_side_sound(&sound_events::ENTITY_PLAYER_ATTACK_CRIT);
            self.send_attack_animation(CAnimate::critical_hit(entity.id()));
        }
        if !critical_attack && !sweep_attack && !stab_attack {
            self.play_server_side_sound(if full_strength_attack {
                &sound_events::ENTITY_PLAYER_ATTACK_STRONG
            } else {
                &sound_events::ENTITY_PLAYER_ATTACK_WEAK
            });
        }
        if magic_boost > 0.0 {
            self.send_attack_animation(CAnimate::magic_critical_hit(entity.id()));
        }
    }

    /// Vanilla `ServerPlayer.crit` / `magicCrit`: the animation goes to the
    /// player's trackers and the player.
    fn send_attack_animation(&self, packet: CAnimate) {
        self.get_world()
            .broadcast_to_entity_trackers(self.id(), packet.clone(), None);
        self.send_packet(packet);
    }

    fn item_attack_interaction(
        &self,
        target: &SharedEntity,
        damage_source: &DamageSource,
        apply_to_target: bool,
    ) {
        let entity = target.as_ref();
        let post_attack_context = EnchantmentPostAttackContext::new(target, damage_source);
        let (source_item, item_hurt_enemy) = {
            let mut inventory = self.inventory.lock();
            inventory.mutate_item_in_hand(InteractionHand::MainHand, |stack| {
                if stack.is_empty() {
                    return (ItemStack::empty(), false);
                }
                let behavior = ITEM_BEHAVIORS.get_behavior(stack.item());
                if let Some(living_target) = entity.as_living_entity() {
                    behavior.hurt_enemy(stack, living_target, self);
                }
                let source_item = stack.copy_with_count(stack.count());
                (source_item, stack.get_weapon().is_some())
            })
        };

        if apply_to_target {
            let world = self.get_world();
            enchantment_helper::do_post_attack_effects_with_item_source(
                &world,
                entity,
                &source_item,
                &post_attack_context,
            );
        }

        if !item_hurt_enemy {
            return;
        }

        let Some(living_target) = entity.as_living_entity() else {
            return;
        };
        let has_infinite_materials = self.has_infinite_materials();
        let mut inventory = self.inventory.lock();
        inventory.mutate_item_in_hand(InteractionHand::MainHand, |stack| {
            if stack.is_empty() {
                return;
            }
            let behavior = ITEM_BEHAVIORS.get_behavior(stack.item());
            behavior.post_hurt_enemy(stack, living_target, self);
            if let Some(damage) = behavior.item_damage_per_attack(stack) {
                stack.hurt_and_break(damage, has_infinite_materials);
            }
        });
    }

    /// Interacts with an entity using the held item.
    pub fn interact_on(
        &self,
        entity: &dyn Entity,
        hand: InteractionHand,
        location: DVec3,
    ) -> InteractionResult {
        if self.is_spectator() {
            // TODO: Open entity menu providers in spectator once that foundation exists.
            return InteractionResult::Pass;
        }

        let inventory_access = InventoryAccess::new(Arc::clone(&self.inventory), hand);
        let original_count = inventory_access.with_item(|item| item.count);
        let result = entity.interact(self, hand, location);

        if self.has_infinite_materials() {
            inventory_access.with_item(|item| {
                if item.count < original_count {
                    item.count = original_count;
                }
            });
        }

        if result.consumes_action() {
            return result;
        }

        if inventory_access.with_item(|item| item.is_empty()) {
            return InteractionResult::Pass;
        }
        let Some(living_entity) = entity.as_living_entity() else {
            return InteractionResult::Pass;
        };
        let result = living_entity.interact_living_entity_with_equippable(self, hand);
        if self.has_infinite_materials() {
            inventory_access.with_item(|item| {
                if item.count < original_count {
                    item.count = original_count;
                }
            });
        }
        if result.consumes_action() {
            return result;
        }

        let item_ref = inventory_access.with_item(|item| item.item());
        let item_behavior = ITEM_BEHAVIORS.get_behavior(item_ref);
        let result = inventory_access.with_item(|item| {
            item_behavior.interact_living_entity(item, self, living_entity, hand)
        });
        if self.has_infinite_materials() {
            inventory_access.with_item(|item| {
                if item.count < original_count {
                    item.count = original_count;
                }
            });
        }
        result
    }

    /// Handles a client request to attack an entity.
    pub fn handle_attack(self: &Arc<Self>, packet: SAttack) {
        if !self.has_client_loaded() || self.is_spectator() {
            return;
        }

        let world = self.get_world();
        let Some(target) = world.get_accessible_entity_by_id(packet.entity_id) else {
            return;
        };

        self.reset_last_action_time();

        let target_pos = target.block_position();
        if !world.world_border_snapshot().is_within_bounds_with_margin(
            f64::from(target_pos.x()),
            f64::from(target_pos.z()),
            0.0,
        ) {
            return;
        }

        let main_hand_item = {
            let inventory = self.inventory.lock();
            let stack = inventory.get_item_in_hand(InteractionHand::MainHand);
            stack.copy_with_count(stack.count())
        };

        if !self.is_within_attack_range_with_buffer(
            &main_hand_item,
            target.bounding_box(),
            ATTACK_RANGE_BUFFER,
        ) {
            return;
        }

        if main_hand_item.get_piercing_weapon().is_some() {
            return;
        }

        if Self::is_invalid_attack_target(self.id(), target.id(), target.entity_type()) {
            self.disconnect(Self::invalid_entity_attacked_message());
            log::warn!(
                "Player {} tried to attack an invalid entity",
                self.gameprofile.name()
            );
            return;
        }

        if self.cannot_attack_with_item(&main_hand_item, 5) {
            return;
        }

        let _ = self.attack(&target);
    }

    pub(super) fn cannot_attack_with_item(&self, item_stack: &ItemStack, tolerance: i32) -> bool {
        let required_strength = item_stack.minimum_attack_charge();
        if required_strength <= 0.0 {
            return false;
        }

        let optimistic_strength = {
            let ticker = self.tick_state.lock().attack_strength_ticker() + tolerance;
            ticker as f32 / self.current_item_attack_strength_delay()
        };
        optimistic_strength < required_strength
    }

    pub(super) fn is_invalid_attack_target(
        player_id: i32,
        target_id: i32,
        target_type: EntityTypeRef,
    ) -> bool {
        target_id == player_id
            || target_type == &vanilla_entities::ITEM
            || target_type == &vanilla_entities::EXPERIENCE_ORB
    }

    /// Handles a client request to interact with an entity.
    pub fn handle_interact(&self, packet: SInteract) {
        if !self.has_client_loaded() {
            return;
        }

        let world = self.get_world();
        self.reset_last_action_time();
        let target = world.get_accessible_entity_by_id(packet.entity_id);
        self.set_crouching(packet.using_secondary_action);
        let Some(target) = target else {
            return;
        };

        let target_pos = target.block_position();
        if !world.world_border_snapshot().is_within_bounds_with_margin(
            f64::from(target_pos.x()),
            f64::from(target_pos.z()),
            0.0,
        ) {
            return;
        }

        if !self.is_within_entity_interaction_range_with_buffer(
            target.bounding_box(),
            ENTITY_INTERACTION_RANGE_BUFFER,
        ) {
            return;
        }

        let animation = self.interact_animation(packet.hand);
        let result = self.interact_on(target.as_ref(), packet.hand, packet.location);
        self.swing_after_interaction(packet.hand, animation, result);
        self.broadcast_inventory_changes();
    }

    /// Vanilla `Player.damageStatsAndHearts`: awards the damage stat and sends
    /// damage indicator particles for hits over one heart.
    pub fn damage_stats_and_hearts(&self, entity: &dyn Entity, old_entity_living_health: f32) {
        let Some(entity) = entity.as_living_entity() else {
            return;
        };
        let actual_damage = old_entity_living_health - entity.get_health();
        // Java `Math.round(float)`.
        self.award_custom_stat_with_count(
            &vanilla_custom_stats::DAMAGE_DEALT,
            (actual_damage * 10.0 + 0.5).floor() as i32,
        );
        if actual_damage > 2.0 {
            let count = (actual_damage * 0.5) as i32;
            let position = entity.position();
            self.get_world().send_particles(
                ParticleData::simple(&vanilla_particle_types::DAMAGE_INDICATOR),
                DVec3::new(
                    position.x,
                    position.y + f64::from(entity.base().dimensions().height) * 0.5,
                    position.z,
                ),
                count,
                DVec3::new(0.1, 0.0, 0.1),
                0.2,
            );
        }
    }
}
