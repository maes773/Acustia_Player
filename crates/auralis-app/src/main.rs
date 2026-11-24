//! Auralis v0.0.1 Alpha — interfaz de escritorio (Slint).
//!
//! Esta capa solo traduce entre la interfaz y `auralis-player`; no contiene
//! lógica de reproducción.

// En release no se abre una consola en Windows.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use auralis_core::{format_time, Query, Settings, TrackInfo};
use auralis_library::{read_cover, scan_folder, CoverImage, ScanEvent};
use auralis_player::{Player, Snapshot, Status};
use slint::{ComponentHandle, Model, VecModel};

mod catalog;
mod power;
mod tray;
use catalog::{folder_names, Folder, LoadState};
use tray::{Tray, TrayAction};

slint::include_modules!();

/// Tamaño (px) de la carátula del reproductor principal.
const MAIN_COVER_PX: u32 = 540;
/// Eventos de escaneo procesados por tick, para no bloquear la interfaz.
const SCAN_EVENTS_PER_TICK: usize = 24;

/// (id de carpeta, índice de pista, carátula)
type CoverMsg = (u64, usize, Option<CoverImage>);

/// Lectura en segundo plano de una carpeta.
struct ScanJob {
    id: u64,
    rx: Receiver<ScanEvent>,
    cancel: Arc<AtomicBool>,
}

struct AppState {
    player: Option<Player>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// Mismo orden que `settings.folders`.
    folders: Vec<Folder>,
    next_id: u64,
    /// Carpeta que se está viendo (índice en `folders`).
    selected: usize,
    /// Carpeta cuyas canciones están en la cola del reproductor. Puede ser distinta
    /// de la que se ve: la reproducción sigue aunque se cambie de pestaña.
    playing_folder: Option<u64>,
    jobs: Vec<ScanJob>,
    query: Query,
    /// Filas mostradas: índices (ascendentes) de las canciones de la carpeta vista.
    visible: Vec<usize>,
    model: Rc<VecModel<TrackItem>>,
    folder_model: Rc<VecModel<FolderItem>>,
    /// Miniaturas por (carpeta, canción).
    covers: HashMap<(u64, usize), slint::Image>,
    cover_tx: Sender<CoverMsg>,
    cover_rx: Receiver<CoverMsg>,
    /// Canción actual (índice en la carpeta que suena) ya reflejada en la interfaz.
    shown_current: Option<usize>,
    duration_ms: Option<u64>,
    /// Canción que se está renombrando: (id de carpeta, índice).
    editing: Option<(u64, usize)>,
    /// Se crea la primera vez que se minimiza a la bandeja.
    tray: Option<Tray>,
}

/// Ruta del archivo de ajustes: `%APPDATA%\Auralis\settings.txt` en Windows.
fn settings_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("Auralis").join("settings.txt"))
}

/// Archivo con las mediciones de sonoridad, junto a los ajustes.
fn loudness_path(settings: &Option<PathBuf>) -> Option<PathBuf> {
    settings.as_ref().and_then(|p| p.parent()).map(|d| d.join("loudness.txt"))
}

/// Guarda las mediciones de sonoridad si hay novedades (escritura atómica).
fn save_loudness(st: &AppState) {
    let (Some(file), Some(p)) = (loudness_path(&st.settings_path), st.player.as_ref()) else {
        return;
    };
    if let Some(text) = p.take_loudness_cache_if_changed() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("txt.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &file).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn save_settings(st: &AppState, ui: &MainWindow) {
    if let Some(path) = &st.settings_path {
        if let Err(e) = st.settings.save(path) {
            ui.set_status_message(format!("No se pudieron guardar los ajustes: {e}").into());
        }
    }
}

fn minimize_to_tray(st: &mut AppState, ui: &MainWindow) {
    if st.tray.is_none() {
        match Tray::new() {
            Ok(t) => st.tray = Some(t),
            Err(e) => {
                ui.set_status_message(format!("Bandeja no disponible: {e}").into());
                ui.window().set_minimized(true);
                return;
            }
        }
    }
    if let Some(t) = &st.tray {
        t.set_visible(true);
    }
    let _ = ui.hide();
}

fn restore_from_tray(st: &AppState, ui: &MainWindow) {
    let _ = ui.show();
    if let Some(t) = &st.tray {
        t.set_visible(false);
    }
}

fn to_image(c: &CoverImage) -> slint::Image {
    let buf =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&c.rgba, c.width, c.height);
    slint::Image::from_rgba8(buf)
}

