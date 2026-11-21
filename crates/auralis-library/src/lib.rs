//! Lectura básica de archivos: búsqueda de pistas en una carpeta, etiquetas
//! (título/artista), duración y carátula incrustada. No hay metadatos online
//! ni edición de etiquetas en esta versión.

mod metadata;
mod scan;

pub use metadata::{read_cover, read_metadata, CoverImage, TrackMeta};
pub use scan::{list_audio_files, scan_folder, ScanEvent, SUPPORTED_EXTENSIONS};
