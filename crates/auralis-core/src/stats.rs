//! Estadísticas de escucha: tiempo por día (música y podcast), canciones y artistas.
//!
//! Lógica pura (sin reloj ni disco propios): quien la usa pasa el «número de día»
//! local (días desde 1970-01-01) y los segundos escuchados. Se guarda como texto
//! `clave=valor` y nunca falla al leer: lo ilegible se ignora.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Segundos de escucha a partir de los cuales una canción cuenta como «reproducida».
pub const PLAY_THRESHOLD_SECS: u32 = 30;

/// Máximo de canciones distintas que se recuerdan (las menos escuchadas se podan).
const MAX_TRACKS: usize = 5000;

// ---- Calendario civil (algoritmos de Howard Hinnant) ------------------------

/// Días desde 1970-01-01 para una fecha civil.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i32 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Fecha civil `(año, mes, día)` de un número de día.
pub fn civil_from_days(z: i32) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Número de día local a partir de segundos Unix y el desfase de la zona horaria.
pub fn day_number(unix_secs: i64, utc_offset_secs: i32) -> i32 {
    (unix_secs + utc_offset_secs as i64).div_euclid(86_400) as i32
}

/// «45 s», «12 min», «3 h 05 min».
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h {:02} min", secs / 3600, secs % 3600 / 60)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayStat {
    pub music_secs: u64,
    pub podcast_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackStat {
    pub plays: u32,
    pub secs: u64,
    pub podcast: bool,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub days: BTreeMap<i32, DayStat>,
    pub tracks: BTreeMap<PathBuf, TrackStat>,
}

/// Resumen listo para mostrar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub today_secs: u64,
    pub week_secs: u64,
    pub month_secs: u64,
    pub year_secs: u64,
    pub total_music_secs: u64,
    pub total_podcast_secs: u64,
    pub days_listened: u32,
    /// Promedio diario (música) sobre los días en que se escuchó algo.
    pub avg_day_secs: u64,
    pub streak_days: u32,
    pub top_artist: Option<(String, u64)>,
    pub top_track: Option<(String, String, u32)>,
    pub top_podcast: Option<(String, u64)>,
    pub total_plays: u32,
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o)
            }
            None => out.push('\\'),
        }
    }
    out
}

impl Stats {
    /// Suma tiempo escuchado a un día y a una canción.
    pub fn add_listen(
        &mut self,
        day: i32,
        podcast: bool,
        path: &Path,
        title: &str,
        artist: &str,
        secs: u32,
    ) {
        if secs == 0 {
            return;
        }
        let d = self.days.entry(day).or_default();
        if podcast {
            d.podcast_secs += secs as u64;
        } else {
            d.music_secs += secs as u64;
        }
        let t = self.tracks.entry(path.to_path_buf()).or_default();
        t.secs += secs as u64;
        t.podcast = podcast;
        if !title.is_empty() {
            t.title = title.to_string();
        }
        t.artist = artist.to_string();
        if self.tracks.len() > MAX_TRACKS {
            self.prune();
        }
    }

    /// Cuenta una reproducción de la canción.
    pub fn add_play(&mut self, path: &Path) {
        self.tracks.entry(path.to_path_buf()).or_default().plays += 1;
    }

    fn prune(&mut self) {
        let mut v: Vec<(u64, PathBuf)> = self
            .tracks
            .iter()
            .map(|(p, t)| (t.secs, p.clone()))
            .collect();
        v.sort();
        let excess = self.tracks.len().saturating_sub(MAX_TRACKS);
        for (_, p) in v.into_iter().take(excess) {
            self.tracks.remove(&p);
        }
    }

    fn range_secs(&self, from: i32, to: i32) -> u64 {
        self.days
            .range(from..=to)
            .map(|(_, d)| d.music_secs + d.podcast_secs)
            .sum()
    }

