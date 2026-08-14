use std::collections::VecDeque;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use futures::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::transcript::SttEvent;
use crate::UiEvent;

#[derive(Clone)]
pub struct WsConfig {
    pub api_key: String,
    pub language: String,
    pub bias_terms: Vec<String>,
}

pub enum WsCmd {
    Configure(WsConfig),
    BeginUtterance,
    Audio(Vec<u8>),
    Finalize,
    Reconnect,
}

const MAX_PENDING_BYTES: usize = 320_000; // ~10 s of audio buffered while (re)connecting
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);
const IDLE_REFRESH_AFTER: Duration = Duration::from_secs(120);
const MAX_CONNECTION_AGE: Duration = Duration::from_secs(8 * 60);

pub fn spawn(rx: UnboundedReceiver<WsCmd>, ui: UnboundedSender<UiEvent>) {
    std::thread::Builder::new()
        .name("xai-ws".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            runtime.block_on(run(rx, ui));
        })
        .expect("spawn websocket thread");
}

struct Session {
    config: Option<WsConfig>,
    pending: VecDeque<Vec<u8>>,
    pending_bytes: usize,
    finalize_pending: bool,
}

impl Session {
    fn buffer_audio(&mut self, chunk: Vec<u8>) {
        self.pending_bytes += chunk.len();
        self.pending.push_back(chunk);
        while self.pending_bytes > MAX_PENDING_BYTES {
            if let Some(dropped) = self.pending.pop_front() {
                self.pending_bytes -= dropped.len();
            } else {
                break;
            }
        }
    }
}

