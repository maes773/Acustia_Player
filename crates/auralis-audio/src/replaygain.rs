//! ReplayGain: lectura de las etiquetas (`REPLAYGAIN_TRACK_GAIN`, etc.) y cálculo de la
//! ganancia que lleva la pista al nivel objetivo de la app.
//!
//! ReplayGain 2.0 toma como referencia −18 LUFS; la app normaliza a [`TARGET_LUFS`]
//! (−14), así que a la ganancia de la etiqueta se le suma la diferencia (+4 dB). El pico
//! de la etiqueta se usa para no pasar del techo de −1 dBFS.

use crate::loudness::{TrackLoudness, TARGET_LUFS};

/// Nivel de referencia de ReplayGain 2.0 (LUFS).
pub const REPLAYGAIN_REFERENCE_LUFS: f32 = -18.0;
const PEAK_CEILING_DB: f32 = -1.0;
const MAX_GAIN_DB: f32 = 15.0;
const MIN_GAIN_DB: f32 = -30.0;

/// Datos ReplayGain de un archivo (lo que falte es `None`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

impl ReplayGain {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Anota una etiqueta (`key` sin importar mayúsculas). La primera fuente gana.
    pub fn set_from_tag(&mut self, key: &str, value: &str) {
        let k = key.trim().to_ascii_lowercase();
        let k = k.strip_prefix("txxx:").unwrap_or(&k);
        match k {
            "replaygain_track_gain" => fill(&mut self.track_gain_db, parse_gain_db(value)),
            "replaygain_track_peak" => fill(&mut self.track_peak, parse_peak(value)),
            "replaygain_album_gain" => fill(&mut self.album_gain_db, parse_gain_db(value)),
            "replaygain_album_peak" => fill(&mut self.album_peak, parse_peak(value)),
            _ => {}
        }
    }

    /// Ganancia lineal para llevar la pista a [`TARGET_LUFS`], o `None` si no hay etiqueta
    /// aplicable. En modo álbum se prefiere la etiqueta de álbum y, si no existe, la de
    /// pista; en modo pista se usa solo la de pista.
    pub fn gain(&self, album_mode: bool) -> Option<f32> {
        let (db, peak) = if album_mode {
            match (self.album_gain_db, self.track_gain_db) {
                (Some(g), _) => (g, self.album_peak.or(self.track_peak)),
                (None, Some(g)) => (g, self.track_peak),
                _ => return None,
            }
        } else {
            (self.track_gain_db?, self.track_peak)
        };
        let mut db = db + (TARGET_LUFS - REPLAYGAIN_REFERENCE_LUFS);
        if let Some(p) = peak.filter(|p| *p > 1e-6) {
            db = db.min(PEAK_CEILING_DB - 20.0 * p.log10());
        }
        Some(10f32.powf(db.clamp(MIN_GAIN_DB, MAX_GAIN_DB) / 20.0))
    }
}

fn fill(slot: &mut Option<f32>, v: Option<f32>) {
    if slot.is_none() {
        *slot = v;
    }
}

/// «-6.54 dB», «+3,2dB», «  -0.5 » → dB.
pub fn parse_gain_db(s: &str) -> Option<f32> {
    let t = s.trim();
    let t = t
        .strip_suffix("dB")
        .or_else(|| t.strip_suffix("DB"))
        .or_else(|| t.strip_suffix("db"))
        .unwrap_or(t)
        .trim()
        .replace(',', ".");
    let v: f32 = t.parse().ok()?;
    (v.is_finite() && v.abs() <= 60.0).then_some(v)
}

/// «0.988553» → pico lineal (>0).
pub fn parse_peak(s: &str) -> Option<f32> {
    let v: f32 = s.trim().replace(',', ".").parse().ok()?;
    (v.is_finite() && v > 0.0 && v < 100.0).then_some(v)
}

/// Qué usó la normalización para una pista (informativo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormSource {
    ReplayGain,
    Measured,
    None,
}

