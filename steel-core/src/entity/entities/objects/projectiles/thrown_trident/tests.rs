use std::io::Cursor;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::read_compound as read_borrowed_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::{init_vanilla_registry, vanilla_entities};

use super::ThrownTridentEntity;
use crate::entity::Entity as _;

#[test]
fn trident_saves_and_loads_dealt_damage() {
    init_vanilla_registry();
    let mut saved = NbtCompound::new();
    saved.insert("DealtDamage", 1_i8);
    let mut bytes = Vec::new();
    saved.write(&mut bytes);
    let borrowed = read_borrowed_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("test nbt should reborrow: {error}"));

    let trident = ThrownTridentEntity::new(&vanilla_entities::TRIDENT, 1, DVec3::ZERO, Weak::new());
    assert!(!trident.dealt_damage());
    trident.load_additional((&borrowed).into());
    assert!(trident.dealt_damage(), "DealtDamage is restored");
    assert_eq!(trident.loyalty(), 0, "a plain trident has no loyalty");

    let mut nbt = NbtCompound::new();
    trident.save_additional(&mut nbt);
    assert_eq!(nbt.byte("DealtDamage"), Some(1));
}

#[test]
fn trident_slows_less_in_water_than_an_arrow() {
    use crate::entity::projectile::AbstractArrow as _;
    init_vanilla_registry();
    let trident = ThrownTridentEntity::new(&vanilla_entities::TRIDENT, 1, DVec3::ZERO, Weak::new());
    assert!((trident.get_water_inertia() - 0.99).abs() < f32::EPSILON);
}
