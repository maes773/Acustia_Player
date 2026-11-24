//! Controlador de reproducción: coordina la lista de pistas, la cola
//! (shuffle/repeat) y el motor de audio. No conoce Slint ni APIs de Windows,
//! así que una interfaz de Android podría reutilizarlo.
//!
//! Uso: la interfaz llama a los métodos de control y, periódicamente, a
//! [`Player::tick`], que procesa los eventos del motor y devuelve un
//! [`Snapshot`] con todo lo que hay que mostrar.

use std::sync::Arc;

use auralis_audio::{AudioError, Engine, Event, LoudnessStore, TrackRequest};
use auralis_core::{Queue, RepeatMode, TrackInfo, MAX_CROSSFADE_SECS};

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
    /// Ticks desde la última comprobación del dispositivo de salida.
    device_ticks: u32,
    /// Normalizar el volumen de cada pista (nivel objetivo común).
    normalize: bool,
    /// Duración del crossfade en ms (0 = desactivado).
    crossfade_ms: u32,
    /// Mediciones de sonoridad; sobreviven al cambio de dispositivo de salida.
    loudness: Arc<LoudnessStore>,
}

/// Cada cuántos `tick()` (≈100 ms) se comprueba si cambió el dispositivo de salida.
const DEVICE_CHECK_TICKS: u32 = 10;


impl Player {
    pub fn new() -> Result<Self, AudioError> {
        let loudness = LoudnessStore::new();
        let engine = Engine::with_loudness(loudness.clone())?;
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
            device_ticks: 0,
            normalize: false,
            crossfade_ms: 0,
            loudness,
        };
        player.engine.set_volume(player.volume);
        Ok(player)
    }

    // ---- Normalización y crossfade ----------------------------------------

    /// Activa o desactiva la normalización de volumen entre canciones. La barra de
    /// volumen sigue siendo el control maestro: la normalización solo iguala el nivel
    /// de partida de cada pista.
    pub fn set_normalize(&mut self, on: bool) {
        self.normalize = on;
        self.engine.set_normalize(on);
        if on {
            self.analyze_loudness_in_background();
        }
    }

    pub fn normalize(&self) -> bool {
        self.normalize
    }

    /// Duración del crossfade entre canciones, en segundos (0 = desactivado).
    /// Solo afecta a los cambios automáticos de pista, no a siguiente/anterior.
    pub fn set_crossfade_secs(&mut self, secs: u32) {
        self.crossfade_ms = secs.min(MAX_CROSSFADE_SECS) * 1000;
        self.engine.set_crossfade_ms(self.crossfade_ms);
    }

    pub fn crossfade_secs(&self) -> u32 {
        self.crossfade_ms / 1000
    }

    /// Carga mediciones de sonoridad guardadas en una sesión anterior.
    pub fn load_loudness_cache(&self, text: &str) {
        self.loudness.load_text(text);
    }

    /// Mediciones para guardar en disco, solo si hay novedades desde la última vez.
    pub fn take_loudness_cache_if_changed(&self) -> Option<String> {
        self.loudness.take_dirty().then(|| self.loudness.to_text())
    }

    /// Pide medir en segundo plano (baja prioridad) la lista actual.
    fn analyze_loudness_in_background(&self) {
        if self.normalize && !self.tracks.is_empty() {
            self.loudness
                .analyze_in_background(self.tracks.iter().map(|t| t.path.clone()).collect());
        }
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
        self.analyze_loudness_in_background();
    }

    /// Sustituye la lista por una versión actualizada de la **misma** carpeta (se
    /// añadieron o quitaron archivos) sin cortar lo que suena: la pista actual se
    /// localiza por su ruta en la lista nueva y sigue por donde iba. Devuelve su índice
    /// nuevo. Si la pista actual ya no está en la lista, la reproducción se detiene.
    pub fn sync_tracks(&mut self, tracks: Vec<TrackInfo>) -> Option<usize> {
        let new_index = self
            .current
            .and_then(|i| self.tracks.get(i))
            .and_then(|cur| tracks.iter().position(|t| t.path == cur.path));
        let Some(ni) = new_index else {
            self.set_tracks(tracks);
            return None;
        };
        let position = self.position_ms();
        self.queue.reset(tracks.len());
        self.queue.play_index(ni);
        self.tracks = tracks;
        self.current = Some(ni);
        // Los identificadores de pista del motor son índices: lo que ya tenía encadenado
        // quedó desactualizado. Se recarga la pista actual en su posición (un corte de
        // pocos ms) para que todo vuelva a coincidir.
        if matches!(self.status, Status::Playing | Status::Paused) {
            if let Some(req) = self.request(ni) {
                let autoplay = self.status == Status::Playing;
                self.pending_start = Some(position);
                self.engine.load(req, position, autoplay);
            }
        }
        self.refresh_next();
        self.analyze_loudness_in_background();
        Some(ni)
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

    /// Estado interno del motor de audio, para depuración.
    pub fn diagnostics(&self) -> String {
        self.engine.diagnostics()
    }

    /// Si el dispositivo de salida predeterminado cambió (se conectaron o quitaron
    /// unos auriculares) o el stream murió, abre un motor nuevo sobre el dispositivo
    /// actual y retoma la pista por donde iba, conservando pausa/reproducción.
    fn reopen_output_if_needed(&mut self) {
        if !self.engine.needs_reopen() {
            return;
        }
        // Si no se puede abrir (p. ej. no queda ningún dispositivo), se conserva el
        // motor actual y se reintenta en la siguiente comprobación.
        let Ok(new_engine) = Engine::with_loudness(self.loudness.clone()) else {
            return;
        };
        let position = self.position_ms();
        new_engine.set_volume(self.volume);
        new_engine.set_normalize(self.normalize);
        new_engine.set_crossfade_ms(self.crossfade_ms);
        let name = new_engine.device_name().to_string();
        // El motor anterior se destruye aquí (su hilo y su stream se cierran).
        self.engine = new_engine;
        match (self.current, self.status) {
            (Some(index), Status::Playing | Status::Paused) => {
                if let Some(req) = self.request(index) {
                    let autoplay = self.status == Status::Playing;
                    self.pending_start = Some(position);
                    self.engine.load(req, position, autoplay);
                    self.refresh_next();
                }
            }
            _ => self.refresh_next(),
        }
        self.last_error = Some(format!("Salida de audio: {name}"));
    }

    /// Procesa eventos del motor y devuelve el estado a mostrar.
    pub fn tick(&mut self) -> Snapshot {
        if let Some(e) = self.engine.take_stream_error() {
            self.last_error = Some(format!("Audio: {e}"));
        }
        self.device_ticks += 1;
        if self.device_ticks >= DEVICE_CHECK_TICKS {
            self.device_ticks = 0;
            self.reopen_output_if_needed();
        }
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
