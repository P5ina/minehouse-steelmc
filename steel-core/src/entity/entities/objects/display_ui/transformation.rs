//! Vanilla `com.mojang.math.Transformation` as display entities persist it,
//! with the `MatrixUtil.svdDecompose` that splits a raw matrix into its parts.
//!
//! The matrix arithmetic follows `JOML` 1.10 operation by operation (no fused
//! multiply-add, the same evaluation order), so a matrix written by hand in a
//! command decomposes to the same floats vanilla gets.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use std::mem;

use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::entity_data::{Quaternionf, Vector3f};
use steel_utils::nbt::{NbtNumeric, nbt_collection_values};

/// Vanilla `Transformation`, kept in its decomposed form.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transformation {
    /// Translation applied last.
    pub translation: Vector3f,
    /// Rotation applied before scaling.
    pub left_rotation: Quaternionf,
    /// Per-axis scale.
    pub scale: Vector3f,
    /// Rotation applied first.
    pub right_rotation: Quaternionf,
}

impl Transformation {
    /// Vanilla `Transformation.IDENTITY`.
    pub const IDENTITY: Self = Self {
        translation: Vector3f::ZERO,
        left_rotation: Quaternionf::IDENTITY,
        scale: Vector3f::new(1.0, 1.0, 1.0),
        right_rotation: Quaternionf::IDENTITY,
    };

    /// Decodes vanilla `Transformation.EXTENDED_CODEC`: the decomposed record,
    /// or else a row-major list of 16 matrix entries.
    #[must_use]
    pub fn decode(tag: &NbtTag) -> Option<Self> {
        Self::decode_record(tag).or_else(|| Self::decode_matrix(tag))
    }

    /// Encodes with vanilla `Transformation.CODEC`, the form vanilla saves.
    #[must_use]
    pub fn encode(&self) -> NbtTag {
        let mut compound = NbtCompound::new();
        compound.insert("translation", vector_tag(self.translation));
        compound.insert("left_rotation", quaternion_tag(self.left_rotation));
        compound.insert("scale", vector_tag(self.scale));
        compound.insert("right_rotation", quaternion_tag(self.right_rotation));
        NbtTag::Compound(compound)
    }

    fn decode_record(tag: &NbtTag) -> Option<Self> {
        let NbtTag::Compound(compound) = tag else {
            return None;
        };
        Some(Self {
            translation: decode_vector(compound.get("translation")?)?,
            left_rotation: decode_quaternion(compound.get("left_rotation")?)?,
            scale: decode_vector(compound.get("scale")?)?,
            right_rotation: decode_quaternion(compound.get("right_rotation")?)?,
        })
    }

    /// Vanilla `ExtraCodecs.MATRIX4F` followed by `Transformation.ensureDecomposed`.
    fn decode_matrix(tag: &NbtTag) -> Option<Self> {
        let values = fixed_floats::<16>(tag)?;
        // `setRowColumn(i >> 2, i & 3)`: the list is row-major, `JOML` stores
        // `m[column][row]`.
        let mut m = [[0.0_f32; 4]; 4];
        for (index, value) in values.into_iter().enumerate() {
            m[index & 3][index >> 2] = value;
        }
        Some(svd_decompose(&m))
    }
}

/// Vanilla `MatrixUtil.svdDecompose(Matrix4fc, ...)`.
fn svd_decompose(m: &[[f32; 4]; 4]) -> Transformation {
    let scale_factor = 1.0 / m[3][3];
    let upper = Mat3 {
        m00: m[0][0],
        m01: m[0][1],
        m02: m[0][2],
        m10: m[1][0],
        m11: m[1][1],
        m12: m[1][2],
        m20: m[2][0],
        m21: m[2][1],
        m22: m[2][2],
    }
    .scale(scale_factor);
    let (left_rotation, scale, right_rotation) = svd_decompose_3(upper);
    Transformation {
        translation: Vector3f::new(
            m[3][0] * scale_factor,
            m[3][1] * scale_factor,
            m[3][2] * scale_factor,
        ),
        left_rotation: left_rotation.into(),
        scale,
        right_rotation: right_rotation.into(),
    }
}

