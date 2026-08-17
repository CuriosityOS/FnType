mod audio;
mod config;
mod dictionary;
mod fn_monitor;
mod inject;
mod keychain;
mod overlay;
mod sys_audio;
mod transcript;
mod ws;

use std::time::Duration;

use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{
    point, px, size, App, AppContext, Application, AsyncApp, BackgroundExecutor, Bounds, Entity,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

use crate::config::Settings;
use crate::dictionary::Dictionary;
use crate::overlay::{Mode, OverlayState, OverlayView, OVERLAY_HEIGHT, OVERLAY_WIDTH};
use crate::transcript::{clean, should_press_return, Accumulator, SttEvent};
use crate::ws::{WsCmd, WsConfig};

pub enum UiEvent {
    FnDown,
    FnUp,
    FnChord,
    MonitorFailed,
    Level(f32),
    ConnChanged(bool, String),
    ConnRefreshing(String),
    Stt(SttEvent),
    WsError(String),
    AudioError(String),
    FinalizeTimeout(u64),
    Hide(u64),
    Tick,
    MenuReconnect,
    MenuImportKey,
    MenuAutoSubmit(bool),
    MenuTerminalSubmit(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Connecting,
    Recording,
    Finalizing,
    Inserting,
    Error,
}

fn hide_from_dock() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            // GPUI forces the regular policy at startup. Accessory preserves the menu-bar
            // item and overlay while removing FnType from the Dock and app switcher.
            let _: bool = msg_send![app, setActivationPolicy: 1_isize];
        }
    }
}

/// Keep only one FnType alive. Reinstall used to leave the previous binary mapped
/// in memory (`open -n`), so the old process kept dictating after a fix shipped.
fn acquire_single_instance() -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;

    let _ = std::fs::create_dir_all(config::config_dir());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(config::lock_path())
        .ok()?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    let _ = std::io::Write::write_all(&mut (&file), format!("{}\n", std::process::id()).as_bytes());
    Some(file)
}

