use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use auralis_core::TrackInfo;
use thread_priority::{set_current_thread_priority, ThreadPriority};

use crate::metadata::{read_metadata, TrackMeta};

/// Extensiones que se muestran en la lista (formatos prioritarios de la v0.0.1).
pub const SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "oga", "aif", "aiff"];

/// Tamaño máximo (px) de las miniaturas de la lista. Las carátulas grandes se
/// piden aparte con `read_cover`.
const LIST_COVER_PX: u32 = 96;

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|s| s.eq_ignore_ascii_case(e))
        })
        .unwrap_or(false)
}

/// Busca recursivamente archivos de audio. Los enlaces simbólicos a carpetas
/// no se siguen (evita ciclos) y las carpetas ilegibles se omiten.
/// El resultado va ordenado por ruta sin distinguir mayúsculas.
pub fn list_audio_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut first = true;
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if first => return Err(e),
            Err(_) => continue,
        };
        first = false;
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() && is_supported(&path) {
                files.push(path);
            }
        }
    }
    files.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
    Ok(files)
}

pub enum ScanEvent {
    /// Lista inicial (solo rutas y nombres): llega enseguida.
    Files(Vec<TrackInfo>),
    /// Etiquetas, duración y miniatura de la pista `index`, leídas en segundo plano.
    Meta { index: usize, meta: TrackMeta },
    /// Terminó la lectura de metadatos.
    Done,
}

/// Escanea `root` en un hilo propio. Primero envía `Files`, luego un `Meta` por
/// pista y por último `Done`. Poner `cancel` a `true` detiene el hilo.
pub fn scan_folder(
    root: PathBuf,
    tx: Sender<ScanEvent>,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("auralis-scan".into())
        .spawn(move || {
            // Prioridad baja: leer etiquetas y decodificar carátulas no debe quitar
            // CPU al hilo de audio. Si el sistema no lo permite, se sigue igual.
            let _ = set_current_thread_priority(ThreadPriority::Min);
            let paths = list_audio_files(&root).unwrap_or_default();
            let infos: Vec<TrackInfo> = paths.iter().cloned().map(TrackInfo::from_path).collect();
            if tx.send(ScanEvent::Files(infos)).is_err() {
                return;
            }
            for (index, path) in paths.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let meta = read_metadata(path, LIST_COVER_PX);
                if tx.send(ScanEvent::Meta { index, meta }).is_err() {
                    return;
                }
            }
            let _ = tx.send(ScanEvent::Done);
        })
        .expect("no se pudo crear el hilo de escaneo")
}

/// Lee las etiquetas de algunas pistas concretas (`(índice, ruta)`), p. ej. las que acaban
/// de aparecer en la carpeta. Envía un `Meta` por pista y al final `Done`.
pub fn scan_paths(
    items: Vec<(usize, PathBuf)>,
    tx: Sender<ScanEvent>,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("auralis-scan-new".into())
        .spawn(move || {
            let _ = set_current_thread_priority(ThreadPriority::Min);
            for (index, path) in items {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let meta = read_metadata(&path, LIST_COVER_PX);
                if tx.send(ScanEvent::Meta { index, meta }).is_err() {
                    return;
                }
            }
            let _ = tx.send(ScanEvent::Done);
        })
        .expect("no se pudo crear el hilo de escaneo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_supported_files_recursively_and_sorted() {
        let base = std::env::temp_dir().join(format!("auralis-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub")).unwrap();
        for f in ["b.mp3", "A.FLAC", "notes.txt", "sub/c.ogg", "sub/d.jpg"] {
            fs::write(base.join(f), b"x").unwrap();
        }
        let found = list_audio_files(&base).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(names, vec!["A.FLAC", "b.mp3", "sub/c.ogg"]);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_root_is_an_error() {
        assert!(list_audio_files(Path::new("/no/existe/auralis")).is_err());
    }
}
