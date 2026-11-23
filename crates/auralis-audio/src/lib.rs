//! Motor de audio de Auralis.
//!
//! Arquitectura (ver `docs/AUDIO.md`):
//!
//! ```text
//!  hilo de UI ──Cmd──▶ hilo decodificador ──f32──▶ ring buffer ──▶ callback CPAL
//!      ▲                    (Symphonia)             (sin locks)     (tiempo real)
//!      └──── poll(): posición por contador atómico + eventos ◀──────────┘
//! ```
//!
//! El callback de CPAL solo usa atómicos y el consumidor del ring buffer:
//! nunca toma locks, no reserva memoria (salvo un redimensionado excepcional
//! del buffer temporal) y no hace E/S.
//!
//! Este crate no depende de Slint ni de APIs de Windows.

mod converter;
mod decoder_thread;
mod engine;
mod error;
mod output;
mod platform;
mod reader;
mod shared;

pub use engine::{Engine, Event};
pub use error::AudioError;
pub use shared::TrackRequest;
