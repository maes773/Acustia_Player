//! Hilo decodificador: lee archivos, convierte al formato de salida y rellena
//! el ring buffer. Es el único que toca disco; el callback de audio nunca espera por él.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ringbuf::traits::{Observer, Producer};
use ringbuf::HeapProd;
use thread_priority::{set_current_thread_priority, ThreadPriority};

use crate::converter::Converter;
use crate::engine::Event;
use crate::reader::TrackReader;
use crate::replaygain::norm_gain;
use crate::shared::{Boundary, BoundaryKind, Shared, TrackRequest};

pub enum Cmd {
    Load {
        req: TrackRequest,
        start_ms: u64,
        epoch: u64,
    },
    Shutdown,
}

struct Active {
    req: TrackRequest,
    reader: TrackReader,
    conv: Option<Converter>,
    /// Fotogramas de salida producidos por esta pista (0 al llegar al final = fallo).
    produced: u64,
    /// Fotogramas de salida correspondientes al punto de la pista en que se empezó
    /// (distinto de 0 tras un seek).
    start_frames: u64,
    /// El archivo se leyó entero y la cola del conversor ya está en `pending`:
    /// en cuanto se vacíe se encadena la pista siguiente.
    eof: bool,
    /// Ganancia de normalización de la pista y el modo (álbum o pista) con que se calculó:
    /// ReplayGain si el archivo lo trae; si no, la sonoridad medida la primera vez que hace falta.
    norm_gain: Option<(bool, f32)>,
    /// Ganancia aplicada al último fotograma: el cambio hasta la ganancia deseada
    /// (p. ej. al activar o desactivar la normalización) se hace con una rampa.
    cur_gain: Option<f32>,
}

impl Active {
    /// Fotogramas de salida que tiene la pista entera, si se conoce su duración.
    fn total_frames(&self, out_rate: u32) -> Option<u64> {
        self.reader
            .duration_ms
            .map(|ms| ms * out_rate as u64 / 1000)
    }
}

/// Mezcla en curso entre la pista que acaba (`active`) y la que entra (`b`).
struct Fade {
    b: Active,
    /// Audio ya convertido (y con su ganancia) pendiente de mezclar.
    a_q: Vec<f32>,
    b_q: Vec<f32>,
    a_eof: bool,
    b_eof: bool,
    /// Duración de la mezcla y fotogramas mezclados hasta ahora.
    total: u64,
    done: u64,
}

enum FadeStep {
    Continue,
    /// La mezcla terminó: sigue sonando `b`.
    Done,
}

enum Pull {
    Data,
    Eof,
    Failed(String),
}

/// Duración mínima de una mezcla (si queda menos de esto se encadena sin mezclar).
const MIN_FADE_MS: u64 = 100;
/// Fotogramas por bloque al mezclar.
const FADE_CHUNK: usize = 2048;

/// Lee un bloque de `a`, lo convierte al formato de salida, le aplica su ganancia de
/// normalización y lo añade a `out`. Al final del archivo vacía también la cola del
/// conversor (sin esto se perdería el final de cada pista).
fn pull(
    ctx: &Ctx,
    a: &mut Active,
    raw: &mut Vec<f32>,
    tmp: &mut Vec<f32>,
    out: &mut Vec<f32>,
) -> Pull {
    raw.clear();
    match a.reader.read(raw) {
        Ok(Some((rate, channels))) => {
            let conv = a.conv.get_or_insert_with(|| {
                Converter::new(rate, channels, ctx.out_rate, ctx.out_channels)
            });
            tmp.clear();
            conv.process(raw, tmp);
            raw.clear();
            a.produced += (tmp.len() / ctx.out_channels) as u64;
            apply_gain(ctx, a, tmp);
            out.extend_from_slice(tmp);
            Pull::Data
        }
        Ok(None) => {
            tmp.clear();
            if let Some(conv) = a.conv.as_mut() {
                conv.finish(tmp);
            }
            a.produced += (tmp.len() / ctx.out_channels) as u64;
            apply_gain(ctx, a, tmp);
            out.extend_from_slice(tmp);
            a.eof = true;
            Pull::Eof
        }
        Err(e) => Pull::Failed(e.to_string()),
    }
}

