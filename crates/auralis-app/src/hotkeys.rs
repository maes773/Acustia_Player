//! Atajos globales de teclado (funcionan con la ventana en segundo plano o en la bandeja).
//! Solo existen en Windows; en otros sistemas `Hotkeys::new()` devuelve un error. La tabla de
//! combinaciones es de `auralis-core` (`hotkeys.rs`).

pub use auralis_core::hotkeys::HotkeyAction;

#[cfg(windows)]
mod imp {
    use auralis_core::hotkeys::{binding_label, Binding, HotkeyAction, Key, DEFAULT_BINDINGS};
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

    fn code_of(k: Key) -> Code {
        match k {
            Key::Space => Code::Space,
            Key::Left => Code::ArrowLeft,
            Key::Right => Code::ArrowRight,
            Key::Up => Code::ArrowUp,
            Key::Down => Code::ArrowDown,
            Key::S => Code::KeyS,
            Key::R => Code::KeyR,
            Key::A => Code::KeyA,
            Key::MediaPlayPause => Code::MediaPlayPause,
            Key::MediaNext => Code::MediaTrackNext,
            Key::MediaPrevious => Code::MediaTrackPrevious,
            Key::MediaStop => Code::MediaStop,
        }
    }

    fn hotkey_of(b: &Binding) -> HotKey {
        let mut m = Modifiers::empty();
        if b.ctrl {
            m |= Modifiers::CONTROL;
        }
        if b.alt {
            m |= Modifiers::ALT;
        }
        if b.shift {
            m |= Modifiers::SHIFT;
        }
        HotKey::new(if m.is_empty() { None } else { Some(m) }, code_of(b.key))
    }

    pub struct Hotkeys {
        manager: GlobalHotKeyManager,
        /// Atajos registrados con éxito: (id del sistema, acción, atajo).
        registered: Vec<(u32, HotkeyAction, HotKey)>,
        /// Descripción de los que el sistema rechazó (normalmente otra aplicación los tiene).
        failed: Vec<String>,
    }

    impl Hotkeys {
        /// Debe llamarse desde el hilo de la interfaz (el que ejecuta el bucle de mensajes).
        pub fn new() -> Result<Self, String> {
            let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
            let mut h = Self {
                manager,
                registered: Vec::new(),
                failed: Vec::new(),
            };
            h.enable();
            Ok(h)
        }

        /// Registra todos los atajos (los que ya lo estaban no se repiten).
        pub fn enable(&mut self) {
            self.failed.clear();
            if !self.registered.is_empty() {
                return;
            }
            for b in DEFAULT_BINDINGS.iter() {
                let hk = hotkey_of(b);
                match self.manager.register(hk) {
                    Ok(()) => self.registered.push((hk.id(), b.action, hk)),
                    Err(_) => self.failed.push(binding_label(b)),
                }
            }
        }

        pub fn disable(&mut self) {
            for (_, _, hk) in self.registered.drain(..) {
                let _ = self.manager.unregister(hk);
            }
            self.failed.clear();
        }

        pub fn failed(&self) -> &[String] {
            &self.failed
        }

        /// Acciones pedidas desde el último aviso (llamar desde el hilo de la interfaz).
        pub fn poll(&self) -> Vec<HotkeyAction> {
            let mut out = Vec::new();
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                if ev.state != HotKeyState::Pressed {
                    continue;
                }
                if let Some((_, action, _)) = self.registered.iter().find(|(id, _, _)| *id == ev.id)
                {
                    out.push(*action);
                }
            }
            out
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use auralis_core::hotkeys::HotkeyAction;

    pub struct Hotkeys;

    impl Hotkeys {
        pub fn new() -> Result<Self, String> {
            Err("los atajos globales solo están disponibles en Windows".into())
        }
        pub fn enable(&mut self) {}
        pub fn disable(&mut self) {}
        pub fn failed(&self) -> &[String] {
            &[]
        }
        pub fn poll(&self) -> Vec<HotkeyAction> {
            Vec::new()
        }
    }
}

pub use imp::Hotkeys;
