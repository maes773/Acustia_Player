//! Edición de etiquetas incrustadas en el archivo (título, artista, álbum, género,
//! año y carátula) con `lofty`.
//!
//! Se escribe solo lo que cambió y el archivo se abre una única vez. La lectura de
//! los campos para el editor no carga las imágenes incrustadas (pueden pesar varios
//! MB); la carátula se pide aparte con [`crate::read_cover`].

use std::io::Cursor;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::FileType;
use lofty::picture::{Picture, PictureType};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};

/// Valores de texto de una canción tal y como están en el archivo (vacío = sin valor).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FullTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// Año de cuatro cifras (o vacío).
    pub year: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CoverChange {
    #[default]
    Keep,
    Remove,
    /// Imagen ya lista para incrustar (JPEG o PNG, ver [`prepare_cover`]).
    Replace(Vec<u8>),
}

/// Cambios a escribir. `None` = no tocar ese campo; `Some("")` = borrarlo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagChanges {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub cover: CoverChange,
}

impl TagChanges {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.genre.is_none()
            && self.year.is_none()
            && self.cover == CoverChange::Keep
    }
}

/// Valida el año escrito por el usuario: vacío o cuatro cifras (0001–9999).
pub fn parse_year(text: &str) -> Result<Option<u32>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<u32>() {
        Ok(y) if t.len() == 4 && y >= 1 => Ok(Some(y)),
        _ => Err("El año debe tener 4 cifras (por ejemplo 1999)".to_string()),
    }
}

pub fn read_full_tags(path: &Path) -> Result<FullTags, String> {
    let tagged = Probe::open(path)
        .map_err(|e| e.to_string())?
        .options(ParseOptions::new().read_cover_art(false))
        .guess_file_type()
        .map_err(|e| e.to_string())?
        .read()
        .map_err(|e| e.to_string())?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(FullTags::default());
    };
    Ok(FullTags {
        title: tag.title().map(|c| c.into_owned()).unwrap_or_default(),
        artist: tag.artist().map(|c| c.into_owned()).unwrap_or_default(),
        album: tag.album().map(|c| c.into_owned()).unwrap_or_default(),
        genre: tag.genre().map(|c| c.into_owned()).unwrap_or_default(),
        year: tag.year().map(|y| format!("{y:04}")).unwrap_or_default(),
    })
}

fn apply_text(tag: &mut Tag, ch: &TagChanges) -> Result<(), String> {
    fn text(value: &Option<String>) -> Option<Option<String>> {
        value.as_ref().map(|v| {
            let v = v.trim();
            (!v.is_empty()).then(|| v.to_string())
        })
    }
    match text(&ch.title) {
        Some(Some(v)) => tag.set_title(v),
        Some(None) => tag.remove_title(),
        None => {}
    }
    match text(&ch.artist) {
        Some(Some(v)) => tag.set_artist(v),
        Some(None) => tag.remove_artist(),
        None => {}
    }
    match text(&ch.album) {
        Some(Some(v)) => tag.set_album(v),
        Some(None) => tag.remove_album(),
        None => {}
    }
    match text(&ch.genre) {
        Some(Some(v)) => tag.set_genre(v),
        Some(None) => tag.remove_genre(),
        None => {}
    }
    if let Some(y) = &ch.year {
        match parse_year(y)? {
            Some(y) => tag.set_year(y),
            None => tag.remove_year(),
        }
    }
    Ok(())
}

