use std::path::PathBuf;

/// Información básica de una pista de la biblioteca.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub path: PathBuf,
    /// Título de las etiquetas si existe; si no, el nombre del archivo sin extensión.
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
}

impl TrackInfo {
    /// Crea una pista solo con la ruta; el título es el nombre del archivo sin extensión.
    pub fn from_path(path: PathBuf) -> Self {
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            path,
            title,
            artist: None,
            duration_ms: None,
        }
    }

    /// Nombre del archivo con extensión.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_file_stem() {
        let t = TrackInfo::from_path(PathBuf::from("/musica/01 - Intro.flac"));
        assert_eq!(t.title, "01 - Intro");
        assert_eq!(t.file_name(), "01 - Intro.flac");
    }
}
