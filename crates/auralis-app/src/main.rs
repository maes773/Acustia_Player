//! Acustia Player v0.0.1 Alpha — interfaz de escritorio (Slint).
//!
//! Esta capa solo traduce entre la interfaz y `auralis-player`; no contiene
//! lógica de reproducción.

// En release no se abre una consola en Windows.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use auralis_core::eq as coreeq;
use auralis_core::history::ago_text;
use auralis_core::stats::{self as corestats, ListenTracker, Stats};
use auralis_core::theme::{self as coretheme, Rgb, Rgba};
use auralis_core::{format_time, History, Query, Settings, TrackInfo, EQ_CUSTOM};
use auralis_library::{
    parse_year, prepare_cover, read_cover, read_full_tags, read_metadata, scan_folder, scan_paths,
    watch_folder, write_tags, CoverChange, CoverImage, ScanEvent, TagChanges, TrackMeta,
    POLL_INTERVAL, SETTLE,
};
use auralis_player::{Player, Snapshot, Status};
use slint::{ComponentHandle, Model, VecModel};

mod autostart;
mod catalog;
mod hotkeys;
mod localtime;
mod power;
mod tray;
use catalog::{folder_names, Folder, LoadState, MetaForm};
use hotkeys::{HotkeyAction, Hotkeys};
use tray::{Tray, TrayAction, TrayState};

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
    last_shuffle: bool,
    last_repeat: i32,
    /// Se crea la primera vez que se minimiza a la bandeja.
    tray: Option<Tray>,
    /// Último estado enviado al menú de la bandeja (para no reescribirlo en cada tick).
    tray_state: Option<TrayState>,
    /// Atajos globales de teclado (si el sistema los admite).
    hotkeys: Option<Hotkeys>,
    /// Color dominante de la carátula que suena (Ambient Light).
    ambient_color: Option<Rgb>,
    /// Ticks (de 100 ms) que faltan para guardar los ajustes de apariencia: mientras se
    /// arrastra un deslizador no se escribe el disco en cada paso.
    theme_save_in: u32,
    /// Mostrar solo las canciones favoritas de la carpeta vista.
    favorites_only: bool,
    history: History,
    history_path: Option<PathBuf>,
    history_model: Rc<VecModel<HistoryItem>>,
    /// Ganancias del ecualizador (dB) y sus textos: se actualizan fila a fila para no
    /// reconstruir los deslizadores mientras se arrastra uno.
    eq_model: Rc<VecModel<f32>>,
    eq_text_model: Rc<VecModel<slint::SharedString>>,
    /// Estadísticas de escucha y su archivo.
    stats: Stats,
    stats_path: Option<PathBuf>,
    stats_dirty: bool,
    tracker: ListenTracker,
    last_tick: Instant,
    /// Milisegundos de reproducción aún no convertidos en segundos enteros.
    listen_ms: u64,
    /// Tamaño y posición de la ventana normal mientras se está en modo widget.
    normal_size: Option<slint::LogicalSize>,
    normal_pos: Option<slint::PhysicalPosition>,
    /// El widget ya se colocó en su posición guardada (al arrancar en modo widget se
    /// hace tras mostrar la ventana).
    widget_placed: bool,
}

/// Tamaño del widget (px lógicos).
const WIDGET_SIZE: (f32, f32) = (380.0, 150.0);

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

/// Archivo de estadísticas, junto a los ajustes.
fn stats_path(settings: &Option<PathBuf>) -> Option<PathBuf> {
    settings
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.join("stats.txt"))
}

fn save_stats(st: &mut AppState) {
    if !st.stats_dirty {
        return;
    }
    if let Some(p) = &st.stats_path {
        if st.stats.save(p).is_ok() {
            st.stats_dirty = false;
        }
    }
}

/// Casilla «Modo podcast» de la carpeta vista y arte (CD/cassette) de la que suena.
fn sync_podcast_ui(st: &AppState, ui: &MainWindow) {
    let selected = st
        .folders
        .get(st.selected)
        .is_some_and(|f| st.settings.is_podcast(&f.path));
    let now = st
        .playing_folder
        .and_then(|id| st.folders.iter().find(|f| f.id == id))
        .is_some_and(|f| st.settings.is_podcast(&f.path));
    ui.set_podcast_mode(selected);
    ui.set_now_podcast(now);
}

