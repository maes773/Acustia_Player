//! Controlador de reproducción: coordina la lista de pistas, la cola
//! (shuffle/repeat) y el motor de audio. No conoce Slint ni APIs de Windows,
//! así que una interfaz de Android podría reutilizarlo.
//!
//! Uso: la interfaz llama a los métodos de control y, periódicamente, a
//! [`Player::tick`], que procesa los eventos del motor y devuelve un
//! [`Snapshot`] con todo lo que hay que mostrar.

use auralis_audio::{AudioError, Engine, Event, TrackRequest};
use auralis_core::{Queue, RepeatMode, TrackInfo};

/// Si la posición supera este umbral, «anterior» reinicia la pista actual
/// en lugar de ir a la anterior (comportamiento habitual de los reproductores).
const RESTART_THRESHOLD_MS: u64 = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub current: Option<usize>,
    pub status: Status,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume: f32,
    /// Último error de reproducción, si lo hubo (se borra al empezar otra pista).
    pub error: Option<String>,
}

pub struct Player {
    engine: Engine,
    tracks: Vec<TrackInfo>,
    queue: Queue,
    status: Status,
    volume: f32,
    current: Option<usize>,
    duration_ms: Option<u64>,
    /// Mientras se espera a que el motor arranque tras un load/seek, la
    /// posición mostrada es el destino pedido (evita saltos visuales).
    pending_start: Option<u64>,
    last_error: Option<String>,
    /// Saltos consecutivos por error; evita bucles si toda la lista falla.
    error_skips: usize,
}

impl Player {
    pub fn new() -> Result<Self, AudioError> {
        let engine = Engine::new()?;
        let player = Self {
            engine,
            tracks: Vec::new(),
            queue: Queue::new(0),
            status: Status::Stopped,
            volume: 0.8,
            current: None,
            duration_ms: None,
            pending_start: None,
            last_error: None,
            error_skips: 0,
        };
        player.engine.set_volume(player.volume);
        Ok(player)
    }

    // ---- Lista de pistas -------------------------------------------------

    /// Sustituye la lista (p. ej. al elegir otra carpeta) y detiene la reproducción.
    pub fn set_tracks(&mut self, tracks: Vec<TrackInfo>) {
        self.engine.pause();
        self.engine.set_next(None);
        self.queue.reset(tracks.len());
        self.tracks = tracks;
        self.status = Status::Stopped;
        self.current = None;
        self.duration_ms = None;
        self.pending_start = None;
        self.last_error = None;
        self.error_skips = 0;
    }