/// Aplica la ganancia de normalización (si está activada) con una rampa lineal a lo
/// largo del bloque cuando cambia.
fn apply_gain(ctx: &Ctx, a: &mut Active, buf: &mut [f32]) {
    let ch = ctx.out_channels;
    let frames = buf.len() / ch;
    if frames == 0 {
        return;
    }
    let target = if ctx.shared.normalize.load(Ordering::Relaxed) {
        let album = ctx.shared.rg_album.load(Ordering::Relaxed);
        match a.norm_gain {
            Some((mode, g)) if mode == album => g,
            _ => {
                let path = &a.req.path;
                let (g, _) = norm_gain(&a.reader.replaygain, album, || {
                    ctx.shared.loudness.loudness_now(path)
                });
                a.norm_gain = Some((album, g));
                g
            }
        }
    } else {
        1.0
    };
    let from = a.cur_gain.unwrap_or(target);
    a.cur_gain = Some(target);
    if from == target {
        if target != 1.0 {
            for s in buf.iter_mut() {
                *s *= target;
            }
        }
        return;
    }
    let step = (target - from) / frames as f32;
    let mut g = from;
    for frame in buf.chunks_exact_mut(ch) {
        g += step;
        for s in frame {
            *s *= g;
        }
    }
}

/// Limitador suave para la zona mezclada: dos pistas a la vez pueden sumar más de 1,0.
fn soft_limit(x: f32) -> f32 {
    const KNEE: f32 = 0.9;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

pub struct Ctx {
    pub shared: Arc<Shared>,
    pub prod: HeapProd<f32>,
    pub out_rate: u32,
    pub out_channels: usize,
    pub events: Sender<(u64, Event)>,
}

struct State {
    ctx: Ctx,
    epoch: u64,
    /// Fotogramas empujados al ring buffer desde el último flush.
    pushed_frames: u64,
    pending: Vec<f32>,
    pending_pos: usize,
}

pub fn run(ctx: Ctx, rx: Receiver<Cmd>) {
    // Prioridad alta: este hilo no debe quedarse sin CPU frente a la interfaz
    // o al escaneo de la biblioteca. Si el sistema no lo permite, se sigue igual.
    let _ = set_current_thread_priority(ThreadPriority::Max);
    crate::platform::register_audio_thread();
    let mut st = State {
        ctx,
        epoch: 0,
        pushed_frames: 0,
        pending: Vec::new(),
        pending_pos: 0,
    };
    let mut active: Option<Active> = None;
    let mut fade: Option<Fade> = None;
    let mut raw: Vec<f32> = Vec::new();
    let mut converted: Vec<f32> = Vec::new();

    loop {
        // 1) Comandos. Si no hay nada que hacer, dormimos hasta recibir uno.
        let idle = active.is_none() && st.pending_pos >= st.pending.len();
        let cmd = if idle {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        } else {
            match rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Shutdown => return,
                Cmd::Load {
                    req,
                    start_ms,
                    epoch,
                } => {
                    st.epoch = epoch;
                    fade = None;
                    // `streaming` antes del flush: el callback hace pre-roll en vez
                    // de arrancar con el buffer vacío.
                    st.ctx.shared.streaming.store(true, Ordering::Relaxed);
                    st.flush();
                    raw.clear();
                    active = st.open(req, start_ms);
                    if active.is_none() {
                        // No se pudo abrir: `open` ya envió el `Error`. No se escribe
                        // `End` (generaría un `Finished` previo al error y el
                        // controlador no saltaría a la siguiente pista).
                        st.ctx.shared.streaming.store(false, Ordering::Relaxed);
                    }
                }
            }
            continue;
        }

        // 2) Vaciar lo pendiente hacia el ring buffer.
        if st.pending_pos < st.pending.len() {
            if !st.push_pending() {
                thread::sleep(Duration::from_millis(5)); // ring buffer lleno
            }
            continue;
        }

        // 3a) Mezcla (crossfade) en curso entre la pista que acaba y la que entra.
        if let Some(mut f) = fade.take() {
            let Some(a) = active.as_mut() else { continue };
            match st.fade_step(a, &mut f, &mut raw, &mut converted) {
                FadeStep::Continue => fade = Some(f),
                FadeStep::Done => active = Some(f.b),
            }
            continue;
        }

        // 3b) Fin de pista ya leída: encadenar la siguiente sin silencio (gapless).
        if active.as_ref().is_some_and(|a| a.eof) {
            let next = st
                .ctx
                .shared
                .next_slot
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            active = match next {
                Some(req) => st.open(req, 0),
                None => None,
            };
            if active.is_none() {
                st.push_end();
            }
            continue;
        }

        let Some(a) = active.as_mut() else { continue };

        // 3c) ¿Toca empezar un crossfade con la pista siguiente?
        if let Some(f) = st.try_start_fade(a) {
            fade = Some(f);
            continue;
        }

        // 3d) Decodificar más audio de la pista actual.
        let id = a.req.id;
        converted.clear();
        st.pending.clear();
        st.pending_pos = 0;
        match pull(&st.ctx, a, &mut raw, &mut converted, &mut st.pending) {
            Pull::Data => {}
            Pull::Eof => {
                if a.produced == 0 {
                    // Llegó al final sin haber producido ni un fotograma: no es una
                    // pista vacía legítima, es un fallo que no debe pasar en silencio.
                    let _ = st.ctx.events.send((
                        st.epoch,
                        Event::Error {
                            id,
                            message: "no se obtuvo audio del archivo".into(),
                        },
                    ));
                    st.push_end();
                    active = None;
                }
            }
            Pull::Failed(message) => {
                let _ = st.ctx.events.send((st.epoch, Event::Error { id, message }));
                st.push_end();
                active = None;
            }
        }
    }
}

