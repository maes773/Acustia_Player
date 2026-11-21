//! Salida de audio con CPAL.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::error::AudioError;
use crate::shared::Shared;

pub struct Output {
    /// Debe mantenerse vivo mientras se reproduzca (no es `Send`).
    pub _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Segundos de audio que el decodificador puede adelantar al ring buffer.
const RING_SECONDS: usize = 1;

pub fn open(shared: Arc<Shared>) -> Result<(Output, HeapProd<f32>), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no hay dispositivo de salida".into()))?;
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
    let mut scratch = vec![0.0f32; 16_384];
    let mut local_ack = 0u64;

    let callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        // --- Tiempo real: solo atómicos y el consumidor del ring buffer. ---
        if data.len() > scratch.len() {
            scratch.resize(data.len(), 0.0); // excepcional
        }
        let buf = &mut scratch[..data.len()];

        let req = shared.flush_req.load(Ordering::Acquire);
        if req != local_ack {
            cons.clear();
            shared.played_frames.store(0, Ordering::Relaxed);
            local_ack = req;
            shared.flush_ack.store(req, Ordering::Release);
        }

        let mut got = 0;
        if shared.playing.load(Ordering::Relaxed) {
            got = cons.pop_slice(buf);
            let gain = f32::from_bits(shared.gain_bits.load(Ordering::Relaxed));
            for s in &mut buf[..got] {
                *s *= gain;
            }
            shared
                .played_frames
                .fetch_add((got / channels) as u64, Ordering::Relaxed);
        }
        buf[got..].fill(0.0);

        for (o, s) in data.iter_mut().zip(buf.iter()) {
            *o = T::from_sample(*s);
        }
    };

    device
        .build_output_stream(
            config,
            callback,
            |e| eprintln!("auralis-audio: error de stream: {e}"),
            None,
        )
        .map_err(|e| AudioError::Stream(e.to_string()))
}
