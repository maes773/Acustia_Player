//! Acustia Player v0.0.1 Alpha — interfaz de escritorio (Slint).
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
use auralis_library::{
    parse_year, prepare_cover, read_cover, read_full_tags, read_metadata, scan_folder, scan_paths,
    watch_folder, write_tags, CoverChange, CoverImage, ScanEvent, TagChanges, TrackMeta,
    POLL_INTERVAL, SETTLE,
};
use auralis_player::{Player, Snapshot, Status};
use slint::{ComponentHandle, Model, VecModel};

mod catalog;
mod power;
mod tray;
use catalog::{folder_names, Folder, LoadState, MetaForm};
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

/// Vigilancia de una carpeta (altas y bajas de archivos).
struct Watch {
    id: u64,
    rx: Receiver<Vec<PathBuf>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
enum CoverChoice {
    Keep,
    Remove,
    Replace(PathBuf),
}

/// Diálogo de edición de metadatos abierto.
struct EditSession {
    folder_id: u64,
    path: PathBuf,
    /// Identifica la sesión: las respuestas de hilos de una sesión anterior se descartan.
    token: u64,
    /// Valores que tiene el archivo (para escribir solo lo que cambió).
    original: MetaForm,
    loaded: bool,
    saving: bool,
    cover: CoverChoice,
    /// Si se pausó la canción que suena para escribir en su archivo:
    /// (estaba sonando, posición en ms).
    resume: Option<(bool, u64)>,
}

/// Resultado de guardar las etiquetas en el archivo.
struct Saved {
    meta: TrackMeta,
    /// Carátula a tamaño del reproductor principal.
    big_cover: Option<CoverImage>,
}

enum TagMsg {
    Loaded {
        token: u64,
        form: Option<MetaForm>,
        cover: Option<CoverImage>,
    },
    Saved {
        token: u64,
        result: Result<Saved, String>,
    },
}

/// Tamaño (px) de la carátula que se muestra en el editor.
const EDIT_COVER_PX: u32 = 264;
/// Tamaño (px) de las miniaturas de la lista (igual que en el escaneo).
const LIST_COVER_PX: u32 = 96;

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
    /// Diálogo de edición de metadatos abierto, si lo hay.
    edit: Option<EditSession>,
    next_token: u64,
    tag_tx: Sender<TagMsg>,
    tag_rx: Receiver<TagMsg>,
    /// Vigilantes de carpeta (uno por carpeta ya leída).
    watchers: Vec<Watch>,
    /// Último estado conocido del reproductor.
    last_status: Status,
    last_position_ms: u64,
    /// Se crea la primera vez que se minimiza a la bandeja.
    tray: Option<Tray>,
}

/// Ruta del archivo de ajustes: `%APPDATA%\Acustia Player\settings.txt` en Windows.
/// Si existe la carpeta de la versión anterior (`Auralis`) y aún no la nueva, se copian
/// sus archivos para no perder carpetas, ajustes y mediciones.
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
    let base = base?;
    let dir = base.join("Acustia Player");
    let old = base.join("Auralis");
    if !dir.join("settings.txt").exists() && old.join("settings.txt").exists() {
        let _ = std::fs::create_dir_all(&dir);
        for name in ["settings.txt", "loudness.txt"] {
            let _ = std::fs::copy(old.join(name), dir.join(name));
        }
    }
    Some(dir.join("settings.txt"))
}

