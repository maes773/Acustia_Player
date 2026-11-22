//! Conversión de canales y frecuencia de muestreo a la configuración de salida.
//!
//! Remuestreo: filtro sinc con ventana Kaiser (β = 9, ≈ 90 dB de rechazo),
//! 80 coeficientes por muestra de salida al subir de frecuencia (más al bajarla),
//! tabla polifásica de 512 fases con interpolación lineal entre fases y cada fase
//! normalizada a ganancia 1 (sin modulación de amplitud). Sustituye a la
//! interpolación lineal de la primera versión, que producía aliasing audible
//! en agudos al pasar de 44,1 kHz a 48 kHz.
//!
//! Para no perder el final de cada pista, `finish` vacía la cola del filtro.

use std::f64::consts::PI;

/// Coeficientes del filtro a cada lado del punto de interpolación (a la
/// frecuencia de salida; se amplían al bajar de frecuencia).
const TAPS_PER_SIDE: f64 = 40.0;
/// Resolución de la tabla polifásica.
const PHASES: usize = 512;
const KAISER_BETA: f64 = 9.0;
/// Frecuencia de corte respecto a la Nyquist más baja de las dos.
const CUTOFF: f64 = 0.97;

pub struct Converter {
    src_ch: usize,
    dst_ch: usize,
    resampler: Option<Resampler>,
    mapped: Vec<f32>,
}

impl Converter {
    pub fn new(src_rate: u32, src_ch: usize, dst_rate: u32, dst_ch: usize) -> Self {
        let dst_ch = dst_ch.max(1);
        let resampler = (src_rate != dst_rate && src_rate > 0 && dst_rate > 0)
            .then(|| Resampler::new(src_rate, dst_rate, dst_ch));
        Self {
            src_ch: src_ch.max(1),
            dst_ch,
            resampler,
            mapped: Vec::new(),
        }
    }

    /// Convierte `input` (entrelazado, `src_ch` canales) y añade el resultado
    /// (entrelazado, `dst_ch` canales, frecuencia de salida) a `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let mut mapped = std::mem::take(&mut self.mapped);
        mapped.clear();
        map_channels(input, self.src_ch, self.dst_ch, &mut mapped);
        match self.resampler.as_mut() {
            Some(r) => r.process(&mapped, out),
            None => out.extend_from_slice(&mapped),
        }
        self.mapped = mapped;
    }

    /// Fin de la pista: emite las muestras que aún quedaban dentro del filtro.
    /// El conversor no debe usarse después.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if let Some(r) = self.resampler.as_mut() {
            r.finish(out);
        }
    }
}

struct Resampler {
    ch: usize,
    /// Fotogramas de origen por fotograma de salida.
    step: f64,
    /// Semiancho del filtro en fotogramas de origen.
    rad: usize,
    taps: usize,
    /// `(PHASES + 1)` filas de `taps` coeficientes.
    table: Vec<f32>,
    coef: Vec<f32>,
    /// Fotogramas de origen entrelazados: historial + pendientes.
    buf: Vec<f32>,
    /// Posición (en fotogramas de `buf`) de la siguiente muestra de salida.
    pos: f64,
}

impl Resampler {
    fn new(src_rate: u32, dst_rate: u32, ch: usize) -> Self {
        let ratio = (dst_rate as f64 / src_rate as f64).min(1.0);
        let fc = CUTOFF * ratio;
        let rad = (TAPS_PER_SIDE / ratio).ceil() as usize;
        let taps = 2 * rad;
        let i0_beta = bessel_i0(KAISER_BETA);

        let mut table = vec![0f32; (PHASES + 1) * taps];
        let mut row = vec![0f64; taps];
        for q in 0..=PHASES {
            let frac = q as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (k, slot) in row.iter_mut().enumerate() {
                // Coeficiente k pesa al fotograma floor(p) + j, con j en -rad+1..=rad.
                let j = k as f64 - (rad as f64 - 1.0);
                let d = frac - j;
                let t = d / rad as f64;
                let window = if t.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(KAISER_BETA * (1.0 - t * t).sqrt()) / i0_beta
                };
                let x = fc * d;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                *slot = fc * sinc * window;
                sum += *slot;
            }
            for (k, v) in row.iter().enumerate() {
                table[q * taps + k] = (v / sum) as f32;
            }
        }

        Self {
            ch,
            step: src_rate as f64 / dst_rate as f64,
            rad,
            taps,
            table,
            coef: vec![0.0; taps],
            // Historial de ceros: x[n < 0] = 0.
            buf: vec![0.0; (rad - 1) * ch],
            pos: (rad - 1) as f64,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        self.run(out, None);
    }

    fn finish(&mut self, out: &mut Vec<f32>) {
        let real_end = (self.buf.len() / self.ch) as f64;
        self.buf.resize(self.buf.len() + self.rad * self.ch, 0.0);
        self.run(out, Some(real_end));
    }