impl State {
    /// Pide al callback que vacíe el ring buffer y espera su confirmación.
    fn flush(&mut self) {
        let shared = &self.ctx.shared;
        let req = shared.flush_req.fetch_add(1, Ordering::AcqRel) + 1;
        // El callback baja primero a silencio con un fundido (hasta `fade_out_ms`) y solo
        // entonces vacía el buffer: hay que esperar ese tiempo, o lo que se escribiera
        // después del flush se borraría junto con el audio viejo.
        let fade = shared.fade_out_ms.load(Ordering::Relaxed) as u64;
        let deadline = Instant::now() + Duration::from_millis(300 + fade);
        while shared.flush_ack.load(Ordering::Acquire) != req && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        self.pushed_frames = 0;
        self.pending.clear();
        self.pending_pos = 0;
        if let Ok(mut b) = shared.boundaries.lock() {
            b.clear();
        }
    }

    fn open(&mut self, req: TrackRequest, start_ms: u64) -> Option<Active> {
        match TrackReader::open(&req.path, start_ms) {
            Ok(reader) => {
                let boundary = Boundary {
                    epoch: self.epoch,
                    start_frame: self.pushed_frames,
                    kind: BoundaryKind::Start {
                        req: req.clone(),
                        offset_ms: start_ms,
                        duration_ms: reader.duration_ms,
                    },
                };
                if let Ok(mut b) = self.ctx.shared.boundaries.lock() {
                    b.push_back(boundary);
                }
                Some(Active {
                    req,
                    reader,
                    conv: None,
                    produced: 0,
                    start_frames: start_ms * self.ctx.out_rate as u64 / 1000,
                    eof: false,
                    norm_gain: None,
                    cur_gain: None,
                })
            }
            Err(e) => {
                let _ = self.ctx.events.send((
                    self.epoch,
                    Event::Error {
                        id: req.id,
                        message: e.to_string(),
                    },
                ));
                None
            }
        }
    }

    /// Si `a` está a punto de acabar y hay una pista siguiente, abre esa pista y
    /// devuelve la mezcla. El tiempo se lee de los ajustes en cada llamada.
    fn try_start_fade(&mut self, a: &Active) -> Option<Fade> {
        let rate = self.ctx.out_rate as u64;
        let cf_frames = self.ctx.shared.crossfade_ms.load(Ordering::Relaxed) as u64 * rate / 1000;
        if cf_frames == 0 || a.eof {
            return None;
        }
        let total = a.total_frames(self.ctx.out_rate)?;
        let remaining = total.saturating_sub(a.start_frames + a.produced);
        let min_frames = MIN_FADE_MS * rate / 1000;
        if remaining > cf_frames || remaining < min_frames {
            // Aún falta (o ya es tarde para mezclar: se encadena sin mezclar).
            return None;
        }
        let req = self
            .ctx
            .shared
            .next_slot
            .lock()
            .ok()
            .and_then(|s| s.clone())?;
        // `open` anota el inicio de la pista nueva en el fotograma de salida actual;
        // `pending` está vacío, así que es justo donde empezará la mezcla.
        let b = self.open(req, 0)?;
        if let Ok(mut slot) = self.ctx.shared.next_slot.lock() {
            slot.take();
        }
        let mut len = remaining.min(cf_frames);
        if let Some(b_total) = b.total_frames(self.ctx.out_rate) {
            len = len.min(b_total / 2); // la pista que entra no puede ser más corta que la mezcla
        }
        if len < min_frames {
            len = min_frames;
        }
        Some(Fade {
            b,
            a_q: Vec::new(),
            b_q: Vec::new(),
            a_eof: false,
            b_eof: false,
            total: len,
            done: 0,
        })
    }

