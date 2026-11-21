use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey, StandardVisualKey};
use symphonia::core::probe::Hint;

/// Imagen RGBA8 ya decodificada y reducida.
#[derive(Debug, Clone)]
pub struct CoverImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub cover: Option<CoverImage>,
}

/// Rellena solo los campos que aún faltan (la primera fuente gana).
fn collect(rev: &MetadataRevision, meta: &mut TrackMeta, cover_px: Option<u32>) {
    for tag in rev.tags() {
        match tag.std_key {
            Some(StandardTagKey::TrackTitle) if meta.title.is_none() => {
                let v = tag.value.to_string();
                if !v.trim().is_empty() {
                    meta.title = Some(v.trim().to_string());
                }
            }
            Some(StandardTagKey::Artist) if meta.artist.is_none() => {
                let v = tag.value.to_string();
                if !v.trim().is_empty() {
                    meta.artist = Some(v.trim().to_string());
                }
            }
            _ => {}
        }
    }
    if let (None, Some(px)) = (&meta.cover, cover_px) {
        let visual = rev
            .visuals()
            .iter()
            .find(|v| v.usage == Some(StandardVisualKey::FrontCover))
            .or_else(|| rev.visuals().first());
        if let Some(v) = visual {
            meta.cover = decode_cover(&v.data, px);
        }
    }
}

fn decode_cover(data: &[u8], max_px: u32) -> Option<CoverImage> {
    let img = image::load_from_memory(data).ok()?;
    let rgba = img.thumbnail(max_px, max_px).to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(CoverImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn probe(path: &Path) -> Option<symphonia::core::probe::ProbeResult> {
    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()
}

/// Lee título, artista, duración y (si existe) la carátula incrustada,
/// reducida a `cover_px` píxeles como máximo. Los errores se traducen en
/// campos vacíos: un archivo ilegible no impide mostrar el resto de la lista.
pub fn read_metadata(path: &Path, cover_px: u32) -> TrackMeta {
    read_inner(path, Some(cover_px))
}

/// Solo la carátula, a un tamaño mayor (para el reproductor principal).
pub fn read_cover(path: &Path, max_px: u32) -> Option<CoverImage> {
    read_inner(path, Some(max_px)).cover
}

fn read_inner(path: &Path, cover_px: Option<u32>) -> TrackMeta {
    let mut meta = TrackMeta::default();
    let Some(mut probed) = probe(path) else {
        return meta;
    };

    // Metadatos previos al contenedor (p. ej. ID3v2 en MP3).
    if let Some(m) = probed.metadata.get() {
        if let Some(rev) = m.current() {
            collect(rev, &mut meta, cover_px);
        }
    }
    // Metadatos del propio contenedor (Vorbis comments, FLAC, RIFF/AIFF...).
    if let Some(rev) = probed.format.metadata().current() {
        collect(rev, &mut meta, cover_px);
    }

    if let Some(track) = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    {
        if let (Some(n), Some(rate)) = (track.codec_params.n_frames, track.codec_params.sample_rate)
        {
            if rate > 0 {
                meta.duration_ms = Some(n * 1000 / rate as u64);
            }
        }
    }
    meta
}
