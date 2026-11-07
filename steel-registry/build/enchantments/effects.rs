use super::{
    AttributeEffectJson, ConditionalDamageImmunityEffectJson, ConditionalEntityEffectJson,
    ConditionalLocationEffectJson, ConditionalValueEffectJson, CrossbowChargingSoundsJson,
    DamageSourcePredicateJson, EnchantmentTargetJson, EntityEffectJson, EntityFlagsPredicateJson,
    EntityPredicateJson, EntityTargetJson, EntityTypePredicateJson,
    EntityTypeSpecificPredicateJson, EntityVehiclePredicateJson, GameType, Ident, Identifier,
    ItemHolderSetJson, LevelBasedValueJson, LevelBasedValueTypedJson, LocationEffectJson,
    MobEffectSelectionJson, MovementAffectedByJson, MovementPredicateJson,
    ParticlePositionSourceJson, ParticleVelocitySourceJson, RequirementsJson, Span,
    TargetedConditionalEntityEffectJson, TargetedConditionalValueEffectJson, ToShoutySnakeCase,
    TokenStream, ValueEffectJson, damage_type_ref_token, generate_float_provider,
    generate_sound_event_ref, identifier_token, quote,
};

pub(super) fn attribute_ref_token(attribute: &Identifier) -> TokenStream {
    assert_eq!(
        attribute.namespace.as_ref(),
        "minecraft",
        "vanilla enchantment attribute references must use the minecraft namespace: {attribute}"
    );
    let ident = Ident::new(&attribute.path.to_shouty_snake_case(), Span::call_site());
    quote! { vanilla_attributes::#ident }
}

pub(super) fn mob_effect_ref_token(effect: &Identifier) -> TokenStream {
    assert_eq!(
        effect.namespace.as_ref(),
        "minecraft",
        "vanilla enchantment mob effect references must use the minecraft namespace: {effect}"
    );
    let ident = Ident::new(&effect.path.to_shouty_snake_case(), Span::call_site());
    quote! { vanilla_mob_effects::#ident }
}

pub(super) fn attribute_modifier_operation_token(operation: &str) -> TokenStream {
    match operation {
        "add_value" => quote! { AttributeModifierOperation::AddValue },
        "add_multiplied_base" => quote! { AttributeModifierOperation::AddMultipliedBase },
        "add_multiplied_total" => quote! { AttributeModifierOperation::AddMultipliedTotal },
        other => panic!("Unknown enchantment attribute modifier operation: {other}"),
    }
}

pub(super) fn option_sound_event_ref_token(sound: Option<&Identifier>) -> TokenStream {
    if let Some(sound) = sound {
        let sound = generate_sound_event_ref(sound);
        quote! { Some(#sound) }
    } else {
        quote! { None }
    }
}

pub(super) fn generate_level_based_value_ref(
    prefix: &str,
    value: &LevelBasedValueJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let ident = Ident::new(
        &format!("{prefix}_LEVEL_VALUE_{}", *counter),
        Span::call_site(),
    );
    *counter += 1;
    let value = generate_level_based_value(prefix, value, statics, counter);

    statics.extend(quote! {
        static #ident: LevelBasedValue = #value;
    });

    quote! { &#ident }
}

pub(super) fn generate_level_based_value(
    prefix: &str,
    value: &LevelBasedValueJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    match value {
        LevelBasedValueJson::Constant(value) => quote! { LevelBasedValue::Constant(#value) },
        LevelBasedValueJson::Typed(value) => match value {
            LevelBasedValueTypedJson::Clamped { value, min, max } => {
                let value = generate_level_based_value_ref(prefix, value, statics, counter);
                quote! { LevelBasedValue::Clamped { value: #value, min: #min, max: #max } }
            }
            LevelBasedValueTypedJson::Exponent { base, power } => {
                let base = generate_level_based_value_ref(prefix, base, statics, counter);
                let power = generate_level_based_value_ref(prefix, power, statics, counter);
                quote! { LevelBasedValue::Exponent { base: #base, power: #power } }
            }
            LevelBasedValueTypedJson::Fraction {
                numerator,
                denominator,
            } => {
                let numerator = generate_level_based_value_ref(prefix, numerator, statics, counter);
                let denominator =
                    generate_level_based_value_ref(prefix, denominator, statics, counter);
                quote! { LevelBasedValue::Fraction { numerator: #numerator, denominator: #denominator } }
            }
            LevelBasedValueTypedJson::LevelsSquared { added } => {
                quote! { LevelBasedValue::LevelsSquared { added: #added } }
            }
            LevelBasedValueTypedJson::Linear {
                base,
                per_level_above_first,
            } => {
                quote! { LevelBasedValue::Linear { base: #base, per_level_above_first: #per_level_above_first } }
            }
            LevelBasedValueTypedJson::Lookup { values, fallback } => {
                let fallback = generate_level_based_value_ref(prefix, fallback, statics, counter);
                quote! { LevelBasedValue::Lookup { values: &[#(#values),*], fallback: #fallback } }
            }
        },
    }
}

pub(super) fn generate_value_effect(
    prefix: &str,
    effect: &ValueEffectJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    match effect {
        ValueEffectJson::Add { value } => {
            let value = generate_level_based_value_ref(prefix, value, statics, counter);
            quote! { EnchantmentValueEffect::Add { value: #value } }
        }
        ValueEffectJson::Set { value } => {
            let value = generate_level_based_value_ref(prefix, value, statics, counter);
            quote! { EnchantmentValueEffect::Set { value: #value } }
        }
        ValueEffectJson::Multiply { factor } => {
            let factor = generate_level_based_value_ref(prefix, factor, statics, counter);
            quote! { EnchantmentValueEffect::Multiply { factor: #factor } }
        }
        ValueEffectJson::RemoveBinomial { chance } => {
            let chance = generate_level_based_value_ref(prefix, chance, statics, counter);
            quote! { EnchantmentValueEffect::RemoveBinomial { chance: #chance } }
        }
    }
}

pub(super) fn entity_target_token(entity: &EntityTargetJson) -> TokenStream {
    match entity {
        EntityTargetJson::This => quote! { EnchantmentEntityTarget::This },
        EntityTargetJson::Attacker => quote! { EnchantmentEntityTarget::Attacker },
        EntityTargetJson::DirectAttacker => quote! { EnchantmentEntityTarget::DirectAttacker },
    }
}

pub(super) fn entity_type_predicate_token(predicate: &EntityTypePredicateJson) -> TokenStream {
    match predicate {
        EntityTypePredicateJson::Any => quote! { EntityTypePredicate::Any },
        EntityTypePredicateJson::Type(entity_type) => {
            let entity_type = identifier_token(entity_type);
            quote! { EntityTypePredicate::Type(#entity_type) }
        }
        EntityTypePredicateJson::Tag(tag) => {
            let tag = identifier_token(tag);
            quote! { EntityTypePredicate::Tag(#tag) }
        }
    }
}

pub(super) fn game_type_token(game_type: GameType) -> TokenStream {
    match game_type {
        GameType::Survival => quote! { GameType::Survival },
        GameType::Creative => quote! { GameType::Creative },
        GameType::Adventure => quote! { GameType::Adventure },
        GameType::Spectator => quote! { GameType::Spectator },
    }
}

pub(super) fn entity_vehicle_predicate_token(
    predicate: &EntityVehiclePredicateJson,
) -> TokenStream {
    match predicate {
        EntityVehiclePredicateJson::Any => quote! { EntityVehiclePredicate::Any },
        EntityVehiclePredicateJson::Present => quote! { EntityVehiclePredicate::Present },
        EntityVehiclePredicateJson::Unsupported => quote! { EntityVehiclePredicate::Unsupported },
    }
}

fn optional_bool_token(value: Option<bool>) -> TokenStream {
    if let Some(value) = value {
        quote! { Some(#value) }
    } else {
        quote! { None }
    }
}

pub(super) fn entity_flags_predicate_token(predicate: &EntityFlagsPredicateJson) -> TokenStream {
    let is_on_ground = optional_bool_token(predicate.is_on_ground);
    let is_flying = optional_bool_token(predicate.is_flying);
    let is_fall_flying = optional_bool_token(predicate.is_fall_flying);
    let is_in_water = optional_bool_token(predicate.is_in_water);
    let unsupported = predicate.unsupported;

    quote! {
        EntityFlagsPredicate {
            is_on_ground: #is_on_ground,
            is_flying: #is_flying,
            is_fall_flying: #is_fall_flying,
            is_in_water: #is_in_water,
            unsupported: #unsupported,
        }
    }
}

pub(super) fn entity_type_specific_predicate_token(
    predicate: &EntityTypeSpecificPredicateJson,
) -> TokenStream {
    match predicate {
        EntityTypeSpecificPredicateJson::Any => quote! { EntityTypeSpecificPredicate::Any },
        EntityTypeSpecificPredicateJson::Player(player) => {
            let game_modes = player.game_modes.iter().copied().map(game_type_token);
            let food_level_min = if let Some(min) = player.food_level_min {
                quote! { Some(#min) }
            } else {
                quote! { None }
            };
            let unsupported = player.unsupported;
            quote! {
                EntityTypeSpecificPredicate::Player(PlayerPredicate {
                    game_modes: &[#(#game_modes),*],
                    food_level_min: #food_level_min,
                    unsupported: #unsupported,
                })
            }
        }
        EntityTypeSpecificPredicateJson::Unsupported => {
            quote! { EntityTypeSpecificPredicate::Unsupported }
        }
    }
}

fn particle_position_source_token(source: &ParticlePositionSourceJson) -> TokenStream {
    let source_type = if source.in_bounding_box {
        quote! { ParticlePositionSourceType::InBoundingBox }
    } else {
        quote! { ParticlePositionSourceType::EntityPosition }
    };
    let offset = source.offset;
    let scale = source.scale;
    quote! {
        ParticlePositionSource {
            source_type: #source_type,
            offset: #offset,
            scale: #scale,
        }
    }
}

fn particle_velocity_source_token(source: &ParticleVelocitySourceJson) -> TokenStream {
    let movement_scale = source.movement_scale;
    let base = generate_float_provider(source.base);
    quote! {
        ParticleVelocitySource {
            movement_scale: #movement_scale,
            base: #base,
        }
    }
}

fn double_bounds_token([min, max]: [Option<f64>; 2]) -> TokenStream {
    let bound =
        |value: Option<f64>| value.map_or_else(|| quote! { None }, |value| quote! { Some(#value) });
    let min = bound(min);
    let max = bound(max);
    quote! { EnchantmentDoubleBounds { min: #min, max: #max } }
}

fn movement_predicate_token(predicate: Option<&MovementPredicateJson>) -> TokenStream {
    let Some(predicate) = predicate else {
        return quote! { None };
    };
    let x = double_bounds_token(predicate.x);
    let y = double_bounds_token(predicate.y);
    let z = double_bounds_token(predicate.z);
    let speed = double_bounds_token(predicate.speed);
    let horizontal_speed = double_bounds_token(predicate.horizontal_speed);
    let vertical_speed = double_bounds_token(predicate.vertical_speed);
    let fall_distance = double_bounds_token(predicate.fall_distance);
    quote! {
        Some(&EntityMovementPredicate {
            x: #x,
            y: #y,
            z: #z,
            speed: #speed,
            horizontal_speed: #horizontal_speed,
            vertical_speed: #vertical_speed,
            fall_distance: #fall_distance,
        })
    }
}

fn movement_affected_by_token(predicate: &MovementAffectedByJson) -> TokenStream {
    match predicate {
        MovementAffectedByJson::Any => quote! { MovementAffectedByPredicate::Any },
        MovementAffectedByJson::Blocks(ItemHolderSetJson::Tag(tag)) => {
            let tag = identifier_token(tag);
            quote! { MovementAffectedByPredicate::Blocks(EnchantmentBlockSet::Tag(#tag)) }
        }
        MovementAffectedByJson::Blocks(ItemHolderSetJson::Direct(blocks)) => {
            let blocks = blocks.iter().map(identifier_token);
            quote! {
                MovementAffectedByPredicate::Blocks(EnchantmentBlockSet::Direct(&[#(#blocks),*]))
            }
        }
        MovementAffectedByJson::Unsupported => quote! { MovementAffectedByPredicate::Unsupported },
    }
}

pub(super) fn entity_predicate_token(predicate: &EntityPredicateJson) -> TokenStream {
    let entity_type = entity_type_predicate_token(&predicate.entity_type);
    let vehicle = entity_vehicle_predicate_token(&predicate.vehicle);
    let flags = entity_flags_predicate_token(&predicate.flags);
    let movement = movement_predicate_token(predicate.movement.as_deref());
    let movement_affected_by = movement_affected_by_token(&predicate.movement_affected_by);
    let periodic_tick = predicate
        .periodic_tick
        .map_or_else(|| quote! { None }, |period| quote! { Some(#period) });
    let type_specific = entity_type_specific_predicate_token(&predicate.type_specific);
    let unsupported = predicate.unsupported;
    quote! {
        EntityPredicate {
            entity_type: #entity_type,
            vehicle: #vehicle,
            flags: #flags,
            movement: #movement,
            movement_affected_by: #movement_affected_by,
            periodic_tick: #periodic_tick,
            type_specific: #type_specific,
            unsupported: #unsupported,
        }
    }
}

pub(super) fn damage_source_predicate_token(predicate: &DamageSourcePredicateJson) -> TokenStream {
    let tags = predicate.tags.iter().map(|tag| {
        let tag_id = identifier_token(&tag.tag);
        let expected = tag.expected;
        quote! {
            DamageSourceTagPredicate {
                tag: #tag_id,
                expected: #expected,
            }
        }
    });
    let is_direct = if let Some(is_direct) = predicate.is_direct {
        quote! { Some(#is_direct) }
    } else {
        quote! { None }
    };

    quote! { DamageSourcePredicate { tags: &[#(#tags),*], is_direct: #is_direct } }
}

pub(super) fn enchantment_target_token(target: &EnchantmentTargetJson) -> TokenStream {
    match target {
        EnchantmentTargetJson::Attacker => quote! { EnchantmentTarget::Attacker },
        EnchantmentTargetJson::DamagingEntity => quote! { EnchantmentTarget::DamagingEntity },
        EnchantmentTargetJson::Victim => quote! { EnchantmentTarget::Victim },
    }
}

pub(super) fn mob_effect_selection_token(selection: &MobEffectSelectionJson) -> TokenStream {
    match selection {
        MobEffectSelectionJson::Single(effect) => {
            let effect = mob_effect_ref_token(effect);
            quote! { MobEffectSelection::Single(#effect) }
        }
        MobEffectSelectionJson::UnsupportedTag(tag) => {
            let tag = identifier_token(tag);
            quote! { MobEffectSelection::UnsupportedTag(#tag) }
        }
    }
}

pub(super) fn generate_entity_effect_ref(
    prefix: &str,
    effect: &EntityEffectJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let ident = Ident::new(
        &format!("{prefix}_ENTITY_EFFECT_{}", *counter),
        Span::call_site(),
    );
    *counter += 1;
    let effect = generate_entity_effect(prefix, effect, statics, counter);

    statics.extend(quote! {
        static #ident: EnchantmentEntityEffect = #effect;
    });

    quote! { &#ident }
}

pub(super) fn generate_entity_effect(
    prefix: &str,
    effect: &EntityEffectJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    match effect {
        EntityEffectJson::AllOf(effects) => {
            let effects = effects
                .iter()
                .map(|effect| generate_entity_effect_ref(prefix, effect, statics, counter));
            quote! { EnchantmentEntityEffect::AllOf(&[#(#effects),*]) }
        }
        EntityEffectJson::ChangeItemDamage { amount } => {
            let amount = generate_level_based_value_ref(prefix, amount, statics, counter);
            quote! { EnchantmentEntityEffect::ChangeItemDamage { amount: #amount } }
        }
        EntityEffectJson::ApplyExhaustion { amount } => {
            let amount = generate_level_based_value_ref(prefix, amount, statics, counter);
            quote! { EnchantmentEntityEffect::ApplyExhaustion { amount: #amount } }
        }
        EntityEffectJson::ApplyImpulse {
            direction,
            coordinate_scale,
            magnitude,
        } => {
            let [direction_x, direction_y, direction_z] = *direction;
            let [scale_x, scale_y, scale_z] = *coordinate_scale;
            let magnitude = generate_level_based_value_ref(prefix, magnitude, statics, counter);
            quote! {
                EnchantmentEntityEffect::ApplyImpulse {
                    direction: DVec3::new(#direction_x, #direction_y, #direction_z),
                    coordinate_scale: DVec3::new(#scale_x, #scale_y, #scale_z),
                    magnitude: #magnitude,
                }
            }
        }
        EntityEffectJson::PlaySound {
            sounds,
            volume,
            pitch,
        } => {
            let sounds = sounds.iter().map(generate_sound_event_ref);
            let volume = generate_float_provider(*volume);
            let pitch = generate_float_provider(*pitch);
            quote! {
                EnchantmentEntityEffect::PlaySound {
                    sounds: &[#(#sounds),*],
                    volume: #volume,
                    pitch: #pitch,
                }
            }
        }
        EntityEffectJson::SpawnParticles {
            particle,
            horizontal_position,
            vertical_position,
            horizontal_velocity,
            vertical_velocity,
            speed,
        } => {
            assert_eq!(
                particle.namespace.as_ref(),
                "minecraft",
                "vanilla enchantment particles must use the minecraft namespace: {particle}"
            );
            let particle = Ident::new(&particle.path.to_shouty_snake_case(), Span::call_site());
            let horizontal_position = particle_position_source_token(horizontal_position);
            let vertical_position = particle_position_source_token(vertical_position);
            let horizontal_velocity = particle_velocity_source_token(horizontal_velocity);
            let vertical_velocity = particle_velocity_source_token(vertical_velocity);
            let speed = generate_float_provider(*speed);
            quote! {
                EnchantmentEntityEffect::SpawnParticles {
                    particle: &vanilla_particle_types::#particle,
                    horizontal_position: #horizontal_position,
                    vertical_position: #vertical_position,
                    horizontal_velocity: #horizontal_velocity,
                    vertical_velocity: #vertical_velocity,
                    speed: #speed,
                }
            }
        }
        EntityEffectJson::DamageEntity {
            min_damage,
            max_damage,
            damage_type,
        } => {
            let min_damage = generate_level_based_value_ref(prefix, min_damage, statics, counter);
            let max_damage = generate_level_based_value_ref(prefix, max_damage, statics, counter);
            let damage_type = damage_type_ref_token(damage_type);
            quote! {
                EnchantmentEntityEffect::DamageEntity {
                    min_damage: #min_damage,
                    max_damage: #max_damage,
                    damage_type: #damage_type,
                }
            }
        }
        EntityEffectJson::Ignite { duration } => {
            let duration = generate_level_based_value_ref(prefix, duration, statics, counter);
            quote! { EnchantmentEntityEffect::Ignite { duration: #duration } }
        }
        EntityEffectJson::ApplyMobEffect {
            to_apply,
            min_duration,
            max_duration,
            min_amplifier,
            max_amplifier,
        } => {
            let to_apply = mob_effect_selection_token(to_apply);
            let min_duration =
                generate_level_based_value_ref(prefix, min_duration, statics, counter);
            let max_duration =
                generate_level_based_value_ref(prefix, max_duration, statics, counter);
            let min_amplifier =
                generate_level_based_value_ref(prefix, min_amplifier, statics, counter);
            let max_amplifier =
                generate_level_based_value_ref(prefix, max_amplifier, statics, counter);
            quote! {
                EnchantmentEntityEffect::ApplyMobEffect {
                    to_apply: #to_apply,
                    min_duration: #min_duration,
                    max_duration: #max_duration,
                    min_amplifier: #min_amplifier,
                    max_amplifier: #max_amplifier,
                }
            }
        }
        EntityEffectJson::ReplaceDisk {
            radius,
            height,
            offset,
            predicate,
            block_state,
            trigger_game_event,
        } => {
            let radius = generate_level_based_value_ref(prefix, radius, statics, counter);
            let height = generate_level_based_value_ref(prefix, height, statics, counter);
            let [offset_x, offset_y, offset_z] = *offset;
            let predicate = if let Some(predicate) = predicate {
                let ident = Ident::new(
                    &format!("{}_BLOCK_PREDICATE_{}", prefix, *counter).to_lowercase(),
                    Span::call_site(),
                );
                *counter += 1;
                let predicate = crate::features::generate_block_predicate(predicate);
                statics.extend(quote! {
                    fn #ident() -> &'static crate::feature::BlockPredicate {
                        #[allow(
                            unused_imports,
                            reason = "the feature predicate generator emits bare names"
                        )]
                        use crate::feature::{BlockPredicate, BlockRefList, FluidRefList};
                        #[allow(
                            unused_imports,
                            reason = "the feature predicate generator emits bare names"
                        )]
                        use crate::{vanilla_blocks, vanilla_fluids};
                        #[allow(
                            unused_imports,
                            reason = "the feature predicate generator emits bare names"
                        )]
                        use glam::IVec3;
                        static PREDICATE: std::sync::LazyLock<BlockPredicate> =
                            std::sync::LazyLock::new(|| #predicate);
                        &PREDICATE
                    }
                });
                quote! { Some(#ident) }
            } else {
                quote! { None }
            };
            let block_state = crate::features::generate_block_state_data(block_state);
            let trigger_game_event = if let Some(event) = trigger_game_event {
                assert_eq!(
                    event.namespace.as_ref(),
                    "minecraft",
                    "vanilla enchantment game events must use the minecraft namespace: {event}"
                );
                let ident = Ident::new(&event.path.to_shouty_snake_case(), Span::call_site());
                quote! { Some(&crate::vanilla_game_events::#ident) }
            } else {
                quote! { None }
            };
            quote! {
                EnchantmentEntityEffect::ReplaceDisk {
                    radius: #radius,
                    height: #height,
                    offset: glam::IVec3::new(#offset_x, #offset_y, #offset_z),
                    predicate: #predicate,
                    block_state: {
                        use crate::vanilla_blocks;
                        use crate::feature::BlockStateData;
                        #block_state
                    },
                    trigger_game_event: #trigger_game_event,
                }
            }
        }
        EntityEffectJson::Unsupported { effect_type } => {
            let effect_type = identifier_token(effect_type);
            quote! { EnchantmentEntityEffect::Unsupported { effect_type: #effect_type } }
        }
    }
}

fn generate_attribute_effect(
    prefix: &str,
    effect: &AttributeEffectJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let amount = generate_level_based_value_ref(prefix, &effect.amount, statics, counter);
    let attribute = attribute_ref_token(&effect.attribute);
    let id = identifier_token(&effect.id);
    let operation = attribute_modifier_operation_token(&effect.operation);
    quote! {
        EnchantmentAttributeEffect {
            amount: #amount,
            attribute: #attribute,
            id: #id,
            operation: #operation,
        }
    }
}

fn generate_location_effect(
    prefix: &str,
    effect: &LocationEffectJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    match effect {
        LocationEffectJson::AllOf(effects) => {
            let effects = effects.iter().map(|effect| {
                let ident = Ident::new(
                    &format!("{prefix}_LOCATION_EFFECT_{}", *counter),
                    Span::call_site(),
                );
                *counter += 1;
                let effect = generate_location_effect(prefix, effect, statics, counter);
                statics.extend(quote! {
                    static #ident: EnchantmentLocationBasedEffect = #effect;
                });
                quote! { &#ident }
            });
            let effects = effects.collect::<Vec<_>>();
            quote! { EnchantmentLocationBasedEffect::AllOf(&[#(#effects),*]) }
        }
        LocationEffectJson::Attribute(effect) => {
            let effect = generate_attribute_effect(prefix, effect, statics, counter);
            quote! { EnchantmentLocationBasedEffect::Attribute(#effect) }
        }
        LocationEffectJson::Entity(effect) => {
            let effect = generate_entity_effect_ref(prefix, effect, statics, counter);
            quote! { EnchantmentLocationBasedEffect::Entity(#effect) }
        }
    }
}

pub(super) fn generate_conditional_location_effects(
    prefix: &str,
    effects: &[ConditionalLocationEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let effect_token =
            generate_location_effect(&entry_prefix, &effect.effect, statics, counter);
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            ConditionalEnchantmentEffect {
                effect: #effect_token,
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_requirements_ref(
    prefix: &str,
    requirements: &RequirementsJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let ident = Ident::new(
        &format!("{prefix}_REQUIREMENTS_{}", *counter),
        Span::call_site(),
    );
    *counter += 1;
    let requirements = generate_requirements_value(prefix, requirements, statics, counter);

    statics.extend(quote! {
        static #ident: EnchantmentEffectRequirements = #requirements;
    });

    quote! { &#ident }
}

pub(super) fn generate_requirements_value(
    prefix: &str,
    requirements: &RequirementsJson,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    match requirements {
        RequirementsJson::AllOf(terms) => {
            let terms = terms
                .iter()
                .map(|term| generate_requirements_ref(prefix, term, statics, counter));
            quote! { EnchantmentEffectRequirements::AllOf(&[#(#terms),*]) }
        }
        RequirementsJson::AnyOf(terms) => {
            let terms = terms
                .iter()
                .map(|term| generate_requirements_ref(prefix, term, statics, counter));
            quote! { EnchantmentEffectRequirements::AnyOf(&[#(#terms),*]) }
        }
        RequirementsJson::Inverted(term) => {
            let term = generate_requirements_ref(prefix, term, statics, counter);
            quote! { EnchantmentEffectRequirements::Inverted(#term) }
        }
        RequirementsJson::EntityProperties { entity, predicate } => {
            let entity = entity_target_token(entity);
            let predicate = entity_predicate_token(predicate);
            quote! {
                EnchantmentEffectRequirements::EntityProperties {
                    entity: #entity,
                    predicate: #predicate,
                }
            }
        }
        RequirementsJson::DamageSourceProperties(predicate) => {
            let predicate = damage_source_predicate_token(predicate);
            quote! { EnchantmentEffectRequirements::DamageSourceProperties(#predicate) }
        }
        RequirementsJson::RandomChance { chance } => {
            let chance = generate_level_based_value_ref(prefix, chance, statics, counter);
            quote! { EnchantmentEffectRequirements::RandomChance { chance: #chance } }
        }
        RequirementsJson::MatchTool { items } => {
            let items = if let Some(items) = items {
                let items = match items {
                    ItemHolderSetJson::Tag(tag) => {
                        let tag = identifier_token(tag);
                        quote! { EnchantmentItemSet::Tag(#tag) }
                    }
                    ItemHolderSetJson::Direct(items) => {
                        let items = items.iter().map(identifier_token);
                        quote! { EnchantmentItemSet::Direct(&[#(#items),*]) }
                    }
                };
                quote! { Some(#items) }
            } else {
                quote! { None }
            };
            quote! { EnchantmentEffectRequirements::MatchTool { items: #items } }
        }
        RequirementsJson::EnchantmentActiveCheck { active } => {
            quote! { EnchantmentEffectRequirements::EnchantmentActiveCheck { active: #active } }
        }
        RequirementsJson::Unsupported { condition } => {
            let condition = identifier_token(condition);
            quote! { EnchantmentEffectRequirements::Unsupported { condition: #condition } }
        }
    }
}

pub(super) fn generate_optional_requirements(
    prefix: &str,
    requirements: &Option<RequirementsJson>,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    if let Some(requirements) = requirements {
        let requirements = generate_requirements_ref(prefix, requirements, statics, counter);
        quote! { Some(#requirements) }
    } else {
        quote! { None }
    }
}

pub(super) fn generate_conditional_value_effects(
    prefix: &str,
    effects: &[ConditionalValueEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let effect_token = generate_value_effect(&entry_prefix, &effect.effect, statics, counter);
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            ConditionalEnchantmentEffect {
                effect: #effect_token,
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_conditional_entity_effects(
    prefix: &str,
    effects: &[ConditionalEntityEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let effect_token = generate_entity_effect(&entry_prefix, &effect.effect, statics, counter);
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            ConditionalEnchantmentEffect {
                effect: #effect_token,
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_damage_immunity_effects(
    prefix: &str,
    effects: &[ConditionalDamageImmunityEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            ConditionalDamageImmunityEffect {
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_targeted_entity_effects(
    prefix: &str,
    effects: &[TargetedConditionalEntityEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let effect_token = generate_entity_effect(&entry_prefix, &effect.effect, statics, counter);
        let enchanted = enchantment_target_token(&effect.enchanted);
        let affected = enchantment_target_token(&effect.affected);
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            TargetedConditionalEnchantmentEffect {
                effect: #effect_token,
                enchanted: #enchanted,
                affected: #affected,
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_targeted_value_effects(
    prefix: &str,
    effects: &[TargetedConditionalValueEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = effects.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_{index}");
        let effect_token = generate_value_effect(&entry_prefix, &effect.effect, statics, counter);
        let enchanted = enchantment_target_token(&effect.enchanted);
        let requirements =
            generate_optional_requirements(&entry_prefix, &effect.requirements, statics, counter);
        quote! {
            TargetedConditionalEnchantmentEffect {
                effect: #effect_token,
                enchanted: #enchanted,
                affected: EnchantmentTarget::Victim,
                requirements: #requirements,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_attribute_effects(
    prefix: &str,
    attributes: &[AttributeEffectJson],
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    let entries = attributes.iter().enumerate().map(|(index, effect)| {
        let entry_prefix = format!("{prefix}_ATTRIBUTE_{index}");
        generate_attribute_effect(&entry_prefix, effect, statics, counter)
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_optional_value_effect(
    prefix: &str,
    effect: &Option<ValueEffectJson>,
    statics: &mut TokenStream,
    counter: &mut usize,
) -> TokenStream {
    if let Some(effect) = effect {
        let effect = generate_value_effect(prefix, effect, statics, counter);
        quote! { Some(#effect) }
    } else {
        quote! { None }
    }
}

pub(super) fn generate_crossbow_charging_sounds(
    sounds: &[CrossbowChargingSoundsJson],
) -> TokenStream {
    let entries = sounds.iter().map(|sounds| {
        let start = option_sound_event_ref_token(sounds.start.as_ref());
        let mid = option_sound_event_ref_token(sounds.mid.as_ref());
        let end = option_sound_event_ref_token(sounds.end.as_ref());
        quote! {
            CrossbowChargingSounds {
                start: #start,
                mid: #mid,
                end: #end,
            }
        }
    });

    quote! { &[#(#entries),*] }
}

pub(super) fn generate_sound_event_refs(sounds: &[Identifier]) -> TokenStream {
    let sounds = sounds.iter().map(generate_sound_event_ref);
    quote! { &[#(#sounds),*] }
}
