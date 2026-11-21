use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::decoder_thread::{self, Cmd, Ctx};
use crate::error::AudioError;
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
        let shared = Arc::new(Shared::new());
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

    /// Carga `req` empezando en `start_ms`. Descarta lo que hubiera en cola de
    /// audio. Si `autoplay` es `false` queda en pausa.
    pub fn load(&mut self, req: TrackRequest, start_ms: u64, autoplay: bool) {
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.current = None;
        self.shared.playing.store(autoplay, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Load {
            req,
            start_ms,
            epoch,
        });
    }

    /// Pista que debe sonar a continuación sin silencio. `None` = nada.
    pub fn set_next(&self, next: Option<TrackRequest>) {
        if let Ok(mut slot) = self.shared.next_slot.lock() {
            *slot = next;
        }
    }

    pub fn play(&self) {
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
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
