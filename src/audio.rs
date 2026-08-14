use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
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
/// Local lookback so FN-down → arm latency does not clip the first phonemes.
const PREROLL_BYTES: usize = 8_000; // 250 ms of PCM16 @ 16 kHz
const HEALTH_INTERVAL: Duration = Duration::from_millis(400);
const REOPEN_BACKOFF: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
enum GateOut {
    BeginUtterance,
    Audio(Vec<u8>),
    Finalize,
}

/// Arms/disarms capture. Idle samples stay on-device in a short preroll.
struct CaptureGate {
    armed: bool,
    preroll: Vec<u8>,
    pending: Vec<u8>,
}

impl CaptureGate {
    fn new() -> Self {
        CaptureGate {
            armed: false,
            preroll: Vec::with_capacity(PREROLL_BYTES),
            pending: Vec::with_capacity(CHUNK_BYTES),
        }
    }

    fn ingest(&mut self, pcm: &[u8]) -> Vec<GateOut> {
        if pcm.is_empty() {
            return Vec::new();
        }
        if !self.armed {
            self.preroll.extend_from_slice(pcm);
            if self.preroll.len() > PREROLL_BYTES {
                let excess = self.preroll.len() - PREROLL_BYTES;
                self.preroll.drain(..excess);
            }
            return Vec::new();
        }
        self.pending.extend_from_slice(pcm);
        self.take_full_chunks()
    }

    fn start(&mut self) -> Vec<GateOut> {
        if self.armed {
            return Vec::new();
        }
        self.armed = true;
        let mut out = vec![GateOut::BeginUtterance];
        if !self.preroll.is_empty() {
            let mut preroll = std::mem::take(&mut self.preroll);
            preroll.append(&mut self.pending);
            self.pending = preroll;
        }
        out.extend(self.take_full_chunks());
        out
    }

    fn stop_finalize(&mut self) -> Vec<GateOut> {
        self.armed = false;
        self.preroll.clear();
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            out.push(GateOut::Audio(std::mem::take(&mut self.pending)));
        }
        out.push(GateOut::Finalize);
        out
    }

    fn stop_discard(&mut self) -> Vec<GateOut> {
        self.armed = false;
        self.pending.clear();
        self.preroll.clear();
        Vec::new()
    }

    fn take_full_chunks(&mut self) -> Vec<GateOut> {
        let mut out = Vec::new();
        while self.pending.len() >= CHUNK_BYTES {
            let chunk: Vec<u8> = self.pending.drain(..CHUNK_BYTES).collect();
            out.push(GateOut::Audio(chunk));
        }
        out
    }
}

pub fn spawn(rx: Receiver<Cmd>, ws: TokioSender<WsCmd>, ui: UnboundedSender<UiEvent>) {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || run(rx, ws, ui))
        .expect("spawn audio thread");
}