/// `open` attaches stderr to /dev/null; mirror eprintln! into the usual log path.
fn setup_file_logging() {
    use std::os::unix::io::AsRawFd;

    let dir = config::log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(stderr_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("stderr.log"))
    else {
        return;
    };
    let Ok(stdout_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("stdout.log"))
    else {
        return;
    };
    unsafe {
        libc::dup2(stderr_file.as_raw_fd(), 2);
        libc::dup2(stdout_file.as_raw_fd(), 1);
    }
    // `dup2` keeps its own references; the original descriptors can now close.
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--import-key-from-pasteboard") {
        match import_key_from_clipboard() {
            Ok(()) => {
                println!("fntype: API key imported into Keychain and cleared from the clipboard.");
                return;
            }
            Err(error) => {
                eprintln!("fntype: {error}");
                std::process::exit(2);
            }
        }
    }
    if let Some(index) = args.iter().position(|a| a == "--import-key-from-file") {
        let Some(path) = args.get(index + 1) else {
            eprintln!("fntype: --import-key-from-file needs a path");
            std::process::exit(2);
        };
        let result_path = format!("{path}.result");
        let outcome = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("could not read key file: {e}"))
            .and_then(|text| keychain::save(&text));
        let _ = std::fs::remove_file(path);
        match outcome {
            Ok(()) => {
                let _ = std::fs::write(&result_path, "ok");
            }
            Err(error) => {
                let _ = std::fs::write(&result_path, format!("error: {error}"));
                std::process::exit(3);
            }
        }
        return;
    }
    if let Some(index) = args.iter().position(|a| a == "--check-to") {
        let perms = inject::permissions();
        let report = format!(
            "api_key={} input_monitoring={} accessibility={}",
            keychain::load().is_some(),
            perms.input_monitoring,
            perms.accessibility
        );
        match args.get(index + 1) {
            Some(path) => {
                let _ = std::fs::write(path, report);
            }
            None => println!("{report}"),
        }
        return;
    }
    if args.iter().any(|a| a == "--check") {
        let perms = inject::permissions();
        println!("api key in keychain : {}", keychain::load().is_some());
        println!("input monitoring    : {}", perms.input_monitoring);
        println!("accessibility       : {}", perms.accessibility);
        return;
    }

    setup_file_logging();
    let Some(_instance_lock) = acquire_single_instance() else {
        eprintln!("fntype: another instance is already running; exiting");
        return;
    };
    eprintln!(
        "fntype: started pid={} build={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );

    Application::new().run(|cx: &mut App| {
        hide_from_dock();
        cx.activate(false);
        inject::request_permissions();

        let (ui_tx, mut ui_rx) = unbounded::<UiEvent>();
        let (ws_tx, ws_rx) = tokio::sync::mpsc::unbounded_channel::<WsCmd>();
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<audio::Cmd>();

        ws::spawn(ws_rx, ui_tx.clone());
        audio::spawn(audio_rx, ws_tx.clone(), ui_tx.clone());
        fn_monitor::spawn(ui_tx.clone());

        // ---- Tray menu ----
        let status_item = MenuItem::with_id("status", "Starting…", false, None);
        let settings = Settings::load();
        let reconnect_item = MenuItem::with_id("reconnect", "Reconnect to xAI", true, None);
        let import_item = MenuItem::with_id("import", "Import API key from clipboard", true, None);
        let dict_item = MenuItem::with_id("dict", "Edit dictionary…", true, None);
        let auto_submit_item = CheckMenuItem::with_id(
            "auto_submit",
            "Press Return after insert",
            true,
            settings.auto_submit,
            None,
        );
        let terminal_item = CheckMenuItem::with_id(
            "terminal_submit",
            "Allow Return in terminals",
            true,
            settings.allow_terminal_submit,
            None,
        );
        let perms_item = MenuItem::with_id("perms", "Grant macOS permissions", true, None);
        let quit_item = MenuItem::with_id("quit", "Quit FnType", true, None);

        let menu = Menu::new();
        let _ = menu.append(&status_item);
        let _ = menu.append(&MenuItem::with_id(
            "hint",
            "Hold Globe/Fn to dictate",
            false,
            None,
        ));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&reconnect_item);
        let _ = menu.append(&import_item);
        let _ = menu.append(&dict_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&auto_submit_item);
        let _ = menu.append(&terminal_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&perms_item);
        let _ = menu.append(&quit_item);

        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_title("fn")
            .with_tooltip("FnType - hold Fn to dictate")
            .build()
        {
            Ok(tray) => std::mem::forget(tray),
            Err(error) => eprintln!("fntype: tray icon failed: {error}"),
        }

        // Menu event pump.
        {
            let ui_tx = ui_tx.clone();
            let auto_submit_item = auto_submit_item.clone();
            let terminal_item = terminal_item.clone();
            let bg = cx.background_executor().clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                let receiver = MenuEvent::receiver();
                loop {
                    while let Ok(event) = receiver.try_recv() {
                        match event.id.as_ref() {
                            "reconnect" => {
                                let _ = ui_tx.unbounded_send(UiEvent::MenuReconnect);
                            }
                            "import" => {
                                let _ = ui_tx.unbounded_send(UiEvent::MenuImportKey);
                            }
                            "dict" => {
                                let path = config::dictionary_path();
                                let _ = Dictionary::load(&path);
                                let _ = std::process::Command::new("open")
                                    .arg("-t")
                                    .arg(&path)
                                    .spawn();
                            }
                            "auto_submit" => {
                                let _ = ui_tx.unbounded_send(UiEvent::MenuAutoSubmit(
                                    auto_submit_item.is_checked(),
                                ));
                            }
                            "terminal_submit" => {
                                let _ = ui_tx.unbounded_send(UiEvent::MenuTerminalSubmit(
                                    terminal_item.is_checked(),
                                ));
                            }
                            "perms" => inject::request_permissions(),
                            "quit" => {
                                let _ = cx.update(|cx| cx.quit());
                            }
                            _ => {}
                        }
                    }
                    bg.timer(Duration::from_millis(150)).await;
                }
            })
            .detach();
        }

        // Animation tick.
        {
            let ui_tx = ui_tx.clone();
            let bg = cx.background_executor().clone();
            bg.clone()
                .spawn(async move {
                    loop {
                        bg.timer(Duration::from_millis(50)).await;
                        if ui_tx.unbounded_send(UiEvent::Tick).is_err() {
                            break;
                        }
                    }
                })
                .detach();
        }

        let overlay_state = cx.new(|_| OverlayState::new());
        let mut coordinator = Coordinator::new(
            overlay_state,
            settings,
            ws_tx,
            audio_tx,
            ui_tx,
            status_item,
            cx.background_executor().clone(),
        );
        coordinator.startup_connect();

        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some(event) = ui_rx.next().await {
                coordinator.handle(event, cx).await;
            }
        })
        .detach();
    });
}