    /// Genera todas las muestras de salida posibles con los datos disponibles.
    /// `limit`: posición máxima (exclusiva) en fotogramas de `buf`.
    fn run(&mut self, out: &mut Vec<f32>, limit: Option<f64>) {
        let (ch, taps, rad) = (self.ch, self.taps, self.rad);
        let frames = self.buf.len() / ch;

        loop {
            let i = self.pos.floor() as usize;
            if i + rad >= frames {
                break;
            }
            if limit.is_some_and(|l| self.pos >= l) {
                break;
            }
            let q = (self.pos - i as f64) * PHASES as f64;
            let q0 = (q as usize).min(PHASES - 1);
            let qf = (q - q0 as f64) as f32;
            let r0 = &self.table[q0 * taps..(q0 + 1) * taps];
            let r1 = &self.table[(q0 + 1) * taps..(q0 + 2) * taps];
            for k in 0..taps {
                self.coef[k] = r0[k] + (r1[k] - r0[k]) * qf;
            }
            let base = (i + 1 - rad) * ch;
            for c in 0..ch {
                let mut acc = 0f32;
                for k in 0..taps {
                    acc += self.coef[k] * self.buf[base + k * ch + c];
                }
                out.push(acc);
            }
            self.pos += self.step;
        }

        // Descarta lo que el filtro ya no necesita.
        let keep_from = (self.pos.floor() as usize).saturating_sub(rad - 1);
        if keep_from > 0 {
            self.buf.drain(..keep_from * ch);
            self.pos -= keep_from as f64;
        }
    }
}

/// Función de Bessel modificada de orden 0 (serie de potencias).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64 {
        term *= half / k as f64;
        let sq = term * term;
        sum += sq;
        if sq < 1e-16 * sum {
            break;
        }
    }
    sum
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

    fn sine(freq: f64, rate: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (2.0 * PI * freq * i as f64 / rate).sin()) as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Convierte `input` completo (mono) en bloques de `chunk` y vacía el filtro.
    fn convert(src: u32, dst: u32, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut c = Converter::new(src, 1, dst, 1);
        let mut out = Vec::new();
        for block in input.chunks(chunk) {
            c.process(block, &mut out);
        }
        c.finish(&mut out);
        out
    }

    #[test]
    fn same_format_is_passthrough() {
        let mut c = Converter::new(44100, 2, 44100, 2);
        let mut out = Vec::new();
        c.process(&[0.1, 0.2, 0.3, 0.4], &mut out);
        c.finish(&mut out);
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
    fn output_length_is_exact_after_finish() {
        let input = vec![0.25f32; 44_100];
        assert_eq!(convert(44_100, 48_000, &input, 1000).len(), 48_000);
        let input = vec![0.25f32; 10_000];
        assert_eq!(convert(22_050, 44_100, &input, 777).len(), 20_000);
        let input = vec![0.25f32; 96_000];
        assert_eq!(convert(96_000, 48_000, &input, 4096).len(), 48_000);
    }

    #[test]
    fn dc_is_preserved_without_gain_ripple() {
        let input = vec![0.25f32; 10_000];
        let out = convert(22_050, 44_100, &input, 1000);
        let mid = &out[200..out.len() - 200];
        let worst = mid.iter().map(|&s| (s - 0.25).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "desviación máxima = {worst}");
    }

    #[test]
    fn sine_44100_to_48000_is_accurate() {
        let input = sine(1000.0, 44_100.0, 44_100, 0.5);
        let out = convert(44_100, 48_000, &input, 1024);
        let expected = sine(1000.0, 48_000.0, out.len(), 0.5);
        let n = out.len();
        let worst = (500..n - 500)
            .map(|i| (out[i] - expected[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "error máximo = {worst}");
    }

    #[test]
    fn high_frequency_tone_keeps_its_level() {
        // 18 kHz debe pasar casi intacto (< 0,5 dB de diferencia).
        let input = sine(18_000.0, 44_100.0, 44_100, 0.5);
        let out = convert(44_100, 48_000, &input, 1024);
        let ratio = rms(&out[500..out.len() - 500]) / rms(&input[500..input.len() - 500]);
        let db = 20.0 * ratio.log10();
        assert!(db.abs() < 0.5, "variación = {db:.3} dB");
    }

    #[test]
    fn downsampling_rejects_frequencies_above_new_nyquist() {
        // 96 kHz -> 48 kHz: 1 kHz pasa; 30 kHz (por encima de 24 kHz) se elimina.
        let n = 96_000;
        let mix: Vec<f32> = sine(1000.0, 96_000.0, n, 0.4)
            .iter()
            .zip(sine(30_000.0, 96_000.0, n, 0.4))
            .map(|(a, b)| a + b)
            .collect();
        let out = convert(96_000, 48_000, &mix, 2048);
        let reference = sine(1000.0, 48_000.0, out.len(), 0.4);
        let residual: Vec<f32> = (1000..out.len() - 1000)
            .map(|i| out[i] - reference[i])
            .collect();
        let db = 20.0 * (rms(&residual) / rms(&reference)).log10();
        assert!(db < -60.0, "residuo = {db:.1} dB");
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let input = sine(440.0, 44_100.0, 20_000, 0.6);
        let a = convert(44_100, 48_000, &input, 1);
        let b = convert(44_100, 48_000, &input, 4096);
        assert_eq!(a.len(), b.len());
        let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "diferencia = {worst}");
    }

    #[test]
    fn stereo_channels_stay_independent() {
        let mut c = Converter::new(44_100, 2, 48_000, 2);
        let left = sine(1000.0, 44_100.0, 20_000, 0.5);
        let interleaved: Vec<f32> = left.iter().flat_map(|&l| [l, 0.0]).collect();
        let mut out = Vec::new();
        for block in interleaved.chunks(2048) {
            c.process(block, &mut out);
        }
        c.finish(&mut out);
        let right_peak = out.chunks_exact(2).map(|f| f[1].abs()).fold(0.0f32, f32::max);
        let left_peak = out.chunks_exact(2).map(|f| f[0].abs()).fold(0.0f32, f32::max);
        assert_eq!(right_peak, 0.0);
        assert!(left_peak > 0.45);
    }

    #[test]
    fn empty_input_produces_nothing() {
        assert!(convert(44_100, 48_000, &[], 16).is_empty());
    }
}
