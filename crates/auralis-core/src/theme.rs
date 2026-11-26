//! Paleta de la interfaz: tema claro/oscuro, color de acento y tinte configurables, y
//! «Ambient Light» (colores tomados de la carátula). Solo cálculo de colores: no depende
//! de ninguna interfaz, así que se prueba aquí y lo puede reutilizar otra (Android).

/// Matiz del acento por defecto (el azul-violeta original de la app).
pub const DEFAULT_HUE: u16 = 232;
/// Valor de [`identify_theme`] cuando matiz y tinte no coinciden con ningún preset.
pub const THEME_CUSTOM: u8 = u8::MAX;
/// Tinte mínimo con Ambient Light activo (para que el fondo «respire» el color).
const AMBIENT_MIN_TINT: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    fn with_alpha(self, a: u8) -> Rgba {
        Rgba {
            r: self.r,
            g: self.g,
            b: self.b,
            a,
        }
    }
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn rgb(self) -> Rgb {
        Rgb::new(self.r, self.g, self.b)
    }
}

/// Todos los colores que usa la interfaz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgba,
    pub light: Rgba,
    pub shadow: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    pub accent: Rgba,
    pub inset_top: Rgba,
    pub inset_bottom: Rgba,
    pub inset_border: Rgba,
    pub row_current: Rgba,
    pub row_hover: Rgba,
    /// Resplandor de Ambient Light (transparente si está desactivado).
    pub glow: Rgba,
    /// El mismo color con alpha 0: final de los degradados.
    pub glow_fade: Rgba,
}

// ---- Presets ----------------------------------------------------------------

/// (nombre, matiz del acento 0–359, tinte del fondo 0–100).
const PRESETS: [(&str, u16, u8); 8] = [
    ("Índigo", DEFAULT_HUE, 0),
    ("Océano", 200, 35),
    ("Menta", 165, 30),
    ("Bosque", 130, 35),
    ("Ámbar", 38, 30),
    ("Atardecer", 14, 35),
    ("Rosa", 335, 30),
    ("Violeta", 275, 35),
];

pub fn preset_count() -> usize {
    PRESETS.len()
}

pub fn preset_name(i: usize) -> Option<&'static str> {
    PRESETS.get(i).map(|p| p.0)
}

pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.0).collect()
}

/// (matiz, tinte) del preset `i`.
pub fn preset_values(i: usize) -> Option<(u16, u8)> {
    PRESETS.get(i).map(|p| (p.1, p.2))
}

/// Índice del preset que coincide con (matiz, tinte), o [`THEME_CUSTOM`].
pub fn identify_theme(hue: u16, tint: u8) -> u8 {
    PRESETS
        .iter()
        .position(|p| p.1 == hue && p.2 == tint)
        .map_or(THEME_CUSTOM, |i| i as u8)
}

// ---- Matemática de color ------------------------------------------------------

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

fn to_u8(x: f32) -> u8 {
    (clamp01(x) * 255.0).round() as u8
}

/// HSL (matiz en grados) → RGB 0–1.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let (s, l) = (clamp01(s), clamp01(l));
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

fn hsl(h: f32, s: f32, l: f32) -> Rgb {
    let (r, g, b) = hsl_to_rgb(h, s, l);
    Rgb::new(to_u8(r), to_u8(g), to_u8(b))
}

/// RGB → (matiz en grados, saturación, luminosidad) HSL.
pub fn rgb_to_hsl(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, clamp01(s), l)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = clamp01(t);
    let m = |x: u8, y: u8| to_u8((x as f32 + (y as f32 - x as f32) * t) / 255.0);
    Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

fn luminance(c: Rgb) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}

/// Relación de contraste WCAG entre dos colores (1 a 21).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

// ---- Paletas ---------------------------------------------------------------

const fn c(r: u8, g: u8, b: u8) -> Rgb {
    Rgb::new(r, g, b)
}

