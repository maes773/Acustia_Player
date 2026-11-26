//! Icono de bandeja del sistema. Solo existe en Windows; en otros sistemas
//! `Tray::new()` devuelve un error y la interfaz minimiza la ventana con normalidad.

/// Acción pedida por el usuario desde el icono o su menú.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    TogglePlay,
    Next,
    Previous,
    ToggleShuffle,
    CycleRepeat,
    Quit,
}

/// Lo que el menú de la bandeja muestra del reproductor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrayState {
    pub playing: bool,
    pub shuffle: bool,
    /// 0 = no repetir, 1 = toda la lista, 2 = una canción.
    pub repeat: i32,
    /// «Título — Artista» de la canción actual (vacío si no hay).
    pub now_playing: String,
}

/// Texto del elemento «Repetir» del menú.
pub fn repeat_label(mode: i32) -> &'static str {
    match mode {
        1 => "Repetir: toda la lista",
        2 => "Repetir: una canción",
        _ => "Repetir: no",
    }
}

/// Texto del elemento de reproducir/pausar y de la línea de la canción actual.
pub fn play_label(playing: bool) -> &'static str {
    if playing {
        "Pausar"
    } else {
        "Reproducir"
    }
}

pub fn now_playing_label(text: &str) -> String {
    if text.is_empty() {
        "Nada en reproducción".to_string()
    } else {
        // Un menú con una línea enorme es ilegible: se recorta.
        let t: String = text.chars().take(60).collect();
        if text.chars().count() > 60 {
            format!("♪ {t}…")
        } else {
            format!("♪ {t}")
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{now_playing_label, play_label, repeat_label, TrayAction, TrayState};
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Tray {
        icon: TrayIcon,
        now: MenuItem,
        play: MenuItem,
        shuffle: CheckMenuItem,
        repeat: MenuItem,
        show_id: MenuId,
        play_id: MenuId,
        next_id: MenuId,
        prev_id: MenuId,
        shuffle_id: MenuId,
        repeat_id: MenuId,
        quit_id: MenuId,
    }

    /// Logo de Acustia Player (PNG de 64 px incrustado en el ejecutable). Si por algo
    /// no se pudiera decodificar, se usa el icono generado en código.
    fn make_icon() -> Result<Icon, String> {
        const LOGO: &[u8] = include_bytes!("../assets/icon-64.png");
        match image::load_from_memory_with_format(LOGO, image::ImageFormat::Png) {
            Ok(img) => {
                let rgba = img.into_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).or_else(|_| fallback_icon())
            }
            Err(_) => fallback_icon(),
        }
    }

    /// Icono generado en código (disco con punto central) para no depender de archivos.
    fn fallback_icon() -> Result<Icon, String> {
        const N: u32 = 32;
        let mut rgba = Vec::with_capacity((N * N * 4) as usize);
        let c = (N as f32 - 1.0) / 2.0;
        for y in 0..N {
            for x in 0..N {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                let (r, g, b) = if d < 6.0 {
                    (255, 255, 255)
                } else {
                    (0x65, 0x77, 0xf5)
                };
                // Borde suavizado de 1 px.
                let a = ((15.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
        Icon::from_rgba(rgba, N, N).map_err(|e| e.to_string())
    }

    impl Tray {
        pub fn new() -> Result<Self, String> {
            let menu = Menu::new();
            let now = MenuItem::new(now_playing_label(""), false, None);
            let play = MenuItem::new(play_label(false), true, None);
            let prev = MenuItem::new("Anterior", true, None);
            let next = MenuItem::new("Siguiente", true, None);
            let shuffle = CheckMenuItem::new("Aleatorio", true, false, None);
            let repeat = MenuItem::new(repeat_label(0), true, None);
            let show = MenuItem::new("Mostrar Acustia Player", true, None);
            let quit = MenuItem::new("Salir", true, None);
            menu.append_items(&[
                &now,
                &PredefinedMenuItem::separator(),
                &play,
                &prev,
                &next,
                &PredefinedMenuItem::separator(),
                &shuffle,
                &repeat,
                &PredefinedMenuItem::separator(),
                &show,
                &quit,
            ])
            .map_err(|e| e.to_string())?;

            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Acustia Player")
                .with_icon(make_icon()?)
                .build()
                .map_err(|e| e.to_string())?;

            Ok(Self {
                icon,
                show_id: show.id().clone(),
                play_id: play.id().clone(),
                next_id: next.id().clone(),
                prev_id: prev.id().clone(),
                shuffle_id: shuffle.id().clone(),
                repeat_id: repeat.id().clone(),
                quit_id: quit.id().clone(),
                now,
                play,
                shuffle,
                repeat,
            })
        }

        /// Actualiza los textos y la marca del menú (llamar solo cuando el estado cambie).
        pub fn set_state(&self, s: &TrayState) {
            self.now.set_text(now_playing_label(&s.now_playing));
            self.play.set_text(play_label(s.playing));
            self.shuffle.set_checked(s.shuffle);
            self.repeat.set_text(repeat_label(s.repeat));
            let tip = if s.now_playing.is_empty() {
                "Acustia Player".to_string()
            } else {
                // El texto de ayuda de la bandeja de Windows admite unos 120 caracteres.
                let t: String = s.now_playing.chars().take(100).collect();
                format!("Acustia Player — {t}")
            };
            let _ = self.icon.set_tooltip(Some(tip));
        }

        pub fn set_visible(&self, visible: bool) {
            let _ = self.icon.set_visible(visible);
        }

        /// Recoge las acciones pendientes (llamar desde el hilo de la interfaz).
        pub fn poll(&self) -> Vec<TrayAction> {
            let mut actions = Vec::new();
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == self.show_id {
                    actions.push(TrayAction::Show);
                } else if ev.id == self.play_id {
                    actions.push(TrayAction::TogglePlay);
                } else if ev.id == self.next_id {
                    actions.push(TrayAction::Next);
                } else if ev.id == self.prev_id {
                    actions.push(TrayAction::Previous);
                } else if ev.id == self.shuffle_id {
                    actions.push(TrayAction::ToggleShuffle);
                } else if ev.id == self.repeat_id {
                    actions.push(TrayAction::CycleRepeat);
                } else if ev.id == self.quit_id {
                    actions.push(TrayAction::Quit);
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                match ev {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => actions.push(TrayAction::Show),
                    _ => {}
                }
            }
            actions
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{TrayAction, TrayState};

    pub struct Tray;

    impl Tray {
        pub fn new() -> Result<Self, String> {
            Err("la bandeja del sistema solo está disponible en Windows".into())
        }
        pub fn set_visible(&self, _visible: bool) {}
        pub fn set_state(&self, _s: &TrayState) {}
        pub fn poll(&self) -> Vec<TrayAction> {
            Vec::new()
        }
    }
}

pub use imp::Tray;
