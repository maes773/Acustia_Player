use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::loudness::LoudnessStore;

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

    // --- Diagnóstico (contadores baratos, solo atómicos en el callback) ---
    /// Veces que CPAL ha llamado al callback.
    pub callbacks: AtomicU64,
    /// Callbacks en los que faltaron muestras mientras se reproducía
    /// (no cuenta el silencio normal tras el final de la última pista).
    pub underruns: AtomicU64,
    /// `true` desde que se pide cargar una pista hasta que el decodificador
    /// escribe el fin de flujo: permite distinguir un corte real del silencio final.
    pub streaming: AtomicBool,
    /// Fotogramas decodificados y empujados al ring buffer (acumulado).
    pub pushed_total: AtomicU64,
    /// Muestras que hay ahora mismo en el ring buffer.
    pub ring_fill: AtomicU64,
    /// Último error notificado por el stream de CPAL (no en tiempo real).
    pub stream_error: Mutex<Option<String>>,
    /// El stream de CPAL informó de un error (p. ej. se desconectaron los auriculares):
    /// el motor debe volver a abrirse sobre el dispositivo predeterminado.
    pub stream_lost: AtomicBool,

    // --- Ajustes de reproducción que lee el hilo decodificador ---
    /// Duración del crossfade en ms (0 = desactivado: gapless).
    pub crossfade_ms: AtomicU32,
    /// Normalizar la sonoridad de cada pista.
    pub normalize: AtomicBool,
    /// Mediciones de sonoridad (compartidas entre motores al cambiar de dispositivo).
    pub loudness: Arc<LoudnessStore>,
}

impl Shared {
    pub fn new(loudness: Arc<LoudnessStore>) -> Self {
        Self {
            playing: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            played_frames: AtomicU64::new(0),
            flush_req: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            boundaries: Mutex::new(VecDeque::new()),
            next_slot: Mutex::new(None),
            callbacks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            streaming: AtomicBool::new(false),
            pushed_total: AtomicU64::new(0),
            ring_fill: AtomicU64::new(0),
            stream_error: Mutex::new(None),
            stream_lost: AtomicBool::new(false),
            crossfade_ms: AtomicU32::new(0),
            normalize: AtomicBool::new(false),
            loudness,
        }
    }
}
