//! Ecualizador gráfico de 10 bandas: frecuencias, presets y utilidades. Sin audio ni
//! interfaz: el DSP vive en `auralis-audio`; aquí solo están los datos, para que otra
//! interfaz (Android) los reutilice.

/// Número de bandas.
pub const EQ_BANDS: usize = 10;

/// Frecuencias centrales de las bandas (Hz), una por octava.
pub const EQ_FREQS_HZ: [f32; EQ_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Rango de cada banda, en décimas de dB (±12 dB).
pub const EQ_MAX_TENTHS: i16 = 120;

/// Valor de `Settings::eq_preset` cuando las bandas no coinciden con ningún preset.
pub const EQ_CUSTOM: u8 = u8::MAX;

/// Ganancias de una banda por banda, en décimas de dB.
pub type EqGains = [i16; EQ_BANDS];

/// Presets: (nombre, ganancias en dB por banda).
const PRESET_TABLE: [(&str, [f32; EQ_BANDS]); 12] = [
    ("Plano", [0.0; EQ_BANDS]),
    (
        "Bajos potenciados",
        [6.0, 5.0, 4.0, 2.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Agudos potenciados",
        [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 5.0, 6.0],
    ),
    ("Rock", [4.5, 3.5, 2.5, 1.0, -1.0, -1.5, 0.5, 2.5, 3.5, 4.0]),
    (
        "Pop",
        [-1.0, 1.0, 3.0, 4.0, 3.0, 0.5, -1.0, -1.0, -0.5, -0.5],
    ),
    ("Jazz", [3.0, 2.5, 1.0, 2.0, -1.5, -1.5, 0.0, 1.0, 2.0, 3.0]),
    (
        "Clásica",
        [3.5, 3.0, 2.0, 1.5, -0.5, -0.5, 0.0, 2.0, 2.5, 3.0],
    ),
    (
        "Electrónica",
        [5.0, 4.0, 1.0, 0.0, -2.0, 1.5, 0.5, 1.5, 4.0, 4.5],
    ),
    (
        "Hip-Hop",
        [5.5, 4.5, 1.5, 3.0, -1.0, -1.0, 1.0, -0.5, 1.5, 3.0],
    ),
    (
        "Voz",
        [-3.0, -2.0, -1.0, 1.0, 3.0, 4.0, 3.5, 2.0, 0.0, -1.0],
    ),
    (
        "Acústico",
        [3.5, 3.0, 2.0, 0.5, 1.0, 1.0, 2.0, 2.5, 3.0, 2.0],
    ),
    (
        "Noche (graves suaves)",
        [-2.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.5, 1.5],
    ),
];

pub fn preset_count() -> usize {
    PRESET_TABLE.len()
}

/// Nombre del preset `index` (`None` si no existe).
pub fn preset_name(index: usize) -> Option<&'static str> {
    PRESET_TABLE.get(index).map(|p| p.0)
}

/// Nombres de todos los presets, en orden.
pub fn preset_names() -> Vec<&'static str> {
    PRESET_TABLE.iter().map(|p| p.0).collect()
}

/// Ganancias del preset `index`, en décimas de dB.
pub fn preset_gains(index: usize) -> Option<EqGains> {
    PRESET_TABLE.get(index).map(|(_, g)| {
        let mut out = [0i16; EQ_BANDS];
        for (o, v) in out.iter_mut().zip(g) {
            *o = (v * 10.0).round() as i16;
        }
        out
    })
}

/// Índice del preset cuyas ganancias coinciden exactamente con `gains`, o [`EQ_CUSTOM`].
pub fn identify_preset(gains: &EqGains) -> u8 {
    (0..preset_count())
        .find(|&i| preset_gains(i).as_ref() == Some(gains))
        .map_or(EQ_CUSTOM, |i| i as u8)
}

/// Limita una ganancia a ±12 dB.
pub fn clamp_tenths(v: i32) -> i16 {
    v.clamp(-(EQ_MAX_TENTHS as i32), EQ_MAX_TENTHS as i32) as i16
}

/// Décimas de dB → dB.
pub fn tenths_to_db(v: i16) -> f32 {
    v as f32 / 10.0
}

/// Etiqueta de una banda ("31 Hz", "1 kHz").
pub fn band_label(index: usize) -> String {
    match EQ_FREQS_HZ.get(index) {
        Some(&f) if f >= 1000.0 => format!("{} kHz", (f / 1000.0) as u32),
        Some(&f) => format!("{} Hz", f as u32),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_within_range_and_identified() {
        assert_eq!(preset_name(0), Some("Plano"));
        assert_eq!(preset_gains(0), Some([0; EQ_BANDS]));
        for i in 0..preset_count() {
            let g = preset_gains(i).unwrap();
            assert!(
                g.iter().all(|v| v.abs() <= EQ_MAX_TENTHS),
                "{}",
                preset_name(i).unwrap()
            );
            assert_eq!(identify_preset(&g), i as u8);
        }
        let mut custom = preset_gains(3).unwrap();
        custom[4] += 1;
        assert_eq!(identify_preset(&custom), EQ_CUSTOM);
        assert_eq!(preset_gains(99), None);
    }

    #[test]
    fn names_are_unique() {
        let mut n = preset_names();
        n.sort_unstable();
        n.dedup();
        assert_eq!(n.len(), preset_count());
    }

    #[test]
    fn clamp_and_labels() {
        assert_eq!(clamp_tenths(500), 120);
        assert_eq!(clamp_tenths(-500), -120);
        assert_eq!(clamp_tenths(35), 35);
        assert_eq!(band_label(0), "31 Hz");
        assert_eq!(band_label(5), "1 kHz");
        assert_eq!(band_label(9), "16 kHz");
        assert_eq!(band_label(10), "");
        assert_eq!(tenths_to_db(-35), -3.5);
    }
}
