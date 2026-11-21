//! Auralis v0.0.1 Alpha — interfaz de escritorio (Slint).
//!
//! Esta capa solo traduce entre la interfaz y `auralis-player`; no contiene
//! lógica de reproducción.

// En release no se abre una consola en Windows.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use auralis_core::{format_time, TrackInfo};
use auralis_library::{read_cover, scan_folder, CoverImage, ScanEvent};
use auralis_player::{Player, Snapshot, Status};
use slint::{ComponentHandle, Model, VecModel};

slint::include_modules!();

/// Tamaño (px) de la carátula del reproductor principal.
const MAIN_COVER_PX: u32 = 540;
/// Eventos de escaneo procesados por tick, para no bloquear la interfaz.
const SCAN_EVENTS_PER_TICK: usize = 24;

type CoverMsg = (usize, Option<CoverImage>);

struct AppState {
    player: Option<Player>,
    model: Rc<VecModel<TrackItem>>,
    scan_rx: Option<Receiver<ScanEvent>>,
    cancel: Arc<AtomicBool>,
    cover_tx: Sender<CoverMsg>,
    cover_rx: Receiver<CoverMsg>,
    shown_current: Option<usize>,
    duration_ms: Option<u64>,
    total: usize,
}

fn to_image(c: &CoverImage) -> slint::Image {
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &c.rgba, c.width, c.height,
    );
    slint::Image::from_rgba8(buf)
}

fn row_from_info(info: &TrackInfo) -> TrackItem {
    TrackItem {
        title: info.title.clone().into(),
        subtitle: info.file_name().into(),
        duration: "".into(),
        cover: slint::Image::default(),
        has_cover: false,
        current: false,
    }
}

fn start_scan(st: &mut AppState, ui: &MainWindow, dir: PathBuf) {
    st.cancel.store(true, Ordering::Relaxed);
    st.cancel = Arc::new(AtomicBool::new(false));
    if let Some(p) = st.player.as_mut() {
        p.set_tracks(Vec::new());
    }
    st.model.set_vec(Vec::new());
    st.total = 0;

    let (tx, rx) = mpsc::channel();
    st.scan_rx = Some(rx);
    scan_folder(dir.clone(), tx, st.cancel.clone());

    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());
    ui.set_folder_name(name.into());
    ui.set_status_message("Buscando música…".into());
}

fn drain_scan(st: &mut AppState, ui: &MainWindow) {
    for _ in 0..SCAN_EVENTS_PER_TICK {
        let msg = match st.scan_rx.as_ref().map(|r| r.try_recv()) {
            Some(Ok(m)) => m,
            _ => break,
        };
        match msg {
            ScanEvent::Files(files) => {
                st.total = files.len();
                let rows: Vec<TrackItem> = files.iter().map(row_from_info).collect();
                st.model.set_vec(rows);
                if let Some(p) = st.player.as_mut() {
                    p.set_tracks(files);
                }
                ui.set_status_message(if st.total == 0 {
                    "No se encontraron archivos compatibles (MP3, FLAC, WAV, OGG, AIFF)".into()
                } else {
                    format!("{} canciones · leyendo etiquetas…", st.total).into()
                });
            }
            ScanEvent::Meta { index, meta } => {
                if let Some(mut item) = st.model.row_data(index) {
                    if let Some(t) = &meta.title {
                        item.title = t.clone().into();
                    }
                    if let Some(a) = &meta.artist {
                        item.subtitle = a.clone().into();
                    }
                    if let Some(d) = meta.duration_ms {
                        item.duration = format_time(d).into();
                    }
                    if let Some(c) = &meta.cover {
                        item.cover = to_image(c);
                        item.has_cover = true;
                    }
                    st.model.set_row_data(index, item);
                }
                if let Some(p) = st.player.as_mut() {
                    p.update_track(index, meta.title, meta.artist, meta.duration_ms);
                }
            }
            ScanEvent::Done => {
                st.scan_rx = None;
                ui.set_status_message(format!("{} canciones", st.total).into());
                break;
            }
        }
    }
}

fn set_current_flag(st: &AppState, index: Option<usize>, value: bool) {
    if let Some(i) = index {
        if let Some(mut row) = st.model.row_data(i) {
            row.current = value;
            st.model.set_row_data(i, row);
        }
    }
}