fn find_folder(st: &AppState, id: u64) -> Option<usize> {
    st.folders.iter().position(|f| f.id == id)
}

fn make_row(
    f: &Folder,
    covers: &HashMap<(u64, usize), slint::Image>,
    i: usize,
    current: bool,
) -> TrackItem {
    let e = &f.entries[i];
    let cover = covers.get(&(f.id, i));
    TrackItem {
        index: i as i32,
        title: e.display_title().into(),
        subtitle: e.subtitle().into(),
        duration: e
            .info
            .duration_ms
            .map(format_time)
            .unwrap_or_default()
            .into(),
        cover: cover.cloned().unwrap_or_default(),
        has_cover: cover.is_some(),
        current,
        edited: e.has_custom_title(),
    }
}

/// Reconstruye las pestañas de carpetas.
fn refresh_folders(st: &AppState, ui: &MainWindow) {
    let items: Vec<FolderItem> = st
        .folders
        .iter()
        .map(|f| FolderItem {
            name: f.name.clone().into(),
            count: f.entries.len() as i32,
            playing: st.playing_folder == Some(f.id),
            missing: f.state == LoadState::Missing,
        })
        .collect();
    st.folder_model.set_vec(items);
    ui.set_selected_folder(st.selected as i32);
}

/// Reconstruye la lista de canciones de la carpeta vista aplicando la búsqueda.
fn refresh_list(st: &mut AppState, ui: &MainWindow) {
    let Some(f) = st.folders.get(st.selected) else {
        st.visible.clear();
        st.model.set_vec(Vec::new());
        ui.set_folder_name("Ninguna carpeta seleccionada".into());
        ui.set_empty_text("Añade una carpeta para ver tu música".into());
        return;
    };
    st.visible = f.visible(&st.query);
    let playing = st.playing_folder == Some(f.id);
    let rows: Vec<TrackItem> = st
        .visible
        .iter()
        .map(|&i| make_row(f, &st.covers, i, playing && st.shown_current == Some(i)))
        .collect();
    st.model.set_vec(rows);
    ui.set_folder_name(f.path.to_string_lossy().into_owned().into());
    ui.set_empty_text(
        match f.state {
            LoadState::Missing => "La carpeta ya no existe o no está disponible",
            LoadState::NotLoaded | LoadState::Loading => "Buscando música…",
            LoadState::Ready if f.entries.is_empty() => {
                "No hay archivos compatibles (MP3, FLAC, WAV, OGG, AIFF)"
            }
            LoadState::Ready => "Ninguna canción coincide con la búsqueda",
        }
        .into(),
    );
}

/// Texto de la línea de estado para la carpeta vista.
fn folder_status(st: &AppState) -> String {
    let Some(f) = st.folders.get(st.selected) else {
        return String::new();
    };
    match f.state {
        LoadState::Missing => "La carpeta no está disponible (¿disco desconectado?)".to_string(),
        LoadState::NotLoaded | LoadState::Loading => "Buscando música…".to_string(),
        LoadState::Ready if !st.query.is_empty() => {
            format!("{} de {} canciones", st.visible.len(), f.entries.len())
        }
        LoadState::Ready => format!("{} canciones", f.entries.len()),
    }
}

/// Empieza a leer la carpeta `idx` si aún no se ha leído (o si faltaba).
fn ensure_loaded(st: &mut AppState, idx: usize) {
    let Some(f) = st.folders.get_mut(idx) else {
        return;
    };
    if !matches!(f.state, LoadState::NotLoaded | LoadState::Missing) {
        return;
    }
    if !f.path.is_dir() {
        f.state = LoadState::Missing;
        return;
    }
    f.state = LoadState::Loading;
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    scan_folder(f.path.clone(), tx, cancel.clone());
    st.jobs.push(ScanJob {
        id: f.id,
        rx,
        cancel,
    });
}

/// Si no hay cola de reproducción, usa la carpeta vista (así el botón de
/// reproducir funciona nada más abrir). No interrumpe nada que esté sonando.
fn adopt_if_idle(st: &mut AppState) {
    if st.playing_folder.is_some() {
        return;
    }
    let Some(f) = st.folders.get(st.selected) else {
        return;
    };
    if f.state != LoadState::Ready || f.entries.is_empty() {
        return;
    }
    let infos: Vec<TrackInfo> = f.entries.iter().map(|e| e.info.clone()).collect();
    let id = f.id;
    if let Some(p) = st.player.as_mut() {
        p.set_tracks(infos);
        st.playing_folder = Some(id);
        st.shown_current = None;
    }
}