fn run(rx: Receiver<Cmd>, ws: TokioSender<WsCmd>, ui: UnboundedSender<UiEvent>) {
    let gate = Arc::new(Mutex::new(CaptureGate::new()));
    let failed = Arc::new(AtomicBool::new(false));
    let mut stream: Option<cpal::Stream> = None;
    let mut next_health = Instant::now() + REOPEN_BACKOFF;

    loop {
        match rx.recv_timeout(HEALTH_INTERVAL) {
            Ok(Cmd::Start) => {
                {
                    let mut gate = gate.lock().unwrap();
                    send_outs(&ws, gate.start());
                }
                if stream.is_none() {
                    match open_stream(gate.clone(), failed.clone(), ws.clone(), ui.clone()) {
                        Ok(new_stream) => stream = Some(new_stream),
                        Err(error) => {
                            let _ = ui.unbounded_send(UiEvent::AudioError(error));
                        }
                    }
                }
            }
            Ok(Cmd::StopFinalize) => {
                {
                    let mut gate = gate.lock().unwrap();
                    send_outs(&ws, gate.stop_finalize());
                }
                stream = None;
            }
            Ok(Cmd::StopDiscard) => {
                {
                    let mut gate = gate.lock().unwrap();
                    send_outs(&ws, gate.stop_discard());
                }
                stream = None;
            }
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= next_health {
                    next_health = Instant::now() + REOPEN_BACKOFF;
                    recover_or_release(&mut stream, &gate, &failed, &ws, &ui);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn recover_or_release(
    stream: &mut Option<cpal::Stream>,
    gate: &Arc<Mutex<CaptureGate>>,
    failed: &Arc<AtomicBool>,
    ws: &TokioSender<WsCmd>,
    ui: &UnboundedSender<UiEvent>,
) {
    let armed = gate.lock().unwrap().armed;
    if !armed {
        if stream.is_some() {
            *stream = None;
        }
        failed.store(false, Ordering::Relaxed);
        return;
    }
    if !failed.swap(false, Ordering::Relaxed) && stream.is_some() {
        return;
    }
    match open_stream(gate.clone(), failed.clone(), ws.clone(), ui.clone()) {
        Ok(new_stream) => *stream = Some(new_stream),
        Err(error) => {
            eprintln!("fntype: microphone reopen failed: {error}");
            let _ = ui.unbounded_send(UiEvent::AudioError(error));
        }
    }
}

fn send_outs(ws: &TokioSender<WsCmd>, outs: Vec<GateOut>) {
    for out in outs {
        match out {
            GateOut::BeginUtterance => {
                let _ = ws.send(WsCmd::BeginUtterance);
            }
            GateOut::Audio(chunk) => {
                let _ = ws.send(WsCmd::Audio(chunk));
            }
            GateOut::Finalize => {
                let _ = ws.send(WsCmd::Finalize);
            }
        }
    }
}

fn open_stream(
    gate: Arc<Mutex<CaptureGate>>,
    failed: Arc<AtomicBool>,
    ws: TokioSender<WsCmd>,
    ui: UnboundedSender<UiEvent>,
) -> Result<cpal::Stream, String> {
    failed.store(false, Ordering::Relaxed);
    let stream = build_stream(gate, failed, ws, ui)?;
    stream
        .play()
        .map_err(|error| format!("Could not start the microphone: {error}"))?;
    Ok(stream)
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
    gate: Arc<Mutex<CaptureGate>>,
    failed: Arc<AtomicBool>,
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
    let label = device.name().unwrap_or_else(|_| "unknown".into());
    eprintln!(
        "fntype: microphone opened device={label} format={:?} rate={} channels={channels}",
        supported.sample_format(),
        supported.sample_rate().0,
    );
    let mut resampler = Resampler::new(sample_rate);
    let mut mono = Vec::<f32>::with_capacity(4_096);
    let mut resampled = Vec::<i16>::with_capacity(4_096);
    let mut pcm_bytes = Vec::<u8>::with_capacity(8_192);
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

        resampled.clear();
        resampler.process(&mono, &mut resampled);
        if resampled.is_empty() {
            return;
        }

        pcm_bytes.clear();
        for sample in &resampled {
            pcm_bytes.extend_from_slice(&sample.to_le_bytes());
        }

        let mut gate = gate.lock().unwrap();
        if gate.armed && last_level.elapsed() >= Duration::from_millis(50) {
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
        for out in gate.ingest(&pcm_bytes) {
            if let GateOut::Audio(chunk) = out {
                if !sent_first_chunk {
                    sent_first_chunk = true;
                    eprintln!("fntype: streaming 16 kHz PCM audio to xAI");
                }
                let _ = ws.send(WsCmd::Audio(chunk));
            }
        }
    };

    let error_handler = move |error: cpal::StreamError| {
        eprintln!("fntype audio stream error: {error}");
        failed.store(true, Ordering::Relaxed);
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

    let stream = stream.map_err(|e| format!("Could not open the microphone: {e}"))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(tag: u8, n: usize) -> Vec<u8> {
        vec![tag; n]
    }

    fn audio_bytes(out: &[GateOut]) -> Vec<u8> {
        out.iter()
            .filter_map(|event| match event {
                GateOut::Audio(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn idle_samples_stay_local() {
        let mut gate = CaptureGate::new();
        assert!(gate.ingest(&pcm(1, CHUNK_BYTES * 2)).is_empty());
        assert!(gate.ingest(&pcm(1, PREROLL_BYTES)).is_empty());
        assert!(!gate.armed);
        assert!(gate.pending.is_empty());
    }

    #[test]
    fn preroll_drops_oldest_bytes_past_cap() {
        let mut gate = CaptureGate::new();
        assert!(gate.ingest(&pcm(1, PREROLL_BYTES + 64)).is_empty());
        assert!(gate.ingest(&pcm(2, PREROLL_BYTES)).is_empty());
        let out = gate.start();
        assert_eq!(out.first(), Some(&GateOut::BeginUtterance));
        let bytes = audio_bytes(&out);
        assert!(bytes.iter().all(|b| *b == 2));
        assert!(bytes.len() <= PREROLL_BYTES);
        assert_eq!(bytes.len() + gate.pending.len(), PREROLL_BYTES);
    }

    #[test]
    fn start_flushes_preroll_before_live_samples() {
        let mut gate = CaptureGate::new();
        assert!(gate.ingest(&pcm(1, CHUNK_BYTES)).is_empty());
        let started = gate.start();
        assert_eq!(started[0], GateOut::BeginUtterance);
        assert_eq!(started[1], GateOut::Audio(pcm(1, CHUNK_BYTES)));
        let live = gate.ingest(&pcm(2, CHUNK_BYTES));
        assert_eq!(live, vec![GateOut::Audio(pcm(2, CHUNK_BYTES))]);
    }

    #[test]
    fn stop_finalize_emits_tail_then_finalize() {
        let mut gate = CaptureGate::new();
        assert_eq!(gate.start(), vec![GateOut::BeginUtterance]);
        assert!(gate.ingest(&pcm(3, 100)).is_empty());
        assert_eq!(
            gate.stop_finalize(),
            vec![GateOut::Audio(pcm(3, 100)), GateOut::Finalize]
        );
        assert!(!gate.armed);
        assert!(gate.ingest(&pcm(4, CHUNK_BYTES)).is_empty());
    }

    #[test]
    fn stop_discard_drops_everything() {
        let mut gate = CaptureGate::new();
        assert!(gate.ingest(&pcm(1, CHUNK_BYTES)).is_empty());
        let _ = gate.start();
        let _ = gate.ingest(&pcm(2, 50));
        assert!(gate.stop_discard().is_empty());
        assert_eq!(gate.start(), vec![GateOut::BeginUtterance]);
        assert_eq!(gate.stop_finalize(), vec![GateOut::Finalize]);
    }

    #[test]
    fn start_sends_begin_utterance_before_audio() {
        let mut gate = CaptureGate::new();
        assert_eq!(gate.start(), vec![GateOut::BeginUtterance]);
        assert!(gate.ingest(&pcm(9, CHUNK_BYTES - 1)).is_empty());
        assert_eq!(
            gate.ingest(&pcm(9, 1)),
            vec![GateOut::Audio(pcm(9, CHUNK_BYTES))]
        );
    }

    #[test]
    fn armed_samples_emit_full_chunks_only() {
        let mut gate = CaptureGate::new();
        let _ = gate.start();
        assert!(gate.ingest(&pcm(7, CHUNK_BYTES - 1)).is_empty());
        assert_eq!(
            gate.ingest(&pcm(7, 1)),
            vec![GateOut::Audio(pcm(7, CHUNK_BYTES))]
        );
        assert!(gate.pending.is_empty());
    }
}