fn apply_snapshot(st: &mut AppState, ui: &MainWindow, s: &Snapshot) {
    // Cambio de pista: resaltar fila, título, artista y carátula.
    if s.current != st.shown_current {
        set_current_flag(st, st.shown_current, false);
        set_current_flag(st, s.current, true);
        st.shown_current = s.current;

        match s.current.and_then(|i| st.model.row_data(i)) {
            Some(row) => {
                ui.set_current_title(row.title.clone());
                ui.set_current_artist(row.subtitle.clone());
                ui.set_has_cover(row.has_cover);
                ui.set_current_cover(row.cover.clone()); // miniatura mientras carga la grande
            }
            None => {
                ui.set_current_title("Auralis".into());
                ui.set_current_artist("Abre una carpeta con música".into());
                ui.set_has_cover(false);
                ui.set_current_cover(slint::Image::default());
            }
        }

        if let (Some(i), Some(p)) = (s.current, st.player.as_ref()) {
            if let Some(track) = p.track(i) {
                let path = track.path.clone();
                let tx = st.cover_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send((i, read_cover(&path, MAIN_COVER_PX)));
                });
            }
        }
    }

    st.duration_ms = s.duration_ms;
    ui.set_playing(s.status == Status::Playing);
    ui.set_shuffle(s.shuffle);
    ui.set_repeat_mode(s.repeat.as_index());
    ui.set_duration_text(format_time(s.duration_ms.unwrap_or(0)).into());

    if ui.get_dragging() {
        // Mientras se arrastra, el tiempo sigue al dedo/cursor.
        let ms = (ui.get_progress() as f64 * s.duration_ms.unwrap_or(0) as f64) as u64;
        ui.set_position_text(format_time(ms).into());
    } else {
        ui.set_position_text(format_time(s.position_ms).into());
        let progress = match s.duration_ms {
            Some(d) if d > 0 => (s.position_ms as f64 / d as f64).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        };
        ui.set_progress(progress);
    }

    if let Some(err) = &s.error {
        ui.set_status_message(err.clone().into());
    }
}

fn drain_covers(st: &mut AppState, ui: &MainWindow) {
    while let Ok((index, cover)) = st.cover_rx.try_recv() {
        if Some(index) != st.shown_current {
            continue; // llegó tarde: ya hay otra pista
        }
        if let Some(c) = cover {
            ui.set_current_cover(to_image(&c));
            ui.set_has_cover(true);
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;

    let (player, init_error) = match Player::new() {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(format!("Sin salida de audio: {e}"))),
    };
    if let Some(msg) = init_error {
        ui.set_status_message(msg.into());
    }

    let model = Rc::new(VecModel::<TrackItem>::default());
    ui.set_tracks(model.clone().into());

    let (cover_tx, cover_rx) = mpsc::channel();
    let state = Rc::new(RefCell::new(AppState {
        player,
        model,
        scan_rx: None,
        cancel: Arc::new(AtomicBool::new(false)),
        cover_tx,
        cover_rx,
        shown_current: None,
        duration_ms: None,
        total: 0,
    }));
    if let Some(p) = state.borrow_mut().player.as_mut() {
        p.set_volume(ui.get_volume());
    }

    // Ejecuta `f` sobre el reproductor si existe.
    macro_rules! with_player {
        ($state:expr, $p:ident => $body:expr) => {{
            if let Some($p) = $state.borrow_mut().player.as_mut() {
                $body;
            }
        }};
    }

    ui.on_pick_folder({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            // El diálogo se abre SIN tener `state` prestado: mientras está abierto
            // el bucle de eventos sigue procesando el temporizador.
            let Some(dir) = rfd::FileDialog::new()
                .set_title("Selecciona tu carpeta de música")
                .pick_folder()
            else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                start_scan(&mut state.borrow_mut(), &ui, dir);
            }
        }
    });
    ui.on_play_index({
        let state = state.clone();
        move |i| with_player!(state, p => p.play_index(i as usize))
    });
    ui.on_toggle_play({
        let state = state.clone();
        move || with_player!(state, p => p.toggle_play_pause())
    });
    ui.on_next_track({
        let state = state.clone();
        move || with_player!(state, p => p.next())
    });
    ui.on_previous_track({
        let state = state.clone();
        move || with_player!(state, p => p.previous())
    });
    ui.on_toggle_shuffle({
        let state = state.clone();
        move || with_player!(state, p => p.toggle_shuffle())
    });
    ui.on_cycle_repeat({
        let state = state.clone();
        move || with_player!(state, p => {
            p.cycle_repeat();
        })
    });
    ui.on_set_volume({
        let state = state.clone();
        move |v| with_player!(state, p => p.set_volume(v))
    });
    ui.on_seek({
        let state = state.clone();
        move |fraction| {
            let mut st = state.borrow_mut();
            let Some(duration) = st.duration_ms else { return };
            let ms = (fraction.clamp(0.0, 1.0) as f64 * duration as f64) as u64;
            if let Some(p) = st.player.as_mut() {
                p.seek_ms(ms);
            }
        }
    });

    // Un único temporizador de 100 ms; no hay más trabajo periódico en la UI.
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), {
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            drain_scan(&mut st, &ui);
            drain_covers(&mut st, &ui);
            let snapshot = st.player.as_mut().map(|p| p.tick());
            if let Some(s) = snapshot {
                apply_snapshot(&mut st, &ui, &s);
            }
        }
    });

    ui.run()
}
