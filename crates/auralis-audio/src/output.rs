//! Salida de audio con CPAL.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::eq::{Equalizer, BANDS};
use crate::error::AudioError;
use crate::shared::Shared;

pub struct Output {
    /// Debe mantenerse vivo mientras se reproduzca (no es `Send`).
    pub _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: usize,
    pub device_name: String,
    pub format: String,
}

/// Segundos de audio que el decodificador puede adelantar al ring buffer.
const RING_SECONDS: usize = 2;
/// Tras cargar una pista, el callback espera a tener este audio acumulado
/// antes de empezar a sonar (evita arrancar con un bloque a medias).
const PREROLL_MS: usize = 60;
/// Rampa mínima (volumen y fundidos sin configurar): evita clics.
const RAMP_MS: f32 = 12.0;

/// Nombre del dispositivo de salida predeterminado ahora mismo (`None` si no hay ninguno).
pub fn default_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

pub fn open(shared: Arc<Shared>) -> Result<(Output, HeapProd<f32>), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no hay dispositivo de salida".into()))?;
    let device_name = device.name().unwrap_or_else(|_| "desconocido".into());
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Device(e.to_string()))?;

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0;

    // La capacidad es múltiplo de `channels` para no partir fotogramas.
    let capacity = sample_rate as usize * channels * RING_SECONDS;
    let (prod, cons) = HeapRb::<f32>::new(capacity).split();

    let stream = match sample_format {
        SampleFormat::F32 => build::<f32>(&device, &config, shared, cons, channels)?,
        SampleFormat::I16 => build::<i16>(&device, &config, shared, cons, channels)?,
        SampleFormat::U16 => build::<u16>(&device, &config, shared, cons, channels)?,
        other => {
            return Err(AudioError::Device(format!(
                "formato de muestra no soportado: {other:?}"
            )))
        }
    };
    stream
        .play()
        .map_err(|e| AudioError::Stream(e.to_string()))?;

    Ok((
        Output {
            _stream: stream,
            sample_rate,
            channels,
            device_name,
            format: format!("{sample_format:?}"),
        },
        prod,
    ))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
    mut cons: HeapCons<f32>,
    channels: usize,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let rate = config.sample_rate.0 as usize;
    // Paso por fotograma de una rampa de `ms` milisegundos (de 0 a 1).
    let step_for = move |ms: f32| 1.0 / (rate as f32 * ms.max(RAMP_MS) / 1000.0);
    let vol_step = step_for(RAMP_MS);
    let preroll_samples = rate * channels * PREROLL_MS / 1000;

    let mut scratch = vec![0.0f32; 16_384];
    let mut local_ack = 0u64;
    // Dos ganancias independientes, ambas con rampas para no producir saltos (clics):
    //  - `vol`: el volumen del usuario (rampa corta, el deslizador responde al instante);
    //  - `fade`: el fundido de pausa / reanudación / cambio de pista, de duración
    //    configurable (la del crossfade, con tope). Se aplica con una curva suave.
    let mut vol = 0.0f32;
    let mut fade = 0.0f32;
    let mut eq = Equalizer::new(config.sample_rate.0, channels);
    let mut eq_version = 0u32;
    let mut prerolling = true;
    let err_shared = shared.clone();

    let callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        // --- Tiempo real: solo atómicos y el consumidor del ring buffer. ---
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
        if data.len() > scratch.len() {
            scratch.resize(data.len(), 0.0); // excepcional
        }
        let buf = &mut scratch[..data.len()];

        let playing = shared.playing.load(Ordering::Relaxed);
        let volume = f32::from_bits(shared.gain_bits.load(Ordering::Relaxed));

        // Petición de flush (nueva pista o seek): primero se baja la ganancia a
        // cero con una rampa y solo entonces se descarta el audio viejo.
        let req = shared.flush_req.load(Ordering::Acquire);
        if req != local_ack && fade <= 0.0 {
            cons.clear();
            eq.reset();
            shared.played_frames.store(0, Ordering::Relaxed);
            local_ack = req;
            shared.flush_ack.store(req, Ordering::Release);
            prerolling = true;
        }
        let flushing = req != local_ack;

        let streaming = shared.streaming.load(Ordering::Relaxed);
        if prerolling && (cons.occupied_len() >= preroll_samples || !streaming) {
            prerolling = false;
        }

        // Ecualizador: se relee solo cuando la interfaz lo cambió.
        let ver = shared.eq_version.load(Ordering::Acquire);
        if ver != eq_version {
            eq_version = ver;
            let mut db = [0.0f32; BANDS];
            for (d, a) in db.iter_mut().zip(shared.eq_gains.iter()) {
                *d = a.load(Ordering::Relaxed) as f32 / 10.0;
            }
            eq.set_gains(shared.eq_enabled.load(Ordering::Relaxed), &db);
        }

        let fade_target = if playing && !flushing { 1.0f32 } else { 0.0 };
        let in_step = step_for(shared.fade_in_ms.load(Ordering::Relaxed) as f32);
        let out_step = step_for(shared.fade_out_ms.load(Ordering::Relaxed) as f32);
        if prerolling || (fade_target <= 0.0 && fade <= 0.0) {
            buf.fill(0.0);
        } else {
            let got = cons.pop_slice(buf);
            if got < buf.len() && playing && streaming && !flushing {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            let frames = got / channels;
            eq.process(&mut buf[..frames * channels]);
            for frame in buf[..frames * channels].chunks_exact_mut(channels) {
                vol += (volume - vol).clamp(-vol_step, vol_step);
                fade += (fade_target - fade).clamp(-out_step, in_step);
                let g = vol * (fade * fade * (3.0 - 2.0 * fade)); // smoothstep
                for s in frame {
                    *s *= g;
                }
            }
            shared
                .played_frames
                .fetch_add(frames as u64, Ordering::Relaxed);
            buf[frames * channels..].fill(0.0);
        }
        shared
            .ring_fill
            .store(cons.occupied_len() as u64, Ordering::Relaxed);

        for (o, s) in data.iter_mut().zip(buf.iter()) {
            *o = T::from_sample(s.clamp(-1.0, 1.0));
        }
    };

    // Los errores del stream se guardan para mostrarlos en la interfaz
    // (en release no hay consola donde verlos).
    let on_error = move |e: cpal::StreamError| {
        err_shared.stream_lost.store(true, Ordering::Relaxed);
        if let Ok(mut slot) = err_shared.stream_error.lock() {
            *slot = Some(e.to_string());
        }
    };

    device
        .build_output_stream(config, callback, on_error, None)
        .map_err(|e| AudioError::Stream(e.to_string()))
}
