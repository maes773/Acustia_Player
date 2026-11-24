//! Datos de las carpetas de música y de sus canciones, sin nada de interfaz
//! (probable sin ventana): nombres mostrados, nombres editados por el usuario y
//! filtro de búsqueda.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use auralis_core::search::search_key;
use auralis_core::{Query, Settings, TrackInfo};

/// Una canción de la lista.
pub struct Entry {
    /// `info.title` es el nombre original: el de las etiquetas o, si no hay, el del archivo.
    pub info: TrackInfo,
    /// Nombre elegido por el usuario. Solo cambia lo que se muestra; el archivo no se toca.
    custom_title: Option<String>,
    key: String,
    /// Ya se leyeron las etiquetas de este archivo (las nuevas aún no).
    pub meta_read: bool,
}

impl Entry {
    pub fn new(info: TrackInfo, custom_title: Option<String>) -> Self {
        let mut e = Self {
            info,
            custom_title,
            key: String::new(),
            meta_read: false,
        };
        e.refresh_key();
        e
    }

    /// Nombre que se muestra: el editado si existe, si no el original.
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.info.title)
    }

    pub fn original_title(&self) -> &str {
        &self.info.title
    }

    pub fn has_custom_title(&self) -> bool {
        self.custom_title.is_some()
    }

    /// Segunda línea de la fila: el artista, o el nombre del archivo si no hay.
    pub fn subtitle(&self) -> String {
        self.info
            .artist
            .clone()
            .unwrap_or_else(|| self.info.file_name())
    }

    pub fn set_custom_title(&mut self, title: Option<String>) {
        self.custom_title = title;
        self.refresh_key();
    }

    /// Datos leídos de las etiquetas en segundo plano.
    pub fn apply_meta(
        &mut self,
        title: Option<String>,
        artist: Option<String>,
        duration_ms: Option<u64>,
    ) {
        self.meta_read = true;
        if let Some(t) = title {
            self.info.title = t;
        }
        if artist.is_some() {
            self.info.artist = artist;
        }
        if duration_ms.is_some() {
            self.info.duration_ms = duration_ms;
        }
        self.refresh_key();
    }

    /// Etiquetas tras escribirlas en el archivo: sustituyen a las anteriores (un campo
    /// vacío es un campo borrado, no «sin cambios») y el nombre editado solo en la app
    /// deja de hacer falta.
    pub fn reset_tags(&mut self, title: Option<String>, artist: Option<String>) {
        self.meta_read = true;
        self.info.title =
            title.unwrap_or_else(|| TrackInfo::from_path(self.info.path.clone()).title);
        self.info.artist = artist;
        self.custom_title = None;
        self.refresh_key();
    }

    pub fn matches(&self, query: &Query) -> bool {
        query.matches_key(&self.key)
    }

    /// Se busca por el nombre mostrado, el original (por si se renombró), el artista
    /// y el nombre del archivo.
    fn refresh_key(&mut self) {
        let file = self.info.file_name();
        self.key = search_key(&[
            self.display_title(),
            &self.info.title,
            self.info.artist.as_deref().unwrap_or(""),
            &file,
        ]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    /// Aún no se ha leído (las carpetas se cargan al seleccionarlas).
    NotLoaded,
    /// Leyendo archivos y etiquetas en segundo plano.
    Loading,
    Ready,
    /// La carpeta ya no existe (disco desconectado, borrada, renombrada…).
    Missing,
}

pub struct Folder {
    /// Identificador estable (los índices cambian al quitar carpetas).
    pub id: u64,
    pub path: PathBuf,
    /// Nombre mostrado en la pestaña (único entre las carpetas).
    pub name: String,
    pub entries: Vec<Entry>,
    pub state: LoadState,
}

impl Folder {
    pub fn new(id: u64, path: PathBuf, name: String) -> Self {
        Self {
            id,
            path,
            name,
            entries: Vec::new(),
            state: LoadState::NotLoaded,
        }
    }

    /// Sustituye las canciones por la lista leída del disco, aplicando los
    /// nombres editados guardados en los ajustes.
    pub fn set_files(&mut self, files: Vec<TrackInfo>, settings: &Settings) {
        self.entries = files
            .into_iter()
            .map(|info| {
                let custom = settings.title_for(&info.path).map(str::to_string);
                Entry::new(info, custom)
            })
            .collect();
    }

    /// Índices de las canciones que cumplen la búsqueda, en el orden original.
    pub fn visible(&self, query: &Query) -> Vec<usize> {
        if query.is_empty() {
            return (0..self.entries.len()).collect();
        }
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches(query))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Resultado de actualizar una carpeta con una lista de archivos nueva.
pub struct SyncPlan {
    /// Índice nuevo de cada canción que ya estaba (por su índice antiguo), o `None` si
    /// el archivo desapareció.
    pub old_to_new: Vec<Option<usize>>,
    /// Índices nuevos de las canciones que no estaban antes.
    pub added: Vec<usize>,
}

impl Folder {
    /// Pone las canciones en el orden de `paths` conservando todo lo ya leído (etiquetas,
    /// nombre editado) de las que siguen existiendo. Las nuevas se crean sin etiquetas.
    pub fn sync_files(&mut self, paths: Vec<PathBuf>, settings: &Settings) -> SyncPlan {
        let mut old: HashMap<PathBuf, (usize, Entry)> = self
            .entries
            .drain(..)
            .enumerate()
            .map(|(i, e)| (e.info.path.clone(), (i, e)))
            .collect();
        let mut old_to_new = vec![None; old.len()];
        let mut added = Vec::new();
        for (new_index, path) in paths.into_iter().enumerate() {
            match old.remove(&path) {
                Some((old_index, entry)) => {
                    old_to_new[old_index] = Some(new_index);
                    self.entries.push(entry);
                }
                None => {
                    let info = TrackInfo::from_path(path);
                    let custom = settings.title_for(&info.path).map(str::to_string);
                    self.entries.push(Entry::new(info, custom));
                    added.push(new_index);
                }
            }
        }
        SyncPlan { old_to_new, added }
    }
}

/// Campos de texto del editor de metadatos.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaForm {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub year: String,
}

/// Campos que cambiaron (`Some(nuevo)`, vacío = borrar); `None` = sin cambios.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormChanges {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
}

