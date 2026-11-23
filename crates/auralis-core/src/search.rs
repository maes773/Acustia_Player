//! Búsqueda de canciones: sin distinguir mayúsculas ni acentos, y con todas las
//! palabras escritas presentes (en cualquier orden) en título, artista o nombre
//! de archivo.

/// Minúsculas y sin acentos comunes del alfabeto latino (`Canción` → `cancion`,
/// `NIÑO` → `nino`), de modo que se pueda buscar sin teclear tildes ni la ñ.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' => out.push('i'),
            'ñ' | 'ń' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'š' | 'ś' => out.push('s'),
            'ž' | 'ź' | 'ż' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            c => out.push(c),
        }
    }
    out
}

/// Texto de búsqueda precalculado de una canción (normalizado, campos unidos).
/// Se calcula una vez por canción y se reutiliza en cada pulsación de tecla.
pub fn search_key(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|f| normalize(f))
        .collect::<Vec<_>>()
        .join("\u{1f}") // separador que no aparece en texto normal
}

/// Consulta ya normalizada y dividida en palabras.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    words: Vec<String>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        Self {
            words: normalize(text)
                .split_whitespace()
                .map(str::to_string)
                .collect(),
        }
    }

    /// Sin palabras: coincide con todo.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// `key` debe provenir de [`search_key`]. Cada palabra debe aparecer en algún
    /// campo; así «queen bohemian» encuentra «Bohemian Rhapsody» de «Queen».
    pub fn matches_key(&self, key: &str) -> bool {
        self.words.iter().all(|w| key.contains(w.as_str()))
    }

    pub fn matches(&self, fields: &[&str]) -> bool {
        self.is_empty() || self.matches_key(&search_key(fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_accents() {
        assert_eq!(normalize("Canción"), "cancion");
        assert_eq!(normalize("NIÑO"), "nino");
        assert_eq!(normalize("Ångström Œuvre Straße"), "angstrom oeuvre strasse");
        assert_eq!(normalize("日本語 123"), "日本語 123");
    }

    #[test]
    fn empty_query_matches_everything() {
        let q = Query::new("   ");
        assert!(q.is_empty());
        assert!(q.matches(&["lo que sea"]));
        assert!(q.matches(&[]));
    }

    #[test]
    fn matches_without_accents_or_case() {
        assert!(Query::new("cancion").matches(&["Mi Canción Favorita", "", "01.mp3"]));
        assert!(Query::new("NINO").matches(&["El niño"]));
        assert!(Query::new("niño").matches(&["El nino"]));
        assert!(!Query::new("rock").matches(&["Mi Canción Favorita"]));
    }

    #[test]
    fn all_words_must_match_in_any_order_across_fields() {
        let fields = ["Bohemian Rhapsody", "Queen", "04 - queen.flac"];
        assert!(Query::new("queen bohemian").matches(&fields));
        assert!(Query::new("rhapsody  QUEEN").matches(&fields));
        assert!(!Query::new("queen thriller").matches(&fields));
    }

    #[test]
    fn matches_by_file_name_too() {
        assert!(Query::new("track07").matches(&["Sin título", "", "track07.mp3"]));
    }

    #[test]
    fn precomputed_key_gives_the_same_result() {
        let fields = ["Canción", "Artísta", "x.mp3"];
        let key = search_key(&fields);
        for q in ["cancion", "artista x", "nada", ""] {
            let q = Query::new(q);
            assert_eq!(q.matches_key(&key), q.matches(&fields));
        }
    }

    #[test]
    fn words_do_not_match_across_field_boundaries() {
        // «ab» no debe encontrarse porque «a» termina un campo y «b» empieza el siguiente.
        assert!(!Query::new("ab").matches(&["xa", "bx"]));
    }
}