/// Archivo con las mediciones de sonoridad, junto a los ajustes.
fn loudness_path(settings: &Option<PathBuf>) -> Option<PathBuf> {
    settings
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.join("loudness.txt"))
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
    st.watchers.retain(|w| {
        if w.id == removed.id {
            w.cancel.store(true, Ordering::Relaxed);
            false
        } else {
            true
        }
    });
    if st.edit.as_ref().map(|e| e.folder_id) == Some(removed.id) {
        st.edit = None;
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
            ui.set_current_title("Acustia Player".into());
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
            start_watch(st, id);
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
    st.last_status = s.status;
    st.last_position_ms = s.position_ms;
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

/// Empieza a vigilar la carpeta `id` (si no se hacía ya y está disponible).
fn start_watch(st: &mut AppState, id: u64) {
    if st.watchers.iter().any(|w| w.id == id) {
        return;
    }
    let Some(f) = st.folders.iter().find(|f| f.id == id) else {
        return;
    };
    if f.state != LoadState::Ready {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    watch_folder(f.path.clone(), POLL_INTERVAL, SETTLE, tx, cancel.clone());
    st.watchers.push(Watch { id, rx, cancel });
}

/// Recoge los avisos de los vigilantes y actualiza las listas.
fn drain_watch(st: &mut AppState, ui: &MainWindow) {
    let mut changes: Vec<(u64, Vec<PathBuf>)> = Vec::new();
    for w in &st.watchers {
        let mut latest = None;
        while let Ok(list) = w.rx.try_recv() {
            latest = Some(list);
        }
        if let Some(list) = latest {
            changes.push((w.id, list));
        }
    }
    for (id, paths) in changes {
        apply_folder_change(st, ui, id, paths);
    }
}

/// La carpeta `id` ahora contiene exactamente `paths`: se añaden las canciones nuevas, se
/// quitan las que ya no están y todo lo demás (etiquetas, carátulas, nombre editado) se
/// conserva. Si esa carpeta es la que suena, la música no se interrumpe.
fn apply_folder_change(st: &mut AppState, ui: &MainWindow, id: u64, paths: Vec<PathBuf>) {
    let Some(fi) = find_folder(st, id) else {
        return;
    };
    if st.folders[fi].state != LoadState::Ready {
        return;
    }
    // Una lectura de etiquetas anterior de esta carpeta usaría índices que ya no valen.
    st.jobs.retain(|j| {
        if j.id == id {
            j.cancel.store(true, Ordering::Relaxed);
            false
        } else {
            true
        }
    });
    let plan = st.folders[fi].sync_files(paths, &st.settings);

    // Las miniaturas se guardan por índice: se pasan a los índices nuevos.
    let old_keys: Vec<(u64, usize)> = st
        .covers
        .keys()
        .filter(|(f, _)| *f == id)
        .copied()
        .collect();
    let taken: Vec<(usize, slint::Image)> = old_keys
        .into_iter()
        .filter_map(|k| st.covers.remove(&k).map(|img| (k.1, img)))
        .collect();
    for (old, img) in taken {
        if let Some(Some(new)) = plan.old_to_new.get(old) {
            st.covers.insert((id, *new), img);
        }
    }

    // Etiquetas de todo lo que aún no se había leído (las canciones nuevas y las que
    // estaban pendientes cuando se canceló la lectura anterior).
    let pending: Vec<(usize, PathBuf)> = st.folders[fi]
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.meta_read)
        .map(|(i, e)| (i, e.info.path.clone()))
        .collect();
    if !pending.is_empty() {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        scan_paths(pending, tx, cancel.clone());
        st.jobs.push(ScanJob { id, rx, cancel });
    }

    if st.playing_folder == Some(id) {
        let infos: Vec<TrackInfo> = st.folders[fi]
            .entries
            .iter()
            .map(|e| e.info.clone())
            .collect();
        let current = st.player.as_mut().and_then(|p| p.sync_tracks(infos));
        let changed = current != st.shown_current;
        st.shown_current = current;
        if changed {
            set_now_playing(st, ui, true);
        }
    }
    if let Some(e) = &st.edit {
        if e.folder_id == id && !st.folders[fi].entries.iter().any(|x| x.info.path == e.path) {
            st.edit = None;
            ui.set_editing(false); // el archivo que se editaba ya no existe
        }
    }
    adopt_if_idle(st);
    refresh_folders(st, ui);
    if fi == st.selected {
        refresh_list(st, ui);
        let n = st.folders[fi].entries.len();
        let added = plan.added.len();
        let removed = plan.old_to_new.iter().filter(|n| n.is_none()).count();
        let mut msg = format!("{n} canciones");
        if added > 0 {
            msg += &format!(" · {added} nueva(s)");
        }
        if removed > 0 {
            msg += &format!(" · {removed} eliminada(s)");
        }
        ui.set_status_message(msg.into());
    }
}

/// Abre el editor de metadatos para la canción `index` de la carpeta vista.
fn begin_edit(st: &mut AppState, ui: &MainWindow, index: usize) {
    let Some(f) = st.folders.get(st.selected) else {
        return;
    };
    let Some(e) = f.entries.get(index) else {
        return;
    };
    let token = st.next_token;
    st.next_token += 1;
    let path = e.info.path.clone();
    let shown = MetaForm {
        title: e.display_title().to_string(),
        artist: e.info.artist.clone().unwrap_or_default(),
        ..MetaForm::default()
    };
    st.edit = Some(EditSession {
        folder_id: f.id,
        path: path.clone(),
        token,
        original: shown.clone(),
        loaded: false,
        saving: false,
        cover: CoverChoice::Keep,
        resume: None,
    });
    ui.set_edit_file(e.info.file_name().into());
    ui.set_edit_title(shown.title.into());
    ui.set_edit_artist(shown.artist.into());
    ui.set_edit_album("".into());
    ui.set_edit_genre("".into());
    ui.set_edit_year("".into());
    ui.set_edit_cover(slint::Image::default());
    ui.set_edit_has_cover(false);
    ui.set_edit_error("".into());
    ui.set_edit_busy(true); // hasta leer los campos completos del archivo
    ui.set_editing(true);

    // Leer los campos completos y la carátula no debe bloquear la interfaz.
    let tx = st.tag_tx.clone();
    std::thread::spawn(move || {
        let form = read_full_tags(&path).ok().map(|t| MetaForm {
            title: t.title,
            artist: t.artist,
            album: t.album,
            genre: t.genre,
            year: t.year,
        });
        let cover = read_cover(&path, EDIT_COVER_PX);
        let _ = tx.send(TagMsg::Loaded { token, form, cover });
    });
}

fn current_form(ui: &MainWindow) -> MetaForm {
    MetaForm {
        title: ui.get_edit_title().to_string(),
        artist: ui.get_edit_artist().to_string(),
        album: ui.get_edit_album().to_string(),
        genre: ui.get_edit_genre().to_string(),
        year: ui.get_edit_year().to_string(),
    }
}

/// Guarda el editor: escribe en el archivo, en un hilo, solo lo que cambió.
fn save_edit(st: &mut AppState, ui: &MainWindow) {
    let form = current_form(ui);
    let Some(sess) = st.edit.as_ref() else { return };
    if sess.saving || !sess.loaded {
        return;
    }
    if let Err(msg) = parse_year(&form.year) {
        ui.set_edit_error(msg.into());
        return;
    }
    let fc = form.changes_from(&sess.original);
    if fc.is_empty() && matches!(sess.cover, CoverChoice::Keep) {
        st.edit = None; // nada que guardar
        ui.set_editing(false);
        return;
    }
    let (token, path, cover_choice) = (sess.token, sess.path.clone(), sess.cover.clone());

    // La canción que está sonando tiene el archivo abierto: se pausa mientras se escribe
    // y después se recarga en su posición (el lector había guardado desplazamientos viejos).
    let is_playing_track = st.playing_folder == Some(sess.folder_id)
        && st
            .shown_current
            .and_then(|i| st.player.as_ref().and_then(|p| p.track(i)))
            .is_some_and(|t| t.path == path);
    let mut resume = None;
    if is_playing_track {
        let was_playing = st.last_status == Status::Playing;
        resume = Some((was_playing, st.last_position_ms));
        if was_playing {
            if let Some(p) = st.player.as_mut() {
                p.toggle_play_pause();
            }
        }
    }
    if let Some(s) = st.edit.as_mut() {
        s.saving = true;
        s.resume = resume;
    }
    ui.set_edit_busy(true);
    ui.set_edit_error("".into());

    let tx = st.tag_tx.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let cover = match cover_choice {
                CoverChoice::Keep => CoverChange::Keep,
                CoverChoice::Remove => CoverChange::Remove,
                CoverChoice::Replace(img) => CoverChange::Replace(prepare_cover(&img)?),
            };
            let changes = TagChanges {
                title: fc.title,
                artist: fc.artist,
                album: fc.album,
                genre: fc.genre,
                year: fc.year,
                cover,
            };
            write_tags(&path, &changes)?;
            // Se vuelve a leer el archivo: lo que se muestra es lo que quedó escrito.
            Ok(Saved {
                meta: read_metadata(&path, LIST_COVER_PX),
                big_cover: read_cover(&path, MAIN_COVER_PX),
            })
        })();
        let _ = tx.send(TagMsg::Saved { token, result });
    });
}