fn import_key_from_clipboard() -> anyhow::Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    let text = clipboard
        .get_text()
        .map_err(|_| anyhow::anyhow!("the clipboard does not contain text"))?;
    keychain::save(&text)?;
    let _ = clipboard.clear();
    Ok(())
}

struct Coordinator {
    state: Entity<OverlayState>,
    settings: Settings,
    dict: Dictionary,
    acc: Accumulator,
    target: Option<inject::Target>,
    phase: Phase,
    session: u64,
    hide_gen: u64,
    level: f32,
    connected: bool,
    api_key: Option<String>,
    bias_at_connect: Vec<String>,
    overlay: Option<WindowHandle<OverlayView>>,
    ws_tx: tokio::sync::mpsc::UnboundedSender<WsCmd>,
    audio_tx: std::sync::mpsc::Sender<audio::Cmd>,
    ui_tx: UnboundedSender<UiEvent>,
    status_item: MenuItem,
    bg: BackgroundExecutor,
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: Entity<OverlayState>,
        settings: Settings,
        ws_tx: tokio::sync::mpsc::UnboundedSender<WsCmd>,
        audio_tx: std::sync::mpsc::Sender<audio::Cmd>,
        ui_tx: UnboundedSender<UiEvent>,
        status_item: MenuItem,
        bg: BackgroundExecutor,
    ) -> Self {
        let dict = Dictionary::load(&config::dictionary_path());
        Coordinator {
            state,
            settings,
            dict,
            acc: Accumulator::default(),
            target: None,
            phase: Phase::Idle,
            session: 0,
            hide_gen: 0,
            level: 0.0,
            connected: false,
            api_key: keychain::load(),
            bias_at_connect: Vec::new(),
            overlay: None,
            ws_tx,
            audio_tx,
            ui_tx,
            status_item,
            bg,
        }
    }

    fn startup_connect(&mut self) {
        if let Some(key) = self.api_key.clone() {
            self.configure_ws(key);
            self.set_status("Connecting to xAI…");
        } else {
            self.set_status("Copy your xAI key, then click Import");
        }
    }

    fn configure_ws(&mut self, api_key: String) {
        let bias = self.dict.bias_terms();
        self.bias_at_connect = bias.clone();
        let _ = self.ws_tx.send(WsCmd::Configure(WsConfig {
            api_key,
            language: self.settings.language.clone(),
            bias_terms: bias,
        }));
    }

    fn set_status(&self, text: &str) {
        self.status_item.set_text(text);
    }

    fn schedule(&self, delay: Duration, event: UiEvent)
    where
        UiEvent: Send,
    {
        let tx = self.ui_tx.clone();
        let bg = self.bg.clone();
        self.bg
            .spawn(async move {
                bg.timer(delay).await;
                let _ = tx.unbounded_send(event);
            })
            .detach();
    }

    fn update_overlay(&self, cx: &mut AsyncApp, f: impl FnOnce(&mut OverlayState)) {
        let _ = self.state.update(cx, |state, cx| {
            f(state);
            cx.notify();
        });
    }

    fn open_overlay(&mut self, cx: &mut AsyncApp) {
        if self.overlay.is_some() {
            return;
        }
        let state = self.state.clone();
        let handle = cx.update(|cx| {
            let display_bounds = cx.primary_display().map(|d| d.bounds()).unwrap_or(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(1440.0), px(900.0)),
            });
            let width = px(OVERLAY_WIDTH);
            let height = px(OVERLAY_HEIGHT);
            let origin = point(
                display_bounds.origin.x + (display_bounds.size.width - width) / 2.0,
                display_bounds.origin.y + display_bounds.size.height - height - px(44.0),
            );
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin,
                        size: size(width, height),
                    })),
                    titlebar: None,
                    focus: false,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| OverlayView::new(state, window, cx)),
            )
        });
        if let Ok(Ok(handle)) = handle {
            self.overlay = Some(handle);
        }
    }

    fn close_overlay(&mut self, cx: &mut AsyncApp) {
        if let Some(handle) = self.overlay.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn schedule_hide(&mut self, delay: Duration) {
        self.hide_gen += 1;
        self.schedule(delay, UiEvent::Hide(self.hide_gen));
    }

    async fn handle(&mut self, event: UiEvent, cx: &mut AsyncApp) {
        match event {
            UiEvent::FnDown => self.begin_recording(cx),
            UiEvent::FnUp => self.end_recording(cx),
            UiEvent::FnChord => self.cancel_for_chord(cx),
            UiEvent::MonitorFailed => {
                self.set_status("Grant Input Monitoring, then relaunch FnType");
            }
            UiEvent::Level(level) => self.level = level,
            UiEvent::Tick => {
                if self.overlay.is_some() {
                    let level = self.level;
                    self.update_overlay(cx, |s| {
                        s.phase += 0.05;
                        s.level = level;
                    });
                }
            }
            UiEvent::ConnChanged(up, label) => {
                self.connected = up;
                self.set_status(&label);
                if !up
                    && matches!(
                        self.phase,
                        Phase::Recording | Phase::Connecting | Phase::Finalizing
                    )
                {
                    self.fail(label, cx);
                }
            }
            UiEvent::ConnRefreshing(label) => {
                self.connected = false;
                self.set_status(&label);
                if self.phase == Phase::Recording {
                    self.phase = Phase::Connecting;
                    self.update_overlay(cx, |s| {
                        s.mode = Mode::Connecting;
                        s.message = "Connecting".into();
                    });
                }
            }
            UiEvent::Stt(event) => self.handle_stt(event, cx).await,
            UiEvent::WsError(message) => {
                if matches!(
                    self.phase,
                    Phase::Recording | Phase::Connecting | Phase::Finalizing
                ) {
                    self.fail(message, cx);
                } else {
                    self.set_status(&message);
                }
            }
            UiEvent::AudioError(message) => {
                if matches!(self.phase, Phase::Recording | Phase::Connecting) {
                    self.fail(message, cx);
                }
            }
            UiEvent::FinalizeTimeout(session) => {
                if session == self.session && self.phase == Phase::Finalizing {
                    let captured = self.acc.best_text();
                    if clean(&captured).is_empty() {
                        let _ = self.ws_tx.send(WsCmd::Reconnect);
                        self.fail(
                            "xAI did not return a transcript. Reconnecting—try again.".into(),
                            cx,
                        );
                    } else {
                        self.complete(captured, cx).await;
                    }
                }
            }
            UiEvent::Hide(generation) => {
                if generation == self.hide_gen {
                    if self.phase == Phase::Error {
                        self.phase = Phase::Idle;
                    }
                    if self.phase == Phase::Idle {
                        self.close_overlay(cx);
                        self.refresh_bias_if_needed();
                    }
                }
            }
            UiEvent::MenuReconnect => {
                self.api_key = keychain::load();
                match self.api_key.clone() {
                    Some(key) => {
                        self.dict = Dictionary::load(&config::dictionary_path());
                        self.configure_ws(key);
                        self.set_status("Connecting to xAI…");
                    }
                    None => self.set_status("No API key saved. Copy it, then click Import"),
                }
            }
            UiEvent::MenuImportKey => match import_key_from_clipboard() {
                Ok(()) => {
                    self.api_key = keychain::load();
                    if let Some(key) = self.api_key.clone() {
                        self.configure_ws(key);
                    }
                    self.set_status("Key imported. Connecting…");
                }
                Err(error) => self.set_status(&format!("Import failed: {error}")),
            },
            UiEvent::MenuAutoSubmit(checked) => {
                self.settings.auto_submit = checked;
                self.settings.save();
            }
            UiEvent::MenuTerminalSubmit(checked) => {
                self.settings.allow_terminal_submit = checked;
                self.settings.save();
            }
        }
    }

    fn begin_recording(&mut self, cx: &mut AsyncApp) {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return;
        }
        if self.api_key.is_none() {
            self.api_key = keychain::load();
        }
        if self.api_key.is_none() {
            self.show_setup_error("Copy your xAI API key, then use the fn menu → Import.", cx);
            return;
        }

        self.target = inject::frontmost_app();
        let Some(target) = self.target.clone() else {
            return;
        };
        self.acc.reset();
        self.session += 1;
        self.level = 0.0;
        self.phase = if self.connected {
            Phase::Recording
        } else {
            Phase::Connecting
        };
        // Open the mic before overlay/dictionary work so capture starts as
        // soon as FN is down. The stream is closed again on FN-up.
        let _ = self.audio_tx.send(audio::Cmd::Start);

        self.dict = Dictionary::load(&config::dictionary_path());
        let connected = self.connected;
        self.update_overlay(cx, |s| {
            s.mode = if connected {
                Mode::Listening
            } else {
                Mode::Connecting
            };
            s.message = if connected {
                "Listening".into()
            } else {
                "Connecting".into()
            };
            s.preview = "".into();
            s.level = 0.0;
            s.target_name = target.name.clone().into();
        });
        self.open_overlay(cx);
    }

    fn end_recording(&mut self, cx: &mut AsyncApp) {
        if !matches!(self.phase, Phase::Recording | Phase::Connecting) {
            return;
        }
        // Audio thread flushes the tail chunk, then sends `Finalize` - order preserved.
        let _ = self.audio_tx.send(audio::Cmd::StopFinalize);
        self.phase = Phase::Finalizing;
        self.update_overlay(cx, |s| {
            s.mode = Mode::Finalizing;
            s.message = "Transcribing".into();
        });
        self.schedule(
            Duration::from_millis(2_800),
            UiEvent::FinalizeTimeout(self.session),
        );
    }

    fn cancel_for_chord(&mut self, cx: &mut AsyncApp) {
        if !matches!(self.phase, Phase::Recording | Phase::Connecting) {
            return;
        }
        let _ = self.audio_tx.send(audio::Cmd::StopDiscard);
        self.acc.reset();
        self.session += 1;
        self.phase = Phase::Idle;
        self.target = None;
        self.update_overlay(cx, |s| {
            s.mode = Mode::Warning;
            s.message = "Cancelled".into();
            s.preview = "Fn was used with another key".into();
        });
        self.schedule_hide(Duration::from_millis(650));
    }

    async fn handle_stt(&mut self, event: SttEvent, cx: &mut AsyncApp) {
        if !matches!(
            self.phase,
            Phase::Recording | Phase::Connecting | Phase::Finalizing
        ) {
            return;
        }
        if self.phase == Phase::Connecting {
            self.phase = Phase::Recording;
            self.update_overlay(cx, |s| {
                s.mode = Mode::Listening;
                s.message = "Listening".into();
            });
        }
        self.acc.ingest(&event);
        let preview = self.dict.apply(&self.acc.preview());
        self.update_overlay(cx, |s| s.preview = preview.into());

        let utterance_done = event.speech_final == Some(true) || event.typ == "transcript.done";
        if utterance_done && self.phase == Phase::Finalizing {
            let text = self.acc.best_text();
            self.complete(text, cx).await;
        }
    }

    async fn complete(&mut self, raw: String, cx: &mut AsyncApp) {
        self.session += 1; // invalidates the pending finalize timeout
        let text = self.dict.apply(&clean(&raw));
        let Some(target) = self.target.clone() else {
            self.fail("The target app went away. Nothing was inserted.".into(), cx);
            return;
        };
        if text.is_empty() {
            self.fail("No speech was detected. Nothing was inserted.".into(), cx);
            return;
        }

        self.phase = Phase::Inserting;
        eprintln!(
            "fntype: inserting {} chars into {} ({})",
            text.chars().count(),
            target.name,
            target.bundle_id
        );
        self.update_overlay(cx, |s| {
            s.mode = Mode::Success;
            s.message = "Inserting".into();
            s.preview = text.clone().into();
        });

        if !inject::activate(target.pid) {
            self.fail("The target app is no longer open.".into(), cx);
            return;
        }
        self.bg.timer(Duration::from_millis(120)).await;

        if inject::frontmost_app().map(|app| app.pid) != Some(target.pid) {
            self.fail("Could not restore focus to the target app.".into(), cx);
            return;
        }

        if inject::focused_element_is_secure() {
            self.fail("FnType will not insert into a password field.".into(), cx);
            return;
        }

        let saved_clipboard = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok());
        let clipboard_set = arboard::Clipboard::new()
            .ok()
            .map(|mut c| c.set_text(text.clone()).is_ok())
            .unwrap_or(false);
        if !clipboard_set {
            self.fail("Could not prepare the transcript for insertion.".into(), cx);
            return;
        }

        inject::post_key(inject::KEY_V, true);

        let press_return = should_press_return(
            &text,
            true,
            target.is_terminal(),
            self.settings.auto_submit,
            self.settings.allow_terminal_submit,
            false,
        );
        if press_return {
            self.bg.timer(Duration::from_millis(130)).await;
            inject::post_key(inject::KEY_RETURN, false);
        }

        self.update_overlay(cx, |s| {
            s.mode = if press_return {
                Mode::Success
            } else {
                Mode::Warning
            };
            s.message = if press_return {
                "Sent".into()
            } else {
                "Pasted • Return skipped".into()
            };
        });

        // Restore the previous clipboard once the paste has landed.
        self.bg.timer(Duration::from_millis(420)).await;
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if clipboard.get_text().ok().as_deref() == Some(text.as_str()) {
                match &saved_clipboard {
                    Some(old) => {
                        let _ = clipboard.set_text(old.clone());
                    }
                    None => {
                        let _ = clipboard.clear();
                    }
                }
            }
        }

        self.phase = Phase::Idle;
        self.target = None;
        self.acc.reset();
        self.schedule_hide(Duration::from_millis(if press_return {
            280
        } else {
            1_250
        }));
    }

    fn fail(&mut self, message: String, cx: &mut AsyncApp) {
        let _ = self.audio_tx.send(audio::Cmd::StopDiscard);
        self.session += 1;
        self.phase = Phase::Error;
        self.target = None;
        self.acc.reset();
        self.update_overlay(cx, |s| {
            s.mode = Mode::Error;
            s.message = "Not inserted".into();
            s.preview = message.into();
        });
        self.open_overlay(cx);
        self.schedule_hide(Duration::from_millis(2_300));
    }

    fn show_setup_error(&mut self, message: &str, cx: &mut AsyncApp) {
        self.phase = Phase::Error;
        self.update_overlay(cx, |s| {
            s.mode = Mode::Error;
            s.message = "Setup needed".into();
            s.preview = message.to_string().into();
            s.target_name = "FnType".into();
        });
        self.open_overlay(cx);
        self.schedule_hide(Duration::from_millis(2_400));
    }

    fn refresh_bias_if_needed(&mut self) {
        let terms = self.dict.bias_terms();
        if terms != self.bias_at_connect {
            if let Some(key) = self.api_key.clone() {
                self.configure_ws(key);
            }
        }
    }
}