/// Escribe los cambios en el propio archivo. Devuelve un mensaje legible si falla
/// (archivo de solo lectura, formato sin soporte de etiquetas, etc.).
pub fn write_tags(path: &Path, ch: &TagChanges) -> Result<(), String> {
    if ch.is_empty() {
        return Ok(());
    }
    if let Some(y) = &ch.year {
        parse_year(y)?; // validar antes de tocar el archivo
    }
    // Se leen también las imágenes: al guardar se reescribe la etiqueta entera y, si no
    // estuvieran cargadas, se perderían las que no se están cambiando.
    let mut tagged = Probe::open(path)
        .map_err(|e| e.to_string())?
        .guess_file_type()
        .map_err(|e| e.to_string())?
        .read()
        .map_err(|e| e.to_string())?;

    // WAV y AIFF no tienen un sitio estándar para imágenes salvo ID3v2: se usa ese.
    let primary = tagged.primary_tag_type();
    let target = match tagged.file_type() {
        FileType::Wav | FileType::Aiff => TagType::Id3v2,
        _ => primary,
    };
    if tagged.tag(target).is_none() {
        tagged.insert_tag(Tag::new(target));
    }
    {
        let tag = tagged
            .tag_mut(target)
            .ok_or_else(|| "el formato no admite etiquetas".to_string())?;
        apply_text(tag, ch)?;
        match &ch.cover {
            CoverChange::Keep => {}
            CoverChange::Remove | CoverChange::Replace(_) => {
                for _ in 0..tag.picture_count() {
                    tag.remove_picture(0);
                }
                if let CoverChange::Replace(bytes) = &ch.cover {
                    let mut pic = Picture::from_reader(&mut Cursor::new(bytes))
                        .map_err(|e| format!("imagen no válida: {e}"))?;
                    pic.set_pic_type(PictureType::CoverFront);
                    tag.push_picture(pic);
                }
            }
        }
    }
    // Si el archivo ya tenía otra etiqueta de texto (p. ej. RIFF INFO en un WAV), se
    // mantiene coherente para que otros programas no muestren el nombre viejo.
    if primary != target {
        if let Some(other) = tagged.tag_mut(primary) {
            apply_text(other, ch)?;
        }
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| e.to_string())
}

/// Tamaño por encima del cual se reduce la imagen elegida antes de incrustarla.
const COVER_KEEP_MAX_BYTES: usize = 900_000;
/// Lado máximo (px) de la carátula incrustada tras reducirla.
const COVER_MAX_PX: u32 = 1000;

fn is_jpeg_or_png(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF]) || b.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// Prepara la imagen elegida por el usuario para incrustarla de forma ligera: un
/// JPEG/PNG de hasta ~900 KB se incrusta tal cual (sin recomprimir ni perder calidad);
/// si es más grande o de otro formato se reduce a 1000 px y se guarda como JPEG.
pub fn prepare_cover(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("no se pudo leer la imagen: {e}"))?;
    if bytes.len() <= COVER_KEEP_MAX_BYTES && is_jpeg_or_png(&bytes) {
        return Ok(bytes);
    }
    let img = image::load_from_memory(&bytes).map_err(|e| format!("imagen no válida: {e}"))?;
    let img = if img.width() > COVER_MAX_PX || img.height() > COVER_MAX_PX {
        img.resize(
            COVER_MAX_PX,
            COVER_MAX_PX,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&rgb)
        .map_err(|e| format!("no se pudo comprimir la imagen: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_validation() {
        assert_eq!(parse_year("").unwrap(), None);
        assert_eq!(parse_year("  1999 ").unwrap(), Some(1999));
        for bad in ["99", "19999", "abcd", "-199", "20 1", "0000"] {
            assert!(parse_year(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_changes_are_detected() {
        assert!(TagChanges::default().is_empty());
        let c = TagChanges {
            genre: Some(String::new()),
            ..Default::default()
        };
        assert!(!c.is_empty());
        let c = TagChanges {
            cover: CoverChange::Remove,
            ..Default::default()
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn jpeg_and_png_signatures() {
        assert!(is_jpeg_or_png(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_jpeg_or_png(&[0x89, b'P', b'N', b'G', 13]));
        assert!(!is_jpeg_or_png(b"GIF89a"));
        assert!(!is_jpeg_or_png(&[]));
    }
}