impl FormChanges {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl MetaForm {
    /// Solo se escribe lo que realmente cambió (ignorando espacios sobrantes).
    pub fn changes_from(&self, original: &MetaForm) -> FormChanges {
        fn d(new: &str, old: &str) -> Option<String> {
            (new.trim() != old.trim()).then(|| new.trim().to_string())
        }
        FormChanges {
            title: d(&self.title, &original.title),
            artist: d(&self.artist, &original.artist),
            album: d(&self.album, &original.album),
            genre: d(&self.genre, &original.genre),
            year: d(&self.year, &original.year),
        }
    }
}

fn last_component(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            p.to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_string()
        })
}

/// Nombre de pestaña para cada carpeta: el de la carpeta; si dos coinciden, se
/// numeran (`Música`, `Música (2)`).
pub fn folder_names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(paths.len());
    for p in paths {
        let base = {
            let b = last_component(p);
            if b.is_empty() {
                "Carpeta".to_string()
            } else {
                b
            }
        };
        let mut candidate = base.clone();
        let mut n = 2;
        while names.contains(&candidate) {
            candidate = format!("{base} ({n})");
            n += 1;
        }
        names.push(candidate);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(path: &str) -> TrackInfo {
        TrackInfo::from_path(PathBuf::from(path))
    }

    #[test]
    fn display_title_prefers_custom_then_original() {
        let mut e = Entry::new(info("/m/01 - Intro.flac"), None);
        assert_eq!(e.display_title(), "01 - Intro");
        e.apply_meta(Some("Intro (etiqueta)".into()), None, None);
        assert_eq!(e.display_title(), "Intro (etiqueta)");
        e.set_custom_title(Some("Mi intro".into()));
        assert_eq!(e.display_title(), "Mi intro");
        assert_eq!(e.original_title(), "Intro (etiqueta)");
        assert!(e.has_custom_title());
        e.set_custom_title(None);
        assert_eq!(e.display_title(), "Intro (etiqueta)");
        assert!(!e.has_custom_title());
    }

    #[test]
    fn reset_tags_replaces_values_and_drops_custom_title() {
        let mut e = Entry::new(info("/m/a.mp3"), Some("Mi nombre".into()));
        assert!(!e.meta_read);
        e.apply_meta(Some("Vieja".into()), Some("Ana".into()), Some(9));
        assert!(e.meta_read);
        e.reset_tags(Some("Nueva".into()), None); // artista borrado
        assert_eq!(e.display_title(), "Nueva");
        assert_eq!(e.info.artist, None);
        assert_eq!(e.info.duration_ms, Some(9));
        assert!(!e.has_custom_title());
        e.reset_tags(None, None); // sin título en el archivo: vuelve al nombre del archivo
        assert_eq!(e.display_title(), "a");
        assert!(e.matches(&Query::new("a.mp3")));
    }

    #[test]
    fn custom_title_never_touches_the_file_path() {
        let mut e = Entry::new(info("/m/a.mp3"), None);
        e.set_custom_title(Some("Otro".into()));
        assert_eq!(e.info.path, PathBuf::from("/m/a.mp3"));
        assert_eq!(e.info.file_name(), "a.mp3");
    }

    #[test]
    fn subtitle_is_artist_or_file_name() {
        let mut e = Entry::new(info("/m/a.mp3"), None);
        assert_eq!(e.subtitle(), "a.mp3");
        e.apply_meta(None, Some("Queen".into()), Some(1000));
        assert_eq!(e.subtitle(), "Queen");
        assert_eq!(e.info.duration_ms, Some(1000));
    }

    #[test]
    fn apply_meta_keeps_existing_values_when_fields_are_missing() {
        let mut e = Entry::new(info("/m/a.mp3"), None);
        e.apply_meta(Some("T".into()), Some("A".into()), Some(5));
        e.apply_meta(None, None, None);
        assert_eq!(
            (
                e.info.title.as_str(),
                e.info.artist.as_deref(),
                e.info.duration_ms
            ),
            ("T", Some("A"), Some(5))
        );
    }

    #[test]
    fn search_finds_by_custom_original_artist_and_file_name() {
        let mut e = Entry::new(info("/m/track07.mp3"), None);
        e.apply_meta(Some("Bohemian Rhapsody".into()), Some("Queen".into()), None);
        e.set_custom_title(Some("Mi himno".into()));
        for q in [
            "himno",
            "bohemian",
            "queen",
            "track07",
            "queen himno",
            "BOHEMIAN",
        ] {
            assert!(e.matches(&Query::new(q)), "debería encontrar «{q}»");
        }
        assert!(!e.matches(&Query::new("thriller")));
    }

    #[test]
    fn search_updates_after_metadata_arrives() {
        let mut e = Entry::new(info("/m/a.mp3"), None);
        assert!(!e.matches(&Query::new("canción")));
        e.apply_meta(Some("Mi Canción".into()), None, None);
        assert!(e.matches(&Query::new("cancion")));
    }

    #[test]
    fn folder_applies_saved_custom_titles_and_filters() {
        let mut settings = Settings::default();
        settings.set_title(Path::new("/m/b.mp3"), Some("Segunda"));
        let mut f = Folder::new(1, PathBuf::from("/m"), "m".into());
        f.set_files(
            vec![info("/m/a.mp3"), info("/m/b.mp3"), info("/m/c.mp3")],
            &settings,
        );
        assert_eq!(f.entries[1].display_title(), "Segunda");
        assert_eq!(f.entries[0].display_title(), "a");
        assert_eq!(f.visible(&Query::new("")), vec![0, 1, 2]);
        assert_eq!(f.visible(&Query::new("segunda")), vec![1]);
        assert_eq!(f.visible(&Query::new("b.mp3")), vec![1]); // por nombre de archivo
        assert!(f.visible(&Query::new("nada")).is_empty());
    }

    #[test]
    fn visible_keeps_original_order() {
        let mut f = Folder::new(1, PathBuf::from("/m"), "m".into());
        f.set_files(
            vec![
                info("/m/z rock.mp3"),
                info("/m/a pop.mp3"),
                info("/m/m rock.mp3"),
            ],
            &Settings::default(),
        );
        assert_eq!(f.visible(&Query::new("rock")), vec![0, 2]);
    }

    fn folder_with(paths: &[&str]) -> Folder {
        let mut f = Folder::new(1, PathBuf::from("/m"), "m".into());
        f.set_files(
            paths.iter().map(|p| info(p)).collect(),
            &Settings::default(),
        );
        f
    }

    #[test]
    fn sync_keeps_loaded_data_and_reports_added_and_removed() {
        let mut f = folder_with(&["/m/a.mp3", "/m/c.mp3", "/m/d.mp3"]);
        f.entries[1].apply_meta(Some("Cé".into()), Some("Artista".into()), Some(5));
        f.entries[2].set_custom_title(Some("Mi d".into()));
        // Llega b (nueva, en medio) y desaparece a.
        let plan = f.sync_files(
            ["/m/b.mp3", "/m/c.mp3", "/m/d.mp3"]
                .iter()
                .map(PathBuf::from)
                .collect(),
            &Settings::default(),
        );
        assert_eq!(plan.old_to_new, vec![None, Some(1), Some(2)]);
        assert_eq!(plan.added, vec![0]);
        assert_eq!(f.entries.len(), 3);
        assert_eq!(f.entries[1].display_title(), "Cé");
        assert_eq!(f.entries[1].info.duration_ms, Some(5));
        assert_eq!(f.entries[2].display_title(), "Mi d");
        assert_eq!(f.entries[0].display_title(), "b");
    }

    #[test]
    fn sync_with_no_changes_is_identity_and_new_files_get_saved_titles() {
        let mut f = folder_with(&["/m/a.mp3", "/m/b.mp3"]);
        let same = f.sync_files(
            ["/m/a.mp3", "/m/b.mp3"].iter().map(PathBuf::from).collect(),
            &Settings::default(),
        );
        assert_eq!(same.old_to_new, vec![Some(0), Some(1)]);
        assert!(same.added.is_empty());
        let mut settings = Settings::default();
        settings.set_title(Path::new("/m/z.mp3"), Some("Z editada"));
        let plan = f.sync_files(
            ["/m/a.mp3", "/m/b.mp3", "/m/z.mp3"]
                .iter()
                .map(PathBuf::from)
                .collect(),
            &settings,
        );
        assert_eq!(plan.added, vec![2]);
        assert_eq!(f.entries[2].display_title(), "Z editada");
        let empty = f.sync_files(Vec::new(), &settings);
        assert_eq!(empty.old_to_new, vec![None, None, None]);
        assert!(f.entries.is_empty());
    }

    #[test]
    fn form_changes_only_include_modified_fields() {
        let orig = MetaForm {
            title: "Canción".into(),
            artist: "Ana".into(),
            album: "".into(),
            genre: "Rock".into(),
            year: "1999".into(),
        };
        assert!(orig.changes_from(&orig).is_empty());
        let mut f = orig.clone();
        f.artist = "  Ana  ".into(); // solo espacios: no cuenta
        assert!(f.changes_from(&orig).is_empty());
        f.title = " Otra ".into();
        f.genre = "".into(); // borrar
        f.album = "Disco".into();
        f.year = "2001".into();
        let c = f.changes_from(&orig);
        assert_eq!(c.title.as_deref(), Some("Otra"));
        assert_eq!(c.genre.as_deref(), Some(""));
        assert_eq!(c.album.as_deref(), Some("Disco"));
        assert_eq!(c.year.as_deref(), Some("2001"));
        assert_eq!(c.artist, None);
    }

    #[test]
    fn new_folder_starts_unloaded() {
        let f = Folder::new(7, PathBuf::from("/m"), "m".into());
        assert_eq!(
            (f.state, f.id, f.entries.len()),
            (LoadState::NotLoaded, 7, 0)
        );
    }

    #[test]
    fn folder_names_use_the_last_component_and_stay_unique() {
        let paths: Vec<PathBuf> = ["/a/Música", "/b/Rock", "/c/Música", "/d/Música"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            folder_names(&paths),
            vec!["Música", "Rock", "Música (2)", "Música (3)"]
        );
    }

    #[test]
    fn folder_names_handle_roots_and_trailing_separators() {
        let names = folder_names(&[PathBuf::from("/"), PathBuf::from("/x/Jazz/")]);
        assert_eq!(names[1], "Jazz");
        assert!(!names[0].is_empty());
    }
}
