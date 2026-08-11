use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::channel::mpsc::UnboundedSender;
use tokio::sync::mpsc::UnboundedSender as TokioSender;

use crate::ws::WsCmd;
use crate::UiEvent;

pub enum Cmd {
    Start,
    /// Stop capturing, flush the tail chunk, then send `finalize` (order preserved).
    StopFinalize,
    StopDiscard,
}

const CHUNK_BYTES: usize = 3_200; // 100 ms of PCM16 @ 16 kHz

pub fn spawn(rx: Receiver<Cmd>, ws: TokioSender<WsCmd>, ui: UnboundedSender<UiEvent>) {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || run(rx, ws, ui))
        .expect("spawn audio thread");
}

fn run(rx: Receiver<Cmd>, ws: TokioSender<WsCmd>, ui: UnboundedSender<UiEvent>) {
    let mut _stream: Option<cpal::Stream> = None;
    let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start => {
                _stream = None;
                pending.lock().unwrap().clear();
                match build_stream(pending.clone(), ws.clone(), ui.clone()) {
                    Ok(new_stream) => {
                        if let Err(error) = new_stream.play() {
                            let _ = ui.unbounded_send(UiEvent::AudioError(error.to_string()));
                        } else {
                            _stream = Some(new_stream);
                        }
                    }
                    Err(error) => {
                        let _ = ui.unbounded_send(UiEvent::AudioError(error));
                    }
                }
            }
            Cmd::StopFinalize => {
                _stream = None; // drop stops the CoreAudio callbacks
                let tail = std::mem::take(&mut *pending.lock().unwrap());
                if !tail.is_empty() {
                    let _ = ws.send(WsCmd::Audio(tail));
                }
                let _ = ws.send(WsCmd::Finalize);
            }
            Cmd::StopDiscard => {
                _stream = None;
                pending.lock().unwrap().clear();
            }
        }
    }
}

struct Resampler {
    src_rate: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn new(src_rate: f64) -> Self {
        Resampler {
            src_rate,
            pos: 0.0,
            prev: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<i16>) {
        if input.is_empty() {
            return;
        }
        let step = self.src_rate / 16_000.0;
        let mut t = self.pos;
        loop {
            let idx = t.floor();
            let i = idx as isize;
            let next = i + 1;
            if next >= input.len() as isize {
                break;
            }
            let frac = t - idx;
            let a = if i < 0 { self.prev } else { input[i as usize] };
            let b = if next < 0 {
                self.prev
            } else {
                input[next as usize]
            };
            let value = a as f64 + (b as f64 - a as f64) * frac;
            out.push((value.clamp(-1.0, 1.0) * 32_767.0) as i16);
            t += step;
        }
        self.pos = t - input.len() as f64;
        self.prev = *input.last().unwrap();
    }
}

fn build_stream(
    pending: Arc<Mutex<Vec<u8>>>,
    ws: TokioSender<WsCmd>,
    ui: UnboundedSender<UiEvent>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "No microphone input is available.".to_string())?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("Microphone is unavailable: {e}"))?;

    let sample_rate = supported.sample_rate().0 as f64;
    let channels = supported.channels() as usize;
    eprintln!(
        "fntype: microphone={} format={:?} rate={} channels={channels}",
        device.name().unwrap_or_else(|_| "unknown".into()),
        supported.sample_format(),
        supported.sample_rate().0,
    );
    let mut resampler = Resampler::new(sample_rate);
    let mut mono = Vec::<f32>::with_capacity(4_096);
    let mut resampled = Vec::<i16>::with_capacity(4_096);
    let mut last_level = Instant::now();
    let mut sent_first_chunk = false;

    let mut process = move |samples: &[f32]| {
        mono.clear();
        if channels <= 1 {
            mono.extend_from_slice(samples);
        } else {
            for frame in samples.chunks_exact(channels) {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }
        if mono.is_empty() {
            return;
        }

        if last_level.elapsed() >= Duration::from_millis(50) {
            last_level = Instant::now();
            let rms = (mono.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>()
                / mono.len() as f64)
                .sqrt();
            const SILENCE_RMS: f64 = 0.008;
            let level = if rms <= SILENCE_RMS {
                0.0
            } else {
                ((rms - SILENCE_RMS) * 12.0).clamp(0.0, 1.0) as f32
            };
            let _ = ui.unbounded_send(UiEvent::Level(level));
        }

        resampled.clear();
        resampler.process(&mono, &mut resampled);
        if resampled.is_empty() {
            return;
        }

        let mut buffer = pending.lock().unwrap();
        for sample in &resampled {
            buffer.extend_from_slice(&sample.to_le_bytes());
        }
        while buffer.len() >= CHUNK_BYTES {
            let chunk: Vec<u8> = buffer.drain(..CHUNK_BYTES).collect();
            if !sent_first_chunk {
                sent_first_chunk = true;
                eprintln!("fntype: streaming 16 kHz PCM audio to xAI");
            }
            let _ = ws.send(WsCmd::Audio(chunk));
        }
    };

    let error_handler = move |error: cpal::StreamError| {
        eprintln!("fntype audio stream error: {error}");
    };

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &supported.config(),
            move |data: &[f32], _| process(data),
            error_handler,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &supported.config(),
            move |data: &[i16], _| {
                let floats: Vec<f32> = data.iter().map(|s| *s as f32 / 32_768.0).collect();
                process(&floats);
            },
            error_handler,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &supported.config(),
            move |data: &[u16], _| {
                let floats: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 - 32_768.0) / 32_768.0)
                    .collect();
                process(&floats);
            },
            error_handler,
            None,
        ),
        other => return Err(format!("Unsupported microphone sample format: {other:?}")),
    };

    stream.map_err(|e| format!("Could not open the microphone: {e}"))
}
