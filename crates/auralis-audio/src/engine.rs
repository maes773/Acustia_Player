use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::decoder_thread::{self, Cmd, Ctx};
use crate::error::AudioError;
use crate::loudness::LoudnessStore;
use crate::output::{self, Output};
use crate::shared::{BoundaryKind, Shared, TrackRequest};

/// Eventos que el motor entrega en `poll()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// El audio de la pista `id` acaba de empezar a sonar (también al encadenar gapless).
    TrackStarted { id: u64, duration_ms: Option<u64> },
    /// Se acabó el audio y no había pista siguiente.
    Finished,
    /// No se pudo abrir o decodificar la pista `id`.
    Error { id: u64, message: String },
}

/// Topes de los fundidos que hace el motor al pausar, reanudar o cambiar de pista a mano
/// (el crossfade configurable puede llegar a 12 s, pero pausar no debe tardar tanto).
const PAUSE_FADE_MAX_MS: u32 = 1000;
const SKIP_FADE_OUT_MAX_MS: u32 = 1500;
const SKIP_FADE_IN_MAX_MS: u32 = 700;
/// Fundido breve para saltos dentro de la misma pista (mover la barra de progreso).
const QUICK_FADE_MS: u32 = 25;

struct CurrentTrack {
    start_frame: u64,
    offset_ms: u64,
}

/// Motor de reproducción. Debe crearse y usarse desde un único hilo
/// (el stream de CPAL no es `Send`); el trabajo pesado va en hilos internos.
pub struct Engine {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    events_rx: Receiver<(u64, Event)>,
    thread: Option<JoinHandle<()>>,
    output: Output,
    current: Option<CurrentTrack>,
}

impl Engine {
    pub fn new() -> Result<Self, AudioError> {
        Self::with_loudness(LoudnessStore::new())
    }