/// Devuelve el reproductor a como estaba antes de escribir en el archivo que sonaba.
fn resume_after_write(st: &mut AppState, resume: Option<(bool, u64)>) {
    let Some((was_playing, position)) = resume else {
        return;
    };
    if let Some(p) = st.player.as_mut() {
        p.seek_ms(position); // reabre el archivo (ya modificado) en esa posición
        if was_playing {
            p.toggle_play_pause();
        }
    }
}

fn refresh_row(st: &mut AppState, ui: &MainWindow, fi: usize, index: usize) {
    if fi != st.selected {
        return;
    }
    if st.query.is_empty() {
        if let Ok(pos) = st.visible.binary_search(&index) {
            let id = st.folders[fi].id;
            let current = st.playing_folder == Some(id) && st.shown_current == Some(index);
            let row = make_row(&st.folders[fi], &st.covers, index, current);
            st.model.set_row_data(pos, row);
        }
    } else {
        refresh_list(st, ui);
        ui.set_status_message(folder_status(st).into());
    }
}

/// Respuestas de los hilos del editor.
fn drain_tags(st: &mut AppState, ui: &MainWindow) {
    while let Ok(msg) = st.tag_rx.try_recv() {
        match msg {
            TagMsg::Loaded { token, form, cover } => {
                let Some(sess) = st.edit.as_mut().filter(|s| s.token == token) else {
                    continue;
                };
                sess.loaded = true;
                if let Some(f) = form {
                    // Si el archivo no trae título, se deja el que se mostraba.
                    let title = if f.title.trim().is_empty() {
                        sess.original.title.clone()
                    } else {
                        f.title.clone()
                    };
                    let form = MetaForm { title, ..f };
                    ui.set_edit_title(form.title.clone().into());
                    ui.set_edit_artist(form.artist.clone().into());
                    ui.set_edit_album(form.album.clone().into());
                    ui.set_edit_genre(form.genre.clone().into());
                    ui.set_edit_year(form.year.clone().into());
                    sess.original = form;
                } else {
                    ui.set_edit_error(
                        "No se pudieron leer todas las etiquetas del archivo; solo se muestran el título y el artista."
                            .into(),
                    );
                }
                if let Some(c) = cover {
                    ui.set_edit_cover(to_image(&c));
                    ui.set_edit_has_cover(true);
                }
                ui.set_edit_busy(false);
            }
            TagMsg::Saved { token, result } => {
                let Some(sess) = st.edit.as_mut().filter(|s| s.token == token) else {
                    continue;
                };
                sess.saving = false;
                let (folder_id, path, resume) =
                    (sess.folder_id, sess.path.clone(), sess.resume.take());
                resume_after_write(st, resume);
                match result {
                    Err(msg) => {
                        ui.set_edit_busy(false);
                        ui.set_edit_error(
                            format!("No se pudo escribir en el archivo: {msg}").into(),
                        );
                    }
                    Ok(saved) => {
                        st.edit = None;
                        ui.set_editing(false);
                        let Some(fi) = find_folder(st, folder_id) else {
                            continue;
                        };
                        let Some(index) = st.folders[fi]
                            .entries
                            .iter()
                            .position(|e| e.info.path == path)
                        else {
                            continue;
                        };
                        st.folders[fi].entries[index]
                            .reset_tags(saved.meta.title, saved.meta.artist);
                        if saved.meta.duration_ms.is_some() {
                            st.folders[fi].entries[index].info.duration_ms = saved.meta.duration_ms;
                        }
                        // El nombre editado solo en la app ya no hace falta.
                        if st.settings.set_title(&path, None).is_some()
                            || st.settings.title_for(&path).is_some()
                        {
                            save_settings(st, ui);
                        }
                        match &saved.meta.cover {
                            Some(c) => {
                                st.covers.insert((folder_id, index), to_image(c));
                            }
                            None => {
                                st.covers.remove(&(folder_id, index));
                            }
                        }
                        if st.playing_folder == Some(folder_id) {
                            if let Some(p) = st.player.as_mut() {
                                let e = &st.folders[fi].entries[index];
                                p.update_track(
                                    index,
                                    Some(e.info.title.clone()),
                                    e.info.artist.clone(),
                                    e.info.duration_ms,
                                );
                            }
                            if st.shown_current == Some(index) {
                                set_now_playing(st, ui, true);
                                if let Some(c) = &saved.big_cover {
                                    ui.set_current_cover(to_image(c));
                                    ui.set_has_cover(true);
                                }
                            }
                        }
                        refresh_row(st, ui, fi, index);
                        ui.set_status_message("Metadatos guardados en el archivo".into());
                    }
                }
            }
        }
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
    let (tag_tx, tag_rx) = mpsc::channel();
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
        edit: None,
        next_token: 1,
        tag_tx,
        tag_rx,
        watchers: Vec::new(),
        last_status: Status::Stopped,
        last_position_ms: 0,
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
            st.settings.crossfade_secs =
                secs.clamp(0, auralis_core::MAX_CROSSFADE_SECS as i32) as u32;
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
            begin_edit(&mut state.borrow_mut(), &ui, i.max(0) as usize);
        }
    });
    ui.on_edit_cancel({
        let state = state.clone();
        move || {
            let mut st = state.borrow_mut();
            // Si se estaba escribiendo, la sesión se descarta pero el reproductor se restaura
            // cuando termine el hilo (el resultado se ignora por no coincidir el token).
            if let Some(sess) = st.edit.take() {
                if sess.saving {
                    resume_after_write(&mut st, sess.resume);
                }
            }
        }
    });
    ui.on_edit_pick_cover({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            // El diálogo se abre sin tener `state` prestado.
            let Some(path) = rfd::FileDialog::new()
                .set_title("Elige la imagen de la carátula")
                .add_filter("Imágenes", &["jpg", "jpeg", "png"])
                .pick_file()
            else {
                return;
            };
            let Some(ui) = weak.upgrade() else { return };
            match slint::Image::load_from_path(&path) {
                Ok(img) => {
                    if let Some(s) = state.borrow_mut().edit.as_mut() {
                        s.cover = CoverChoice::Replace(path);
                        ui.set_edit_cover(img);
                        ui.set_edit_has_cover(true);
                        ui.set_edit_error("".into());
                    }
                }
                Err(_) => ui.set_edit_error("No se pudo abrir esa imagen (usa JPG o PNG)".into()),
            }
        }
    });
    ui.on_edit_remove_cover({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Some(s) = state.borrow_mut().edit.as_mut() {
                s.cover = CoverChoice::Remove;
                ui.set_edit_cover(slint::Image::default());
                ui.set_edit_has_cover(false);
            }
        }
    });
    ui.on_edit_save({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                save_edit(&mut state.borrow_mut(), &ui);
            }
        }
    });
    // Alternativa cuando el archivo no se puede modificar: el título se guarda solo en la app.
    ui.on_edit_save_local({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let title = ui.get_edit_title().to_string();
            let Some(sess) = st.edit.take() else { return };
            ui.set_editing(false);
            let Some(fi) = find_folder(&st, sess.folder_id) else {
                return;
            };
            let Some(index) = st.folders[fi]
                .entries
                .iter()
                .position(|e| e.info.path == sess.path)
            else {
                return;
            };
            let effective = st.settings.set_title(&sess.path, Some(&title));
            st.folders[fi].entries[index].set_custom_title(effective);
            if st.playing_folder == Some(sess.folder_id) && st.shown_current == Some(index) {
                set_now_playing(&st, &ui, false);
            }
            refresh_row(&mut st, &ui, fi, index);
            save_settings(&st, &ui);
            ui.set_status_message(
                "Nombre guardado solo en la app (el archivo no se modificó)".into(),
            );
        }
    });

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
            drain_watch(&mut st, &ui);
            drain_tags(&mut st, &ui);
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
