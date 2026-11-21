use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Mutex;

/// Pista que el motor debe reproducir. `id` es opaco para el motor
/// (el controlador usa el índice en la lista de pistas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRequest {
    pub id: u64,
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum BoundaryKind {
    /// El audio de esta pista empieza en `start_frame`.
    Start {
        req: TrackRequest,
        offset_ms: u64,
        duration_ms: Option<u64>,
    },
    /// No hay más audio: lo que se haya reproducido hasta aquí es el final.
    End,
}

/// Marca en el flujo de audio, en fotogramas de salida desde el último flush.
#[derive(Debug)]
pub(crate) struct Boundary {
    pub epoch: u64,
    pub start_frame: u64,
    pub kind: BoundaryKind,
}

/// Estado compartido entre el hilo de UI, el decodificador y el callback.
///
/// El callback de audio solo toca los campos atómicos.
pub(crate) struct Shared {
    pub playing: AtomicBool,
    /// Ganancia lineal (f32 en bits).
    pub gain_bits: AtomicU32,
    /// Fotogramas consumidos por el callback desde el último flush.
    pub played_frames: AtomicU64,
    /// El decodificador incrementa `flush_req`; el callback vacía el ring
    /// buffer y confirma copiando el valor en `flush_ack`.
    pub flush_req: AtomicU64,
    pub flush_ack: AtomicU64,
    /// Se incrementa en cada `load`; descarta eventos de cargas anteriores.
    pub epoch: AtomicU64,
    pub boundaries: Mutex<VecDeque<Boundary>>,
    /// Pista que se encadena (gapless) cuando termina la actual.
    pub next_slot: Mutex<Option<TrackRequest>>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            playing: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            played_frames: AtomicU64::new(0),
            flush_req: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            boundaries: Mutex::new(VecDeque::new()),
            next_slot: Mutex::new(None),
        }
    }
}