/// Vanilla private `MatrixUtil.svdDecompose(Matrix3f, ...)`: returns `(u, s, v)`.
fn svd_decompose_3(input: Mat3) -> (Quat, Vector3f, Quat) {
    let mut scratch = input.transpose().mul(&input);
    let mut v = eigenvalue_jacobi(&mut scratch, 5);
    // Vanilla compares the float squares against the double literal `1.0E-6`.
    let zero_column_0 = f64::from(scratch.m00) < 1.0E-6;
    let zero_column_1 = f64::from(scratch.m11) < 1.0E-6;

    let rotated = input.rotate(v);
    let mut u = Quat::IDENTITY;

    let p = if zero_column_0 {
        qr_givens_quat(rotated.m11, -rotated.m10)
    } else {
        qr_givens_quat(rotated.m00, rotated.m01)
    };
    u = u.mul(p.around_z_quat());
    let after_z = p.around_z_mat().transpose().mul(&rotated);

    let p = if zero_column_0 {
        qr_givens_quat(after_z.m22, -after_z.m20)
    } else {
        qr_givens_quat(after_z.m00, after_z.m02)
    }
    .inverse();
    u = u.mul(p.around_y_quat());
    let after_y = p.around_y_mat().transpose().mul(&after_z);

    let p = if zero_column_1 {
        qr_givens_quat(after_y.m22, -after_y.m21)
    } else {
        qr_givens_quat(after_y.m11, after_y.m12)
    };
    u = u.mul(p.around_x_quat());
    let diagonal = p.around_x_mat().transpose().mul(&after_y);

    v = v.conjugate();
    (
        u,
        Vector3f::new(diagonal.m00, diagonal.m11, diagonal.m22),
        v,
    )
}

/// Vanilla `MatrixUtil.eigenvalueJacobi`.
fn eigenvalue_jacobi(in_out: &mut Mat3, steps: usize) -> Quat {
    let mut result = Quat::IDENTITY;
    for _ in 0..steps {
        step_jacobi(in_out, &mut result);
    }
    result.normalize()
}

/// Vanilla `MatrixUtil.stepJacobi`.
#[expect(
    clippy::manual_midpoint,
    reason = "vanilla averages as `0.5 * (a + b)` in float; `f32::midpoint` rounds differently"
)]
fn step_jacobi(m: &mut Mat3, output: &mut Quat) {
    if m.m01 * m.m01 + m.m10 * m.m10 > 1.0E-6 {
        let p = approx_givens_quat(m.m00, 0.5 * (m.m01 + m.m10), m.m11);
        *output = output.mul(p.around_z_quat());
        *m = similarity_transform(m, p.around_z_mat());
    }

    if m.m02 * m.m02 + m.m20 * m.m20 > 1.0E-6 {
        let p = approx_givens_quat(m.m00, 0.5 * (m.m02 + m.m20), m.m22).inverse();
        *output = output.mul(p.around_y_quat());
        *m = similarity_transform(m, p.around_y_mat());
    }

    if m.m12 * m.m12 + m.m21 * m.m21 > 1.0E-6 {
        let p = approx_givens_quat(m.m11, 0.5 * (m.m12 + m.m21), m.m22);
        *output = output.mul(p.around_x_quat());
        *m = similarity_transform(m, p.around_x_mat());
    }
}

/// Vanilla `MatrixUtil.similarityTransform`: `qᵀ · a · q`.
fn similarity_transform(a: &Mat3, q: Mat3) -> Mat3 {
    let aq = a.mul(&q);
    q.transpose().mul(&aq)
}

/// Vanilla `MatrixUtil.approxGivensQuat`.
fn approx_givens_quat(a11: f32, a12: f32, a22: f32) -> GivensParameters {
    // `MatrixUtil.G = 3 + 2·√2`, computed in float.
    let g = 3.0_f32 + 2.0 * java_sqrt(2.0);
    let ch = 2.0 * (a11 - a22);
    if g * a12 * a12 < ch * ch {
        GivensParameters::from_unnormalized(a12, ch)
    } else {
        // `MatrixUtil.PI_4`.
        GivensParameters::from_positive_angle(FRAC_PI_4 as f32)
    }
}

