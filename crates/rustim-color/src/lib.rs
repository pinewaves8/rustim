//! The 3b1b palette and colour helpers.
//!
//! Colours are `peniko::Color` throughout so they pass straight to the renderer
//! with no conversion step. The palette is defined here, once, as `const`s —
//! `CLAUDE.md` records that the tutorial scattered these as inline literals.

pub use peniko::Color;

/// `peniko`'s colour type is generic over colour space; the whole workspace
/// works in sRGB, so pin it here rather than at every call site.
pub type Srgb = peniko::color::AlphaColor<peniko::color::Srgb>;

const fn srgb8(r: u8, g: u8, b: u8) -> Srgb {
    peniko::color::AlphaColor::from_rgba8(r, g, b, 255)
}

/// Frame background — 3b1b's near-black, `#121212`.
pub const BG: Srgb = srgb8(0x12, 0x12, 0x12);

/// Default foreground.
pub const FG: Srgb = srgb8(0xFF, 0xFF, 0xFF);

/// Pythagorean scene: the square on leg *a*.
pub const BLUE: Srgb = srgb8(0x64, 0xB5, 0xF6);

/// Pythagorean scene: the square on leg *b*.
pub const YELLOW: Srgb = srgb8(0xFF, 0xD5, 0x4F);

/// Pythagorean scene: the square on the hypotenuse, and the triangle itself.
pub const RED: Srgb = srgb8(0xEF, 0x53, 0x50);

/// Multiply a colour's alpha, for fades. Composes multiplicatively with a
/// shape's own `fill_opacity`, which is the behaviour `CLAUDE.md` specifies.
pub fn with_alpha_factor(c: Srgb, factor: f32) -> Srgb {
    c.multiply_alpha(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_is_3b1b_black() {
        let [r, g, b, a] = BG.components;
        assert_eq!(
            (r, g, b, a),
            (
                0x12 as f32 / 255.0,
                0x12 as f32 / 255.0,
                0x12 as f32 / 255.0,
                1.0
            )
        );
    }

    #[test]
    fn alpha_factor_composes_multiplicatively() {
        let half = with_alpha_factor(BLUE, 0.5);
        let quarter = with_alpha_factor(half, 0.5);
        assert!((quarter.components[3] - 0.25).abs() < 1e-6);
    }
}
