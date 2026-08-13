use gpui::{
    div, prelude::*, px, rgba, Context, Entity, Rgba, SharedString, Window, WindowAppearance,
};

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

#[derive(Clone, Copy, PartialEq, Debug)]
struct Palette {
    background: Rgba,
    border: Rgba,
    bar: Rgba,
}

fn is_light(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Light | WindowAppearance::VibrantLight
    )
}

fn palette(appearance: WindowAppearance) -> Palette {
    if is_light(appearance) {
        Palette {
            background: rgba(0xF4F5F7EF),
            border: rgba(0x00000018),
            bar: rgba(0x1A1B1DD9),
        }
    } else {
        Palette {
            background: rgba(0x0A0B0DEF),
            border: rgba(0xFFFFFF18),
            bar: rgba(0xFFFFFFD9),
        }
    }
}

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

impl OverlayView {
    pub fn new(state: Entity<OverlayState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        OverlayView { state }
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let palette = palette(window.appearance());
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
                .bg(palette.bar)
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
                    .bg(palette.background)
                    .border_1()
                    .border_color(palette.border)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_appearances_use_the_light_palette() {
        for appearance in [WindowAppearance::Light, WindowAppearance::VibrantLight] {
            assert!(is_light(appearance));
            let colors = palette(appearance);
            assert_eq!(colors.background, rgba(0xF4F5F7EF));
            assert_eq!(colors.bar, rgba(0x1A1B1DD9));
        }
    }

    #[test]
    fn dark_appearances_keep_the_existing_palette() {
        for appearance in [WindowAppearance::Dark, WindowAppearance::VibrantDark] {
            assert!(!is_light(appearance));
            let colors = palette(appearance);
            assert_eq!(colors.background, rgba(0x0A0B0DEF));
            assert_eq!(colors.bar, rgba(0xFFFFFFD9));
        }
    }
}
