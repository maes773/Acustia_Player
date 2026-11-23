//! Icono de bandeja del sistema. Solo existe en Windows; en otros sistemas
//! `Tray::new()` devuelve un error y la interfaz minimiza la ventana con normalidad.

/// Acción pedida por el usuario desde el icono o su menú.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    TogglePlay,
    Quit,
}

#[cfg(windows)]
mod imp {
    use super::TrayAction;
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Tray {
        icon: TrayIcon,
        show_id: MenuId,
        play_id: MenuId,
        quit_id: MenuId,
    }

    /// Logo de Auralis (PNG de 64 px incrustado en el ejecutable). Si por algo
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
            let show = MenuItem::new("Mostrar Auralis", true, None);
            let play = MenuItem::new("Reproducir / Pausar", true, None);
            let quit = MenuItem::new("Salir", true, None);
            menu.append_items(&[&show, &play, &PredefinedMenuItem::separator(), &quit])
                .map_err(|e| e.to_string())?;

            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Auralis")
                .with_icon(make_icon()?)
                .build()
                .map_err(|e| e.to_string())?;

            Ok(Self {
                icon,
                show_id: show.id().clone(),
                play_id: play.id().clone(),
                quit_id: quit.id().clone(),
            })
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
    use super::TrayAction;

    pub struct Tray;

    impl Tray {
        pub fn new() -> Result<Self, String> {
            Err("la bandeja del sistema solo está disponible en Windows".into())
        }
        pub fn set_visible(&self, _visible: bool) {}
        pub fn poll(&self) -> Vec<TrayAction> {
            Vec::new()
        }
    }
}

pub use imp::Tray;