    /// Como [`new`](Self::new) pero compartiendo las mediciones de sonoridad de otro
    /// motor (así no se pierden al reabrir la salida de audio).
    pub fn with_loudness(loudness: Arc<LoudnessStore>) -> Result<Self, AudioError> {
        let shared = Arc::new(Shared::new(loudness));
        let (output, prod) = output::open(shared.clone())?;
        let (tx, rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();

        let ctx = Ctx {
            shared: shared.clone(),
            prod,
            out_rate: output.sample_rate,
            out_channels: output.channels,
            events: events_tx,
        };
        let thread = thread::Builder::new()
            .name("auralis-decoder".into())
            .spawn(move || decoder_thread::run(ctx, rx))?;

        Ok(Self {
            shared,
            tx,
            events_rx,
            thread: Some(thread),
            output,
            current: None,
        })
    }

    /// Carga `req` empezando en `start_ms` como un **cambio de pista**: lo que suena baja
    /// a silencio y la pista nueva sube, con la duración del crossfade (con tope).
    /// Si `autoplay` es `false` queda en pausa.
    pub fn load(&mut self, req: TrackRequest, start_ms: u64, autoplay: bool) {
        let cf = self.shared.crossfade_ms.load(Ordering::Relaxed);
        self.shared
            .fade_out_ms
            .store(cf.min(SKIP_FADE_OUT_MAX_MS), Ordering::Relaxed);
        self.shared
            .fade_in_ms
            .store(cf.min(SKIP_FADE_IN_MAX_MS), Ordering::Relaxed);
        self.load_inner(req, start_ms, autoplay);
    }

    /// Como [`load`](Self::load) pero para saltar dentro de la misma pista (seek,
    /// recargas internas): fundido mínimo, para que la barra de progreso responda ya.
    pub fn load_quick(&mut self, req: TrackRequest, start_ms: u64, autoplay: bool) {
        self.shared
            .fade_out_ms
            .store(QUICK_FADE_MS, Ordering::Relaxed);
        self.shared
            .fade_in_ms
            .store(QUICK_FADE_MS, Ordering::Relaxed);
        self.load_inner(req, start_ms, autoplay);
    }

    fn load_inner(&mut self, req: TrackRequest, start_ms: u64, autoplay: bool) {
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.current = None;
        self.shared.playing.store(autoplay, Ordering::Relaxed);
        let sent = self.tx.send(Cmd::Load {
            req,
            start_ms,
            epoch,
        });
        if sent.is_err() {
            self.report("el hilo decodificador se detuvo".into());
        }
    }

    fn report(&self, message: String) {
        if let Ok(mut slot) = self.shared.stream_error.lock() {
            *slot = Some(message);
        }
    }

    /// Último error del stream de audio (CPAL) o del hilo decodificador, si lo hubo.
    pub fn take_stream_error(&self) -> Option<String> {
        self.shared
            .stream_error
            .lock()
            .ok()
            .and_then(|mut s| s.take())
    }

    /// Activa o desactiva la normalización de volumen. Afecta a lo que se decodifica
    /// a partir de ahora (hasta ~2 s de audio ya en cola suenan como estaban).
    pub fn set_normalize(&self, on: bool) {
        self.shared.normalize.store(on, Ordering::Relaxed);
    }

    /// Con la normalización activa: usar la ganancia ReplayGain de álbum (si el archivo la
    /// trae) en vez de la de pista. Sin etiquetas se usa siempre la sonoridad medida.
    pub fn set_replaygain_album(&self, on: bool) {
        self.shared.rg_album.store(on, Ordering::Relaxed);
    }

    /// Ecualizador de 10 bandas. `gains_tenths` en décimas de dB (±120). Se aplica en el
    /// callback, así que se oye al instante y con una transición suave.
    pub fn set_eq(&self, enabled: bool, gains_tenths: &[i16; crate::eq::BANDS]) {
        for (slot, g) in self.shared.eq_gains.iter().zip(gains_tenths) {
            slot.store((*g).clamp(-120, 120) as i32, Ordering::Relaxed);
        }
        self.shared.eq_enabled.store(enabled, Ordering::Relaxed);
        self.shared.eq_version.fetch_add(1, Ordering::Release);
    }

    /// Duración del crossfade entre pistas, en ms (0 = desactivado). Solo se aplica en
    /// los cambios automáticos de pista y si la duración de la pista se conoce.
    pub fn set_crossfade_ms(&self, ms: u32) {
        self.shared.crossfade_ms.store(ms, Ordering::Relaxed);
    }

    /// Mediciones de sonoridad (para guardarlas en disco y pedir análisis).
    pub fn loudness(&self) -> &Arc<LoudnessStore> {
        &self.shared.loudness
    }

    /// Nombre del dispositivo por el que está sonando.
    pub fn device_name(&self) -> &str {
        &self.output.device_name
    }

    /// `true` si el stream falló (dispositivo desconectado) o si el dispositivo
    /// predeterminado del sistema ya no es el que estamos usando (p. ej. se
    /// conectaron o quitaron los auriculares). Entonces hay que crear un `Engine` nuevo.
    /// Consulta al sistema: llamar como mucho una vez por segundo.
    pub fn needs_reopen(&self) -> bool {
        if self.shared.stream_lost.load(Ordering::Relaxed) {
            return true;
        }
        match output::default_device_name() {
            Some(name) => name != self.output.device_name,
            None => false, // sin dispositivos: se mantiene hasta que aparezca uno
        }
    }

    /// Resumen del estado interno para depuración (se muestra en la interfaz).
    pub fn diagnostics(&self) -> String {
        let s = &self.shared;
        let rate = self.output.sample_rate.max(1) as f64;
        let ch = self.output.channels.max(1) as f64;
        let fill = s.ring_fill.load(Ordering::Relaxed) as f64 / (rate * ch) * 100.0;
        format!(
            "{} · {} Hz · {} canales · {} | callbacks {} · buffer {:.0}% · underruns {} · decodificado {:.1}s · reproducido {:.1}s · {}",
            self.output.device_name,
            self.output.sample_rate,
            self.output.channels,
            self.output.format,
            s.callbacks.load(Ordering::Relaxed),
            fill,
            s.underruns.load(Ordering::Relaxed),
            s.pushed_total.load(Ordering::Relaxed) as f64 / rate,
            s.played_frames.load(Ordering::Relaxed) as f64 / rate,
            if s.playing.load(Ordering::Relaxed) { "play" } else { "pausa" },
        )
    }

    /// Pista que debe sonar a continuación sin silencio. `None` = nada.
    pub fn set_next(&self, next: Option<TrackRequest>) {
        if let Ok(mut slot) = self.shared.next_slot.lock() {
            *slot = next;
        }
    }

    fn set_pause_fade(&self) {
        let ms = self
            .shared
            .crossfade_ms
            .load(Ordering::Relaxed)
            .min(PAUSE_FADE_MAX_MS);
        self.shared.fade_out_ms.store(ms, Ordering::Relaxed);
        self.shared.fade_in_ms.store(ms, Ordering::Relaxed);
    }

    /// Reanuda con un fundido de entrada (la duración del crossfade, hasta 1 s).
    pub fn play(&self) {
        self.set_pause_fade();
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    /// Pausa con un fundido de salida: el audio ya en cola sigue sonando mientras baja.
    pub fn pause(&self) {
        self.set_pause_fade();
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Volumen lineal 0.0..=1.0 (se aplica una curva cuadrática, más natural al oído).
    pub fn set_volume(&self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        self.shared
            .gain_bits
            .store((v * v).to_bits(), Ordering::Relaxed);
    }

    /// Posición actual dentro de la pista que suena, en ms.
    pub fn position_ms(&self) -> u64 {
        match &self.current {
            Some(c) => {
                let played = self.shared.played_frames.load(Ordering::Relaxed);
                let frames = played.saturating_sub(c.start_frame);
                c.offset_ms + frames * 1000 / self.output.sample_rate as u64
            }
            None => 0,
        }
    }

    /// Procesa las marcas de pista alcanzadas por el audio y los errores del
    /// decodificador. Llamar periódicamente (p. ej. cada 100 ms).
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        let played = self.shared.played_frames.load(Ordering::Relaxed);

        if let Ok(mut b) = self.shared.boundaries.lock() {
            while let Some(front) = b.front() {
                let stale = front.epoch != epoch;
                if !stale && front.start_frame > played {
                    break;
                }
                let boundary = b.pop_front().expect("front existe");
                if stale {
                    continue;
                }
                match boundary.kind {
                    BoundaryKind::Start {
                        req,
                        offset_ms,
                        duration_ms,
                    } => {
                        self.current = Some(CurrentTrack {
                            start_frame: boundary.start_frame,
                            offset_ms,
                        });
                        events.push(Event::TrackStarted {
                            id: req.id,
                            duration_ms,
                        });
                    }
                    BoundaryKind::End => {
                        self.current = None;
                        events.push(Event::Finished);
                    }
                }
            }
        }

        while let Ok((ev_epoch, ev)) = self.events_rx.try_recv() {
            if ev_epoch == epoch {
                events.push(ev);
            }
        }
        events
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