/// Vanilla `MatrixUtil.qrGivensQuat`.
fn qr_givens_quat(a1: f32, a2: f32) -> GivensParameters {
    let p = f64::from(a1).hypot(f64::from(a2)) as f32;
    let mut sh = if p > 1.0E-6 { a2 } else { 0.0 };
    let mut ch = a1.abs() + java_max(p, 1.0E-6);
    if a1 < 0.0 {
        mem::swap(&mut sh, &mut ch);
    }
    GivensParameters::from_unnormalized(sh, ch)
}

/// Vanilla `GivensParameters`: a rotation stored as half-angle sine and cosine.
#[derive(Debug, Clone, Copy)]
struct GivensParameters {
    sin_half: f32,
    cos_half: f32,
}

impl GivensParameters {
    fn from_unnormalized(sin_half: f32, cos_half: f32) -> Self {
        let w = joml_invsqrt(sin_half * sin_half + cos_half * cos_half);
        Self {
            sin_half: w * sin_half,
            cos_half: w * cos_half,
        }
    }

    fn from_positive_angle(angle: f32) -> Self {
        let sin = java_sin(angle / 2.0);
        Self {
            sin_half: sin,
            cos_half: joml_cos_from_sin(sin, angle / 2.0),
        }
    }

    const fn inverse(self) -> Self {
        Self {
            sin_half: -self.sin_half,
            cos_half: self.cos_half,
        }
    }

    fn cos(self) -> f32 {
        self.cos_half * self.cos_half - self.sin_half * self.sin_half
    }

    fn sin(self) -> f32 {
        2.0 * self.sin_half * self.cos_half
    }

    const fn around_x_quat(self) -> Quat {
        Quat::new(self.sin_half, 0.0, 0.0, self.cos_half)
    }

    const fn around_y_quat(self) -> Quat {
        Quat::new(0.0, self.sin_half, 0.0, self.cos_half)
    }

    const fn around_z_quat(self) -> Quat {
        Quat::new(0.0, 0.0, self.sin_half, self.cos_half)
    }

    fn around_x_mat(self) -> Mat3 {
        let (c, s) = (self.cos(), self.sin());
        Mat3 {
            m00: 1.0,
            m01: 0.0,
            m02: 0.0,
            m10: 0.0,
            m11: c,
            m12: s,
            m20: 0.0,
            m21: -s,
            m22: c,
        }
    }

    fn around_y_mat(self) -> Mat3 {
        let (c, s) = (self.cos(), self.sin());
        Mat3 {
            m00: c,
            m01: 0.0,
            m02: -s,
            m10: 0.0,
            m11: 1.0,
            m12: 0.0,
            m20: s,
            m21: 0.0,
            m22: c,
        }
    }

    fn around_z_mat(self) -> Mat3 {
        let (c, s) = (self.cos(), self.sin());
        Mat3 {
            m00: c,
            m01: s,
            m02: 0.0,
            m10: -s,
            m11: c,
            m12: 0.0,
            m20: 0.0,
            m21: 0.0,
            m22: 1.0,
        }
    }
}

/// `JOML` `Matrix3f`; `mCR` is column `C`, row `R`.
#[derive(Debug, Clone, Copy)]
struct Mat3 {
    m00: f32,
    m01: f32,
    m02: f32,
    m10: f32,
    m11: f32,
    m12: f32,
    m20: f32,
    m21: f32,
    m22: f32,
}

impl Mat3 {
    /// `Matrix3f.mul(right)`: `this · right`.
    fn mul(&self, r: &Self) -> Self {
        Self {
            m00: self.m00 * r.m00 + (self.m10 * r.m01 + self.m20 * r.m02),
            m01: self.m01 * r.m00 + (self.m11 * r.m01 + self.m21 * r.m02),
            m02: self.m02 * r.m00 + (self.m12 * r.m01 + self.m22 * r.m02),
            m10: self.m00 * r.m10 + (self.m10 * r.m11 + self.m20 * r.m12),
            m11: self.m01 * r.m10 + (self.m11 * r.m11 + self.m21 * r.m12),
            m12: self.m02 * r.m10 + (self.m12 * r.m11 + self.m22 * r.m12),
            m20: self.m00 * r.m20 + (self.m10 * r.m21 + self.m20 * r.m22),
            m21: self.m01 * r.m20 + (self.m11 * r.m21 + self.m21 * r.m22),
            m22: self.m02 * r.m20 + (self.m12 * r.m21 + self.m22 * r.m22),
        }
    }