/// Paleta original de la app (la que ya conoces).
fn default_palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: c(0x2a, 0x2e, 0x38).with_alpha(255),
            light: c(0x37, 0x3c, 0x49).with_alpha(255),
            shadow: c(0x1a, 0x1d, 0x24).with_alpha(255),
            text: c(0xe8, 0xeb, 0xf3).with_alpha(255),
            text_dim: c(0x8f, 0x98, 0xad).with_alpha(255),
            accent: c(0x8d, 0x9b, 0xff).with_alpha(255),
            inset_top: c(0x1f, 0x22, 0x2a).with_alpha(255),
            inset_bottom: c(0x30, 0x34, 0x3f).with_alpha(255),
            inset_border: Rgba::new(255, 255, 255, 0x12),
            row_current: c(0x34, 0x3a, 0x48).with_alpha(255),
            row_hover: Rgba::new(255, 255, 255, 0x0c),
            glow: Rgba::new(0x8d, 0x9b, 0xff, 0),
            glow_fade: Rgba::new(0x8d, 0x9b, 0xff, 0),
        }
    } else {
        Palette {
            bg: c(0xe4, 0xe8, 0xef).with_alpha(255),
            light: c(0xff, 0xff, 0xff).with_alpha(255),
            shadow: c(0xb9, 0xc1, 0xd1).with_alpha(255),
            text: c(0x3b, 0x42, 0x56).with_alpha(255),
            text_dim: c(0x8c, 0x95, 0xaa).with_alpha(255),
            accent: c(0x65, 0x77, 0xf5).with_alpha(255),
            inset_top: c(0xd3, 0xd9, 0xe4).with_alpha(255),
            inset_bottom: c(0xee, 0xf1, 0xf6).with_alpha(255),
            inset_border: Rgba::new(255, 255, 255, 0x80),
            row_current: c(0xd9, 0xde, 0xea).with_alpha(255),
            row_hover: Rgba::new(0, 0, 0, 0x08),
            glow: Rgba::new(0x65, 0x77, 0xf5, 0),
            glow_fade: Rgba::new(0x65, 0x77, 0xf5, 0),
        }
    }
}

/// Superficies con el matiz `h` (versión «teñida» de la paleta original).
fn tinted_surfaces(dark: bool, h: f32) -> [Rgb; 8] {
    if dark {
        [
            hsl(h, 0.26, 0.18), // bg
            hsl(h, 0.28, 0.24), // light
            hsl(h, 0.30, 0.11), // shadow
            hsl(h, 0.30, 0.93), // text
            hsl(h, 0.14, 0.64), // text_dim
            hsl(h, 0.28, 0.14), // inset_top
            hsl(h, 0.24, 0.22), // inset_bottom
            hsl(h, 0.25, 0.24), // row_current
        ]
    } else {
        [
            hsl(h, 0.40, 0.92),
            hsl(h, 0.60, 0.995),
            hsl(h, 0.28, 0.77),
            hsl(h, 0.25, 0.28),
            hsl(h, 0.13, 0.55),
            hsl(h, 0.30, 0.85),
            hsl(h, 0.45, 0.95),
            hsl(h, 0.30, 0.88),
        ]
    }
}

/// Acento de matiz `h`; se aclara (oscuro) u oscurece (claro) hasta tener contraste ≥ 3
/// con el fondo, así un amarillo sigue leyéndose sobre blanco.
fn accent_for(dark: bool, h: f32, sat: f32, bg: Rgb) -> Rgb {
    let mut l = if dark { 0.76 } else { 0.62 };
    let mut col = hsl(h, sat, l);
    for _ in 0..30 {
        if contrast(col, bg) >= 3.0 {
            break;
        }
        l += if dark { 0.02 } else { -0.02 };
        col = hsl(h, sat, l);
    }
    col
}