/// Pone la carpeta `idx` como la que suena (cola nueva). Devuelve `false` si no hay audio.
fn make_playing(st: &mut AppState, idx: usize) -> bool {
    let Some(f) = st.folders.get(idx) else {
        return false;
    };
    if st.playing_folder == Some(f.id) {
        return true;
    }
    let infos: Vec<TrackInfo> = f.entries.iter().map(|e| e.info.clone()).collect();
    let id = f.id;
    let Some(p) = st.player.as_mut() else {
        return false;
    };
    p.set_tracks(infos);
    st.playing_folder = Some(id);
    st.shown_current = None;
    true
}

/// Muestra la carpeta `idx` (la lee si hace falta). No guarda ajustes.
fn show_folder(st: &mut AppState, ui: &MainWindow, idx: usize) {
    if idx < st.folders.len() {
        st.selected = idx;
        st.settings.select_folder(idx);
        ensure_loaded(st, idx);
        adopt_if_idle(st);
    }
    refresh_folders(st, ui);
    refresh_list(st, ui);
    ui.set_status_message(folder_status(st).into());
}

fn rename_all(st: &mut AppState) {
    let names = folder_names(&st.settings.folders);
    for (f, n) in st.folders.iter_mut().zip(names) {
        f.name = n;
    }
}

fn add_folder(st: &mut AppState, ui: &MainWindow, dir: PathBuf) {
    let (idx, is_new) = st.settings.add_folder(dir.clone());
    if is_new {
        let id = st.next_id;
        st.next_id += 1;
        st.folders.push(Folder::new(id, dir, String::new()));
        rename_all(st);
    }
    show_folder(st, ui, idx);
    save_settings(st, ui);
}

fn remove_folder(st: &mut AppState, ui: &MainWindow, idx: usize) {
    if st.settings.remove_folder(idx).is_none() || idx >= st.folders.len() {
        return;
    }
    let removed = st.folders.remove(idx);
    // Detiene su lectura en segundo plano.
    st.jobs.retain(|j| {
        if j.id == removed.id {
            j.cancel.store(true, Ordering::Relaxed);
            false
        } else {
            true
        }
    });
    st.covers.retain(|(id, _), _| *id != removed.id);
    if st.editing.map(|(id, _)| id) == Some(removed.id) {
        st.editing = None;
        ui.set_editing(false);
    }
    if st.playing_folder == Some(removed.id) {
        if let Some(p) = st.player.as_mut() {
            p.set_tracks(Vec::new());
        }
        st.playing_folder = None;
        st.shown_current = None;
        st.duration_ms = None;
        set_now_playing(st, ui, true);
    }
    rename_all(st);
    let sel = st.settings.selected;
    show_folder(st, ui, sel);
    save_settings(st, ui);
}

/// Título, artista y (si `reset_cover`) carátula del reproductor principal.
fn set_now_playing(st: &AppState, ui: &MainWindow, reset_cover: bool) {
    let entry = st
        .playing_folder
        .and_then(|id| st.folders.iter().find(|f| f.id == id))
        .and_then(|f| st.shown_current.map(|i| (f, i)))
        .and_then(|(f, i)| f.entries.get(i).map(|e| (f.id, i, e)));
    match entry {
        Some((id, i, e)) => {
            ui.set_current_title(e.display_title().into());
            ui.set_current_artist(e.subtitle().into());
            if reset_cover {
                let cover = st.covers.get(&(id, i));
                ui.set_has_cover(cover.is_some());
                ui.set_current_cover(cover.cloned().unwrap_or_default()); // miniatura mientras carga la grande
            }
        }
        None => {
            ui.set_current_title("Auralis".into());
            ui.set_current_artist("Abre una carpeta con música".into());
            if reset_cover {
                ui.set_has_cover(false);
                ui.set_current_cover(slint::Image::default());
            }
        }
    }
}

