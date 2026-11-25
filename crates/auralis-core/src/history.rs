//! Historial de reproducción: las últimas canciones que han empezado a sonar.
//!
//! Se guarda en un archivo aparte de los ajustes (cambia con cada canción), en texto
//! plano: `segundos_unix \t ruta \t título \t artista`. Guarda título y artista tal como
//! se veían al reproducirlos, así el historial se entiende aunque la carpeta ya no esté.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Máximo de entradas que se recuerdan.
pub const HISTORY_LIMIT: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Momento en que empezó a sonar (segundos desde 1970).
    pub played_at: u64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// La más reciente primero.
    entries: Vec<HistoryEntry>,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl History {
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Anota que `path` empezó a sonar. Si es la misma canción que ya era la última
    /// (p. ej. se recargó al editarla o al cambiar de dispositivo) no se duplica: solo se
    /// actualizan sus datos. Devuelve `true` si cambió algo.
    pub fn record(&mut self, played_at: u64, path: PathBuf, title: String, artist: String) -> bool {
        if let Some(first) = self.entries.first_mut() {
            if first.path == path {
                let same = first.title == title && first.artist == artist;
                first.title = title;
                first.artist = artist;
                return !same;
            }
        }
        self.entries.insert(
            0,
            HistoryEntry {
                played_at,
                path,
                title,
                artist,
            },
        );
        self.entries.truncate(HISTORY_LIMIT);
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                e.played_at,
                esc(&e.path.to_string_lossy()),
                esc(&e.title),
                esc(&e.artist)
            ));
        }
        out
    }

    /// Nunca falla: las líneas dañadas se ignoran.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let mut it = line.split('\t');
            let (Some(t), Some(p), Some(title), Some(artist)) =
                (it.next(), it.next(), it.next(), it.next())
            else {
                continue;
            };
            let Ok(played_at) = t.trim().parse::<u64>() else {
                continue;
            };
            let path = unesc(p);
            if path.is_empty() {
                continue;
            }
            entries.push(HistoryEntry {
                played_at,
                path: PathBuf::from(path),
                title: unesc(title),
                artist: unesc(artist),
            });
        }
        entries.truncate(HISTORY_LIMIT);
        Self { entries }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(b) => Ok(Self::parse(&String::from_utf8_lossy(&b))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Guardado atómico (archivo temporal + renombrar).
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
}

/// Texto breve para «hace cuánto»: «ahora», «hace 5 min», «hace 3 h», «hace 2 d».
pub fn ago_text(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=44 => "ahora".into(),
        45..=3599 => format!("hace {} min", (d + 30) / 60),
        3600..=86_399 => format!("hace {} h", d / 3600),
        _ => format!("hace {} d", d / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(h: &mut History, t: u64, p: &str) -> bool {
        h.record(t, PathBuf::from(p), format!("T {p}"), "A".into())
    }

    #[test]
    fn most_recent_first_and_consecutive_duplicates_merge() {
        let mut h = History::default();
        assert!(rec(&mut h, 1, "/a"));
        assert!(rec(&mut h, 2, "/b"));
        assert!(!rec(&mut h, 3, "/b")); // misma canción recargada
        assert!(rec(&mut h, 4, "/a")); // volver a una anterior sí cuenta
        let paths: Vec<_> = h
            .entries()
            .iter()
            .map(|e| e.path.to_str().unwrap())
            .collect();
        assert_eq!(paths, ["/a", "/b", "/a"]);
        assert_eq!(h.entries()[1].played_at, 2); // la hora no se pisa
    }

    #[test]
    fn reload_refreshes_title_after_an_edit() {
        let mut h = History::default();
        h.record(1, "/a".into(), "Vieja".into(), "X".into());
        assert!(h.record(2, "/a".into(), "Nueva".into(), "X".into()));
        assert_eq!(h.len(), 1);
        assert_eq!(h.entries()[0].title, "Nueva");
    }

    #[test]
    fn limit_is_enforced() {
        let mut h = History::default();
        for i in 0..(HISTORY_LIMIT + 50) {
            h.record(
                i as u64,
                PathBuf::from(format!("/p{i}")),
                "t".into(),
                "a".into(),
            );
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(
            h.entries()[0].path,
            PathBuf::from(format!("/p{}", HISTORY_LIMIT + 49))
        );
    }

    #[test]
    fn roundtrip_with_awkward_text_and_garbage_lines() {
        let mut h = History::default();
        h.record(
            5,
            PathBuf::from("C:\\Música\\a\tb.mp3"),
            "Con\ttab\ny \\ barra".into(),
            "".into(),
        );
        h.record(6, PathBuf::from("/x.flac"), "日本語".into(), "Ñandú".into());
        assert_eq!(History::parse(&h.to_text()), h);
        let g = History::parse("basura\n\tx\ty\tz\nabc\t/p\tt\ta\n7\t\tt\ta\n8\t/ok\tt\ta\n");
        assert_eq!(g.len(), 1);
        assert_eq!(g.entries()[0].path, PathBuf::from("/ok"));
    }

    #[test]
    fn save_and_load() {
        let d = std::env::temp_dir().join(format!("acustia-hist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let f = d.join("h").join("history.txt");
        assert_eq!(History::load(&f).unwrap(), History::default());
        let mut h = History::default();
        h.record(1, "/a".into(), "A".into(), "B".into());
        h.save(&f).unwrap();
        assert_eq!(History::load(&f).unwrap(), h);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn ago() {
        assert_eq!(ago_text(100, 100), "ahora");
        assert_eq!(ago_text(100, 200), "ahora");
        assert_eq!(ago_text(1000, 1000 - 300), "hace 5 min");
        assert_eq!(ago_text(100_000, 100_000 - 7300), "hace 2 h");
        assert_eq!(ago_text(1_000_000, 1_000_000 - 200_000), "hace 2 d");
    }
}