    pub fn track(&self, index: usize) -> Option<&TrackInfo> {
        self.tracks.get(index)
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Actualiza datos leídos en segundo plano (etiquetas/duración).
    pub fn update_track(
        &mut self,
        index: usize,
        title: Option<String>,
        artist: Option<String>,
        duration_ms: Option<u64>,
    ) {
        if let Some(t) = self.tracks.get_mut(index) {
            if let Some(title) = title {
                t.title = title;
            }
            if artist.is_some() {
                t.artist = artist;
            }
            if duration_ms.is_some() {
                t.duration_ms = duration_ms;
            }
        }
    }

    // ---- Controles ---------------------------------------------------------

    /// El usuario elige una pista de la lista.
    pub fn play_index(&mut self, index: usize) {
        if self.queue.select(index) {
            self.start(index, 0, true);
        }
    }

    pub fn toggle_play_pause(&mut self) {
        match self.status {
            Status::Playing => {
                self.engine.pause();
                self.status = Status::Paused;
            }
            Status::Paused => {
                self.engine.play();
                self.status = Status::Playing;
            }
            Status::Stopped => {
                let index = self.current.or_else(|| self.queue.peek_next_auto());
                if let Some(i) = index {
                    self.queue.play_index(i);
                    self.start(i, 0, true);
                }
            }
        }
    }

    pub fn next(&mut self) {
        if let Some(i) = self.queue.next_manual() {
            self.start(i, 0, true);
        }
    }

    pub fn previous(&mut self) {
        let position = self.position_ms();
        if self.current.is_some() && position > RESTART_THRESHOLD_MS {
            self.seek_ms(0);
        } else if let Some(i) = self.queue.previous() {
            self.start(i, 0, true);
        } else {
            self.seek_ms(0);
        }
    }

    pub fn seek_ms(&mut self, ms: u64) {
        let Some(index) = self.current else { return };
        let ms = match self.duration_ms {
            // Dejamos un pequeño margen para no caer justo en el final.
            Some(d) => ms.min(d.saturating_sub(100)),
            None => ms,
        };
        let autoplay = self.status == Status::Playing;
        if let Some(req) = self.request(index) {
            self.pending_start = Some(ms);
            self.engine.load(req, ms, autoplay);
            if self.status == Status::Stopped {
                self.status = Status::Paused;
            }
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        self.engine.set_volume(self.volume);
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.queue.set_shuffle(on);
        self.refresh_next();
    }

    pub fn toggle_shuffle(&mut self) {
        let on = !self.queue.shuffle();
        self.set_shuffle(on);
    }

    pub fn cycle_repeat(&mut self) -> RepeatMode {
        let mode = self.queue.cycle_repeat();
        self.refresh_next();
        mode
    }

    // ---- Interno -----------------------------------------------------------

    fn request(&self, index: usize) -> Option<TrackRequest> {
        self.tracks.get(index).map(|t| TrackRequest {
            id: index as u64,
            path: t.path.clone(),
        })
    }

    /// Empieza `index` (la cola ya está posicionada por el llamador).
    fn start(&mut self, index: usize, start_ms: u64, autoplay: bool) {
        let Some(req) = self.request(index) else { return };
        self.current = Some(index);
        self.duration_ms = self.tracks[index].duration_ms;
        self.pending_start = Some(start_ms);
        self.last_error = None;
        self.engine.load(req, start_ms, autoplay);
        self.status = if autoplay { Status::Playing } else { Status::Paused };
        self.refresh_next();
    }

    /// Indica al motor qué pista encadenar (gapless) tras la actual.
    fn refresh_next(&mut self) {
        let next = if self.current.is_some() {
            self.queue.peek_next_auto().and_then(|i| self.request(i))
        } else {
            None
        };
        self.engine.set_next(next);
    }

    fn position_ms(&self) -> u64 {
        self.pending_start.unwrap_or_else(|| self.engine.position_ms())
    }

    fn display_name(&self, index: usize) -> String {
        self.tracks
            .get(index)
            .map(|t| t.file_name())
            .unwrap_or_default()
    }

    fn on_error(&mut self, failed: usize, message: String) {
        self.last_error = Some(format!("{}: {message}", self.display_name(failed)));
        if self.status == Status::Playing && self.error_skips < self.tracks.len() {
            self.error_skips += 1;
            // Si falló la pista encadenada, la cola aún apunta a la anterior:
            // la marcamos como actual para saltar a la siguiente de verdad.
            self.queue.play_index(failed);
            if let Some(next) = self.queue.next_manual() {
                if next != failed {
                    let error = self.last_error.take();
                    self.start(next, 0, true);
                    self.last_error = error;
                    return;
                }
            }
        }
        self.engine.pause();
        self.status = Status::Stopped;
        self.pending_start = None;
    }

    /// Procesa eventos del motor y devuelve el estado a mostrar.
    pub fn tick(&mut self) -> Snapshot {
        for event in self.engine.poll() {
            match event {
                Event::TrackStarted { id, duration_ms } => {
                    let index = id as usize;
                    if index < self.tracks.len() {
                        self.queue.play_index(index);
                        self.current = Some(index);
                        self.duration_ms = duration_ms.or(self.tracks[index].duration_ms);
                        self.pending_start = None;
                        self.error_skips = 0;
                        if self.status == Status::Stopped {
                            self.status = Status::Playing;
                        }
                        self.refresh_next();
                    }
                }
                Event::Finished => {
                    self.engine.pause();
                    self.status = Status::Stopped;
                    self.pending_start = None;
                }
                Event::Error { id, message } => self.on_error(id as usize, message),
            }
        }

        let position_ms = match self.status {
            Status::Stopped => 0,
            _ => {
                let p = self.position_ms();
                self.duration_ms.map_or(p, |d| p.min(d))
            }
        };
        Snapshot {
            current: self.current,
            status: self.status,
            position_ms,
            duration_ms: self.duration_ms,
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
            volume: self.volume,
            error: self.last_error.clone(),
        }
    }
}