/// Calcula la paleta.
///
/// * `dark`: modo base (oscuro / claro).
/// * `hue`: matiz del acento (grados); `tint`: cuánto se tiñe el fondo con ese matiz (0–1).
/// * `ambient`: color dominante de la carátula; si hay, **manda sobre** `hue` (acento, tinte
///   mínimo del 35 % y resplandor); si es `None` se usa la configuración manual.
pub fn palette(dark: bool, hue: f32, tint: f32, ambient: Option<Rgb>) -> Palette {
    let base = default_palette(dark);
    let (h, sat_override, tint) = match ambient {
        Some(col) => {
            let (h, s, _) = rgb_to_hsl(col);
            (h, Some(s.clamp(0.55, 1.0)), tint.max(AMBIENT_MIN_TINT))
        }
        None => (hue.rem_euclid(360.0), None, clamp01(tint)),
    };
    if ambient.is_none() && (h - DEFAULT_HUE as f32).abs() < 0.5 && tint <= 0.0 {
        return base;
    }

    let t = tinted_surfaces(dark, h);
    let m = |def: Rgba, tinted: Rgb| mix(def.rgb(), tinted, tint).with_alpha(255);
    let bg = m(base.bg, t[0]);
    let accent_sat = sat_override.unwrap_or(if dark { 1.0 } else { 0.88 });
    let accent = accent_for(dark, h, accent_sat, bg.rgb()).with_alpha(255);

    let (glow, glow_fade) = match ambient {
        Some(col) => {
            let (gh, gs, gl) = rgb_to_hsl(col);
            let g = hsl(gh, gs.clamp(0.5, 1.0), gl.clamp(0.45, 0.62));
            (
                g.with_alpha(if dark { 0x70 } else { 0x58 }),
                g.with_alpha(0),
            )
        }
        None => (accent.rgb().with_alpha(0), accent.rgb().with_alpha(0)),
    };

    Palette {
        bg,
        light: m(base.light, t[1]),
        shadow: m(base.shadow, t[2]),
        text: m(base.text, t[3]),
        text_dim: m(base.text_dim, t[4]),
        accent,
        inset_top: m(base.inset_top, t[5]),
        inset_bottom: m(base.inset_bottom, t[6]),
        inset_border: base.inset_border,
        row_current: m(base.row_current, t[7]),
        row_hover: base.row_hover,
        glow,
        glow_fade,
    }
}

// ---- Color dominante de una carátula -------------------------------------------------

