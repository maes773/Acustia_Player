//! Lógica pura de Auralis: modelos, cola de reproducción, shuffle y repeat.
//!
//! Este crate no depende de audio, interfaz ni de APIs de Windows, de modo que
//! puede reutilizarse tal cual en Android.

mod queue;
mod time;
mod track;

pub use queue::{Queue, RepeatMode};
pub use time::format_time;
pub use track::TrackInfo;