/// Procesa un evento de escaneo. Devuelve `true` si la tarea terminó.
fn handle_scan_event(
    st: &mut AppState,
    ui: &MainWindow,
    id: u64,
    msg: ScanEvent,
    dirty: &mut bool,
) -> bool {
    let Some(fi) = find_folder(st, id) else {
        return true;
    };
    match msg {
        ScanEvent::Files(files) => {
            st.folders[fi].set_files(files, &st.settings);
            st.folders[fi].state = LoadState::Ready;
            adopt_if_idle(st);
            refresh_folders(st, ui);
            if fi == st.selected {
                refresh_list(st, ui);
                let n = st.folders[fi].entries.len();
                ui.set_status_message(if n == 0 {
                    "No se encontraron archivos compatibles (MP3, FLAC, WAV, OGG, AIFF)".into()
                } else {
                    format!("{n} canciones · leyendo etiquetas…").into()
                });
            }
            false
        }
        ScanEvent::Meta { index, meta } => {
            if index >= st.folders[fi].entries.len() {
                return false;
            }
            if let Some(c) = &meta.cover {
                st.covers.insert((id, index), to_image(c));
            }
            st.folders[fi].entries[index].apply_meta(
                meta.title.clone(),
                meta.artist.clone(),
                meta.duration_ms,
            );
            if st.playing_folder == Some(id) {
                if let Some(p) = st.player.as_mut() {
                    p.update_track(index, meta.title, meta.artist, meta.duration_ms);
                }
                if st.shown_current == Some(index) {
                    set_now_playing(st, ui, false);
                }
            }
            if fi == st.selected {
                if st.query.is_empty() {
                    if let Ok(pos) = st.visible.binary_search(&index) {
                        let current =
                            st.playing_folder == Some(id) && st.shown_current == Some(index);
                        let row = make_row(&st.folders[fi], &st.covers, index, current);
                        st.model.set_row_data(pos, row);
                    }
                } else {
                    // Con una búsqueda activa, las etiquetas nuevas pueden cambiar qué canciones coinciden.
                    *dirty = true;
                }
            }
            false
        }
        ScanEvent::Done => {
            if fi == st.selected {
                ui.set_status_message(folder_status(st).into());
            }
            true
        }
    }
}

fn drain_scan(st: &mut AppState, ui: &MainWindow) {
    let mut budget = SCAN_EVENTS_PER_TICK;
    let mut dirty = false;
    let mut j = 0;
    while j < st.jobs.len() && budget > 0 {
        match st.jobs[j].rx.try_recv() {
            Ok(msg) => {
                budget -= 1;
                let id = st.jobs[j].id;
                if handle_scan_event(st, ui, id, msg, &mut dirty) {
                    st.jobs.remove(j);
                }
            }
            Err(TryRecvError::Empty) => j += 1,
            Err(TryRecvError::Disconnected) => {
                st.jobs.remove(j);
            }
        }
    }
    if dirty {
        refresh_list(st, ui);
        ui.set_status_message(folder_status(st).into());
    }
}

/// Marca o desmarca la fila `idx` como la que suena (si se está viendo esa carpeta).
fn set_current_flag(st: &AppState, idx: Option<usize>, value: bool) {
    let (Some(i), Some(f)) = (idx, st.folders.get(st.selected)) else {
        return;
    };
    if st.playing_folder != Some(f.id) {
        return;
    }
    if let Ok(pos) = st.visible.binary_search(&i) {
        if let Some(mut row) = st.model.row_data(pos) {
            row.current = value;
            st.model.set_row_data(pos, row);
        }
    }
}