/// Suma el tiempo que lleva sonando la canción actual. Se llama en cada tick (100 ms);
/// el tiempo en pausa no cuenta y un tick muy largo (app bloqueada) se recorta.
fn account_listening(st: &mut AppState, s: &Snapshot) {
    let now = Instant::now();
    let dt = now
        .duration_since(st.last_tick)
        .min(Duration::from_millis(500));
    st.last_tick = now;
    if s.status != Status::Playing {
        return;
    }
    // Repetir una canción: la posición vuelve al principio → cuenta como otra reproducción.
    if let Some(d) = s.duration_ms {
        if st.last_position_ms + 4000 >= d
            && s.position_ms < 4000
            && st.last_position_ms > s.position_ms
        {
            st.tracker.reset();
        }
    }
    st.listen_ms += dt.as_millis() as u64;
    let secs = (st.listen_ms / 1000) as u32;
    if secs == 0 {
        return;
    }
    st.listen_ms %= 1000;
    let Some(folder) = st
        .playing_folder
        .and_then(|id| st.folders.iter().find(|f| f.id == id))
    else {
        return;
    };
    let Some(entry) = s.current.and_then(|i| folder.entries.get(i)) else {
        return;
    };
    let podcast = st.settings.is_podcast(&folder.path);
    let path = entry.info.path.clone();
    let title = entry.display_title().to_string();
    let artist = entry.info.artist.clone().unwrap_or_default();
    st.stats
        .add_listen(localtime::today(), podcast, &path, &title, &artist, secs);
    if st.tracker.tick(&path, secs) {
        st.stats.add_play(&path);
    }
    st.stats_dirty = true;
}

fn stat_row(label: &str, value: String) -> StatRow {
    StatRow {
        label: label.into(),
        value: value.into(),
    }
}

/// Rellena el panel de estadísticas.
fn refresh_stats_ui(st: &AppState, ui: &MainWindow) {
    use corestats::format_duration as fd;
    let today = localtime::today();
    let r = st.stats.summary(today);
    let (mut music, mut pod) = (0usize, 0usize);
    for f in &st.folders {
        if st.settings.is_podcast(&f.path) {
            pod += f.entries.len();
        } else {
            music += f.entries.len();
        }
    }
    let mut rows = vec![
        stat_row("Escuchado hoy", fd(r.today_secs)),
        stat_row("Últimos 7 días", fd(r.week_secs)),
        stat_row("Este mes", fd(r.month_secs)),
        stat_row("Este año", fd(r.year_secs)),
        stat_row("Música escuchada (total)", fd(r.total_music_secs)),
        stat_row("Podcast escuchado (total)", fd(r.total_podcast_secs)),
        stat_row("Promedio por día con música", fd(r.avg_day_secs)),
        stat_row("Días con escucha", r.days_listened.to_string()),
        stat_row("Racha actual", format!("{} días", r.streak_days)),
        stat_row("Canciones locales (carpetas leídas)", music.to_string()),
        stat_row("Episodios de podcast (carpetas leídas)", pod.to_string()),
        stat_row("Reproducciones contadas", r.total_plays.to_string()),
    ];
    rows.push(stat_row(
        "Artista más escuchado",
        r.top_artist
            .map(|(n, s)| format!("{n} · {}", fd(s)))
            .unwrap_or_else(|| "—".into()),
    ));
    rows.push(stat_row(
        "Canción más escuchada",
        r.top_track
            .map(|(t, a, n)| {
                if a.is_empty() {
                    format!("{t} · {n}×")
                } else {
                    format!("{t} — {a} · {n}×")
                }
            })
            .unwrap_or_else(|| "—".into()),
    ));
    rows.push(stat_row(
        "Podcast más escuchado",
        r.top_podcast
            .map(|(n, s)| format!("{n} · {}", fd(s)))
            .unwrap_or_else(|| "—".into()),
    ));
    ui.set_stats_summary(Rc::new(VecModel::from(rows)).into());

    let tracks: Vec<StatRow> = st
        .stats
        .top_tracks(8)
        .into_iter()
        .map(|(t, a, n)| {
            let label = if a.is_empty() {
                t
            } else {
                format!("{t} — {a}")
            };
            stat_row(&label, format!("{n}×"))
        })
        .collect();
    ui.set_stats_top_tracks(Rc::new(VecModel::from(tracks)).into());
    let artists: Vec<StatRow> = st
        .stats
        .top_artists(8)
        .into_iter()
        .map(|(n, s)| stat_row(&n, fd(s)))
        .collect();
    ui.set_stats_top_artists(Rc::new(VecModel::from(artists)).into());

    let days = st.stats.last_days(today, 14);
    let max = days.iter().copied().max().unwrap_or(0);
    let bars: Vec<f32> = days
        .iter()
        .map(|&d| if max == 0 { 0.0 } else { d as f32 / max as f32 })
        .collect();
    ui.set_stats_bars(Rc::new(VecModel::from(bars)).into());
    ui.set_stats_bars_caption(format!("Últimos 14 días · máximo {}", fd(max)).into());
}

