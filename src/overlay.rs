use gpui::{div, prelude::*, px, rgba, Context, Entity, SharedString, Window};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Listening,
    Connecting,
    Finalizing,
    Success,
    Warning,
    Error,
}

pub const OVERLAY_WIDTH: f32 = 132.0;
pub const OVERLAY_HEIGHT: f32 = 38.0;

pub struct OverlayState {
    pub mode: Mode,
    pub message: SharedString,
    pub preview: SharedString,
    pub target_name: SharedString,
    pub level: f32,
    pub phase: f32,
}

impl OverlayState {
    pub fn new() -> Self {
        OverlayState {
            mode: Mode::Listening,
            message: "Listening".into(),
            preview: "".into(),
            target_name: "Current app".into(),
            level: 0.0,
            phase: 0.0,
        }
    }
}

pub struct OverlayView {
    pub state: Entity<OverlayState>,
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let responds_to_audio = matches!(state.mode, Mode::Listening | Mode::Connecting);

        // A quiet, symmetrical waveform: perfectly flat at silence, animated only by mic energy.
        let bars = (0..9).map(|i| {
            let wave = ((state.phase * 8.5 + i as f32 * 0.78).sin() + 1.0) / 2.0;
            let contour = 0.42 + 0.58 * ((i as f32 + 1.0) / 10.0 * std::f32::consts::PI).sin();
            let height = if responds_to_audio && state.level > 0.0 {
                3.0 + 16.0 * state.level * wave * contour
            } else {
                3.0
            };

            div()
                .w(px(3.0))
                .h(px(height))
                .rounded_full()
                .bg(rgba(0xFFFFFFD9))
        });

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .size_full()
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgba(0x0A0B0DEF))
                    .border_1()
                    .border_color(rgba(0xFFFFFF18))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(64.0))
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(3.5))
                            .children(bars),
                    ),
            )
    }
}
