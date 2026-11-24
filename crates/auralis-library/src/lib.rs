//! Lectura de archivos: búsqueda de pistas en una carpeta, etiquetas (título/artista),
//! duración y carátula incrustada; edición de etiquetas en el propio archivo y
//! vigilancia de cambios en las carpetas. No hay metadatos online.

mod metadata;
mod scan;
mod tags;
mod watch;

pub use metadata::{read_cover, read_metadata, CoverImage, TrackMeta};
pub use scan::{list_audio_files, scan_folder, scan_paths, ScanEvent, SUPPORTED_EXTENSIONS};
pub use tags::{
    parse_year, prepare_cover, read_full_tags, write_tags, CoverChange, FullTags, TagChanges,
};
pub use watch::{watch_folder, POLL_INTERVAL, SETTLE};