fn apply_snapshot(st: &mut AppState, ui: &MainWindow, s: &Snapshot) {
    // Cambio de pista: resaltar fila, título, artista y carátula.
    if s.current != st.shown_current {
        set_current_flag(st, st.shown_current, false);
        set_current_flag(st, s.current, true);
        st.shown_current = s.current;
        set_now_playing(st, ui, true);

        if let (Some(i), Some(id), Some(p)) = (s.current, st.playing_folder, st.player.as_ref()) {
            if let Some(track) = p.track(i) {
                let path = track.path.clone();
                let tx = st.cover_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send((id, i, read_cover(&path, MAIN_COVER_PX)));
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
    while let Ok((id, index, cover)) = st.cover_rx.try_recv() {
        if Some(index) != st.shown_current || Some(id) != st.playing_folder {
            continue; // llegó tarde: ya hay otra pista
        }
        if let Some(c) = cover {
            ui.set_current_cover(to_image(&c));
            ui.set_has_cover(true);
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    // Antes de crear el audio: el hilo decodificador no debe ser limitado en segundo plano.
    power::disable_power_throttling();
    let ui = MainWindow::new()?;

    let (player, init_error) = match Player::new() {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(format!("Sin salida de audio: {e}"))),
    };

    // Ajustes guardados: carpetas, carpeta seleccionada, tema y nombres editados.
    let settings_path = settings_path();
    let (settings, load_error) = match settings_path.as_deref().map(Settings::load) {
        Some(Ok(s)) => (s, None),
        Some(Err(e)) => (
            Settings::default(),
            Some(format!("No se pudieron leer los ajustes: {e}")),
        ),
        None => (Settings::default(), None),
    };
    ui.global::<Theme>().set_dark(settings.dark);
    ui.set_normalize(settings.normalize);
    ui.set_crossfade_secs(settings.crossfade_secs as i32);

    let model = Rc::new(VecModel::<TrackItem>::default());
    ui.set_tracks(model.clone().into());
    let folder_model = Rc::new(VecModel::<FolderItem>::default());
    ui.set_folders(folder_model.clone().into());

    let names = folder_names(&settings.folders);
    let folders: Vec<Folder> = settings
        .folders
        .iter()
        .zip(names)
        .enumerate()
        .map(|(i, (path, name))| Folder::new(i as u64 + 1, path.clone(), name))
        .collect();

    let (cover_tx, cover_rx) = mpsc::channel();
    let state = Rc::new(RefCell::new(AppState {
        player,
        selected: settings.selected,
        next_id: folders.len() as u64 + 1,
        folders,
        settings,
        settings_path,
        playing_folder: None,
        jobs: Vec::new(),
        query: Query::default(),
        visible: Vec::new(),
        model,
        folder_model,
        covers: HashMap::new(),
        cover_tx,
        cover_rx,
        shown_current: None,
        duration_ms: None,
        editing: None,
        tray: None,
    }));
    {
        let mut st = state.borrow_mut();
        let loudness_file = loudness_path(&st.settings_path);
        let (normalize, crossfade) = (st.settings.normalize, st.settings.crossfade_secs);
        if let Some(p) = st.player.as_mut() {
            p.set_volume(ui.get_volume());
            if let Some(text) = loudness_file.and_then(|f| std::fs::read(f).ok()) {
                p.load_loudness_cache(&String::from_utf8_lossy(&text));
            }
            p.set_crossfade_secs(crossfade);
            p.set_normalize(normalize);
        }
        let idx = st.selected;
        show_folder(&mut st, &ui, idx);
        if let Some(msg) = init_error.or(load_error) {
            ui.set_status_message(msg.into());
        }
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
                .set_title("Añade una carpeta de música")
                .pick_folder()
            else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                add_folder(&mut state.borrow_mut(), &ui, dir);
            }
        }
    });
    ui.on_select_folder({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                show_folder(&mut st, &ui, i.max(0) as usize);
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_remove_folder({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            if let Some(ui) = weak.upgrade() {
                remove_folder(&mut state.borrow_mut(), &ui, i.max(0) as usize);
            }
        }
    });
    ui.on_search_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |text| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                st.query = Query::new(&text);
                refresh_list(&mut st, &ui);
                ui.set_status_message(folder_status(&st).into());
            }
        }
    });
    ui.on_theme_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |dark| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                st.settings.dark = dark;
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_normalize_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.normalize = on;
            if let Some(p) = st.player.as_mut() {
                p.set_normalize(on);
            }
            save_settings(&st, &ui);
        }
    });
    ui.on_crossfade_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |secs| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.crossfade_secs = secs.clamp(0, auralis_core::MAX_CROSSFADE_SECS as i32) as u32;
            let applied = st.settings.crossfade_secs;
            if let Some(p) = st.player.as_mut() {
                p.set_crossfade_secs(applied);
            }
            save_settings(&st, &ui);
        }
    });
    ui.on_edit_track({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let i = i.max(0) as usize;
            let Some(f) = st.folders.get(st.selected) else {
                return;
            };
            let Some(e) = f.entries.get(i) else { return };
            ui.set_edit_text(e.display_title().into());
            ui.set_edit_original(e.original_title().into());
            ui.set_edit_has_custom(e.has_custom_title());
            let id = f.id;
            st.editing = Some((id, i));
            ui.set_editing(true);
        }
    });
    // `Some(text)` guarda el nombre; `None` restaura el original (un texto vacío también).
    let apply_rename = {
        let state = state.clone();
        let weak = ui.as_weak();
        move |text: Option<String>| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let Some((id, i)) = st.editing.take() else {
                return;
            };
            let Some(fi) = find_folder(&st, id) else {
                return;
            };
            let Some(path) = st.folders[fi].entries.get(i).map(|e| e.info.path.clone()) else {
                return;
            };
            let effective = st.settings.set_title(&path, text.as_deref());
            st.folders[fi].entries[i].set_custom_title(effective);
            if fi == st.selected {
                if st.query.is_empty() {
                    if let Ok(pos) = st.visible.binary_search(&i) {
                        let current = st.playing_folder == Some(id) && st.shown_current == Some(i);
                        let row = make_row(&st.folders[fi], &st.covers, i, current);
                        st.model.set_row_data(pos, row);
                    }
                } else {
                    refresh_list(&mut st, &ui);
                    ui.set_status_message(folder_status(&st).into());
                }
            }
            if st.playing_folder == Some(id) && st.shown_current == Some(i) {
                set_now_playing(&st, &ui, false);
            }
            save_settings(&st, &ui);
        }
    };
    ui.on_rename_confirmed({
        let apply = apply_rename.clone();
        move |text| apply(Some(text.to_string()))
    });
    ui.on_rename_reset(move || apply_rename(None));

    ui.on_play_index({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let mut st = state.borrow_mut();
            let sel = st.selected;
            // Tocar una canción de otra carpeta cambia la cola a esa carpeta.
            let switched = st.playing_folder != st.folders.get(sel).map(|f| f.id);
            if !make_playing(&mut st, sel) {
                return;
            }
            if switched {
                if let Some(ui) = weak.upgrade() {
                    refresh_folders(&st, &ui);
                }
            }
            if let Some(p) = st.player.as_mut() {
                p.play_index(i.max(0) as usize);
            }
        }
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
        move || {
            with_player!(state, p => {
                p.cycle_repeat();
            })
        }
    });
    ui.on_set_volume({
        let state = state.clone();
        move |v| with_player!(state, p => p.set_volume(v))
    });
    ui.on_seek({
        let state = state.clone();
        move |fraction| {
            let mut st = state.borrow_mut();
            let Some(duration) = st.duration_ms else {
                return;
            };
            let ms = (fraction.clamp(0.0, 1.0) as f64 * duration as f64) as u64;
            if let Some(p) = st.player.as_mut() {
                p.seek_ms(ms);
            }
        }
    });

    ui.on_minimize_to_tray({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                minimize_to_tray(&mut state.borrow_mut(), &ui);
            }
        }
    });

    // Cerrar con la X sale de la aplicación (la bandeja es solo para el botón de minimizar).
    ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });

    // Un único temporizador de 100 ms; no hay más trabajo periódico en la UI.
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), {
        let state = state.clone();
        let weak = ui.as_weak();
        let mut ticks = 0u32;
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            // Cada ~30 s se guardan las mediciones de sonoridad nuevas (si las hay).
            ticks += 1;
            if ticks % 300 == 0 {
                save_loudness(&st);
            }

            // Acciones del icono de bandeja (siguen funcionando con la ventana oculta).
            let actions = st.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
            for action in actions {
                match action {
                    TrayAction::Show => restore_from_tray(&st, &ui),
                    TrayAction::TogglePlay => {
                        if let Some(p) = st.player.as_mut() {
                            p.toggle_play_pause();
                        }
                    }
                    TrayAction::Quit => {
                        let _ = slint::quit_event_loop();
                    }
                }
            }

            drain_scan(&mut st, &ui);
            drain_covers(&mut st, &ui);
            let snapshot = st.player.as_mut().map(|p| p.tick());
            if let Some(s) = snapshot {
                apply_snapshot(&mut st, &ui, &s);
            }
            if ui.get_show_debug() {
                if let Some(p) = st.player.as_ref() {
                    ui.set_debug_text(p.diagnostics().into());
                }
            }
        }
    });

    // `run_event_loop_until_quit` mantiene la aplicación viva con la ventana
    // oculta (bandeja); sale con `quit_event_loop`.
    ui.show()?;
    let result = slint::run_event_loop_until_quit();
    save_loudness(&state.borrow());
    result
}
