//! Normalización de volumen.
//!
//! Se mide la sonoridad integrada de cada pista (ITU-R BS.1770 / EBU R128, en LUFS:
//! filtro de ponderación K, bloques de 400 ms con solape del 75 % y doble compuerta)
//! y su pico de muestra. Con eso se calcula una ganancia por pista que la lleva al
//! nivel objetivo, limitada para que el pico no pase de −1 dBFS (sin recortar).
//! La ganancia se aplica en el hilo decodificador, **antes** del control de volumen
//! de la interfaz: la barra de volumen sigue siendo el control maestro.
//!
//! Las mediciones se guardan en un [`LoudnessStore`] (memoria + texto para guardarlo
//! en disco). Una medición «parcial» (solo los primeros segundos, la que se hace al
//! vuelo cuando se toca una pista que aún no se había analizado) se sustituye por la
//! completa que calcula un hilo de segundo plano.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use thread_priority::{set_current_thread_priority, ThreadPriority};

use crate::reader::TrackReader;

/// Nivel objetivo (el mismo que «Normal» en Spotify).
pub const TARGET_LUFS: f32 = -14.0;
/// El pico de la pista, tras la ganancia, no pasa de este nivel.
const PEAK_CEILING_DB: f32 = -1.0;
/// Límites de la ganancia calculada.
const MAX_GAIN_DB: f32 = 12.0;
const MIN_GAIN_DB: f32 = -24.0;
/// Segundos que se analizan al vuelo cuando la pista no está en la caché.
pub const QUICK_SECONDS: f32 = 45.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackLoudness {
    /// Sonoridad integrada en LUFS.
    pub lufs: f32,
    /// Pico de muestra (lineal, 1.0 = fondo de escala).
    pub peak: f32,
}

impl TrackLoudness {
    /// Ganancia lineal que lleva la pista a `target` LUFS sin pasar del techo de pico.
    pub fn gain_for(&self, target: f32) -> f32 {
        let mut db = target - self.lufs;
        if self.peak > 1e-6 {
            let peak_db = 20.0 * self.peak.log10();
            db = db.min(PEAK_CEILING_DB - peak_db);
        }
        let db = db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        10f32.powf(db / 20.0)
    }
}