    const fn transpose(&self) -> Self {
        Self {
            m00: self.m00,
            m01: self.m10,
            m02: self.m20,
            m10: self.m01,
            m11: self.m11,
            m12: self.m21,
            m20: self.m02,
            m21: self.m12,
            m22: self.m22,
        }
    }

    /// `Matrix3f.scale(float)`.
    fn scale(&self, factor: f32) -> Self {
        Self {
            m00: self.m00 * factor,
            m01: self.m01 * factor,
            m02: self.m02 * factor,
            m10: self.m10 * factor,
            m11: self.m11 * factor,
            m12: self.m12 * factor,
            m20: self.m20 * factor,
            m21: self.m21 * factor,
            m22: self.m22 * factor,
        }
    }

    /// `Matrix3f.rotate(Quaternionfc)`: `this · R(q)`.
    fn rotate(&self, q: Quat) -> Self {
        let w2 = q.w * q.w;
        let x2 = q.x * q.x;
        let y2 = q.y * q.y;
        let z2 = q.z * q.z;
        let zw = q.z * q.w;
        let dzw = zw + zw;
        let xy = q.x * q.y;
        let dxy = xy + xy;
        let xz = q.x * q.z;
        let dxz = xz + xz;
        let yw = q.y * q.w;
        let dyw = yw + yw;
        let yz = q.y * q.z;
        let dyz = yz + yz;
        let xw = q.x * q.w;
        let dxw = xw + xw;
        let rm00 = w2 + x2 - z2 - y2;
        let rm01 = dxy + dzw;
        let rm02 = dxz - dyw;
        let rm10 = dxy - dzw;
        let rm11 = y2 - z2 + w2 - x2;
        let rm12 = dyz + dxw;
        let rm20 = dyw + dxz;
        let rm21 = dyz - dxw;
        let rm22 = z2 - y2 - x2 + w2;
        Self {
            m00: self.m00 * rm00 + self.m10 * rm01 + self.m20 * rm02,
            m01: self.m01 * rm00 + self.m11 * rm01 + self.m21 * rm02,
            m02: self.m02 * rm00 + self.m12 * rm01 + self.m22 * rm02,
            m10: self.m00 * rm10 + self.m10 * rm11 + self.m20 * rm12,
            m11: self.m01 * rm10 + self.m11 * rm11 + self.m21 * rm12,
            m12: self.m02 * rm10 + self.m12 * rm11 + self.m22 * rm12,
            m20: self.m00 * rm20 + self.m10 * rm21 + self.m20 * rm22,
            m21: self.m01 * rm20 + self.m11 * rm21 + self.m21 * rm22,
            m22: self.m02 * rm20 + self.m12 * rm21 + self.m22 * rm22,
        }
    }
}

/// `JOML` `Quaternionf` arithmetic.
#[derive(Debug, Clone, Copy)]
struct Quat {
    x: f32,
    y: f32,
    z: f32,
    w: f32,
}

impl Quat {
    const IDENTITY: Self = Self::new(0.0, 0.0, 0.0, 1.0);

    const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// `Quaternionf.mul(q)`: `this · q`.
    fn mul(self, q: Self) -> Self {
        Self {
            x: self.w * q.x + (self.x * q.w + (self.y * q.z + -self.z * q.y)),
            y: self.w * q.y + (-self.x * q.z + (self.y * q.w + self.z * q.x)),
            z: self.w * q.z + (self.x * q.y + (-self.y * q.x + self.z * q.w)),
            w: self.w * q.w + (-self.x * q.x + (-self.y * q.y + -self.z * q.z)),
        }
    }

    fn normalize(self) -> Self {
        let inv_norm =
            joml_invsqrt(self.x * self.x + (self.y * self.y + (self.z * self.z + self.w * self.w)));
        Self {
            x: self.x * inv_norm,
            y: self.y * inv_norm,
            z: self.z * inv_norm,
            w: self.w * inv_norm,
        }
    }

    const fn conjugate(self) -> Self {
        Self::new(-self.x, -self.y, -self.z, self.w)
    }
}