    /// Calcula el resumen tomando `today` como el día actual.
    pub fn summary(&self, today: i32) -> Summary {
        let (y, m, _) = civil_from_days(today);
        let month_start = days_from_civil(y, m, 1);
        let year_start = days_from_civil(y, 1, 1);
        let mut s = Summary {
            today_secs: self.range_secs(today, today),
            week_secs: self.range_secs(today - 6, today),
            month_secs: self.range_secs(month_start, today),
            year_secs: self.range_secs(year_start, today),
            ..Summary::default()
        };
        let mut music_days = 0u32;
        for d in self.days.values() {
            s.total_music_secs += d.music_secs;
            s.total_podcast_secs += d.podcast_secs;
            if d.music_secs > 0 {
                music_days += 1;
            }
        }
        s.days_listened = self
            .days
            .values()
            .filter(|d| d.music_secs + d.podcast_secs > 0)
            .count() as u32;
        if music_days > 0 {
            s.avg_day_secs = s.total_music_secs / music_days as u64;
        }
        // Racha: días consecutivos con escucha, terminando hoy (o ayer si hoy aún no).
        let active = |d: i32| {
            self.days
                .get(&d)
                .is_some_and(|x| x.music_secs + x.podcast_secs > 0)
        };
        let mut d = if active(today) { today } else { today - 1 };
        while active(d) {
            s.streak_days += 1;
            d -= 1;
        }
        // Artista más escuchado (solo música) por tiempo.
        let mut by_artist: BTreeMap<&str, u64> = BTreeMap::new();
        let mut by_show: BTreeMap<&str, u64> = BTreeMap::new();
        for t in self.tracks.values() {
            if t.artist.trim().is_empty() {
                continue;
            }
            let map = if t.podcast {
                &mut by_show
            } else {
                &mut by_artist
            };
            *map.entry(t.artist.as_str()).or_default() += t.secs;
        }
        s.top_artist = by_artist
            .into_iter()
            .max_by_key(|(n, v)| (*v, std::cmp::Reverse(*n)))
            .map(|(n, v)| (n.to_string(), v));
        s.top_podcast = by_show
            .into_iter()
            .max_by_key(|(n, v)| (*v, std::cmp::Reverse(*n)))
            .map(|(n, v)| (n.to_string(), v));
        s.total_plays = self.tracks.values().map(|t| t.plays).sum();
        s.top_track = self
            .tracks
            .values()
            .filter(|t| !t.podcast && t.plays > 0)
            .max_by_key(|t| (t.plays, t.secs))
            .map(|t| (t.title.clone(), t.artist.clone(), t.plays));
        s
    }

    /// Las `n` canciones (no podcast) más reproducidas: `(título, artista, reproducciones)`.
    pub fn top_tracks(&self, n: usize) -> Vec<(String, String, u32)> {
        let mut v: Vec<&TrackStat> = self
            .tracks
            .values()
            .filter(|t| !t.podcast && t.plays > 0)
            .collect();
        v.sort_by(|a, b| (b.plays, b.secs).cmp(&(a.plays, a.secs)));
        v.into_iter()
            .take(n)
            .map(|t| (t.title.clone(), t.artist.clone(), t.plays))
            .collect()
    }

