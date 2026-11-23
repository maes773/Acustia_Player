//! Ajustes que Auralis recuerda entre sesiones: tema, carpetas añadidas, carpeta
//! seleccionada y nombres de canciones editados por el usuario.
//!
//! Formato: texto plano, una entrada por línea (`clave=valor`), con `\`, saltos de
//! línea y tabuladores escapados. Se eligió texto propio para no depender de
//! `serde`; es legible y tolerante: las líneas desconocidas o dañadas se ignoran
//! en vez de impedir arrancar.
//!
//! ```text
//! version=1
//! dark=1
//! selected=0
//! folder=C:\\Users\\yo\\Música
//! title=C:\\Users\\yo\\Música\\01.mp3<TAB>Mi nombre para la canción
//! ```
//!
//! Los nombres editados se guardan **aparte del archivo de audio**: nunca se
//! modifica el archivo ni sus etiquetas.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// Tema oscuro activado.
    pub dark: bool,
    /// Carpetas de música, en el orden en que se añadieron.
    pub folders: Vec<PathBuf>,
    /// Carpeta seleccionada (siempre un índice válido, o 0 si no hay carpetas).
    pub selected: usize,
    /// Ruta del archivo → nombre mostrado elegido por el usuario.
    pub titles: BTreeMap<PathBuf, String>,
}