impl From<Quat> for Quaternionf {
    fn from(q: Quat) -> Self {
        Self::new(q.x, q.y, q.z, q.w)
    }
}

/// `JOML` `Math.invsqrt(float)`.
fn joml_invsqrt(r: f32) -> f32 {
    1.0 / java_sqrt(r)
}

/// `JOML` `Math.sqrt(float)`: `(float) java.lang.Math.sqrt(r)`.
fn java_sqrt(r: f32) -> f32 {
    f64::from(r).sqrt() as f32
}

/// `JOML` `Math.sin(float)`: `(float) java.lang.Math.sin(rad)`.
fn java_sin(rad: f32) -> f32 {
    f64::from(rad).sin() as f32
}

/// `JOML` `Math.cosFromSin(float, float)` without fast math.
fn joml_cos_from_sin(sin: f32, angle: f32) -> f32 {
    let two_pi = TAU as f32;
    let cos = java_sqrt(1.0 - sin * sin);
    let a = angle + FRAC_PI_2 as f32;
    let mut b = a - ((a / two_pi) as i32) as f32 * two_pi;
    if b < 0.0 {
        b += two_pi;
    }
    if b >= PI as f32 { -cos } else { cos }
}

/// `java.lang.Math.max(float, float)`, which propagates `NaN`.
const fn java_max(a: f32, b: f32) -> f32 {
    if a.is_nan() { a } else { a.max(b) }
}

/// Decodes `Codec.FLOAT.listOf()` constrained by `Util.fixedSize`.
fn fixed_floats<const N: usize>(tag: &NbtTag) -> Option<[f32; N]> {
    let values = nbt_collection_values(tag)?;
    let floats: Vec<f32> = values
        .iter()
        .map(NbtNumeric::codec_f32)
        .collect::<Option<_>>()?;
    floats.try_into().ok()
}

/// Vanilla `ExtraCodecs.VECTOR3F`.
fn decode_vector(tag: &NbtTag) -> Option<Vector3f> {
    let [x, y, z] = fixed_floats::<3>(tag)?;
    Some(Vector3f::new(x, y, z))
}

/// Vanilla `ExtraCodecs.QUATERNIONF`: normalized `[x, y, z, w]` components,
/// or else an `{angle, axis}` axis-angle.
fn decode_quaternion(tag: &NbtTag) -> Option<Quaternionf> {
    if let Some([x, y, z, w]) = fixed_floats::<4>(tag) {
        return Some(Quat::new(x, y, z, w).normalize().into());
    }
    let NbtTag::Compound(compound) = tag else {
        return None;
    };
    let angle = compound.get("angle")?.codec_f32()?;
    let axis = decode_vector(compound.get("axis")?)?;
    Some(axis_angle_quaternion(angle, axis))
}

/// `new Quaternionf(new AxisAngle4f(angle, axis))`; the axis is not normalized.
fn axis_angle_quaternion(angle: f32, axis: Vector3f) -> Quaternionf {
    let two_pi = TAU as f32;
    // `AxisAngle4f` wraps the angle into `[0, 2π)`.
    let angle = if f64::from(angle) < 0.0 {
        two_pi + angle % two_pi
    } else {
        angle
    } % two_pi;
    let half = angle * 0.5;
    let sin = java_sin(half);
    let cos = joml_cos_from_sin(sin, half);
    Quaternionf::new(axis.x * sin, axis.y * sin, axis.z * sin, cos)
}

fn vector_tag(vector: Vector3f) -> NbtTag {
    NbtTag::List(NbtList::Float(vec![vector.x, vector.y, vector.z]))
}

fn quaternion_tag(quaternion: Quaternionf) -> NbtTag {
    NbtTag::List(NbtList::Float(vec![
        quaternion.x,
        quaternion.y,
        quaternion.z,
        quaternion.w,
    ]))
}

#[cfg(test)]
mod tests {
    use std::f32::consts as f32_consts;

    use super::*;