    /// Mezcla un bloque de las dos pistas (potencia constante) y lo deja en `pending`.
    fn fade_step(
        &mut self,
        a: &mut Active,
        f: &mut Fade,
        raw: &mut Vec<f32>,
        tmp: &mut Vec<f32>,
    ) -> FadeStep {
        let ch = self.ctx.out_channels;
        // Completar las dos colas. Un fallo de lectura se trata como fin de esa pista.
        while !f.a_eof && f.a_q.len() / ch < FADE_CHUNK {
            match pull(&self.ctx, a, raw, tmp, &mut f.a_q) {
                Pull::Data => {}
                Pull::Eof | Pull::Failed(_) => f.a_eof = true,
            }
        }
        while !f.b_eof && f.b_q.len() / ch < FADE_CHUNK {
            match pull(&self.ctx, &mut f.b, raw, tmp, &mut f.b_q) {
                Pull::Data => {}
                Pull::Eof => f.b_eof = true,
                Pull::Failed(message) => {
                    let id = f.b.req.id;
                    let _ = self
                        .ctx
                        .events
                        .send((self.epoch, Event::Error { id, message }));
                    f.b_eof = true;
                    f.b.eof = true;
                }
            }
        }
        let (na, nb) = (f.a_q.len() / ch, f.b_q.len() / ch);
        // Si una pista se queda sin audio, la otra sigue con la otra como silencio
        // para que la curva de la mezcla no tenga saltos.
        let mut n = if f.a_eof {
            nb
        } else if f.b_eof {
            na
        } else {
            na.min(nb)
        };
        n = n.min((f.total - f.done) as usize);
        self.pending.clear();
        self.pending_pos = 0;
        for i in 0..n {
            let t = (f.done + i as u64) as f64 / f.total as f64;
            let (ga, gb) = {
                let x = t * std::f64::consts::FRAC_PI_2;
                (x.cos() as f32, x.sin() as f32)
            };
            for c in 0..ch {
                let sa = if i < na { f.a_q[i * ch + c] } else { 0.0 };
                let sb = if i < nb { f.b_q[i * ch + c] } else { 0.0 };
                self.pending.push(soft_limit(sa * ga + sb * gb));
            }
        }
        f.a_q.drain(..(n.min(na) * ch));
        f.b_q.drain(..(n.min(nb) * ch));
        f.done += n as u64;

        let finished =
            f.done >= f.total || (f.a_eof && f.b_eof && f.a_q.is_empty() && f.b_q.is_empty());
        if !finished {
            return FadeStep::Continue;
        }
        // Fin de la mezcla: lo que quede de la pista que entra sigue a ganancia 1.
        // De la que sale se descarta el resto (su ganancia ya es 0).
        self.pending.extend_from_slice(&f.b_q);
        f.b_q.clear();
        FadeStep::Done
    }

    fn push_end(&mut self) {
        let boundary = Boundary {
            epoch: self.epoch,
            start_frame: self.pushed_frames,
            kind: BoundaryKind::End,
        };
        if let Ok(mut b) = self.ctx.shared.boundaries.lock() {
            b.push_back(boundary);
        }
        // Ya no se producirá más audio: el silencio posterior no es un corte.
        self.ctx.shared.streaming.store(false, Ordering::Relaxed);
    }

    /// Empuja lo que quepa (solo fotogramas completos). `false` si no hay sitio.
    fn push_pending(&mut self) -> bool {
        let ch = self.ctx.out_channels;
        let vacant = self.ctx.prod.vacant_len();
        let vacant = vacant - vacant % ch;
        if vacant == 0 {
            return false;
        }
        let remaining = &self.pending[self.pending_pos..];
        let n = remaining.len().min(vacant);
        let pushed = self.ctx.prod.push_slice(&remaining[..n]);
        self.pending_pos += pushed;
        self.pushed_frames += (pushed / ch) as u64;
        self.ctx
            .shared
            .pushed_total
            .fetch_add((pushed / ch) as u64, Ordering::Relaxed);
        true
    }
}
