//! Conversión de canales y frecuencia de muestreo a la configuración de salida.
//!
//! Remuestreo: interpolación lineal. Es simple y barata pero de calidad básica
//! (puede introducir aliasing). Solo se usa cuando la frecuencia del archivo
//! difiere de la del dispositivo; mejorarlo (p. ej. con `rubato`) queda para
//! una versión posterior.

pub struct Converter {
    src_ch: usize,
    dst_ch: usize,
    /// Fotogramas de origen por fotograma de salida.
    step: f64,
    resample: bool,
    /// Posición fraccionaria; el índice 0 es el último fotograma del bloque anterior.
    t: f64,
    prev: Vec<f32>,
    started: bool,
    mapped: Vec<f32>,
}

impl Converter {
    pub fn new(src_rate: u32, src_ch: usize, dst_rate: u32, dst_ch: usize) -> Self {
        Self {
            src_ch: src_ch.max(1),
            dst_ch: dst_ch.max(1),
            step: src_rate as f64 / dst_rate as f64,
            resample: src_rate != dst_rate,
            t: 0.0,
            prev: vec![0.0; dst_ch.max(1)],
            started: false,
            mapped: Vec::new(),
        }
    }

    /// Convierte `input` (entrelazado, `src_ch` canales) y añade el resultado
    /// (entrelazado, `dst_ch` canales, frecuencia de salida) a `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let mut mapped = std::mem::take(&mut self.mapped);
        mapped.clear();
        map_channels(input, self.src_ch, self.dst_ch, &mut mapped);

        if !self.resample {
            out.extend_from_slice(&mapped);
            self.mapped = mapped;
            return;
        }

        let ch = self.dst_ch;
        let frames = mapped.len() / ch;
        if frames == 0 {
            self.mapped = mapped;
            return;
        }
        if !self.started {
            self.prev.copy_from_slice(&mapped[..ch]);
            self.started = true;
        }

        // Secuencia virtual: [prev, mapped...]; necesitamos los índices i e i+1.
        let sample = |prev: &[f32], idx: usize, c: usize| -> f32 {
            if idx == 0 {
                prev[c]
            } else {
                mapped[(idx - 1) * ch + c]
            }
        };

        while (self.t.floor() as usize) < frames {
            let i = self.t.floor() as usize;
            let frac = (self.t - i as f64) as f32;
            for c in 0..ch {
                let a = sample(&self.prev, i, c);
                let b = sample(&self.prev, i + 1, c);
                out.push(a + (b - a) * frac);
            }
            self.t += self.step;
        }

        self.t -= frames as f64;
        let last = (frames - 1) * ch;
        self.prev.copy_from_slice(&mapped[last..last + ch]);
        self.mapped = mapped;
    }
}

/// Reasigna canales. Mono → duplicado; N → mono = promedio; en el resto se
/// copian los canales comunes y los sobrantes se descartan o rellenan con
/// silencio. No hay downmix envolvente real (limitación documentada).
fn map_channels(input: &[f32], src: usize, dst: usize, out: &mut Vec<f32>) {
    if src == dst {
        out.extend_from_slice(input);
        return;
    }
    for frame in input.chunks_exact(src) {
        if dst == 1 {
            out.push(frame.iter().sum::<f32>() / src as f32);
        } else if src == 1 {
            out.extend(std::iter::repeat(frame[0]).take(dst));
        } else {
            for c in 0..dst {
                out.push(if c < src { frame[c] } else { 0.0 });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_format_is_passthrough() {
        let mut c = Converter::new(44100, 2, 44100, 2);
        let mut out = Vec::new();
        c.process(&[0.1, 0.2, 0.3, 0.4], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn mono_to_stereo_duplicates() {
        let mut c = Converter::new(48000, 1, 48000, 2);
        let mut out = Vec::new();
        c.process(&[0.5, -0.5], &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn stereo_to_mono_averages() {
        let mut c = Converter::new(48000, 2, 48000, 1);
        let mut out = Vec::new();
        c.process(&[1.0, 0.0, 0.5, 0.5], &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn resample_length_is_proportional_across_blocks() {
        // 22050 → 44100: ~2 fotogramas de salida por cada uno de entrada.
        let mut c = Converter::new(22050, 1, 44100, 1);
        let mut out = Vec::new();
        for _ in 0..10 {
            c.process(&vec![0.25; 1000], &mut out);
        }
        let expected = 20_000i64;
        assert!((out.len() as i64 - expected).abs() <= 2, "len = {}", out.len());
        assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }
}