/// Ganancia de normalización: la etiqueta ReplayGain si existe; si no, la sonoridad
/// medida por la app; si no hay ninguna, 1,0 (sin cambio).
pub fn norm_gain(
    rg: &ReplayGain,
    album_mode: bool,
    measured: impl FnOnce() -> Option<TrackLoudness>,
) -> (f32, NormSource) {
    if let Some(g) = rg.gain(album_mode) {
        return (g, NormSource::ReplayGain);
    }
    match measured() {
        Some(l) => (l.gain_for(TARGET_LUFS), NormSource::Measured),
        None => (1.0, NormSource::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(g: f32) -> f32 {
        20.0 * g.log10()
    }

    #[test]
    fn parses_common_formats() {
        assert_eq!(parse_gain_db("-6.54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("+3,2dB"), Some(3.2));
        assert_eq!(parse_gain_db("  -0.5 "), Some(-0.5));
        assert_eq!(parse_gain_db("-6.54 DB"), Some(-6.54));
        assert_eq!(parse_gain_db("abc"), None);
        assert_eq!(parse_gain_db("500 dB"), None);
        assert_eq!(parse_gain_db("NaN"), None);
        assert_eq!(parse_peak("0.988553"), Some(0.988553));
        assert_eq!(parse_peak("0"), None);
        assert_eq!(parse_peak("-1"), None);
        assert_eq!(parse_peak("x"), None);
    }

    #[test]
    fn tag_keys_ignore_case_and_id3_prefix() {
        let mut rg = ReplayGain::default();
        rg.set_from_tag("REPLAYGAIN_TRACK_GAIN", "-7.00 dB");
        rg.set_from_tag("TXXX:replaygain_album_gain", "-5.00 dB");
        rg.set_from_tag("replaygain_track_peak", "0.9");
        rg.set_from_tag("replaygain_track_gain", "-1.0 dB"); // la primera gana
        rg.set_from_tag("otra", "1");
        assert_eq!(rg.track_gain_db, Some(-7.0));
        assert_eq!(rg.album_gain_db, Some(-5.0));
        assert_eq!(rg.track_peak, Some(0.9));
        assert!(!rg.is_empty());
        assert!(ReplayGain::default().is_empty());
    }

    #[test]
    fn reference_is_shifted_to_the_app_target() {
        // −6 dB de etiqueta (referencia −18) → −2 dB para −14 LUFS.
        let rg = ReplayGain {
            track_gain_db: Some(-6.0),
            ..Default::default()
        };
        assert!((db(rg.gain(false).unwrap()) - (-2.0)).abs() < 1e-3);
    }

    #[test]
    fn peak_limits_the_boost() {
        // +8 dB pedidos pero el pico es 0,9 (−0,9 dBFS): solo caben ~−0,1 dB.
        let rg = ReplayGain {
            track_gain_db: Some(4.0),
            track_peak: Some(0.9),
            ..Default::default()
        };
        let g = db(rg.gain(false).unwrap());
        assert!(g < 0.0 && g > -0.3, "{g}");
        // Sin pico, sube lo que pide la etiqueta.
        let rg = ReplayGain {
            track_gain_db: Some(4.0),
            ..Default::default()
        };
        assert!((db(rg.gain(false).unwrap()) - 8.0).abs() < 1e-3);
    }

    #[test]
    fn album_vs_track_modes() {
        let rg = ReplayGain {
            track_gain_db: Some(-10.0),
            album_gain_db: Some(-4.0),
            ..Default::default()
        };
        assert!((db(rg.gain(false).unwrap()) - (-6.0)).abs() < 1e-3);
        assert!((db(rg.gain(true).unwrap()) - 0.0).abs() < 1e-3);
        // Modo álbum sin etiqueta de álbum: usa la de pista.
        let only_track = ReplayGain {
            track_gain_db: Some(-10.0),
            ..Default::default()
        };
        assert!((db(only_track.gain(true).unwrap()) - (-6.0)).abs() < 1e-3);
        // Modo pista con solo etiqueta de álbum: no se usa (se medirá).
        let only_album = ReplayGain {
            album_gain_db: Some(-4.0),
            ..Default::default()
        };
        assert_eq!(only_album.gain(false), None);
    }

    #[test]
    fn extreme_values_are_clamped() {
        let rg = ReplayGain {
            track_gain_db: Some(55.0),
            ..Default::default()
        };
        assert!(db(rg.gain(false).unwrap()) <= MAX_GAIN_DB + 1e-3);
        let rg = ReplayGain {
            track_gain_db: Some(-55.0),
            ..Default::default()
        };
        assert!(db(rg.gain(false).unwrap()) >= MIN_GAIN_DB - 1e-3);
    }

    #[test]
    fn falls_back_to_measurement_then_unity() {
        let none = ReplayGain::default();
        assert_eq!(norm_gain(&none, false, || None), (1.0, NormSource::None));
        let m = TrackLoudness {
            lufs: -20.0,
            peak: 0.2,
        };
        let (g, src) = norm_gain(&none, false, || Some(m));
        assert_eq!(src, NormSource::Measured);
        assert!((db(g) - 6.0).abs() < 1e-3);
        let tagged = ReplayGain {
            track_gain_db: Some(-6.0),
            ..Default::default()
        };
        // Con etiqueta no se llega a medir (la medición puede ser costosa).
        let (_, src) = norm_gain(&tagged, false, || panic!("no debe medirse"));
        assert_eq!(src, NormSource::ReplayGain);
    }
}
