//! Lógica pura de Auralis: modelos, cola de reproducción, shuffle y repeat,
//! ajustes persistentes y búsqueda.
//!
//! Este crate no depende de audio, interfaz ni de APIs de Windows (solo `std`),
//! de modo que puede reutilizarse tal cual en Android.

pub mod eq;
pub mod history;
pub mod hotkeys;
mod queue;
pub mod search;
pub mod settings;
pub mod stats;
pub mod theme;
mod time;
mod track;

pub use eq::{EqGains, EQ_BANDS, EQ_CUSTOM};
pub use history::{History, HistoryEntry};
pub use queue::{Queue, RepeatMode};
pub use search::Query;
pub use settings::{same_path, Settings, MAX_CROSSFADE_SECS};
pub use time::format_time;
pub use track::TrackInfo;
