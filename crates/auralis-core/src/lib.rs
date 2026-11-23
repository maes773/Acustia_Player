//! Lógica pura de Auralis: modelos, cola de reproducción, shuffle y repeat,
//! ajustes persistentes y búsqueda.
//!
//! Este crate no depende de audio, interfaz ni de APIs de Windows (solo `std`),
//! de modo que puede reutilizarse tal cual en Android.

mod queue;
pub mod search;
pub mod settings;
mod time;
mod track;

pub use queue::{Queue, RepeatMode};
pub use search::Query;
pub use settings::{same_path, Settings};
pub use time::format_time;
pub use track::TrackInfo;
