//! Hilo decodificador: lee archivos, convierte al formato de salida y rellena
//! el ring buffer. Es el único que toca disco; el callback de audio nunca espera por él.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};
use ringbuf::HeapProd;
use thread_priority::{set_current_thread_priority, ThreadPriority};

use crate::converter::Converter;
use crate::engine::Event;
use crate::reader::TrackReader;
use crate::shared::{Boundary, BoundaryKind, Shared, TrackRequest};

pub enum Cmd {
    Load {
        req: TrackRequest,
        start_ms: u64,
        epoch: u64,
    },
    Shutdown,
}

struct Active {
    req: TrackRequest,
    reader: TrackReader,
    conv: Option<Converter>,
    /// Fotogramas de salida producidos por esta pista (0 al llegar al final = fallo).
    produced: u64,
    /// El archivo se leyó entero y la cola del conversor ya está en `pending`:
    /// en cuanto se vacíe se encadena la pista siguiente.
    eof: bool,
}

pub struct Ctx {
    pub shared: Arc<Shared>,
    pub prod: HeapProd<f32>,
    pub out_rate: u32,
    pub out_channels: usize,
    pub events: Sender<(u64, Event)>,
}

struct State {
    ctx: Ctx,
    epoch: u64,
    /// Fotogramas empujados al ring buffer desde el último flush.
    pushed_frames: u64,
    pending: Vec<f32>,
    pending_pos: usize,
}

pub fn run(ctx: Ctx, rx: Receiver<Cmd>) {
    // Prioridad alta: este hilo no debe quedarse sin CPU frente a la interfaz
    // o al escaneo de la biblioteca. Si el sistema no lo permite, se sigue igual.
    let _ = set_current_thread_priority(ThreadPriority::Max);
    crate::platform::register_audio_thread();
    let mut st = State {
        ctx,
        epoch: 0,
        pushed_frames: 0,
        pending: Vec::new(),
        pending_pos: 0,
    };
    let mut active: Option<Active> = None;
    let mut raw: Vec<f32> = Vec::new();
    let mut converted: Vec<f32> = Vec::new();

    loop {
        // 1) Comandos. Si no hay nada que hacer, dormimos hasta recibir uno.
        let idle = active.is_none() && st.pending_pos >= st.pending.len();
        let cmd = if idle {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        } else {
            match rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Shutdown => return,
                Cmd::Load {
                    req,
                    start_ms,
                    epoch,
                } => {
                    st.epoch = epoch;
                    // `streaming` antes del flush: el callback hace pre-roll en vez
                    // de arrancar con el buffer vacío.
                    st.ctx.shared.streaming.store(true, Ordering::Relaxed);
                    st.flush();
                    raw.clear();
                    active = st.open(req, start_ms);
                    if active.is_none() {
                        // No se pudo abrir: `open` ya envió el `Error`. No se escribe
                        // `End` (generaría un `Finished` previo al error y el
                        // controlador no saltaría a la siguiente pista).
                        st.ctx.shared.streaming.store(false, Ordering::Relaxed);
                    }
                }
            }
            continue;
        }

        // 2) Vaciar lo pendiente hacia el ring buffer.
        if st.pending_pos < st.pending.len() {
            if !st.push_pending() {
                thread::sleep(Duration::from_millis(5)); // ring buffer lleno
            }
            continue;
        }

        // 3) Decodificar más audio.
        if active.as_ref().is_some_and(|a| a.eof) {
            // Fin de pista (con la cola del conversor ya vaciada): encadenar la
            // siguiente sin silencio (gapless).
            let next = st
                .ctx
                .shared
                .next_slot
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            active = match next {
                Some(req) => st.open(req, 0),
                None => None,
            };
            if active.is_none() {
                st.push_end();
            }
            continue;
        }
        let Some(a) = active.as_mut() else { continue };
        let id = a.req.id;
        match a.reader.read(&mut raw) {
            Ok(Some((rate, channels))) => {
                let conv = a.conv.get_or_insert_with(|| {
                    Converter::new(rate, channels, st.ctx.out_rate, st.ctx.out_channels)
                });
                converted.clear();
                conv.process(&raw, &mut converted);
                raw.clear();
                a.produced += (converted.len() / st.ctx.out_channels) as u64;
                st.pending.clear();
                st.pending.extend_from_slice(&converted);
                st.pending_pos = 0;
            }
            Ok(None) => {
                // Vaciar la cola del filtro de remuestreo: sin esto se perdería el
                // final de cada pista (y cada empalme gapless dejaría un salto).
                converted.clear();
                if let Some(conv) = a.conv.as_mut() {
                    conv.finish(&mut converted);
                }
                a.produced += (converted.len() / st.ctx.out_channels) as u64;
                if a.produced == 0 {
                    // Llegó al final sin haber producido ni un fotograma: no es una
                    // pista vacía legítima, es un fallo que no debe pasar en silencio.
                    let _ = st.ctx.events.send((
                        st.epoch,
                        Event::Error {
                            id,
                            message: "no se obtuvo audio del archivo".into(),
                        },
                    ));
                    st.push_end();
                    active = None;
                } else {
                    st.pending.clear();
                    st.pending.extend_from_slice(&converted);
                    st.pending_pos = 0;
                    a.eof = true;
                }
            }
            Err(e) => {
                let _ = st.ctx.events.send((
                    st.epoch,
                    Event::Error {
                        id,
                        message: e.to_string(),
                    },
                ));
                st.push_end();
                active = None;
            }
        }
    }
}