/// Entra o sale del modo widget (ventana compacta sin marco que recuerda su sitio).
fn set_widget_mode(st: &mut AppState, ui: &MainWindow, on: bool) {
    if ui.get_widget_mode() == on || ui.get_editing() {
        return;
    }
    let w = ui.window();
    if on {
        let scale = w.scale_factor();
        let size = w.size();
        st.normal_size = Some(slint::LogicalSize::new(
            size.width as f32 / scale,
            size.height as f32 / scale,
        ));
        st.normal_pos = Some(w.position());
        ui.set_settings_open(false);
        ui.set_stats_open(false);
        ui.set_history_open(false);
        ui.set_audio_open(false);
        ui.set_appearance_open(false);
        ui.set_widget_mode(true);
        w.set_size(slint::LogicalSize::new(WIDGET_SIZE.0, WIDGET_SIZE.1));
        place_widget(st, ui);
        st.widget_placed = true;
    } else {
        let p = w.position();
        st.settings.widget_pos = Some((p.x, p.y));
        ui.set_widget_mode(false);
        let size = st
            .normal_size
            .unwrap_or_else(|| slint::LogicalSize::new(1060.0, 680.0));
        w.set_size(size);
        if let Some(p) = st.normal_pos {
            w.set_position(p);
        }
        schedule_theme_save(st);
    }
}