    /// Los `n` artistas más escuchados (solo música): `(nombre, segundos)`.
    pub fn top_artists(&self, n: usize) -> Vec<(String, u64)> {
        let mut m: BTreeMap<&str, u64> = BTreeMap::new();
        for t in self.tracks.values().filter(|t| !t.podcast) {
            if !t.artist.trim().is_empty() {
                *m.entry(t.artist.as_str()).or_default() += t.secs;
            }
        }
        let mut v: Vec<(String, u64)> = m.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// Segundos totales (música + podcast) de los últimos `n` días terminando en `today`,
    /// del más antiguo al más reciente.
    pub fn last_days(&self, today: i32, n: usize) -> Vec<u64> {
        (0..n as i32)
            .rev()
            .map(|i| self.range_secs(today - i, today - i))
            .collect()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("version=1\n");
        for (day, d) in &self.days {
            out.push_str(&format!("d={day}\t{}\t{}\n", d.music_secs, d.podcast_secs));
        }
        for (p, t) in &self.tracks {
            out.push_str(&format!(
                "t={}\t{}\t{}\t{}\t{}\t{}\n",
                escape(&p.to_string_lossy()),
                t.plays,
                t.secs,
                t.podcast as u8,
                escape(&t.title),
                escape(&t.artist)
            ));
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Stats::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let f: Vec<&str> = v.split('\t').collect();
            match k {
                "d" if f.len() == 3 => {
                    if let (Ok(d), Ok(m), Ok(p)) = (f[0].parse(), f[1].parse(), f[2].parse()) {
                        s.days.insert(
                            d,
                            DayStat {
                                music_secs: m,
                                podcast_secs: p,
                            },
                        );
                    }
                }
                "t" if f.len() == 6 => {
                    let path = PathBuf::from(unescape(f[0]));
                    if let (Ok(plays), Ok(secs)) = (f[1].parse(), f[2].parse()) {
                        if !path.as_os_str().is_empty() {
                            s.tracks.insert(
                                path,
                                TrackStat {
                                    plays,
                                    secs,
                                    podcast: f[3] == "1",
                                    title: unescape(f[4]),
                                    artist: unescape(f[5]),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(b) => Ok(Self::parse(&String::from_utf8_lossy(&b))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

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

/// Acumula el tiempo escuchado de la canción en curso y avisa cuándo cuenta como
/// reproducción. El llamador pasa los segundos reales transcurridos mientras suena.
#[derive(Debug, Default)]
pub struct ListenTracker {
    current: Option<PathBuf>,
    acc: u32,
    counted: bool,
}

impl ListenTracker {
    /// Suma `secs` a `path`. Devuelve `true` UNA vez por pista cuando se cruza el umbral.
    pub fn tick(&mut self, path: &Path, secs: u32) -> bool {
        if self.current.as_deref() != Some(path) {
            self.current = Some(path.to_path_buf());
            self.acc = 0;
            self.counted = false;
        }
        self.acc += secs;
        if !self.counted && self.acc >= PLAY_THRESHOLD_SECS {
            self.counted = true;
            return true;
        }
        false
    }

    /// Olvida la pista actual (p. ej. al repetirla, para que cuente otra vez).
    pub fn reset(&mut self) {
        self.current = None;
        self.acc = 0;
        self.counted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversions_roundtrip_and_known_values() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        for d in (-800..40_000).step_by(37) {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(format_duration(0), "0 s");
        assert_eq!(format_duration(59), "59 s");
        assert_eq!(format_duration(60), "1 min");
        assert_eq!(format_duration(3599), "59 min");
        assert_eq!(format_duration(3600), "1 h 00 min");
        assert_eq!(format_duration(3600 * 130 + 65 * 60), "131 h 05 min");
    }

    #[test]
    fn day_number_respects_timezone() {
        // 2026-11-26 02:00 UTC es todavía el 25 en Bogotá (UTC-5).
        let utc = days_from_civil(2026, 11, 26) as i64 * 86_400 + 2 * 3600;
        assert_eq!(day_number(utc, 0), days_from_civil(2026, 11, 26));
        assert_eq!(day_number(utc, -5 * 3600), days_from_civil(2026, 11, 25));
        assert_eq!(day_number(-1, 0), -1);
    }

    fn sample() -> (Stats, i32) {
        let today = days_from_civil(2026, 11, 25);
        let mut s = Stats::default();
        s.add_listen(today, false, Path::new("/a.mp3"), "A", "Ana", 600);
        s.add_listen(today - 1, false, Path::new("/b.mp3"), "B", "Ana", 300);
        s.add_listen(today - 20, false, Path::new("/c.mp3"), "C", "Beto", 1000);
        s.add_listen(today - 40, true, Path::new("/p.mp3"), "Ep", "Show", 3600);
        s.add_listen(
            days_from_civil(2025, 12, 31),
            false,
            Path::new("/c.mp3"),
            "C",
            "Beto",
            60,
        );
        for _ in 0..3 {
            s.add_play(Path::new("/a.mp3"));
        }
        s.add_play(Path::new("/c.mp3"));
        (s, today)
    }

    #[test]
    fn summary_windows_totals_and_tops() {
        let (s, today) = sample();
        let r = s.summary(today);
        assert_eq!(r.today_secs, 600);
        assert_eq!(r.week_secs, 900);
        assert_eq!(r.month_secs, 600 + 300 + 1000); // noviembre: hace 20 días es el 5
        assert_eq!(r.year_secs, 600 + 300 + 1000 + 3600);
        assert_eq!(r.total_music_secs, 600 + 300 + 1000 + 60);
        assert_eq!(r.total_podcast_secs, 3600);
        assert_eq!(r.streak_days, 2);
        assert_eq!(r.top_artist, Some(("Beto".into(), 1060)));
        assert_eq!(r.top_podcast, Some(("Show".into(), 3600)));
        assert_eq!(r.top_track, Some(("A".into(), "Ana".into(), 3)));
        assert_eq!(r.total_plays, 4);
        assert_eq!(s.top_tracks(1).len(), 1);
        assert_eq!(s.top_artists(5)[0].0, "Beto");
        assert_eq!(s.last_days(today, 3), vec![0, 300, 600]);
    }

    #[test]
    fn streak_can_end_yesterday_and_empty_is_safe() {
        let (s, today) = sample();
        assert_eq!(s.summary(today + 1).streak_days, 2);
        assert_eq!(s.summary(today + 2).streak_days, 0);
        let e = Stats::default().summary(today);
        assert_eq!(e, Summary::default());
        assert!(Stats::default().top_tracks(3).is_empty());
    }

    #[test]
    fn roundtrip_with_awkward_text_and_garbage() {
        let mut s = Stats::default();
        s.add_listen(
            5,
            false,
            Path::new("C:\\m\\a\tb.mp3"),
            "Tí\ttulo\n",
            "Ar\\tista",
            12,
        );
        s.add_play(Path::new("C:\\m\\a\tb.mp3"));
        let r = Stats::parse(&s.to_text());
        assert_eq!(r, s);
        let g = Stats::parse("d=x\t1\t2\nd=1\t2\nt=a\tb\nbasura\nt=\t1\t1\t0\tx\ty\n");
        assert_eq!(g, Stats::default());
    }

    #[test]
    fn zero_seconds_are_ignored_and_pruning_keeps_the_most_listened() {
        let mut s = Stats::default();
        s.add_listen(1, false, Path::new("/z"), "Z", "Z", 0);
        assert!(s.days.is_empty() && s.tracks.is_empty());
        for i in 0..=MAX_TRACKS {
            s.add_listen(
                1,
                false,
                Path::new(&format!("/t{i}")),
                "t",
                "a",
                1 + i as u32,
            );
        }
        assert_eq!(s.tracks.len(), MAX_TRACKS);
        assert!(s.tracks.contains_key(Path::new(&format!("/t{MAX_TRACKS}"))));
        assert!(!s.tracks.contains_key(Path::new("/t0")));
    }

    #[test]
    fn listen_tracker_counts_once_per_track_and_resets() {
        let mut t = ListenTracker::default();
        let a = Path::new("/a");
        assert!(!t.tick(a, 29));
        assert!(t.tick(a, 1));
        assert!(!t.tick(a, 100));
        assert!(!t.tick(Path::new("/b"), 10));
        assert!(t.tick(Path::new("/b"), 20));
        t.reset();
        assert!(t.tick(Path::new("/b"), 30));
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("acustia-stats-{}", std::process::id()));
        let p = dir.join("s.txt");
        let (s, _) = sample();
        s.save(&p).unwrap();
        assert_eq!(Stats::load(&p).unwrap(), s);
        assert_eq!(Stats::load(&dir.join("nada")).unwrap(), Stats::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
