//! Ecualizador de 10 bandas (filtros peaking biquad, una octava entre bandas).
//!
//! Corre en el callback de audio: no reserva memoria ni toma locks. Los coeficientes se
//! recalculan solo cuando cambian las ganancias (unas pocas funciones trigonométricas),
//! y para evitar crujidos al mover un deslizador el cambio se reparte con una
//! interpolación de los coeficientes a lo largo de unos milisegundos.
//!
//! Para que subir bandas no sature, se aplica un «preamp» automático igual al máximo de
//! la respuesta en frecuencia combinada (en negativo; las bandas contiguas se suman, así
//! que puede superar la mayor ganancia individual): con el ecualizador activado ninguna
//! frecuencia queda por encima de su nivel original.

pub const BANDS: usize = 10;
pub const FREQS_HZ: [f32; BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
/// Q de cada banda: una octava de ancho.
const Q: f64 = 1.414;
/// Máximo de canales que se ecualizan a la vez (el resto pasa sin tocar).
pub const MAX_CHANNELS: usize = 8;
/// Fotogramas durante los que se interpolan los coeficientes tras un cambio.
const SMOOTH_FRAMES: u32 = 512;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Coef {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coef {
    const IDENTITY: Coef = Coef {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

/// Coeficientes de un filtro peaking (Audio EQ Cookbook, R. Bristow-Johnson).
fn peaking(rate: f64, freq: f64, gain_db: f64, q: f64) -> Coef {
    if gain_db.abs() < 1e-3 || freq >= rate * 0.45 {
        return Coef::IDENTITY;
    }
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * freq / rate;
    let alpha = w0.sin() / (2.0 * q);
    let cw = w0.cos();
    let a0 = 1.0 + alpha / a;
    Coef {
        b0: (1.0 + alpha * a) / a0,
        b1: (-2.0 * cw) / a0,
        b2: (1.0 - alpha * a) / a0,
        a1: (-2.0 * cw) / a0,
        a2: (1.0 - alpha / a) / a0,
    }
}

impl Coef {
    /// Magnitud de la respuesta a `w` rad/muestra.
    fn magnitude(&self, w: f64) -> f64 {
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// Mayor ganancia (dB, ≥ 0) de la respuesta combinada de las bandas, en una rejilla
/// logarítmica de 20 Hz a casi Nyquist.
fn peak_boost_db(coefs: &[Coef; BANDS], rate: f64) -> f64 {
    const POINTS: usize = 96;
    let (lo, hi) = (20.0f64, (rate * 0.49).min(22_000.0));
    let mut best = 0.0f64;
    for i in 0..POINTS {
        let f = lo * (hi / lo).powf(i as f64 / (POINTS - 1) as f64);
        let w = 2.0 * std::f64::consts::PI * f / rate;
        let db: f64 = coefs.iter().map(|c| 20.0 * c.magnitude(w).log10()).sum();
        best = best.max(db);
    }
    best
}

/// Limitador suave: no toca nada por debajo de 0,9 y por encima se aproxima a 1,0 sin
/// pasarlo (el pico de la señal ecualizada puede superar al original aunque la respuesta
/// en frecuencia no suba, por los desfases de los filtros).
fn soft_limit(x: f32) -> f32 {
    const KNEE: f32 = 0.9;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

pub struct Equalizer {
    rate: f64,
    channels: usize,
    /// Coeficientes que se están aplicando y a los que se va acercando.
    cur: [Coef; BANDS],
    target: [Coef; BANDS],
    /// Pasos de interpolación restantes.
    smooth_left: u32,
    step: [[f64; 5]; BANDS],
    pre_cur: f64,
    pre_target: f64,
    pre_step: f64,
    state: [[State; MAX_CHANNELS]; BANDS],
    /// Todas las bandas a 0 dB y sin transición pendiente: se omite todo el proceso.
    bypass: bool,
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate: rate.max(8000) as f64,
            channels: channels.clamp(1, MAX_CHANNELS),
            cur: [Coef::IDENTITY; BANDS],
            target: [Coef::IDENTITY; BANDS],
            smooth_left: 0,
            step: [[0.0; 5]; BANDS],
            pre_cur: 1.0,
            pre_target: 1.0,
            pre_step: 0.0,
            state: [[State::default(); MAX_CHANNELS]; BANDS],
            bypass: true,
        }
    }

    /// Fija las ganancias en dB (una por banda). Con `enabled = false` equivale a todo en 0.
    pub fn set_gains(&mut self, enabled: bool, gains_db: &[f32; BANDS]) {
        for (i, t) in self.target.iter_mut().enumerate() {
            let g = if enabled {
                (gains_db[i] as f64).clamp(-12.0, 12.0)
            } else {
                0.0
            };
            *t = peaking(self.rate, FREQS_HZ[i] as f64, g, Q);
        }
        let max_boost = peak_boost_db(&self.target, self.rate);
        self.pre_target = 10f64.powf(-max_boost / 20.0);
        let n = SMOOTH_FRAMES as f64;
        for (i, s) in self.step.iter_mut().enumerate() {
            let (c, t) = (&self.cur[i], &self.target[i]);
            *s = [
                (t.b0 - c.b0) / n,
                (t.b1 - c.b1) / n,
                (t.b2 - c.b2) / n,
                (t.a1 - c.a1) / n,
                (t.a2 - c.a2) / n,
            ];
        }
        self.pre_step = (self.pre_target - self.pre_cur) / n;
        self.smooth_left = SMOOTH_FRAMES;
        self.bypass = false;
    }

    /// Olvida el estado de los filtros (tras vaciar el buffer: no hay cola que continuar).
    pub fn reset(&mut self) {
        self.state = [[State::default(); MAX_CHANNELS]; BANDS];
    }

    fn all_identity(&self) -> bool {
        self.cur.iter().all(|c| *c == Coef::IDENTITY) && (self.pre_cur - 1.0).abs() < 1e-12
    }

    /// Procesa `buf` (muestras entrelazadas, `channels` por fotograma).
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.bypass {
            return;
        }
        let ch = self.channels;
        for frame in buf.chunks_exact_mut(ch.max(1)) {
            if self.smooth_left > 0 {
                for i in 0..BANDS {
                    let (c, s) = (&mut self.cur[i], &self.step[i]);
                    c.b0 += s[0];
                    c.b1 += s[1];
                    c.b2 += s[2];
                    c.a1 += s[3];
                    c.a2 += s[4];
                }
                self.pre_cur += self.pre_step;
                self.smooth_left -= 1;
                if self.smooth_left == 0 {
                    self.cur = self.target;
                    self.pre_cur = self.pre_target;
                    if self.all_identity() {
                        self.bypass = true; // desactivado del todo: vuelve a ser gratis
                        self.state = [[State::default(); MAX_CHANNELS]; BANDS];
                        return;
                    }
                }
            }
            for (c, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample as f64 * self.pre_cur;
                for b in 0..BANDS {
                    let co = &self.cur[b];
                    if *co == Coef::IDENTITY {
                        continue;
                    }
                    let st = &mut self.state[b][c];
                    // Forma directa II transpuesta.
                    let y = co.b0 * x + st.z1;
                    st.z1 = co.b1 * x - co.a1 * y + st.z2;
                    st.z2 = co.b2 * x - co.a2 * y;
                    x = y;
                }
                *sample = soft_limit(x as f32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f32, secs: f32, channels: usize) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        let mut v = Vec::with_capacity(n * channels);
        for i in 0..n {
            let s = 0.25 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin();
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    fn rms_tail(buf: &[f32], channels: usize) -> f32 {
        let tail = &buf[buf.len() / 2..];
        let _ = channels;
        (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
    }

    /// Ganancia medida (dB) de una senoidal a `freq` con las ganancias dadas.
    fn measure(rate: u32, freq: f32, gains: [f32; BANDS]) -> f32 {
        let mut eq = Equalizer::new(rate, 2);
        eq.set_gains(true, &gains);
        let mut x = sine(rate, freq, 1.0, 2);
        let before = rms_tail(&x, 2);
        eq.process(&mut x);
        20.0 * (rms_tail(&x, 2) / before).log10()
    }

    #[test]
    fn flat_is_transparent_and_bypassed() {
        let mut eq = Equalizer::new(48_000, 2);
        let mut x = sine(48_000, 1000.0, 0.2, 2);
        let y = x.clone();
        eq.process(&mut x);
        assert_eq!(x, y); // sin configurar: no toca nada
        eq.set_gains(true, &[0.0; BANDS]);
        eq.process(&mut x);
        for (a, b) in x.iter().zip(&y) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn a_boost_raises_its_band_and_leaves_far_bands_alone() {
        let mut g = [0.0; BANDS];
        g[5] = 6.0; // +6 dB a 1 kHz
                    // El preamp resta 6 dB a todo: en 1 kHz el resultado neto es ~0 dB y lejos −6 dB.
        let at_1k = measure(48_000, 1000.0, g);
        let far_low = measure(48_000, 62.0, g);
        let far_high = measure(48_000, 12_000.0, g);
        assert!(at_1k.abs() < 0.5, "1 kHz: {at_1k}");
        assert!((far_low + 6.0).abs() < 0.8, "62 Hz: {far_low}");
        assert!((far_high + 6.0).abs() < 1.0, "12 kHz: {far_high}");
        assert!(at_1k - far_low > 5.0);
    }

    #[test]
    fn a_cut_lowers_its_band() {
        let mut g = [0.0; BANDS];
        g[3] = -9.0; // −9 dB a 250 Hz (sin boosts → sin preamp)
        let at = measure(44_100, 250.0, g);
        assert!((at + 9.0).abs() < 0.5, "{at}");
        let far = measure(44_100, 4_000.0, g);
        assert!(far.abs() < 0.5, "{far}");
    }

    #[test]
    fn never_clips_when_everything_is_boosted() {
        let mut eq = Equalizer::new(48_000, 2);
        eq.set_gains(true, &[12.0; BANDS]);
        // Ruido determinista casi a fondo de escala.
        let mut seed = 1u32;
        let mut x: Vec<f32> = (0..96_000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 1.6
            })
            .collect();
        let rms_in = (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        eq.process(&mut x);
        let rms_out = (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        // Ninguna frecuencia sube (el preamp compensa el máximo de la respuesta combinada):
        // la energía del ruido no puede aumentar y el pico queda cerca del original.
        assert!(rms_out <= rms_in * 1.02, "{rms_out} vs {rms_in}");
        assert!(peak <= 1.0, "{peak}");
    }

    #[test]
    fn changes_are_smooth_without_jumps() {
        let mut eq = Equalizer::new(48_000, 1);
        let mut x = sine(48_000, 200.0, 0.5, 1);
        let mut g = [0.0; BANDS];
        let half = x.len() / 2;
        eq.process(&mut x[..half]);
        g[2] = 12.0;
        g[3] = 12.0;
        eq.set_gains(true, &g);
        eq.process(&mut x[half..]);
        let max_step = x
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // Una senoidal de 200 Hz con amplitud ≤0,3 no cambia más de ~0,03 entre muestras.
        assert!(max_step < 0.06, "salto de {max_step}");
    }

    #[test]
    fn disabling_returns_to_exact_bypass() {
        let mut eq = Equalizer::new(48_000, 2);
        let mut g = [0.0; BANDS];
        g[0] = 8.0;
        eq.set_gains(true, &g);
        let mut x = sine(48_000, 100.0, 0.2, 2);
        eq.process(&mut x);
        eq.set_gains(false, &g);
        let mut warm = sine(48_000, 100.0, 0.2, 2);
        eq.process(&mut warm); // transición
        let mut y = sine(48_000, 100.0, 0.1, 2);
        let z = y.clone();
        eq.process(&mut y);
        assert_eq!(y, z);
    }

    #[test]
    fn soft_limit_is_transparent_below_the_knee_and_bounded() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.9), -0.9);
        assert!(soft_limit(3.0) <= 1.0 && soft_limit(3.0) > 0.99);
        assert!(soft_limit(-3.0) >= -1.0);
        assert!(soft_limit(0.95) > 0.9 && soft_limit(0.95) < 1.0);
    }

    #[test]
    fn high_bands_are_skipped_at_low_sample_rates() {
        // A 22,05 kHz la banda de 16 kHz está por encima de Nyquist×0,9: no se aplica.
        assert_eq!(peaking(22_050.0, 16_000.0, 6.0, Q), Coef::IDENTITY);
    }
}