/// Color dominante «vistoso» de una imagen RGBA (`w`×`h`), o `None` si es gris, casi negra o
/// transparente (entonces se usa el tema manual). Se muestrean unos 4000 píxeles: el coste es
/// despreciable aunque la carátula sea grande.
///
/// Los píxeles se reparten en 12 sectores de matiz, ponderados por saturación·brillo (así un
/// fondo gris o negro no gana a un objeto de color); gana el sector con más peso y el
/// resultado es la media de sus píxeles.
pub fn dominant_color(rgba: &[u8], w: usize, h: usize) -> Option<Rgb> {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let step = (((w * h) as f32 / 4000.0).sqrt().ceil() as usize).max(1);
    const BINS: usize = 12;
    let mut weight = [0.0f32; BINS];
    let mut sum = [[0.0f32; 3]; BINS];
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let i = (y * w + x) * 4;
            if rgba[i + 3] >= 128 {
                let col = Rgb::new(rgba[i], rgba[i + 1], rgba[i + 2]);
                let (hh, s, l) = rgb_to_hsl(col);
                // Saturación y brillo en el sentido HSV aproximado: ignora grises y casi negros.
                let v = (l + s * l.min(1.0 - l)).clamp(0.0, 1.0);
                if s >= 0.22 && v >= 0.15 {
                    let wgt = s * v;
                    let bin = ((hh / 360.0 * BINS as f32) as usize).min(BINS - 1);
                    weight[bin] += wgt;
                    sum[bin][0] += col.r as f32 * wgt;
                    sum[bin][1] += col.g as f32 * wgt;
                    sum[bin][2] += col.b as f32 * wgt;
                }
            }
            x += step;
        }
        y += step;
    }
    let (best, &w_best) = weight
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    // Menos de ~1 % de píxeles vistosos: la imagen es prácticamente gris.
    let sampled = (w / step).max(1) * (h / step).max(1);
    if w_best < 0.004 * sampled as f32 {
        return None;
    }
    let s = sum[best];
    Some(Rgb::new(
        (s[0] / w_best).round() as u8,
        (s[1] / w_best).round() as u8,
        (s[2] / w_best).round() as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, px: [u8; 4]) -> Vec<u8> {
        px.iter().copied().cycle().take(w * h * 4).collect()
    }

    fn fill_rect(
        img: &mut [u8],
        w: usize,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        px: [u8; 4],
    ) {
        for y in y0..y1 {
            for x in x0..x1 {
                img[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&px);
            }
        }
    }

    #[test]
    fn default_settings_reproduce_the_original_palette() {
        assert_eq!(
            palette(true, DEFAULT_HUE as f32, 0.0, None),
            default_palette(true)
        );
        assert_eq!(
            palette(false, DEFAULT_HUE as f32, 0.0, None),
            default_palette(false)
        );
        assert_eq!(default_palette(false).bg, Rgba::new(0xe4, 0xe8, 0xef, 255));
        assert_eq!(
            default_palette(true).accent,
            Rgba::new(0x8d, 0x9b, 0xff, 255)
        );
    }

    #[test]
    fn hsl_roundtrip() {
        for &(r, g, b) in &[
            (255u8, 0u8, 0u8),
            (10, 200, 90),
            (0x8d, 0x9b, 0xff),
            (30, 30, 30),
            (255, 255, 255),
        ] {
            let (h, s, l) = rgb_to_hsl(Rgb::new(r, g, b));
            let back = hsl(h, s, l);
            for (a, b) in [(r, back.r), (g, back.g), (b, back.b)] {
                assert!((a as i32 - b as i32).abs() <= 1, "{r},{g},{b} -> {back:?}");
            }
        }
        assert_eq!(rgb_to_hsl(Rgb::new(255, 0, 0)).0, 0.0);
        assert!((rgb_to_hsl(Rgb::new(0, 255, 0)).0 - 120.0).abs() < 0.01);
        assert!((rgb_to_hsl(Rgb::new(0, 0, 255)).0 - 240.0).abs() < 0.01);
    }

    #[test]
    fn every_hue_keeps_readable_accent_and_text() {
        for dark in [false, true] {
            for hue in (0..360).step_by(5) {
                for tint in [0.0, 0.5, 1.0] {
                    let p = palette(dark, hue as f32, tint, None);
                    assert!(
                        contrast(p.accent.rgb(), p.bg.rgb()) >= 2.95,
                        "acento dark={dark} hue={hue} tint={tint}: {}",
                        contrast(p.accent.rgb(), p.bg.rgb())
                    );
                    assert!(
                        contrast(p.text.rgb(), p.bg.rgb()) >= 6.0,
                        "texto dark={dark} hue={hue}"
                    );
                    assert!(
                        contrast(p.text_dim.rgb(), p.bg.rgb()) >= 2.2,
                        "dim dark={dark} hue={hue}"
                    );
                    // El relieve neumórfico sigue siendo luz arriba-izquierda y sombra abajo-derecha.
                    assert!(
                        luminance(p.light.rgb()) > luminance(p.bg.rgb()),
                        "luz dark={dark} hue={hue}"
                    );
                    assert!(
                        luminance(p.shadow.rgb()) < luminance(p.bg.rgb()),
                        "sombra dark={dark} hue={hue}"
                    );
                }
            }
        }
    }

    #[test]
    fn dark_mode_is_dark_and_light_mode_is_light() {
        let d = palette(true, 120.0, 1.0, None);
        let l = palette(false, 120.0, 1.0, None);
        assert!(luminance(d.bg.rgb()) < 0.05);
        assert!(luminance(l.bg.rgb()) > 0.6);
        assert_ne!(d.bg, l.bg);
    }

    #[test]
    fn tint_zero_keeps_surfaces_whatever_the_hue() {
        let a = palette(true, 10.0, 0.0, None);
        let b = default_palette(true);
        assert_eq!(
            (a.bg, a.light, a.shadow, a.text),
            (b.bg, b.light, b.shadow, b.text)
        );
        assert_ne!(a.accent, b.accent); // el acento sí cambia
        let tinted = palette(true, 10.0, 1.0, None);
        assert_ne!(tinted.bg, b.bg);
    }

    #[test]
    fn hue_is_wrapped_and_tint_clamped() {
        assert_eq!(
            palette(true, 370.0, 5.0, None),
            palette(true, 10.0, 1.0, None)
        );
        assert_eq!(
            palette(true, -350.0, -1.0, None),
            palette(true, 10.0, 0.0, None)
        );
    }

    #[test]
    fn ambient_overrides_manual_hue_and_adds_glow() {
        let red = Rgb::new(220, 40, 40);
        let a = palette(true, 232.0, 0.0, Some(red));
        let manual = palette(true, 232.0, 0.0, None);
        assert_ne!(a.accent, manual.accent);
        assert!(a.glow.a > 0 && a.glow_fade.a == 0);
        assert_eq!(a.glow.rgb(), a.glow_fade.rgb());
        let (h, _, _) = rgb_to_hsl(a.accent.rgb());
        assert!(h < 15.0 || h > 345.0, "acento rojizo, matiz {h}");
        assert_eq!(manual.glow.a, 0); // sin ambient no hay resplandor
        assert_ne!(a.bg, manual.bg); // el fondo se tiñe un mínimo
        assert!(contrast(a.accent.rgb(), a.bg.rgb()) >= 2.95);
        // El mismo color en modo claro sigue siendo legible.
        let al = palette(false, 232.0, 0.0, Some(Rgb::new(240, 220, 60)));
        assert!(contrast(al.accent.rgb(), al.bg.rgb()) >= 2.95);
    }

    #[test]
    fn presets_are_consistent() {
        assert_eq!(preset_name(0), Some("Índigo"));
        assert_eq!(preset_values(0), Some((DEFAULT_HUE, 0)));
        for i in 0..preset_count() {
            let (h, t) = preset_values(i).unwrap();
            assert_eq!(identify_theme(h, t), i as u8);
            assert!(h < 360 && t <= 100);
        }
        assert_eq!(identify_theme(1, 1), THEME_CUSTOM);
        let mut n = preset_names();
        n.sort_unstable();
        n.dedup();
        assert_eq!(n.len(), preset_count());
    }

    #[test]
    fn dominant_color_prefers_colourful_pixels_over_a_grey_background() {
        // 100×100 de fondo gris con un cuadrado rojo de 30×30: gana el rojo.
        let mut img = solid(100, 100, [120, 120, 120, 255]);
        fill_rect(&mut img, 100, 10, 10, 40, 40, [200, 30, 30, 255]);
        let d = dominant_color(&img, 100, 100).unwrap();
        assert!(d.r > 180 && d.g < 60 && d.b < 60, "{d:?}");
    }

    #[test]
    fn dominant_color_picks_the_largest_colourful_area() {
        let mut img = solid(120, 120, [0, 0, 0, 255]);
        fill_rect(&mut img, 120, 0, 0, 100, 100, [30, 60, 220, 255]); // azul grande
        fill_rect(&mut img, 120, 100, 100, 120, 120, [220, 30, 30, 255]); // rojo pequeño
        let d = dominant_color(&img, 120, 120).unwrap();
        assert!(d.b > 180 && d.r < 60, "{d:?}");
    }

    #[test]
    fn dominant_color_none_for_grey_black_transparent_or_bad_input() {
        assert_eq!(
            dominant_color(&solid(50, 50, [128, 128, 128, 255]), 50, 50),
            None
        );
        assert_eq!(dominant_color(&solid(50, 50, [0, 0, 0, 255]), 50, 50), None);
        assert_eq!(dominant_color(&solid(50, 50, [255, 0, 0, 0]), 50, 50), None);
        assert_eq!(dominant_color(&[1, 2, 3], 10, 10), None);
        assert_eq!(dominant_color(&[], 0, 0), None);
    }

    #[test]
    fn dominant_color_samples_big_images_quickly_and_ignores_stray_pixels() {
        // Una imagen enorme casi gris con unos pocos píxeles de color: no basta para un tema.
        let mut img = solid(1000, 1000, [90, 90, 90, 255]);
        fill_rect(&mut img, 1000, 0, 0, 3, 3, [255, 0, 0, 255]);
        assert_eq!(dominant_color(&img, 1000, 1000), None);
        let mut img = solid(1000, 1000, [90, 90, 90, 255]);
        fill_rect(&mut img, 1000, 0, 0, 500, 500, [0, 200, 60, 255]);
        let d = dominant_color(&img, 1000, 1000).unwrap();
        assert!(d.g > 150 && d.r < 40);
    }
}