/// Mueve el widget a su última posición (si hay una razonable guardada).
fn place_widget(st: &AppState, ui: &MainWindow) {
    if let Some((x, y)) = st.settings.widget_pos {
        if x > -8000 && y > -8000 {
            ui.window().set_position(slint::PhysicalPosition::new(x, y));
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

fn to_color(c: Rgba) -> slint::Color {
    slint::Color::from_argb_u8(c.a, c.r, c.g, c.b)
}

/// Calcula la paleta (modo + acento + tinte + Ambient Light) y la escribe en el tema de la UI.
fn apply_palette(st: &AppState, ui: &MainWindow) {
    let ambient = if st.settings.ambient {
        st.ambient_color
    } else {
        None
    };
    let p = coretheme::palette(
        st.settings.dark,
        st.settings.theme_hue as f32,
        st.settings.theme_tint as f32 / 100.0,
        ambient,
    );
    let t = ui.global::<Theme>();
    t.set_dark(st.settings.dark);
    t.set_bg(to_color(p.bg));
    t.set_light(to_color(p.light));
    t.set_shadow(to_color(p.shadow));
    t.set_text(to_color(p.text));
    t.set_text_dim(to_color(p.text_dim));
    t.set_accent(to_color(p.accent));
    t.set_inset_top(to_color(p.inset_top));
    t.set_inset_bottom(to_color(p.inset_bottom));
    t.set_inset_border(to_color(p.inset_border));
    t.set_row_current(to_color(p.row_current));
    t.set_row_hover(to_color(p.row_hover));
    t.set_glow(to_color(p.glow));
    t.set_glow_fade(to_color(p.glow_fade));
    ui.set_ambient_active(ambient.is_some());
}

/// Refleja matiz, tinte y preset de los ajustes en los controles del panel de apariencia.
fn sync_theme_controls(st: &AppState, ui: &MainWindow) {
    ui.set_theme_hue(st.settings.theme_hue as f32 / 359.0);
    ui.set_theme_tint(st.settings.theme_tint as f32 / 100.0);
    let preset = coretheme::identify_theme(st.settings.theme_hue, st.settings.theme_tint);
    ui.set_theme_preset(if preset == coretheme::THEME_CUSTOM {
        -1
    } else {
        preset as i32
    });
}

/// Los colores de los presets según el modo (el acento que tendría cada uno).
fn preset_swatches(dark: bool) -> Vec<slint::Color> {
    (0..coretheme::preset_count())
        .filter_map(coretheme::preset_values)
        .map(|(h, _)| to_color(coretheme::palette(dark, h as f32, 0.0, None).accent))
        .collect()
}

/// Cambios de apariencia hechos con deslizadores: se aplican al momento y se guardan en
/// cuanto el usuario deja de moverlos.
fn schedule_theme_save(st: &mut AppState) {
    st.theme_save_in = 8;
}

/// Texto breve de los atajos para el panel (la tabla completa está en `auralis-core`).
const HOTKEYS_HELP: &str = "Teclas multimedia (reproducir/pausar, siguiente, anterior, detener) · \
Ctrl+Alt+Espacio reproducir/pausar · Ctrl+Alt+→ / ← siguiente / anterior · \
Ctrl+Alt+↑ / ↓ volumen · Ctrl+Alt+S aleatorio · Ctrl+Alt+R repetir · \
Ctrl+Alt+A mostrar la ventana";

fn hotkeys_status(st: &AppState) -> String {
    if !st.settings.hotkeys {
        return String::new();
    }
    match &st.hotkeys {
        None if !cfg!(windows) => "Los atajos globales solo están disponibles en Windows.".into(),
        None => "No se pudieron activar los atajos globales.".into(),
        Some(h) if h.failed().is_empty() => String::new(),
        Some(h) => format!(
            "Otra aplicación ya usa: {}. Esos atajos no funcionarán.",
            h.failed().join(", ")
        ),
    }
}

/// Acción pedida desde la bandeja o un atajo global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    PlayPause,
    Pause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    VolumeUp,
    VolumeDown,
    Show,
    Quit,
}

impl From<TrayAction> for Command {
    fn from(a: TrayAction) -> Self {
        match a {
            TrayAction::Show => Command::Show,
            TrayAction::TogglePlay => Command::PlayPause,
            TrayAction::Next => Command::Next,
            TrayAction::Previous => Command::Previous,
            TrayAction::ToggleShuffle => Command::Shuffle,
            TrayAction::CycleRepeat => Command::Repeat,
            TrayAction::Quit => Command::Quit,
        }
    }
}

impl From<HotkeyAction> for Command {
    fn from(a: HotkeyAction) -> Self {
        match a {
            HotkeyAction::PlayPause => Command::PlayPause,
            HotkeyAction::Next => Command::Next,
            HotkeyAction::Previous => Command::Previous,
            HotkeyAction::Stop => Command::Pause,
            HotkeyAction::VolumeUp => Command::VolumeUp,
            HotkeyAction::VolumeDown => Command::VolumeDown,
            HotkeyAction::ToggleShuffle => Command::Shuffle,
            HotkeyAction::CycleRepeat => Command::Repeat,
            HotkeyAction::ShowWindow => Command::Show,
        }
    }
}

/// Ejecuta una acción sobre el reproductor. La interfaz se actualiza sola en el siguiente tick
/// (instantánea del reproductor), salvo el volumen, que vive en la propia ventana.
fn run_command(st: &mut AppState, ui: &MainWindow, cmd: Command) {
    match cmd {
        Command::Show => restore_from_tray(st, ui),
        Command::Quit => {
            let _ = slint::quit_event_loop();
        }
        Command::VolumeUp | Command::VolumeDown => {
            let step = if cmd == Command::VolumeUp {
                0.05
            } else {
                -0.05
            };
            let v = (ui.get_volume() + step).clamp(0.0, 1.0);
            ui.set_volume(v);
            if let Some(p) = st.player.as_mut() {
                p.set_volume(v);
            }
        }
        _ => {
            let status = st.last_status;
            let Some(p) = st.player.as_mut() else { return };
            match cmd {
                Command::PlayPause => p.toggle_play_pause(),
                Command::Pause => {
                    if status == Status::Playing {
                        p.toggle_play_pause();
                    }
                }
                Command::Next => p.next(),
                Command::Previous => p.previous(),
                Command::Shuffle => p.toggle_shuffle(),
                Command::Repeat => {
                    p.cycle_repeat();
                }
                _ => {}
            }
        }
    }
}

/// Activa o desactiva los atajos globales según los ajustes (primera vez: los crea).
fn apply_hotkeys(st: &mut AppState, ui: &MainWindow) {
    if st.settings.hotkeys {
        match st.hotkeys.as_mut() {
            Some(h) => h.enable(),
            None => st.hotkeys = Hotkeys::new().ok(),
        }
    } else if let Some(h) = st.hotkeys.as_mut() {
        h.disable();
    }
    ui.set_hotkeys_enabled(st.settings.hotkeys);
    ui.set_hotkeys_status(hotkeys_status(st).into());
}

/// Mantiene el menú de la bandeja al día (solo escribe si algo cambió).
fn sync_tray(st: &mut AppState) {
    let Some(t) = st.tray.as_ref() else { return };
    let now_playing = st
        .playing_folder
        .and_then(|id| st.folders.iter().find(|f| f.id == id))
        .and_then(|f| st.shown_current.and_then(|i| f.entries.get(i)))
        .map(|e| {
            let artist = e.info.artist.as_deref().unwrap_or("");
            if artist.is_empty() {
                e.display_title().to_string()
            } else {
                format!("{} — {artist}", e.display_title())
            }
        })
        .unwrap_or_default();
    let now = TrayState {
        playing: st.last_status == Status::Playing,
        shuffle: st.last_shuffle,
        repeat: st.last_repeat,
        now_playing,
    };
    if st.tray_state.as_ref() != Some(&now) {
        t.set_state(&now);
        st.tray_state = Some(now);
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

fn history_path(settings: &Option<PathBuf>) -> Option<PathBuf> {
    settings
        .as_deref()
        .and_then(Path::parent)
        .map(|d| d.join("history.txt"))
}

fn save_history(st: &AppState) {
    if let Some(p) = &st.history_path {
        let _ = st.history.save(p); // el historial es secundario: un fallo no molesta
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Reconstruye la lista del historial (las 100 más recientes).
fn refresh_history(st: &AppState) {
    let now = now_secs();
    let known: HashSet<&Path> = st
        .folders
        .iter()
        .flat_map(|f| f.entries.iter().map(|e| e.info.path.as_path()))
        .collect();
    let items: Vec<HistoryItem> = st
        .history
        .entries()
        .iter()
        .take(100)
        .map(|e| {
            let ago = ago_text(now, e.played_at);
            HistoryItem {
                title: e.title.clone().into(),
                subtitle: if e.artist.is_empty() {
                    ago
                } else {
                    format!("{} · {}", e.artist, ago)
                }
                .into(),
                available: known.contains(e.path.as_path()),
            }
        })
        .collect();
    st.history_model.set_vec(items);
}

/// Anota en el historial la canción `index` de la carpeta que suena.
fn record_history(st: &mut AppState, ui: &MainWindow, index: usize) {
    let Some(f) = st
        .playing_folder
        .and_then(|id| st.folders.iter().find(|f| f.id == id))
    else {
        return;
    };
    let Some(e) = f.entries.get(index) else {
        return;
    };
    let (path, title, artist) = (
        e.info.path.clone(),
        e.display_title().to_string(),
        e.info.artist.clone().unwrap_or_default(),
    );
    if st.history.record(now_secs(), path, title, artist) {
        save_history(st);
        if ui.get_history_open() {
            refresh_history(st);
        }
    }
}

/// Reproduce la canción `path` buscándola en las carpetas añadidas: cambia a su pestaña
/// y la pone en cola. `false` si ya no está en ninguna.
fn play_path(st: &mut AppState, ui: &MainWindow, path: &Path) -> bool {
    let found = st.folders.iter().enumerate().find_map(|(fi, f)| {
        f.entries
            .iter()
            .position(|e| e.info.path == path)
            .map(|ti| (fi, ti))
    });
    let Some((fi, ti)) = found else { return false };
    show_folder(st, ui, fi);
    if !make_playing(st, fi) {
        return false;
    }
    refresh_folders(st, ui);
    if let Some(p) = st.player.as_mut() {
        p.play_index(ti);
    }
    true
}

/// «+3.5», «-2.0», «0».
fn fmt_db(tenths: i16) -> String {
    if tenths == 0 {
        "0".to_string()
    } else {
        format!("{:+.1}", tenths as f32 / 10.0)
    }
}

/// Refleja los ajustes del ecualizador en la interfaz (fila a fila si ya había datos).
fn sync_eq_ui(st: &AppState, ui: &MainWindow) {
    let gains = st.settings.eq_gains;
    let db: Vec<f32> = gains.iter().map(|&t| coreeq::tenths_to_db(t)).collect();
    let texts: Vec<slint::SharedString> = gains.iter().map(|&t| fmt_db(t).into()).collect();
    if st.eq_model.row_count() == db.len() {
        for (i, (v, t)) in db.iter().zip(texts).enumerate() {
            if st.eq_model.row_data(i) != Some(*v) {
                st.eq_model.set_row_data(i, *v);
            }
            st.eq_text_model.set_row_data(i, t);
        }
    } else {
        st.eq_model.set_vec(db);
        st.eq_text_model.set_vec(texts);
    }
    let preset = st.settings.eq_preset();
    ui.set_eq_preset(if preset == EQ_CUSTOM {
        -1
    } else {
        preset as i32
    });
    ui.set_eq_preset_label(
        coreeq::preset_name(preset as usize)
            .unwrap_or("Personalizado")
            .into(),
    );
    ui.set_eq_enabled(st.settings.eq_enabled);
}

/// Aplica el ecualizador al motor, actualiza la interfaz y guarda.
fn apply_eq(st: &mut AppState, ui: &MainWindow) {
    let (enabled, gains) = (st.settings.eq_enabled, st.settings.eq_gains);
    if let Some(p) = st.player.as_mut() {
        p.set_eq(enabled, gains);
    }
    sync_eq_ui(st, ui);
    save_settings(st, ui);
}

fn make_row(
    f: &Folder,
    settings: &Settings,
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
        favorite: settings.is_favorite(&e.info.path),
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
            podcast: st.settings.is_podcast(&f.path),
        })
        .collect();
    st.folder_model.set_vec(items);
    ui.set_selected_folder(st.selected as i32);
    sync_podcast_ui(st, ui);
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
    if st.favorites_only {
        let settings = &st.settings;
        st.visible
            .retain(|&i| settings.is_favorite(&f.entries[i].info.path));
    }
    let playing = st.playing_folder == Some(f.id);
    let rows: Vec<TrackItem> = st
        .visible
        .iter()
        .map(|&i| {
            make_row(
                f,
                &st.settings,
                &st.covers,
                i,
                playing && st.shown_current == Some(i),
            )
        })
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
            LoadState::Ready if st.favorites_only && st.query.is_empty() => {
                "Aún no hay favoritas en esta carpeta: pulsa la estrella de una canción"
            }
            LoadState::Ready if st.favorites_only => "Ninguna favorita coincide con la búsqueda",
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
        LoadState::Ready if st.favorites_only => {
            format!("★ {} de {} canciones", st.visible.len(), f.entries.len())
        }
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
    sync_podcast_ui(st, ui);
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
                        let row =
                            make_row(&st.folders[fi], &st.settings, &st.covers, index, current);
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
        if let Some(i) = s.current {
            record_history(st, ui, i);
        }

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
    st.last_shuffle = s.shuffle;
    st.last_repeat = s.repeat.as_index();
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
            let row = make_row(&st.folders[fi], &st.settings, &st.covers, index, current);
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
        let ambient = cover
            .as_ref()
            .and_then(|c| coretheme::dominant_color(&c.rgba, c.width as usize, c.height as usize));
        if let Some(c) = cover {
            ui.set_current_cover(to_image(&c));
            ui.set_has_cover(true);
        }
        // Sin carátula (o gris) no hay color que seguir: se vuelve al tema elegido.
        if ambient != st.ambient_color {
            st.ambient_color = ambient;
            if st.settings.ambient {
                apply_palette(st, ui);
            }
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

    let history_path = history_path(&settings_path);
    let history = history_path
        .as_deref()
        .and_then(|p| History::load(p).ok())
        .unwrap_or_default();
    let history_model = Rc::new(VecModel::<HistoryItem>::default());
    ui.set_history_items(history_model.clone().into());
    let eq_model = Rc::new(VecModel::<f32>::default());
    let eq_text_model = Rc::new(VecModel::<slint::SharedString>::default());
    ui.set_eq_gains(eq_model.clone().into());
    ui.set_eq_gain_texts(eq_text_model.clone().into());
    let preset_names: Vec<slint::SharedString> =
        coreeq::preset_names().into_iter().map(Into::into).collect();
    ui.set_eq_preset_names(Rc::new(VecModel::from(preset_names)).into());
    let band_names: Vec<slint::SharedString> = (0..coreeq::EQ_BANDS)
        .map(|i| coreeq::band_label(i).into())
        .collect();
    ui.set_eq_band_names(Rc::new(VecModel::from(band_names)).into());
    ui.set_replaygain_album(settings.replaygain_album);

    let stats_path = stats_path(&settings_path);
    let stats = stats_path
        .as_deref()
        .and_then(|p| Stats::load(p).ok())
        .unwrap_or_default();
    ui.set_widget_on_start(settings.widget_on_start);
    ui.set_widget_on_top(settings.widget_on_top);
    ui.set_autostart(settings.autostart);
    if !autostart::supported() {
        ui.set_autostart_status("El inicio con el equipo solo funciona en Windows.".into());
    } else if settings.autostart {
        // Renueva la ruta registrada por si se movió el programa (en segundo plano).
        std::thread::spawn(|| {
            let _ = autostart::set(true);
        });
    }
    // Arrancar en modo widget: la ventana nace ya compacta y sin marco.
    if settings.widget_on_start {
        ui.set_widget_mode(true);
    }

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
        last_shuffle: false,
        last_repeat: 0,
        last_position_ms: 0,
        tray: None,
        tray_state: None,
        hotkeys: None,
        ambient_color: None,
        theme_save_in: 0,
        favorites_only: false,
        history,
        history_path,
        history_model,
        eq_model,
        eq_text_model,
        stats,
        stats_path,
        stats_dirty: false,
        tracker: ListenTracker::default(),
        last_tick: Instant::now(),
        listen_ms: 0,
        normal_size: None,
        normal_pos: None,
        widget_placed: false,
    }));
    {
        let mut st = state.borrow_mut();
        let loudness_file = loudness_path(&st.settings_path);
        let (normalize, crossfade) = (st.settings.normalize, st.settings.crossfade_secs);
        let (rg_album, eq_on, eq_gains) = (
            st.settings.replaygain_album,
            st.settings.eq_enabled,
            st.settings.eq_gains,
        );
        if let Some(p) = st.player.as_mut() {
            p.set_replaygain_album(rg_album);
            p.set_eq(eq_on, eq_gains);
            p.set_volume(ui.get_volume());
            if let Some(text) = loudness_file.and_then(|f| std::fs::read(f).ok()) {
                p.load_loudness_cache(&String::from_utf8_lossy(&text));
            }
            p.set_crossfade_secs(crossfade);
            p.set_normalize(normalize);
        }
        sync_eq_ui(&st, &ui);
        // Apariencia: paleta inicial (sin animar para no ver un destello), presets y atajos.
        ui.set_ambient(st.settings.ambient);
        let names: Vec<slint::SharedString> = coretheme::preset_names()
            .into_iter()
            .map(Into::into)
            .collect();
        ui.set_theme_preset_names(Rc::new(VecModel::from(names)).into());
        ui.set_theme_preset_colors(
            Rc::new(VecModel::from(preset_swatches(st.settings.dark))).into(),
        );
        sync_theme_controls(&st, &ui);
        apply_palette(&st, &ui);
        ui.set_hotkeys_help(HOTKEYS_HELP.into());
        apply_hotkeys(&mut st, &ui);
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
                ui.set_theme_preset_colors(Rc::new(VecModel::from(preset_swatches(dark))).into());
                apply_palette(&st, &ui);
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_ambient_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.ambient = on;
            apply_palette(&st, &ui);
            save_settings(&st, &ui);
        }
    });
    ui.on_theme_preset_selected({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            if let Some((h, t)) = coretheme::preset_values(i.max(0) as usize) {
                st.settings.theme_hue = h;
                st.settings.theme_tint = t;
                sync_theme_controls(&st, &ui);
                apply_palette(&st, &ui);
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_theme_hue_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.theme_hue = (v.clamp(0.0, 1.0) * 359.0).round() as u16;
            let preset = coretheme::identify_theme(st.settings.theme_hue, st.settings.theme_tint);
            ui.set_theme_preset(if preset == coretheme::THEME_CUSTOM {
                -1
            } else {
                preset as i32
            });
            apply_palette(&st, &ui);
            schedule_theme_save(&mut st);
        }
    });
    ui.on_theme_tint_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.theme_tint = (v.clamp(0.0, 1.0) * 100.0).round() as u8;
            let preset = coretheme::identify_theme(st.settings.theme_hue, st.settings.theme_tint);
            ui.set_theme_preset(if preset == coretheme::THEME_CUSTOM {
                -1
            } else {
                preset as i32
            });
            apply_palette(&st, &ui);
            schedule_theme_save(&mut st);
        }
    });
    ui.on_theme_reset({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            if let Some((h, t)) = coretheme::preset_values(0) {
                st.settings.theme_hue = h;
                st.settings.theme_tint = t;
            }
            sync_theme_controls(&st, &ui);
            apply_palette(&st, &ui);
            save_settings(&st, &ui);
        }
    });
    ui.on_hotkeys_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.hotkeys = on;
            apply_hotkeys(&mut st, &ui);
            save_settings(&st, &ui);
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
    ui.on_toggle_favorite({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let (fi, index) = (st.selected, i.max(0) as usize);
            let Some(path) = st
                .folders
                .get(fi)
                .and_then(|f| f.entries.get(index))
                .map(|e| e.info.path.clone())
            else {
                return;
            };
            let now_fav = st.settings.toggle_favorite(&path);
            save_settings(&st, &ui);
            if st.favorites_only {
                // La lista cambia (una canción deja de aparecer): se reconstruye.
                refresh_list(&mut st, &ui);
            } else {
                refresh_row(&mut st, &ui, fi, index);
            }
            ui.set_status_message(
                if now_fav {
                    "Añadida a favoritos".to_string()
                } else if st.favorites_only {
                    folder_status(&st)
                } else {
                    "Quitada de favoritos".to_string()
                }
                .into(),
            );
        }
    });
    ui.on_favorites_only_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.favorites_only = on;
            refresh_list(&mut st, &ui);
            ui.set_status_message(folder_status(&st).into());
        }
    });
    ui.on_history_opened({
        let state = state.clone();
        move || refresh_history(&state.borrow())
    });
    ui.on_history_play({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let Some(path) = st
                .history
                .entries()
                .get(i.max(0) as usize)
                .map(|e| e.path.clone())
            else {
                return;
            };
            if play_path(&mut st, &ui, &path) {
                ui.set_history_open(false);
                save_settings(&st, &ui);
            } else {
                ui.set_status_message("Esa canción ya no está en ninguna de tus carpetas".into());
            }
        }
    });
    ui.on_history_clear({
        let state = state.clone();
        move || {
            let mut st = state.borrow_mut();
            st.history.clear();
            save_history(&st);
            refresh_history(&st);
        }
    });
    ui.on_eq_toggled({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.eq_enabled = on;
            apply_eq(&mut st, &ui);
        }
    });
    ui.on_eq_preset_selected({
        let state = state.clone();
        let weak = ui.as_weak();
        move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            if let Some(g) = coreeq::preset_gains(i.max(0) as usize) {
                st.settings.eq_gains = g;
                st.settings.eq_enabled = true; // elegir un preset lo activa
                apply_eq(&mut st, &ui);
            }
        }
    });
    ui.on_eq_band_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |band, db| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            let band = band.max(0) as usize;
            if band >= coreeq::EQ_BANDS {
                return;
            }
            // Pasos de 0,5 dB.
            let tenths = coreeq::clamp_tenths(((db * 2.0).round() * 5.0) as i32);
            if st.settings.eq_gains[band] == tenths && st.settings.eq_enabled {
                return;
            }
            st.settings.eq_gains[band] = tenths;
            st.settings.eq_enabled = true; // tocar una banda lo activa
            apply_eq(&mut st, &ui);
        }
    });
    ui.on_eq_reset({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.eq_gains = [0; coreeq::EQ_BANDS];
            apply_eq(&mut st, &ui);
        }
    });
    ui.on_replaygain_album_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.settings.replaygain_album = on;
            if let Some(p) = st.player.as_mut() {
                p.set_replaygain_album(on);
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

    ui.on_widget_toggle({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let on = !ui.get_widget_mode();
                set_widget_mode(&mut state.borrow_mut(), &ui, on);
            }
        }
    });
    ui.on_widget_drag({
        let weak = ui.as_weak();
        move |dx, dy| {
            if let Some(ui) = weak.upgrade() {
                let w = ui.window();
                let scale = w.scale_factor();
                let p = w.position();
                w.set_position(slint::PhysicalPosition::new(
                    p.x + (dx * scale).round() as i32,
                    p.y + (dy * scale).round() as i32,
                ));
            }
        }
    });
    ui.on_widget_on_start_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                st.settings.widget_on_start = v;
                ui.set_widget_on_start(v);
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_widget_on_top_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                st.settings.widget_on_top = v;
                ui.set_widget_on_top(v);
                save_settings(&st, &ui);
            }
        }
    });
    ui.on_autostart_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                match autostart::set(v) {
                    Ok(()) => {
                        st.settings.autostart = v;
                        ui.set_autostart(v);
                        ui.set_autostart_status("".into());
                        save_settings(&st, &ui);
                    }
                    Err(e) => {
                        ui.set_autostart_status(format!("No se pudo cambiar: {e}").into());
                    }
                }
            }
        }
    });
    ui.on_podcast_changed({
        let state = state.clone();
        let weak = ui.as_weak();
        move |v| {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                let Some(path) = st.folders.get(st.selected).map(|f| f.path.clone()) else {
                    return;
                };
                st.settings.set_podcast(&path, v);
                save_settings(&st, &ui);
                refresh_folders(&st, &ui);
            }
        }
    });
    ui.on_stats_opened({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                save_stats(&mut st);
                refresh_stats_ui(&st, &ui);
            }
        }
    });
    ui.on_stats_reset({
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut st = state.borrow_mut();
                st.stats = Stats::default();
                st.tracker.reset();
                st.stats_dirty = true;
                save_stats(&mut st);
                refresh_stats_ui(&st, &ui);
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
                save_stats(&mut st);
                if ui.get_stats_open() {
                    refresh_stats_ui(&st, &ui);
                }
            }
            // Modo widget: colocarlo tras mostrar la ventana y recordar dónde se suelta.
            if ui.get_widget_mode() {
                if !st.widget_placed {
                    if ticks >= 2 {
                        let w = ui.window();
                        w.set_size(slint::LogicalSize::new(WIDGET_SIZE.0, WIDGET_SIZE.1));
                        place_widget(&st, &ui);
                        st.widget_placed = true;
                    }
                } else {
                    let p = ui.window().position();
                    if st.settings.widget_pos != Some((p.x, p.y)) {
                        st.settings.widget_pos = Some((p.x, p.y));
                        schedule_theme_save(&mut st);
                    }
                }
            }

            // Acciones del icono de bandeja (siguen funcionando con la ventana oculta).
            let mut commands: Vec<Command> = st
                .tray
                .as_ref()
                .map(|t| t.poll())
                .unwrap_or_default()
                .into_iter()
                .map(Command::from)
                .collect();
            // Atajos globales de teclado (también con la ventana en la bandeja).
            if let Some(h) = st.hotkeys.as_ref() {
                commands.extend(h.poll().into_iter().map(Command::from));
            }
            for cmd in commands {
                run_command(&mut st, &ui, cmd);
            }
            if st.theme_save_in > 0 {
                st.theme_save_in -= 1;
                if st.theme_save_in == 0 {
                    save_settings(&st, &ui);
                }
            }

            drain_scan(&mut st, &ui);
            drain_watch(&mut st, &ui);
            drain_tags(&mut st, &ui);
            drain_covers(&mut st, &ui);
            let snapshot = st.player.as_mut().map(|p| p.tick());
            if let Some(s) = snapshot {
                account_listening(&mut st, &s);
                apply_snapshot(&mut st, &ui, &s);
            }
            sync_tray(&mut st);
            if ui.get_show_debug() {
                if let Some(p) = st.player.as_ref() {
                    ui.set_debug_text(p.diagnostics().into());
                }
            }
        }
    });

    // Los fundidos entre paletas se activan después del primer fotograma.
    let fade_timer = slint::Timer::default();
    fade_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Theme>().set_fade_ms(600);
            }
        }
    });

    // `run_event_loop_until_quit` mantiene la aplicación viva con la ventana
    // oculta (bandeja); sale con `quit_event_loop`.
    ui.show()?;
    let result = slint::run_event_loop_until_quit();
    {
        // Ajustes de apariencia pendientes (deslizador movido justo antes de salir).
        let st = state.borrow();
        if let Some(path) = &st.settings_path {
            let _ = st.settings.save(path);
        }
    }
    save_loudness(&state.borrow());
    save_stats(&mut state.borrow_mut());
    result
}