/// Clave de comparación de rutas: sin separador final y, en Windows, sin
/// distinguir mayúsculas (`C:\Música` y `c:\música\` son la misma carpeta).
fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let t = s.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        t.to_lowercase()
    } else {
        t.to_string()
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl Settings {
    /// Lee ajustes de texto. Nunca falla: lo que no se entiende se ignora.
    pub fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        let mut selected = 0usize;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "dark" => s.dark = value.trim() == "1",
                "selected" => selected = value.trim().parse().unwrap_or(0),
                "folder" => {
                    let path = PathBuf::from(unescape(value));
                    if !path.as_os_str().is_empty() && !s.folders.iter().any(|f| same_path(f, &path))
                    {
                        s.folders.push(path);
                    }
                }
                "title" => {
                    if let Some((path, title)) = value.split_once('\t') {
                        let title = unescape(title);
                        if !title.trim().is_empty() {
                            s.titles.insert(PathBuf::from(unescape(path)), title);
                        }
                    }
                }
                _ => {}
            }
        }
        s.selected = if selected < s.folders.len() { selected } else { 0 };
        s
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "version={SETTINGS_VERSION}\ndark={}\nselected={}\n",
            self.dark as u8, self.selected
        );
        for f in &self.folders {
            out.push_str(&format!("folder={}\n", escape(&f.to_string_lossy())));
        }
        for (path, title) in &self.titles {
            out.push_str(&format!(
                "title={}\t{}\n",
                escape(&path.to_string_lossy()),
                escape(title)
            ));
        }
        out
    }

    /// Carga desde `path`. Si el archivo no existe (primer arranque) devuelve los
    /// valores por defecto; otros errores de lectura se propagan.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(Self::parse(&String::from_utf8_lossy(&bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Guarda de forma atómica: escribe un archivo temporal y lo renombra, de modo
    /// que un cierre brusco no deja los ajustes a medias.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, self.to_text())?;
        fs::rename(&tmp, path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    // ---- Carpetas ----------------------------------------------------------

    /// Añade una carpeta y devuelve `(índice, nueva)`. Si ya estaba, no se
    /// duplica y se devuelve su índice con `nueva = false`.
    pub fn add_folder(&mut self, path: PathBuf) -> (usize, bool) {
        if let Some(i) = self.folders.iter().position(|f| same_path(f, &path)) {
            return (i, false);
        }
        self.folders.push(path);
        (self.folders.len() - 1, true)
    }

    /// Quita una carpeta de la lista (no toca los archivos de música). Los nombres
    /// editados de sus canciones se conservan por si se vuelve a añadir.
    pub fn remove_folder(&mut self, index: usize) -> Option<PathBuf> {
        if index >= self.folders.len() {
            return None;
        }
        let removed = self.folders.remove(index);
        if index < self.selected {
            self.selected -= 1;
        }
        if self.selected >= self.folders.len() {
            self.selected = self.folders.len().saturating_sub(1);
        }
        Some(removed)
    }

    pub fn select_folder(&mut self, index: usize) {
        if index < self.folders.len() {
            self.selected = index;
        }
    }

    // ---- Nombres editados --------------------------------------------------

    pub fn title_for(&self, file: &Path) -> Option<&str> {
        self.titles.get(file).map(String::as_str)
    }

    /// Fija el nombre mostrado de una canción. `None` o un texto vacío (o solo
    /// espacios) restaura el nombre original. Devuelve el nombre efectivo.
    pub fn set_title(&mut self, file: &Path, title: Option<&str>) -> Option<String> {
        match title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => {
                self.titles.insert(file.to_path_buf(), t.to_string());
                Some(t.to_string())
            }
            None => {
                self.titles.remove(file);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("auralis-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Settings {
        let mut s = Settings {
            dark: true,
            ..Default::default()
        };
        s.add_folder(PathBuf::from(r"C:\Users\yo\Música"));
        s.add_folder(PathBuf::from("/home/yo/Música/rock & roll"));
        s.selected = 1;
        s.set_title(Path::new(r"C:\Users\yo\Música\01.mp3"), Some("Mi canción"));
        s.set_title(Path::new("/home/yo/a.flac"), Some("Con\ttab, \\barra y\nsalto ñ 日本語"));
        s
    }

    #[test]
    fn roundtrip_keeps_everything_including_awkward_characters() {
        let s = sample();
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn defaults_for_empty_or_garbage_input() {
        assert_eq!(Settings::parse(""), Settings::default());
        let g = Settings::parse("esto no es un ajuste\n=\nfolder\n\u{0}\u{1}\ndark=quizá\nselected=abc\n");
        assert_eq!(g, Settings::default());
    }

    #[test]
    fn unknown_keys_and_future_versions_are_ignored() {
        let s = Settings::parse("version=99\nfuturo=1\ndark=1\nfolder=/a\n");
        assert!(s.dark);
        assert_eq!(s.folders, vec![PathBuf::from("/a")]);
    }

    #[test]
    fn selected_is_clamped_to_a_valid_folder() {
        assert_eq!(Settings::parse("selected=5\nfolder=/a\nfolder=/b\n").selected, 0);
        assert_eq!(Settings::parse("selected=1\nfolder=/a\nfolder=/b\n").selected, 1);
        assert_eq!(Settings::parse("selected=3\n").selected, 0);
    }

    #[test]
    fn duplicate_folders_are_not_added_twice() {
        let mut s = Settings::default();
        assert_eq!(s.add_folder(PathBuf::from("/musica")), (0, true));
        assert_eq!(s.add_folder(PathBuf::from("/musica/")), (0, false));
        assert_eq!(s.add_folder(PathBuf::from("/otra")), (1, true));
        assert_eq!(s.folders.len(), 2);
        // Y al leer un archivo con duplicados.
        assert_eq!(Settings::parse("folder=/a\nfolder=/a/\n").folders.len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_ignore_case() {
        let mut s = Settings::default();
        s.add_folder(PathBuf::from(r"C:\Música"));
        assert_eq!(s.add_folder(PathBuf::from(r"c:\música\")), (0, false));
    }

    #[test]
    fn removing_folders_keeps_the_selection_coherent() {
        let mk = || {
            let mut s = Settings::default();
            for f in ["/a", "/b", "/c"] {
                s.add_folder(PathBuf::from(f));
            }
            s
        };
        let mut s = mk();
        s.selected = 2;
        s.remove_folder(0); // quitar una anterior: la seleccionada sigue siendo /c
        assert_eq!((s.selected, &s.folders[s.selected]), (1, &PathBuf::from("/c")));

        let mut s = mk();
        s.selected = 2;
        s.remove_folder(2); // quitar la seleccionada (última): pasa a la anterior
        assert_eq!(s.selected, 1);

        let mut s = mk();
        s.selected = 0;
        s.remove_folder(0); // quitar la seleccionada (primera): queda la siguiente
        assert_eq!((s.selected, &s.folders[0]), (0, &PathBuf::from("/b")));

        let mut s = mk();
        s.remove_folder(0);
        s.remove_folder(0);
        s.remove_folder(0);
        assert_eq!((s.selected, s.folders.len()), (0, 0));
        assert_eq!(s.remove_folder(0), None);
    }

    #[test]
    fn select_ignores_out_of_range() {
        let mut s = Settings::default();
        s.add_folder(PathBuf::from("/a"));
        s.select_folder(7);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn titles_are_trimmed_and_can_be_restored() {
        let mut s = Settings::default();
        let f = Path::new("/m/x.mp3");
        assert_eq!(s.set_title(f, Some("  Nombre  ")), Some("Nombre".into()));
        assert_eq!(s.title_for(f), Some("Nombre"));
        assert_eq!(s.set_title(f, Some("   ")), None); // vacío = restaurar original
        assert_eq!(s.title_for(f), None);
        s.set_title(f, Some("Otra vez"));
        s.set_title(f, None);
        assert!(s.titles.is_empty());
    }

    #[test]
    fn removing_a_folder_keeps_its_custom_titles() {
        let mut s = Settings::default();
        s.add_folder(PathBuf::from("/m"));
        s.set_title(Path::new("/m/x.mp3"), Some("Mío"));
        s.remove_folder(0);
        assert_eq!(s.title_for(Path::new("/m/x.mp3")), Some("Mío"));
    }

    #[test]
    fn save_and_load_roundtrip_atomically() {
        let dir = temp_dir("rt");
        let file = dir.join("sub").join("settings.txt"); // crea la subcarpeta
        let s = sample();
        s.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), s);
        assert!(!dir.join("sub").join("settings.txt.tmp").exists(), "no debe quedar el temporal");
        // Sobrescribir uno existente.
        let mut s2 = s.clone();
        s2.dark = false;
        s2.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), s2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(Settings::load(&dir.join("no-existe.txt")).unwrap(), Settings::default());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_utf8_does_not_prevent_loading() {
        let dir = temp_dir("utf8");
        let file = dir.join("settings.txt");
        fs::write(&file, b"dark=1\nfolder=/a\n\xff\xfe\xfd\n").unwrap();
        let s = Settings::load(&file).unwrap();
        assert!(s.dark);
        assert_eq!(s.folders.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
