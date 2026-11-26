use std::time::{SystemTime, UNIX_EPOCH};

/// Modo de repetición: desactivado, toda la lista o una canción.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    /// Ciclo del botón: Off → All → One → Off.
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    /// Índice estable para la interfaz: 0 = Off, 1 = All, 2 = One.
    pub fn as_index(self) -> i32 {
        match self {
            RepeatMode::Off => 0,
            RepeatMode::All => 1,
            RepeatMode::One => 2,
        }
    }
}

/// PRNG xorshift64*: suficiente para barajar, sin dependencias externas.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Cola de reproducción sobre los índices `0..len` de la lista de pistas.
///
/// El orden original es siempre `0..len` y **nunca se modifica**: el shuffle
/// solo mantiene un orden de reproducción aparte (`order`). Al desactivarlo se
/// vuelve al orden original continuando desde la pista actual.
///
/// Decisión de diseño: con Repeat All + Shuffle la lista barajada se repite
/// en el mismo orden (no se vuelve a barajar al dar la vuelta). Así
/// `peek_next_auto` es una función pura y el motor puede precargar la
/// siguiente pista (gapless) sin que luego la cola decida otra cosa.
pub struct Queue {
    len: usize,
    order: Vec<usize>,
    pos: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    rng: XorShift,
}