impl State {
    /// Pide al callback que vacíe el ring buffer y espera su confirmación.
    fn flush(&mut self) {
        let shared = &self.ctx.shared;
        let req = shared.flush_req.fetch_add(1, Ordering::AcqRel) + 1;
        let deadline = Instant::now() + Duration::from_millis(300);
        while shared.flush_ack.load(Ordering::Acquire) != req && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        self.pushed_frames = 0;
        self.pending.clear();
        self.pending_pos = 0;
        if let Ok(mut b) = shared.boundaries.lock() {
            b.clear();
        }
    }

    fn open(&mut self, req: TrackRequest, start_ms: u64) -> Option<Active> {
        match TrackReader::open(&req.path, start_ms) {
            Ok(reader) => {
                let boundary = Boundary {
                    epoch: self.epoch,
                    start_frame: self.pushed_frames,
                    kind: BoundaryKind::Start {
                        req: req.clone(),
                        offset_ms: start_ms,
                        duration_ms: reader.duration_ms,
                    },
                };
                if let Ok(mut b) = self.ctx.shared.boundaries.lock() {
                    b.push_back(boundary);
                }
                Some(Active {
                    req,
                    reader,
                    conv: None,
                    produced: 0,
                    eof: false,
                })
            }
            Err(e) => {
                let _ = self.ctx.events.send((
                    self.epoch,
                    Event::Error {
                        id: req.id,
                        message: e.to_string(),
                    },
                ));
                None
            }
        }
    }

    fn push_end(&mut self) {
        let boundary = Boundary {
            epoch: self.epoch,
            start_frame: self.pushed_frames,
            kind: BoundaryKind::End,
        };
        if let Ok(mut b) = self.ctx.shared.boundaries.lock() {
            b.push_back(boundary);
        }
        // Ya no se producirá más audio: el silencio posterior no es un corte.
        self.ctx.shared.streaming.store(false, Ordering::Relaxed);
    }

    /// Empuja lo que quepa (solo fotogramas completos). `false` si no hay sitio.
    fn push_pending(&mut self) -> bool {
        let ch = self.ctx.out_channels;
        let vacant = self.ctx.prod.vacant_len();
        let vacant = vacant - vacant % ch;
        if vacant == 0 {
            return false;
        }
        let remaining = &self.pending[self.pending_pos..];
        let n = remaining.len().min(vacant);
        let pushed = self.ctx.prod.push_slice(&remaining[..n]);
        self.pending_pos += pushed;
        self.pushed_frames += (pushed / ch) as u64;
        self.ctx
            .shared
            .pushed_total
            .fetch_add((pushed / ch) as u64, Ordering::Relaxed);
        true
    }
}
