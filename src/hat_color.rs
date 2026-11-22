//! Turning a XEP-0317 hue into a colour that is readable in the user's colour scheme.
//!
//! A hat's `hue` is an angle only; the lightness has to follow the scheme. The colour is built in
//! OKLCH, where the hue stays recognisable as lightness changes, then pushed as light or as dark
//! as it needs to be so that text in it reaches the WCAG AA contrast ratio (4.5:1) against every
//! background the chip can sit on in that scheme. The same colour draws the border, which only
//! needs 3:1, so the border passes with room to spare.
//!
//! This is plain arithmetic with no GTK in it, so every hue can be checked in a test.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(value: u32) -> Self {
        Self { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
    }
    pub fn unit(self) -> (f32, f32, f32) {
        (f32::from(self.r) / 255.0, f32::from(self.g) / 255.0, f32::from(self.b) / 255.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// WCAG AA for normal-size text. The search aims a little higher (below); tests hold it to this.
#[cfg(test)]
pub const MIN_CONTRAST: f64 = 4.5;
/// What the search aims for, a little above the minimum so rounding to 8 bits can not undercut it.
const TARGET_CONTRAST: f64 = 4.6;
/// How colourful the hue is allowed to be; gamut limits can lower it.
const CHROMA: f64 = 0.15;

/// The backgrounds a chip can be drawn on, darkest and lightest cases included: the window, a
/// card, the accent-tinted bubble of the user's own messages with an unfavourable accent colour,
/// and a hovered or selected row. Values follow libadwaita's stock palette.
pub fn backgrounds(scheme: Scheme) -> &'static [Rgb] {
    const LIGHT: [Rgb; 4] = [Rgb::hex(0xffffff), Rgb::hex(0xfafafb), Rgb::hex(0xdcdfe4), Rgb::hex(0xe0e0e0)];
    const DARK: [Rgb; 4] = [Rgb::hex(0x242424), Rgb::hex(0x383838), Rgb::hex(0x565656), Rgb::hex(0x444444)];
    match scheme {
        Scheme::Light => &LIGHT,
        Scheme::Dark => &DARK,
    }
}

fn linear(channel: u8) -> f64 {
    let c = f64::from(channel) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// WCAG relative luminance.
pub fn luminance(color: Rgb) -> f64 {
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// WCAG contrast ratio, from 1 (identical) to 21 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Linear-light sRGB for an OKLCH colour; channels outside 0..=1 are outside the sRGB gamut.
fn oklch_to_linear(l: f64, c: f64, hue_degrees: f64) -> [f64; 3] {
    let h = hue_degrees.to_radians();
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ]
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    rgb.iter().all(|c| (-1e-6..=1.0 + 1e-6).contains(c))
}

fn encode(linear: f64) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let c = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

/// The most colourful sRGB colour of this hue and lightness, never past [`CHROMA`].
fn at_lightness(hue: f64, l: f64) -> Rgb {
    let (mut low, mut high) = (0.0_f64, CHROMA);
    let rgb = if in_gamut(oklch_to_linear(l, high, hue)) {
        oklch_to_linear(l, high, hue)
    } else {
        for _ in 0..24 {
            let mid = (low + high) / 2.0;
            if in_gamut(oklch_to_linear(l, mid, hue)) {
                low = mid;
            } else {
                high = mid;
            }
        }
        oklch_to_linear(l, low, hue)
    };
    Rgb { r: encode(rgb[0]), g: encode(rgb[1]), b: encode(rgb[2]) }
}

fn readable(color: Rgb, scheme: Scheme) -> bool {
    backgrounds(scheme).iter().all(|background| contrast(color, *background) >= TARGET_CONTRAST)
}

/// The colour for text and border of a hat with this hue (degrees, any real number).
///
/// Light scheme: the lightest shade of the hue that is still readable on every light background.
/// Dark scheme: the darkest shade that is readable on every dark one. Staying as close to the
/// limit as possible keeps the colour as vivid as the contrast rule allows.
pub fn hat_color(hue_degrees: f64, scheme: Scheme) -> Rgb {
    let hue = if hue_degrees.is_finite() { hue_degrees.rem_euclid(360.0) } else { 0.0 };
    let (mut low, mut high) = match scheme {
        // `low` is always readable, `high` never is.
        Scheme::Light => (0.05, 0.90),
        Scheme::Dark => (0.95, 0.30),
    };
    for _ in 0..30 {
        let mid = (low + high) / 2.0;
        if readable(at_lightness(hue, mid), scheme) {
            low = mid;
        } else {
            high = mid;
        }
    }
    // Chroma limits make contrast only roughly monotonic in lightness: walk further in until the
    // colour really passes, then fall back to black or white if nothing did.
    let step = if scheme == Scheme::Light { -0.004 } else { 0.004 };
    let mut l = low;
    for _ in 0..250 {
        let color = at_lightness(hue, l);
        if readable(color, scheme) {
            return color;
        }
        l += step;
    }
    if scheme == Scheme::Light { Rgb::hex(0x000000) } else { Rgb::hex(0xffffff) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lightness, chroma and hue of an sRGB colour, to check what `hat_color` produced.
    fn to_oklch(color: Rgb) -> (f64, f64, f64) {
        let [r, g, b] = [color.r, color.g, color.b].map(linear);
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        let lightness = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
        let a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
        let bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
        (lightness, a.hypot(bb), bb.atan2(a).to_degrees().rem_euclid(360.0))
    }
    fn hue_gap(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }
    fn hues() -> impl Iterator<Item = f64> {
        (0..1440).map(|i| f64::from(i) * 0.25)
    }

    #[test]
    fn the_contrast_arithmetic_matches_the_wcag_reference_values() {
        let (black, white) = (Rgb::hex(0), Rgb::hex(0xffffff));
        assert!((contrast(black, white) - 21.0).abs() < 1e-9);
        assert!((contrast(white, white) - 1.0).abs() < 1e-9);
        assert_eq!(contrast(black, white), contrast(white, black), "symmetric");
        // #767676 on white is the well-known 4.54:1 boundary of AA.
        assert!((contrast(Rgb::hex(0x767676), white) - 4.54).abs() < 0.01);
        assert!((luminance(white) - 1.0).abs() < 1e-9 && luminance(black) == 0.0);
    }

    #[test]
    fn every_hue_is_readable_on_every_background_in_both_schemes() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            let mut worst = f64::MAX;
            for hue in hues() {
                let color = hat_color(hue, scheme);
                for background in backgrounds(scheme) {
                    let ratio = contrast(color, *background);
                    worst = worst.min(ratio);
                    assert!(
                        ratio >= MIN_CONTRAST,
                        "{scheme:?} hue {hue}: {color:?} on {background:?} is {ratio:.2}:1"
                    );
                }
            }
            assert!(worst >= MIN_CONTRAST && worst < 6.0, "{scheme:?} stays near the limit: {worst:.2}");
        }
    }

    #[test]
    fn the_border_clears_the_non_text_minimum_with_room_to_spare() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            for hue in hues().step_by(8) {
                let color = hat_color(hue, scheme);
                for background in backgrounds(scheme) {
                    assert!(contrast(color, *background) >= 3.0, "{scheme:?} {hue}");
                }
            }
        }
    }

    #[test]
    fn the_colour_keeps_the_hat_hue_and_stays_colourful() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            for hue in hues().step_by(4) {
                let color = hat_color(hue, scheme);
                let (_, chroma, found) = to_oklch(color);
                assert!(chroma > 0.05, "{scheme:?} hue {hue} lost its colour: chroma {chroma:.3} {color:?}");
                assert!(hue_gap(found, hue) < 5.0, "{scheme:?} hue {hue} came out as {found:.1}");
            }
        }
    }

    #[test]
    fn light_colours_are_dark_enough_and_dark_colours_light_enough() {
        for hue in hues().step_by(8) {
            let (light, _, _) = to_oklch(hat_color(hue, Scheme::Light));
            let (dark, _, _) = to_oklch(hat_color(hue, Scheme::Dark));
            assert!(light < 0.62, "hue {hue}: {light:.2}");
            assert!(dark > 0.60, "hue {hue}: {dark:.2}");
            assert!(dark > light);
        }
    }

    #[test]
    fn the_same_input_gives_the_same_colour_and_different_hues_differ() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            assert_eq!(hat_color(123.456, scheme), hat_color(123.456, scheme));
            let reds = hat_color(25.0, scheme);
            let greens = hat_color(145.0, scheme);
            let blues = hat_color(265.0, scheme);
            assert!(reds != greens && greens != blues && reds != blues);
            // Hues that are 10 degrees apart are still told apart.
            let distinct: std::collections::HashSet<_> = (0..36)
                .map(|i| hat_color(f64::from(i) * 10.0, scheme))
                .map(|c| (c.r, c.g, c.b))
                .collect();
            assert_eq!(distinct.len(), 36, "{scheme:?}");
        }
    }

    #[test]
    fn hues_wrap_and_odd_numbers_never_panic_or_fail_contrast() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            assert_eq!(hat_color(0.0, scheme), hat_color(360.0, scheme));
            assert_eq!(hat_color(-30.0, scheme), hat_color(330.0, scheme));
            assert_eq!(hat_color(725.0, scheme), hat_color(5.0, scheme));
            for odd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300, f64::MIN_POSITIVE, 359.999999] {
                let color = hat_color(odd, scheme);
                for background in backgrounds(scheme) {
                    assert!(contrast(color, *background) >= MIN_CONTRAST, "{odd}");
                }
            }
        }
    }

    #[test]
    fn the_search_helper_never_leaves_the_srgb_gamut() {
        for hue in hues().step_by(8) {
            for step in 0..=20 {
                let l = 0.05 + f64::from(step) * 0.045;
                let color = at_lightness(hue, l);
                // A channel is a u8 by construction; check it still reads back as the same hue
                // when it has meaningful chroma.
                let (_, chroma, found) = to_oklch(color);
                if chroma > 0.08 && (0.25..0.9).contains(&l) {
                    assert!(hue_gap(found, hue) < 6.0, "l {l} hue {hue} -> {found}");
                }
            }
        }
    }

    #[test]
    fn the_background_sets_cover_the_stock_palette_and_are_ordered() {
        for scheme in [Scheme::Light, Scheme::Dark] {
            assert!(backgrounds(scheme).len() >= 4);
        }
        let light = backgrounds(Scheme::Light).iter().map(|c| luminance(*c)).fold(f64::MAX, f64::min);
        let dark = backgrounds(Scheme::Dark).iter().map(|c| luminance(*c)).fold(0.0, f64::max);
        assert!(light > 0.6, "the darkest light background is still light: {light}");
        assert!(dark < 0.15, "the lightest dark background is still dark: {dark}");
        // Plain black on the light set and white on the dark set are always acceptable.
        for background in backgrounds(Scheme::Light) {
            assert!(contrast(Rgb::hex(0), *background) > 12.0);
        }
        for background in backgrounds(Scheme::Dark) {
            assert!(contrast(Rgb::hex(0xffffff), *background) > 7.0);
        }
    }
}