impl Queue {
    pub fn new(len: usize) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self::with_seed(len, seed)
    }

    pub fn with_seed(len: usize, seed: u64) -> Self {
        Self {
            len,
            order: (0..len).collect(),
            pos: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            rng: XorShift::new(seed),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reinicia la cola con una nueva longitud (p. ej. al cambiar de carpeta).
    /// Conserva los ajustes de shuffle y repeat.
    pub fn reset(&mut self, len: usize) {
        self.len = len;
        self.pos = None;
        self.order = (0..len).collect();
        if self.shuffle {
            self.shuffle_order(None);
        }
    }

    /// Orden de reproducción actual (el original si el shuffle está desactivado).
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Índice (en la lista de pistas) de la pista actual.
    pub fn current(&self) -> Option<usize> {
        self.pos.and_then(|p| self.order.get(p).copied())
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    pub fn cycle_repeat(&mut self) -> RepeatMode {
        self.repeat = self.repeat.cycle();
        self.repeat
    }

    /// Activa o desactiva el shuffle sin tocar el orden original.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        let current = self.current();
        self.shuffle = on;
        if on {
            self.shuffle_order(current);
        } else {
            self.order = (0..self.len).collect();
            self.pos = current;
        }
    }

    /// Baraja `order`; si hay pista actual, queda en primera posición.
    fn shuffle_order(&mut self, first: Option<usize>) {
        self.order = (0..self.len).collect();
        for i in (1..self.len).rev() {
            let j = self.rng.below(i + 1);
            self.order.swap(i, j);
        }
        match first {
            Some(c) if c < self.len => {
                if let Some(p) = self.order.iter().position(|&x| x == c) {
                    self.order.swap(0, p);
                }
                self.pos = Some(0);
            }
            _ => self.pos = None,
        }
    }

    /// Selección explícita del usuario (clic en la lista). Con shuffle activo
    /// se vuelve a barajar dejando `index` primero, para que no se salten
    /// pistas que quedaban antes en el orden anterior.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        if self.shuffle {
            self.shuffle_order(Some(index));
            true
        } else {
            self.play_index(index)
        }
    }

    /// Sincroniza el cursor con la pista que el motor acaba de empezar, sin
    /// alterar el orden. Devuelve `false` si el índice no existe.
    pub fn play_index(&mut self, index: usize) -> bool {
        match self.order.iter().position(|&x| x == index) {
            Some(p) => {
                self.pos = Some(p);
                true
            }
            None => false,
        }
    }

    /// Posición (en `order`) de la pista que sigue de forma automática.
    fn next_auto_pos(&self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        match self.pos {
            None => Some(0),
            Some(p) => match self.repeat {
                RepeatMode::One => Some(p),
                RepeatMode::All => Some((p + 1) % self.len),
                RepeatMode::Off => (p + 1 < self.len).then_some(p + 1),
            },
        }
    }

    /// Qué sonará cuando termine la pista actual (no modifica la cola).
    pub fn peek_next_auto(&self) -> Option<usize> {
        self.next_auto_pos().map(|p| self.order[p])
    }

    /// Avanza como si la pista actual hubiera terminado sola.
    pub fn advance_auto(&mut self) -> Option<usize> {
        let p = self.next_auto_pos()?;
        self.pos = Some(p);
        Some(self.order[p])
    }

    /// Botón «siguiente». Con Repeat Off en la última pista no hace nada (`None`);
    /// con Repeat All o One da la vuelta al principio.
    pub fn next_manual(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let p = match self.pos {
            None => 0,
            Some(p) if p + 1 < self.len => p + 1,
            Some(_) if self.repeat == RepeatMode::Off => return None,
            Some(_) => 0,
        };
        self.pos = Some(p);
        Some(self.order[p])
    }

    /// Botón «anterior». `None` significa que no hay pista anterior (el
    /// llamador decide reiniciar la actual). Con Repeat All en la primera
    /// pista da la vuelta a la última.
    pub fn previous(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let p = match self.pos? {
            0 if self.repeat == RepeatMode::All => self.len - 1,
            0 => return None,
            p => p - 1,
        };
        self.pos = Some(p);
        Some(self.order[p])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_permutation(order: &[usize], len: usize) -> bool {
        let mut v = order.to_vec();
        v.sort_unstable();
        v == (0..len).collect::<Vec<_>>()
    }

    #[test]
    fn sequential_off() {
        let mut q = Queue::with_seed(3, 1);
        assert!(q.play_index(0));
        assert_eq!(q.peek_next_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(2));
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.advance_auto(), None);
        assert_eq!(q.current(), Some(2));
    }

    #[test]
    fn repeat_one_keeps_same_track_automatically_but_manual_moves_on() {
        let mut q = Queue::with_seed(3, 1);
        q.set_repeat(RepeatMode::One);
        q.play_index(1);
        assert_eq!(q.peek_next_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(1));
        assert_eq!(q.next_manual(), Some(2));
        assert_eq!(q.next_manual(), Some(0)); // da la vuelta
    }

    #[test]
    fn repeat_all_wraps() {
        let mut q = Queue::with_seed(3, 1);
        q.set_repeat(RepeatMode::All);
        q.play_index(2);
        assert_eq!(q.peek_next_auto(), Some(0));
        assert_eq!(q.advance_auto(), Some(0));
        assert_eq!(q.previous(), Some(2));
    }

    #[test]
    fn repeat_off_manual_edges() {
        let mut q = Queue::with_seed(3, 1);
        q.play_index(2);
        assert_eq!(q.next_manual(), None);
        assert_eq!(q.current(), Some(2));
        q.play_index(0);
        assert_eq!(q.previous(), None);
        assert_eq!(q.current(), Some(0));
    }

    #[test]
    fn repeat_cycle_has_three_states() {
        let mut q = Queue::with_seed(1, 1);
        assert_eq!(q.repeat(), RepeatMode::Off);
        assert_eq!(q.cycle_repeat(), RepeatMode::All);
        assert_eq!(q.cycle_repeat(), RepeatMode::One);
        assert_eq!(q.cycle_repeat(), RepeatMode::Off);
    }

    #[test]
    fn shuffle_is_permutation_with_current_first() {
        for seed in 1..50 {
            let mut q = Queue::with_seed(20, seed);
            q.play_index(7);
            q.set_shuffle(true);
            assert!(is_permutation(q.order(), 20));
            assert_eq!(q.current(), Some(7));
            assert_eq!(q.order()[0], 7);
        }
    }

    #[test]
    fn shuffle_visits_every_track_once_with_repeat_off() {
        let mut q = Queue::with_seed(10, 42);
        q.set_shuffle(true);
        assert!(q.select(3));
        assert_eq!(q.current(), Some(3));
        let mut seen = vec![q.current().unwrap()];
        while let Some(i) = q.advance_auto() {
            seen.push(i);
        }
        assert_eq!(seen.len(), 10);
        assert!(is_permutation(&seen, 10));
    }

    #[test]
    fn disabling_shuffle_restores_original_order_from_current() {
        let mut q = Queue::with_seed(6, 9);
        q.play_index(2);
        q.set_shuffle(true);
        q.advance_auto();
        let cur = q.current().unwrap();
        q.set_shuffle(false);
        assert_eq!(q.order(), &[0, 1, 2, 3, 4, 5]);
        assert_eq!(q.current(), Some(cur));
        // continúa en orden original desde la pista actual
        assert_eq!(
            q.peek_next_auto(),
            if cur + 1 < 6 { Some(cur + 1) } else { None }
        );
    }

    #[test]
    fn shuffle_peek_matches_advance_including_wrap() {
        let mut q = Queue::with_seed(5, 7);
        q.set_repeat(RepeatMode::All);
        q.set_shuffle(true);
        q.play_index(0);
        for _ in 0..12 {
            let peeked = q.peek_next_auto();
            assert_eq!(q.advance_auto(), peeked);
        }
    }

    #[test]
    fn play_index_in_shuffle_keeps_order_and_moves_cursor() {
        let mut q = Queue::with_seed(8, 3);
        q.set_shuffle(true);
        let before = q.order().to_vec();
        assert!(q.play_index(5));
        assert_eq!(q.current(), Some(5));
        assert_eq!(q.order(), &before[..]);
        assert!(!q.play_index(99));
    }

    #[test]
    fn select_without_shuffle_is_plain_cursor_move() {
        let mut q = Queue::with_seed(5, 3);
        assert!(q.select(3));
        assert_eq!(q.order(), &[0, 1, 2, 3, 4]);
        assert_eq!(q.peek_next_auto(), Some(4));
        assert!(!q.select(9));
    }

    #[test]
    fn reset_keeps_settings_and_handles_empty() {
        let mut q = Queue::with_seed(4, 1);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        q.reset(0);
        assert!(q.is_empty());
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.next_manual(), None);
        assert_eq!(q.previous(), None);
        q.reset(3);
        assert!(q.shuffle());
        assert_eq!(q.repeat(), RepeatMode::All);
        assert!(is_permutation(q.order(), 3));
        assert_eq!(q.current(), None);
        assert!(q.peek_next_auto().is_some());
    }

    #[test]
    fn repeat_index_values() {
        assert_eq!(RepeatMode::Off.as_index(), 0);
        assert_eq!(RepeatMode::All.as_index(), 1);
        assert_eq!(RepeatMode::One.as_index(), 2);
    }
}