// ---------------------------------------------------------------------------
// Medidor BS.1770
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn process(&mut self, x: f64) -> f64 {
        // Forma directa II transpuesta.
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Filtro de ponderación K para la frecuencia `fs` (etapa de estante alto + paso alto).
fn k_filters(fs: f64) -> [Biquad; 2] {
    use std::f64::consts::PI;
    // Etapa 1: estante alto (modela la cabeza).
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let s1 = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    // Etapa 2: paso alto de 38 Hz.
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let s2 = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [s1, s2]
}

/// Peso de cada canal en la suma de potencias (BS.1770): el LFE no cuenta y los
/// envolventes traseros pesan +1,5 dB.
fn channel_weight(index: usize, channels: usize) -> f64 {
    if channels == 6 {
        match index {
            3 => 0.0,
            4 | 5 => 1.41,
            _ => 1.0,
        }
    } else {
        1.0
    }
}

pub struct Meter {
    channels: usize,
    filters: Vec<[Biquad; 2]>,
    weights: Vec<f64>,
    /// Fotogramas por subbloque de 100 ms.
    sub_len: usize,
    sub_filled: usize,
    /// Suma de cuadrados ponderada del subbloque actual.
    sub_energy: f64,
    /// Energía media (por fotograma) de los últimos 4 subbloques.
    recent: [f64; 4],
    n_sub: usize,
    /// Potencia media de cada bloque de 400 ms.
    blocks: Vec<f64>,
    peak: f32,
    frames: u64,
}

impl Meter {
    pub fn new(rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        Self {
            channels,
            filters: (0..channels).map(|_| k_filters(rate as f64)).collect(),
            weights: (0..channels).map(|c| channel_weight(c, channels)).collect(),
            sub_len: ((rate as usize) / 10).max(1),
            sub_filled: 0,
            sub_energy: 0.0,
            recent: [0.0; 4],
            n_sub: 0,
            blocks: Vec::new(),
            peak: 0.0,
            frames: 0,
        }
    }

    /// `samples` es audio entrelazado con los canales con que se creó el medidor.
    pub fn process(&mut self, samples: &[f32]) {
        let ch = self.channels;
        for frame in samples.chunks_exact(ch) {
            let mut e = 0.0;
            for (c, &s) in frame.iter().enumerate() {
                self.peak = self.peak.max(s.abs());
                let f = &mut self.filters[c];
                let shelved = f[0].process(s as f64);
                let y = f[1].process(shelved);
                e += self.weights[c] * y * y;
            }
            self.sub_energy += e;
            self.sub_filled += 1;
            self.frames += 1;
            if self.sub_filled == self.sub_len {
                self.close_subblock();
            }
        }
    }

    fn close_subblock(&mut self) {
        self.recent.rotate_left(1);
        self.recent[3] = self.sub_energy / self.sub_filled as f64;
        self.sub_energy = 0.0;
        self.sub_filled = 0;
        self.n_sub += 1;
        if self.n_sub >= 4 {
            self.blocks.push(self.recent.iter().sum::<f64>() / 4.0);
        }
    }

    /// Sonoridad integrada con doble compuerta. `None` si no hay audio audible.
    pub fn finish(mut self) -> Option<TrackLoudness> {
        // Pista de menos de 400 ms: se mide con lo que haya, como un único bloque.
        if self.blocks.is_empty() && self.frames > 0 {
            let mut total = self.sub_energy;
            let mut n = self.sub_filled;
            for r in &self.recent[(4 - self.n_sub.min(4))..] {
                total += r * self.sub_len as f64;
                n += self.sub_len;
            }
            if n > 0 {
                self.blocks.push(total / n as f64);
            }
        }
        let to_lufs = |z: f64| -0.691 + 10.0 * z.log10();
        let abs_gated: Vec<f64> = self
            .blocks
            .iter()
            .copied()
            .filter(|&z| z > 0.0 && to_lufs(z) > -70.0)
            .collect();
        if abs_gated.is_empty() {
            return None;
        }
        let mean = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
        let rel_threshold = to_lufs(mean) - 10.0;
        let rel: Vec<f64> = abs_gated
            .iter()
            .copied()
            .filter(|&z| to_lufs(z) > rel_threshold)
            .collect();
        let used = if rel.is_empty() { &abs_gated } else { &rel };
        let z = used.iter().sum::<f64>() / used.len() as f64;
        Some(TrackLoudness {
            lufs: to_lufs(z) as f32,
            peak: self.peak,
        })
    }
}

/// Analiza un archivo. `max_seconds` limita cuánto audio se mide (`None` = todo).
/// Devuelve `None` si no se pudo leer o es silencio.
pub fn analyze_file(path: &Path, max_seconds: Option<f32>) -> Option<TrackLoudness> {
    let mut reader = TrackReader::open(path, 0).ok()?;
    let mut meter: Option<Meter> = None;
    let mut fmt = (0u32, 0usize);
    let mut raw: Vec<f32> = Vec::new();
    let mut measured_frames = 0f64;
    loop {
        raw.clear();
        match reader.read(&mut raw) {
            Ok(Some((rate, ch))) => {
                if meter.is_none() || fmt != (rate, ch) {
                    // Un cambio de formato a mitad de archivo no ocurre en la práctica;
                    // si ocurre, se deja de medir en vez de mezclar formatos.
                    if meter.is_some() {
                        break;
                    }
                    meter = Some(Meter::new(rate, ch));
                    fmt = (rate, ch);
                }
                if let Some(m) = meter.as_mut() {
                    m.process(&raw);
                }
                measured_frames += (raw.len() / ch.max(1)) as f64;
                if let Some(limit) = max_seconds {
                    if measured_frames / rate.max(1) as f64 >= limit as f64 {
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    meter?.finish()
}

// ---------------------------------------------------------------------------
// Caché de mediciones + hilo de análisis en segundo plano
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    /// Tamaño y fecha del archivo cuando se midió: si cambian, la medición no vale.
    stamp: u64,
    loudness: TrackLoudness,
    /// Medición de solo los primeros segundos (no se guarda en disco).
    partial: bool,
}

struct Inner {
    cache: Mutex<HashMap<PathBuf, Entry>>,
    dirty: AtomicBool,
    /// Número de la última petición de análisis: una petición nueva cancela la anterior.
    generation: AtomicU64,
}

fn file_stamp(path: &Path) -> Option<u64> {
    let md = std::fs::metadata(path).ok()?;
    let secs = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(md.len().wrapping_mul(1_000_003) ^ secs)
}

pub struct LoudnessStore {
    inner: Arc<Inner>,
    tx: Mutex<Sender<(u64, Vec<PathBuf>)>>,
}

impl LoudnessStore {
    pub fn new() -> Arc<Self> {
        let inner = Arc::new(Inner {
            cache: Mutex::new(HashMap::new()),
            dirty: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::channel();
        let worker_inner = inner.clone();
        // El hilo termina solo cuando se destruye el almacén (se cierra el canal).
        let _ = thread::Builder::new()
            .name("auralis-loudness".into())
            .spawn(move || worker(worker_inner, rx));
        Arc::new(Self {
            inner,
            tx: Mutex::new(tx),
        })
    }

    /// Medición guardada para `path` (completa o parcial), si sigue siendo válida.
    pub fn lookup(&self, path: &Path) -> Option<TrackLoudness> {
        let stamp = file_stamp(path)?;
        let cache = self.inner.cache.lock().ok()?;
        cache
            .get(path)
            .filter(|e| e.stamp == stamp)
            .map(|e| e.loudness)
    }

    fn has_full(&self, path: &Path) -> bool {
        let Some(stamp) = file_stamp(path) else {
            return false;
        };
        self.inner
            .cache
            .lock()
            .map(|c| c.get(path).is_some_and(|e| e.stamp == stamp && !e.partial))
            .unwrap_or(false)
    }

    fn insert(&self, path: &Path, loudness: TrackLoudness, partial: bool) {
        insert(&self.inner, path, loudness, partial);
    }

    /// Medición para reproducir ahora: la guardada o, si no hay, una rápida de los
    /// primeros segundos (que se guarda como parcial). `None` = no se pudo medir.
    pub fn loudness_now(&self, path: &Path) -> Option<TrackLoudness> {
        if let Some(l) = self.lookup(path) {
            return Some(l);
        }
        let l = analyze_file(path, Some(QUICK_SECONDS))?;
        self.insert(path, l, true);
        Some(l)
    }

    /// Pide medir en segundo plano (y a baja prioridad) estas pistas completas.
    /// Sustituye a cualquier petición anterior aún sin terminar.
    pub fn analyze_in_background(&self, paths: Vec<PathBuf>) {
        let gen = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send((gen, paths));
        }
    }

    /// Hay mediciones nuevas desde la última vez que se pidió el texto.
    pub fn take_dirty(&self) -> bool {
        self.inner.dirty.swap(false, Ordering::AcqRel)
    }

    /// Mediciones completas como texto (`ruta<TAB>sello<TAB>lufs<TAB>pico` por línea).
    pub fn to_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Ok(cache) = self.inner.cache.lock() {
            for (path, e) in cache.iter().filter(|(_, e)| !e.partial) {
                let p = path.to_string_lossy();
                if p.contains(['\t', '\n', '\r']) {
                    continue;
                }
                lines.push(format!(
                    "{p}\t{}\t{}\t{}",
                    e.stamp, e.loudness.lufs, e.loudness.peak
                ));
            }
        }
        lines.sort();
        let mut out = String::from("auralis-loudness 1\n");
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
        out
    }

    /// Carga mediciones guardadas con [`to_text`](Self::to_text). Ignora lo dañado.
    pub fn load_text(&self, text: &str) {
        let Ok(mut cache) = self.inner.cache.lock() else {
            return;
        };
        for line in text.lines().skip(1) {
            let mut it = line.split('\t');
            let (Some(p), Some(s), Some(l), Some(k)) = (it.next(), it.next(), it.next(), it.next())
            else {
                continue;
            };
            let (Ok(stamp), Ok(lufs), Ok(peak)) = (s.parse(), l.parse::<f32>(), k.parse::<f32>())
            else {
                continue;
            };
            if !lufs.is_finite() || !peak.is_finite() {
                continue;
            }
            cache.insert(
                PathBuf::from(p),
                Entry {
                    stamp,
                    loudness: TrackLoudness { lufs, peak },
                    partial: false,
                },
            );
        }
    }
}

fn insert(inner: &Inner, path: &Path, loudness: TrackLoudness, partial: bool) {
    let Some(stamp) = file_stamp(path) else {
        return;
    };
    if let Ok(mut cache) = inner.cache.lock() {
        // Una medición parcial nunca pisa una completa.
        if partial
            && cache
                .get(path)
                .is_some_and(|e| e.stamp == stamp && !e.partial)
        {
            return;
        }
        cache.insert(
            path.to_path_buf(),
            Entry {
                stamp,
                loudness,
                partial,
            },
        );
        if !partial {
            inner.dirty.store(true, Ordering::Release);
        }
    }
}

fn worker(inner: Arc<Inner>, rx: Receiver<(u64, Vec<PathBuf>)>) {
    // Trabajo de fondo: que nunca compita con el hilo decodificador ni con la interfaz.
    let _ = set_current_thread_priority(ThreadPriority::Min);
    while let Ok((mut gen, mut paths)) = rx.recv() {
        // Si ya hay una petición más reciente en cola, se salta directamente a ella.
        while let Ok(newer) = rx.try_recv() {
            gen = newer.0;
            paths = newer.1;
        }
        for path in paths {
            if inner.generation.load(Ordering::Acquire) != gen {
                break; // llegó otra petición: se abandona esta
            }
            let up_to_date = file_stamp(&path).is_some_and(|stamp| {
                inner
                    .cache
                    .lock()
                    .map(|c| c.get(&path).is_some_and(|e| e.stamp == stamp && !e.partial))
                    .unwrap_or(false)
            });
            if up_to_date {
                continue;
            }
            if let Some(l) = analyze_file(&path, None) {
                insert(&inner, &path, l, false);
            }
        }
    }
}

impl LoudnessStore {
    /// ¿Hay ya una medición completa y vigente? (para pruebas y diagnóstico)
    pub fn is_complete(&self, path: &Path) -> bool {
        self.has_full(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, amp: f64, rate: u32, secs: f64, channels: usize) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        let mut v = Vec::with_capacity(n * channels);
        for i in 0..n {
            let s =
                (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32;
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    fn measure(samples: &[f32], rate: u32, ch: usize) -> Option<TrackLoudness> {
        let mut m = Meter::new(rate, ch);
        // En trozos irregulares: el resultado no debe depender del tamaño de bloque.
        for chunk in samples.chunks(ch * 1111) {
            m.process(chunk);
        }
        m.finish()
    }

    #[test]
    fn ebu_reference_997hz_minus_23_dbfs_stereo_is_minus_23_lufs() {
        // EBU Tech 3341: seno de 997 Hz a −23 dBFS en ambos canales → −23,0 LUFS.
        let amp = 10f64.powf(-23.0 / 20.0);
        for rate in [48_000u32, 44_100, 96_000] {
            let l = measure(&sine(997.0, amp, rate, 6.0, 2), rate, 2).unwrap();
            assert!((l.lufs + 23.0).abs() < 0.1, "{rate} Hz: {} LUFS", l.lufs);
            assert!((l.peak - amp as f32).abs() < 1e-3);
        }
    }

    #[test]
    fn six_db_louder_reads_six_lu_higher() {
        let a = measure(&sine(440.0, 0.05, 48_000, 5.0, 2), 48_000, 2).unwrap();
        let b = measure(&sine(440.0, 0.10, 48_000, 5.0, 2), 48_000, 2).unwrap();
        assert!(
            ((b.lufs - a.lufs) - 6.02).abs() < 0.05,
            "{} vs {}",
            a.lufs,
            b.lufs
        );
    }

    #[test]
    fn mono_counts_less_than_stereo_with_same_samples() {
        // Dos canales iguales suman el doble de potencia que uno: +3 LU.
        let m = measure(&sine(997.0, 0.1, 48_000, 5.0, 1), 48_000, 1).unwrap();
        let s = measure(&sine(997.0, 0.1, 48_000, 5.0, 2), 48_000, 2).unwrap();
        assert!(((s.lufs - m.lufs) - 3.01).abs() < 0.05);
    }

    #[test]
    fn k_weighting_attenuates_low_and_boosts_high_frequencies() {
        let mid = measure(&sine(1000.0, 0.1, 48_000, 5.0, 2), 48_000, 2).unwrap();
        let low = measure(&sine(30.0, 0.1, 48_000, 5.0, 2), 48_000, 2).unwrap();
        let high = measure(&sine(8000.0, 0.1, 48_000, 5.0, 2), 48_000, 2).unwrap();
        assert!(
            low.lufs < mid.lufs - 3.0,
            "graves {} vs {}",
            low.lufs,
            mid.lufs
        );
        assert!(
            high.lufs > mid.lufs + 2.0,
            "agudos {} vs {}",
            high.lufs,
            mid.lufs
        );
    }

    #[test]
    fn silence_has_no_loudness() {
        assert!(measure(&vec![0.0; 48_000 * 2 * 3], 48_000, 2).is_none());
    }

    #[test]
    fn quiet_passages_are_gated_out() {
        // 5 s fuertes + 5 s muy bajos (−40 dB): la compuerta relativa deja casi solo lo fuerte.
        let mut loud = sine(997.0, 0.2, 48_000, 5.0, 2);
        let only_loud = measure(&loud, 48_000, 2).unwrap();
        loud.extend(sine(997.0, 0.002, 48_000, 5.0, 2));
        let both = measure(&loud, 48_000, 2).unwrap();
        assert!(
            (both.lufs - only_loud.lufs).abs() < 0.3,
            "{} vs {}",
            both.lufs,
            only_loud.lufs
        );
    }

    #[test]
    fn very_short_clip_is_still_measured() {
        let l = measure(&sine(997.0, 0.1, 48_000, 0.2, 2), 48_000, 2);
        assert!(l.is_some());
    }

    #[test]
    fn gain_reaches_target_and_respects_peak_ceiling() {
        // Pista flojita y sin picos: sube hasta el objetivo.
        let quiet = TrackLoudness {
            lufs: -20.0,
            peak: 0.1,
        };
        let g = quiet.gain_for(-14.0);
        assert!((20.0 * g.log10() - 6.0).abs() < 0.01);
        // Pista floja pero con pico alto: la ganancia se limita para no pasar de −1 dBFS.
        let peaky = TrackLoudness {
            lufs: -20.0,
            peak: 0.9,
        };
        let g = peaky.gain_for(-14.0);
        assert!(
            0.9 * g <= 10f32.powf(-1.0 / 20.0) + 1e-4,
            "pico {}",
            0.9 * g
        );
        // Pista fuerte: baja.
        let loud = TrackLoudness {
            lufs: -8.0,
            peak: 1.0,
        };
        assert!((20.0 * loud.gain_for(-14.0).log10() + 6.0).abs() < 0.01);
        // Límites.
        let silent_ish = TrackLoudness {
            lufs: -60.0,
            peak: 0.0,
        };
        assert!((20.0 * silent_ish.gain_for(-14.0).log10() - 12.0).abs() < 0.01);
    }

    #[test]
    fn cache_text_roundtrip_ignores_garbage_and_partials() {
        let dir = std::env::temp_dir().join(format!("auralis_loud_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"yy").unwrap();
        let store = LoudnessStore::new();
        store.insert(
            &a,
            TrackLoudness {
                lufs: -9.5,
                peak: 0.8,
            },
            false,
        );
        store.insert(
            &b,
            TrackLoudness {
                lufs: -12.0,
                peak: 0.5,
            },
            true,
        ); // parcial
        let text = store.to_text();
        assert!(text.contains("a.bin") && !text.contains("b.bin"));

        let other = LoudnessStore::new();
        other.load_text(&format!("{text}basura sin tabuladores\nx\t1\tNaN\t0\n\n"));
        assert_eq!(
            other.lookup(&a),
            Some(TrackLoudness {
                lufs: -9.5,
                peak: 0.8
            })
        );
        assert!(other.lookup(&b).is_none());
        assert!(other.is_complete(&a));

        // Si el archivo cambia, la medición deja de valer.
        std::fs::write(&a, b"xxxxxxxxxxxx").unwrap();
        assert!(other.lookup(&a).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_never_overwrites_full() {
        let dir = std::env::temp_dir().join(format!("auralis_loud2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        std::fs::write(&a, b"x").unwrap();
        let store = LoudnessStore::new();
        store.insert(
            &a,
            TrackLoudness {
                lufs: -9.0,
                peak: 0.8,
            },
            false,
        );
        store.insert(
            &a,
            TrackLoudness {
                lufs: -30.0,
                peak: 0.1,
            },
            true,
        );
        assert_eq!(store.lookup(&a).unwrap().lufs, -9.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
