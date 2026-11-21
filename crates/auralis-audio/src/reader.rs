//! Lectura y decodificación de un archivo con Symphonia.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::error::AudioError;

pub struct TrackReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sample_buf: Option<SampleBuffer<f32>>,
    pub duration_ms: Option<u64>,
}

impl TrackReader {
    /// Abre `path` y, si `start_ms > 0`, se posiciona en ese instante.
    pub fn open(path: &Path, start_ms: u64) -> Result<Self, AudioError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        // `enable_gapless` hace que los lectores informen del retardo/relleno
        // del codificador (p. ej. cabecera LAME/Xing en MP3) para recortarlo.
        let fmt_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &fmt_opts,
            &MetadataOptions::default(),
        )?;
        let mut format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| AudioError::Decode("no hay pistas de audio decodificables".into()))?;
        let track_id = track.id;
        let params = track.codec_params.clone();

        let duration_ms = match (params.n_frames, params.sample_rate) {
            (Some(n), Some(rate)) if rate > 0 => Some(n * 1000 / rate as u64),
            _ => None,
        };

        let mut decoder =
            symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;

        if start_ms > 0 {
            let time = Time::new(start_ms / 1000, (start_ms % 1000) as f64 / 1000.0);
            format.seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(track_id),
                },
            )?;
            decoder.reset();
        }

        Ok(Self {
            format,
            decoder,
            track_id,
            sample_buf: None,
            duration_ms,
        })
    }

    /// Decodifica el siguiente paquete y añade las muestras (f32 entrelazado)
    /// a `out`. Devuelve `(frecuencia, canales)` del bloque, o `None` si se
    /// llegó al final del archivo. Los paquetes corruptos se omiten.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<Option<(u32, usize)>, AudioError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None)
                }
                Err(SymError::ResetRequired) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let channels = spec.channels.count();
                    let needed = decoded.capacity() * channels;
                    let too_small = self
                        .sample_buf
                        .as_ref()
                        .map_or(true, |b| b.capacity() < needed);
                    if too_small {
                        self.sample_buf =
                            Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
                    }
                    let buf = self.sample_buf.as_mut().expect("buffer creado arriba");
                    buf.copy_interleaved_ref(decoded);
                    out.extend_from_slice(buf.samples());
                    return Ok(Some((spec.rate, channels)));
                }
                // Paquete corrupto o error de E/S recuperable: se omite.
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