    fn floats(values: &[f32]) -> NbtTag {
        NbtTag::List(NbtList::Float(values.to_vec()))
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0E-5,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn record_form_normalizes_component_quaternions_and_round_trips() {
        let mut compound = NbtCompound::new();
        compound.insert("translation", floats(&[1.0, 2.0, 3.0]));
        compound.insert("left_rotation", floats(&[0.0, 0.0, 0.0, 2.0]));
        compound.insert("scale", floats(&[2.0, 2.0, 2.0]));
        compound.insert("right_rotation", NbtTag::IntArray(vec![0, 3, 0, 4]));
        let decoded = Transformation::decode(&NbtTag::Compound(compound))
            .expect("the decomposed record should decode");

        assert_eq!(decoded.translation, Vector3f::new(1.0, 2.0, 3.0));
        assert_eq!(decoded.left_rotation, Quaternionf::IDENTITY);
        assert_eq!(decoded.right_rotation, Quaternionf::new(0.0, 0.6, 0.0, 0.8));
        assert_eq!(Transformation::decode(&decoded.encode()), Some(decoded));
    }

    #[test]
    fn axis_angle_rotations_keep_the_axis_unnormalized() {
        let mut axis_angle = NbtCompound::new();
        axis_angle.insert("angle", f32_consts::PI);
        axis_angle.insert("axis", floats(&[0.0, 2.0, 0.0]));
        let rotation =
            decode_quaternion(&NbtTag::Compound(axis_angle)).expect("axis-angle should decode");

        assert_close(rotation.x, 0.0);
        assert_close(rotation.y, 2.0);
        assert_close(rotation.z, 0.0);
        assert_close(rotation.w, 0.0);
    }

    #[test]
    fn incomplete_records_and_wrong_sizes_are_rejected() {
        let mut compound = NbtCompound::new();
        compound.insert("translation", floats(&[1.0, 2.0, 3.0]));
        compound.insert("left_rotation", floats(&[0.0, 0.0, 0.0, 1.0]));
        compound.insert("scale", floats(&[1.0, 1.0]));
        compound.insert("right_rotation", floats(&[0.0, 0.0, 0.0, 1.0]));

        assert_eq!(Transformation::decode(&NbtTag::Compound(compound)), None);
        assert_eq!(Transformation::decode(&floats(&[1.0; 15])), None);
    }

    #[test]
    fn matrix_form_decomposes_translation_rotation_and_scale() {
        // Row-major: a 90° turn about Y, scaled by 2, then moved by (1, 2, 3).
        let matrix = floats(&[
            0.0, 0.0, 2.0, 1.0, //
            0.0, 2.0, 0.0, 2.0, //
            -2.0, 0.0, 0.0, 3.0, //
            0.0, 0.0, 0.0, 1.0,
        ]);
        let decoded = Transformation::decode(&matrix).expect("16 floats should decode");

        assert_eq!(decoded.translation, Vector3f::new(1.0, 2.0, 3.0));
        assert_close(decoded.scale.x, 2.0);
        assert_close(decoded.scale.y, 2.0);
        assert_close(decoded.scale.z, 2.0);

        // The two rotations together must rebuild the original turn.
        let left = Quat::new(
            decoded.left_rotation.x,
            decoded.left_rotation.y,
            decoded.left_rotation.z,
            decoded.left_rotation.w,
        );
        let right = Quat::new(
            decoded.right_rotation.x,
            decoded.right_rotation.y,
            decoded.right_rotation.z,
            decoded.right_rotation.w,
        );
        let combined = left.mul(right);
        let half = f32_consts::FRAC_PI_4;
        let sign = combined.w.signum();
        assert_close(combined.x * sign, 0.0);
        assert_close(combined.y * sign, half.sin());
        assert_close(combined.z * sign, 0.0);
        assert_close(combined.w * sign, half.cos());
    }

    #[test]
    fn matrix_form_divides_by_the_homogeneous_weight() {
        let matrix = floats(&[
            2.0, 0.0, 0.0, 4.0, //
            0.0, 2.0, 0.0, 6.0, //
            0.0, 0.0, 2.0, 8.0, //
            0.0, 0.0, 0.0, 2.0,
        ]);
        let decoded = Transformation::decode(&matrix).expect("16 floats should decode");

        assert_eq!(decoded.translation, Vector3f::new(2.0, 3.0, 4.0));
        assert_close(decoded.scale.x, 1.0);
        assert_close(decoded.scale.y, 1.0);
        assert_close(decoded.scale.z, 1.0);
    }
}
