//! Vigilancia de una carpeta: avisa cuando aparecen o desaparecen archivos de audio.
//!
//! Se comprueba por sondeo (cada pocos segundos, a prioridad mínima y solo mirando los
//! nombres de archivo, sin leer etiquetas). Es menos instantáneo que las notificaciones
//! del sistema, pero no depende de ninguna API de Windows, funciona igual en carpetas de
//! red o discos externos y es fácil de probar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thread_priority::{set_current_thread_priority, ThreadPriority};

use crate::scan::list_audio_files;

/// Pausa entre comprobaciones en la aplicación.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Tiempo que debe permanecer estable un archivo nuevo (tamaño sin cambios) antes de
/// avisar: así no se lee una canción que aún se está copiando.
pub const SETTLE: Duration = Duration::from_millis(1500);

fn sizes(paths: &[PathBuf]) -> Vec<Option<u64>> {
    paths
        .iter()
        .map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .collect()
}

/// Duerme `d` por tramos pequeños. Devuelve `false` si se canceló.
fn sleep_cancellable(d: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(20).min(d));
    }
    !cancel.load(Ordering::Relaxed)
}

/// Vigila `root`. Cada vez que cambia el conjunto de archivos de audio envía la lista
/// completa nueva (ordenada, como `list_audio_files`). La lista inicial no se envía.
/// Si la carpeta deja de estar disponible (disco desconectado) no se envía nada.
pub fn watch_folder(
    root: PathBuf,
    interval: Duration,
    settle: Duration,
    tx: Sender<Vec<PathBuf>>,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("auralis-watch".into())
        .spawn(move || {
            let _ = set_current_thread_priority(ThreadPriority::Min);
            let mut known = list_audio_files(&root).ok();
            loop {
                if !sleep_cancellable(interval, &cancel) {
                    return;
                }
                let Ok(mut now) = list_audio_files(&root) else {
                    continue; // carpeta no disponible: se conserva lo que había
                };
                if known.as_ref() == Some(&now) {
                    continue;
                }
                // Hay cambios. Se espera a que los archivos nuevos dejen de crecer.
                for _ in 0..20 {
                    let before = sizes(&now);
                    if !sleep_cancellable(settle, &cancel) {
                        return;
                    }
                    let Ok(again) = list_audio_files(&root) else {
                        break;
                    };
                    let stable = again == now && sizes(&again) == before;
                    now = again;
                    if stable {
                        break;
                    }
                }
                if known.as_ref() != Some(&now) {
                    known = Some(now.clone());
                    if tx.send(now).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("no se pudo crear el hilo de vigilancia")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("auralis_watch_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const FAST: Duration = Duration::from_millis(40);

    #[test]
    fn reports_added_and_removed_files_and_stops_on_cancel() {
        let dir = temp_dir("basic");
        fs::write(dir.join("a.mp3"), b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let h = watch_folder(dir.clone(), FAST, FAST, tx, cancel.clone());

        // Sin cambios: no hay avisos.
        assert!(rx.recv_timeout(Duration::from_millis(400)).is_err());

        fs::write(dir.join("b.flac"), b"yy").unwrap();
        let list = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("aviso de archivo nuevo");
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|p| p.ends_with("b.flac")));

        // Un archivo que no es de audio no cuenta.
        fs::write(dir.join("notas.txt"), b"z").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(400)).is_err());

        fs::remove_file(dir.join("a.mp3")).unwrap();
        let list = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("aviso de archivo borrado");
        assert_eq!(list.len(), 1);

        // Subcarpetas nuevas también.
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("c.ogg"), b"w").unwrap();
        let list = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("aviso en subcarpeta");
        assert_eq!(list.len(), 2);

        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn waits_for_a_growing_file_before_reporting() {
        let dir = temp_dir("growing");
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let h = watch_folder(
            dir.clone(),
            FAST,
            Duration::from_millis(120),
            tx,
            cancel.clone(),
        );
        // Un archivo que sigue creciendo durante ~0,5 s.
        let path = dir.join("copiando.mp3");
        let writer = thread::spawn({
            let path = path.clone();
            move || {
                let mut data = Vec::new();
                for _ in 0..10 {
                    data.extend_from_slice(&[0u8; 100]);
                    fs::write(&path, &data).unwrap();
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });
        let started = Instant::now();
        let list = rx.recv_timeout(Duration::from_secs(8)).expect("aviso");
        assert_eq!(list.len(), 1);
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            1000,
            "avisó con el archivo completo"
        );
        assert!(started.elapsed() >= Duration::from_millis(400));
        writer.join().unwrap();
        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_folder_is_not_reported_as_empty() {
        let dir = temp_dir("missing");
        fs::write(dir.join("a.mp3"), b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let h = watch_folder(dir.clone(), FAST, FAST, tx, cancel.clone());
        thread::sleep(Duration::from_millis(100));
        fs::remove_dir_all(&dir).unwrap(); // como un disco desconectado
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap();
    }
}