async fn run(mut rx: UnboundedReceiver<WsCmd>, ui: UnboundedSender<UiEvent>) {
    let mut session = Session {
        config: None,
        pending: VecDeque::new(),
        pending_bytes: 0,
        finalize_pending: false,
    };
    let mut backoff = Duration::from_millis(1_200);

    loop {
        // Wait until configured.
        while session.config.is_none() {
            match rx.recv().await {
                Some(WsCmd::Configure(config)) => session.config = Some(config),
                Some(_) => {} // audio/finalize without a key: drop
                None => return,
            }
        }

        let config = session.config.clone().unwrap();
        match connect(&config).await {
            Err(error) => {
                let _ = ui.unbounded_send(UiEvent::ConnChanged(false, friendly(&error)));
                // Drain commands while backing off so Configure is not missed.
                let sleep = tokio::time::sleep(backoff);
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        _ = &mut sleep => break,
                        cmd = rx.recv() => match cmd {
                            Some(WsCmd::Configure(new_config)) => { session.config = Some(new_config); break; }
                            Some(WsCmd::Audio(chunk)) => session.buffer_audio(chunk),
                            Some(WsCmd::Finalize) => session.finalize_pending = true,
                            Some(WsCmd::BeginUtterance | WsCmd::Reconnect) => {}
                            None => return,
                        }
                    }
                }
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
            Ok(stream) => {
                backoff = Duration::from_millis(1_200);
                let (mut write, mut read) = stream.split();
                let connected_at = Instant::now();
                let mut last_audio_at = connected_at;
                let mut last_server_at = connected_at;
                let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                heartbeat.tick().await;
                let mut ready = false;
                let mut refreshing = false;
                let mut reconfigure: Option<WsConfig> = None;

                'connection: loop {
                    tokio::select! {
                        incoming = read.next() => {
                            let message = match incoming {
                                Some(Ok(message)) => {
                                    last_server_at = Instant::now();
                                    message
                                }
                                Some(Err(error)) => {
                                    let _ = ui.unbounded_send(UiEvent::ConnChanged(false, friendly(&error.to_string())));
                                    break 'connection;
                                }
                                None => {
                                    let _ = ui.unbounded_send(UiEvent::ConnChanged(false, "xAI closed the connection.".into()));
                                    break 'connection;
                                }
                            };
                            let text = match &message {
                                Message::Text(text) => text.to_string(),
                                Message::Close(_) => {
                                    let _ = ui.unbounded_send(UiEvent::ConnChanged(false, "xAI closed the connection.".into()));
                                    break 'connection;
                                }
                                _ => continue,
                            };
                            let Ok(event) = serde_json::from_str::<SttEvent>(&text) else { continue };
                            match event.typ.as_str() {
                                "transcript.created" => {
                                    ready = true;
                                    eprintln!("fntype: xAI STT ready");
                                    let _ = ui.unbounded_send(UiEvent::ConnChanged(true, "xAI realtime ready".into()));
                                    while let Some(chunk) = session.pending.pop_front() {
                                        session.pending_bytes -= chunk.len();
                                        if write.send(Message::binary(chunk)).await.is_err() { break 'connection; }
                                    }
                                    if session.finalize_pending {
                                        session.finalize_pending = false;
                                        if write.send(Message::text(r#"{"type":"Finalize"}"#)).await.is_err() { break 'connection; }
                                    }
                                }
                                "transcript.partial" | "transcript.done" => {
                                    eprintln!(
                                        "fntype: xAI event={} chars={} is_final={:?} speech_final={:?}",
                                        event.typ,
                                        event.text.as_deref().map_or(0, str::len),
                                        event.is_final,
                                        event.speech_final,
                                    );
                                    let _ = ui.unbounded_send(UiEvent::Stt(event));
                                }
                                "error" => {
                                    let message = event.message.unwrap_or_else(|| "xAI returned an unknown transcription error.".into());
                                    let _ = ui.unbounded_send(UiEvent::WsError(message));
                                }
                                _ => {}
                            }
                        }
                        _ = heartbeat.tick() => {
                            if last_server_at.elapsed() >= HEARTBEAT_TIMEOUT {
                                eprintln!("fntype: xAI heartbeat timed out");
                                let _ = ui.unbounded_send(UiEvent::ConnChanged(
                                    false,
                                    "xAI stopped responding. Reconnecting…".into(),
                                ));
                                break 'connection;
                            }
                            // Only refresh when the socket has been idle. Max-age
                            // reconnects wait for the next BeginUtterance so a long
                            // hold is not torn down mid-speech.
                            if last_audio_at.elapsed() >= IDLE_REFRESH_AFTER {
                                eprintln!("fntype: refreshing idle xAI connection");
                                refreshing = true;
                                break 'connection;
                            }
                            if write.send(Message::Ping(Vec::new().into())).await.is_err() {
                                break 'connection;
                            }
                        }
                        cmd = rx.recv() => match cmd {
                            Some(WsCmd::BeginUtterance) => {
                                if connection_needs_refresh(connected_at, last_audio_at, Instant::now()) {
                                    eprintln!("fntype: refreshing xAI connection before dictation");
                                    refreshing = true;
                                    let _ = ui.unbounded_send(UiEvent::ConnRefreshing(
                                        "Refreshing xAI connection…".into(),
                                    ));
                                    break 'connection;
                                }
                            }
                            Some(WsCmd::Audio(chunk)) => {
                                last_audio_at = Instant::now();
                                if ready {
                                    if write.send(Message::binary(chunk)).await.is_err() { break 'connection; }
                                } else {
                                    session.buffer_audio(chunk);
                                }
                            }
                            Some(WsCmd::Finalize) => {
                                last_audio_at = Instant::now();
                                eprintln!("fntype: finalizing current utterance");
                                if ready {
                                    if write.send(Message::text(r#"{"type":"Finalize"}"#)).await.is_err() { break 'connection; }
                                } else {
                                    session.finalize_pending = true;
                                }
                            }
                            Some(WsCmd::Reconnect) => {
                                eprintln!("fntype: reconnecting after missing transcript");
                                refreshing = true;
                                let _ = ui.unbounded_send(UiEvent::ConnRefreshing(
                                    "Reconnecting to xAI…".into(),
                                ));
                                break 'connection;
                            }
                            Some(WsCmd::Configure(new_config)) => {
                                reconfigure = Some(new_config);
                                let _ = write.send(Message::text(r#"{"type":"audio.done"}"#)).await;
                                break 'connection;
                            }
                            None => {
                                let _ = write.send(Message::text(r#"{"type":"audio.done"}"#)).await;
                                return;
                            }
                        }
                    }
                }

                if let Some(new_config) = reconfigure {
                    session.config = Some(new_config);
                } else if !refreshing {
                    let _ = ui
                        .unbounded_send(UiEvent::ConnChanged(false, "Reconnecting to xAI…".into()));
                    tokio::time::sleep(Duration::from_millis(900)).await;
                }
            }
        }
    }
}

fn connection_needs_refresh(connected_at: Instant, last_audio_at: Instant, now: Instant) -> bool {
    now.duration_since(connected_at) >= MAX_CONNECTION_AGE
        || now.duration_since(last_audio_at) >= IDLE_REFRESH_AFTER
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(config: &WsConfig) -> Result<WsStream, String> {
    let mut url = String::from(
        "wss://api.x.ai/v1/stt?sample_rate=16000&encoding=pcm&interim_results=true&filler_words=false&endpointing=5000&vad_threshold=0.03",
    );
    let language = config.language.trim();
    if !language.is_empty() {
        url.push_str(&format!(
            "&language={}",
            utf8_percent_encode(language, NON_ALPHANUMERIC)
        ));
    }
    for term in config.bias_terms.iter().take(100) {
        url.push_str(&format!(
            "&keyterm={}",
            utf8_percent_encode(term, NON_ALPHANUMERIC)
        ));
    }

    let mut request = url
        .into_client_request()
        .map_err(|e| format!("bad request: {e}"))?;
    let auth = format!("Bearer {}", config.api_key)
        .parse()
        .map_err(|_| "bad API key format".to_string())?;
    request.headers_mut().insert("Authorization", auth);

    let connect = tokio_tungstenite::connect_async(request);
    match tokio::time::timeout(Duration::from_secs(10), connect).await {
        Err(_) => Err("timed out".into()),
        Ok(Err(error)) => Err(error.to_string()),
        Ok(Ok((stream, _response))) => Ok(stream),
    }
}

fn friendly(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") {
        "The xAI API key was rejected. Re-import it from the menu bar.".into()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "xAI took too long to respond.".into()
    } else if lower.contains("dns")
        || lower.contains("lookup")
        || lower.contains("connection refused")
        || lower.contains("network")
    {
        "No connection to xAI. Check your internet.".into()
    } else {
        format!("xAI connection issue: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_old_or_idle_connections() {
        let now = Instant::now();
        assert!(!connection_needs_refresh(
            now - Duration::from_secs(30),
            now - Duration::from_secs(10),
            now,
        ));
        assert!(connection_needs_refresh(
            now - MAX_CONNECTION_AGE,
            now - Duration::from_secs(10),
            now,
        ));
        assert!(connection_needs_refresh(
            now - Duration::from_secs(30),
            now - IDLE_REFRESH_AFTER,
            now,
        ));
    }
}
