//! Atajos globales de teclado: tabla de acciones y combinaciones (datos puros). El registro
//! real en el sistema operativo está en `auralis-app` (solo Windows).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    CycleRepeat,
    ShowWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Space,
    Left,
    Right,
    Up,
    Down,
    S,
    R,
    A,
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    MediaStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub action: HotkeyAction,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

const fn combo(action: HotkeyAction, key: Key) -> Binding {
    Binding {
        action,
        ctrl: true,
        alt: true,
        shift: false,
        key,
    }
}

const fn media(action: HotkeyAction, key: Key) -> Binding {
    Binding {
        action,
        ctrl: false,
        alt: false,
        shift: false,
        key,
    }
}

/// Atajos por defecto: teclas multimedia (sin modificadores) y Ctrl+Alt+tecla.
pub const DEFAULT_BINDINGS: [Binding; 12] = [
    media(HotkeyAction::PlayPause, Key::MediaPlayPause),
    media(HotkeyAction::Next, Key::MediaNext),
    media(HotkeyAction::Previous, Key::MediaPrevious),
    media(HotkeyAction::Stop, Key::MediaStop),
    combo(HotkeyAction::PlayPause, Key::Space),
    combo(HotkeyAction::Next, Key::Right),
    combo(HotkeyAction::Previous, Key::Left),
    combo(HotkeyAction::VolumeUp, Key::Up),
    combo(HotkeyAction::VolumeDown, Key::Down),
    combo(HotkeyAction::ToggleShuffle, Key::S),
    combo(HotkeyAction::CycleRepeat, Key::R),
    combo(HotkeyAction::ShowWindow, Key::A),
];

pub fn key_label(k: Key) -> &'static str {
    match k {
        Key::Space => "Espacio",
        Key::Left => "←",
        Key::Right => "→",
        Key::Up => "↑",
        Key::Down => "↓",
        Key::S => "S",
        Key::R => "R",
        Key::A => "A",
        Key::MediaPlayPause => "Tecla multimedia Reproducir/Pausar",
        Key::MediaNext => "Tecla multimedia Siguiente",
        Key::MediaPrevious => "Tecla multimedia Anterior",
        Key::MediaStop => "Tecla multimedia Detener",
    }
}

pub fn binding_label(b: &Binding) -> String {
    let mut s = String::new();
    if b.ctrl {
        s.push_str("Ctrl+");
    }
    if b.alt {
        s.push_str("Alt+");
    }
    if b.shift {
        s.push_str("Mayús+");
    }
    s.push_str(key_label(b.key));
    s
}

pub fn action_label(a: HotkeyAction) -> &'static str {
    match a {
        HotkeyAction::PlayPause => "Reproducir / pausar",
        HotkeyAction::Next => "Siguiente canción",
        HotkeyAction::Previous => "Canción anterior",
        HotkeyAction::Stop => "Pausar",
        HotkeyAction::VolumeUp => "Subir volumen",
        HotkeyAction::VolumeDown => "Bajar volumen",
        HotkeyAction::ToggleShuffle => "Aleatorio",
        HotkeyAction::CycleRepeat => "Repetir (lista → canción → no)",
        HotkeyAction::ShowWindow => "Mostrar la ventana",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_combination_is_registered_twice() {
        let mut seen = HashSet::new();
        for b in DEFAULT_BINDINGS {
            assert!(
                seen.insert((b.ctrl, b.alt, b.shift, b.key as u8)),
                "{b:?} repetido"
            );
        }
    }

    #[test]
    fn labels() {
        assert_eq!(binding_label(&DEFAULT_BINDINGS[5]), "Ctrl+Alt+→");
        assert_eq!(binding_label(&DEFAULT_BINDINGS[4]), "Ctrl+Alt+Espacio");
        let shifted = Binding {
            shift: true,
            ..DEFAULT_BINDINGS[4]
        };
        assert_eq!(binding_label(&shifted), "Ctrl+Alt+Mayús+Espacio");
        assert!(binding_label(&DEFAULT_BINDINGS[0]).starts_with("Tecla multimedia"));
    }

    #[test]
    fn media_keys_have_no_modifiers_and_combos_have_ctrl_alt() {
        for b in DEFAULT_BINDINGS {
            let is_media = matches!(
                b.key,
                Key::MediaPlayPause | Key::MediaNext | Key::MediaPrevious | Key::MediaStop
            );
            if is_media {
                assert!(!b.ctrl && !b.alt && !b.shift);
            } else {
                assert!(b.ctrl && b.alt);
            }
        }
    }
}
