use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType,
};
use futures::channel::mpsc::UnboundedSender;

use crate::UiEvent;

extern "C" {
    fn CGEventSourceFlagsState(state_id: u32) -> u64;
}

const SECONDARY_FN_MASK: u64 = 0x0080_0000;
const COMBINED_SESSION_STATE: u32 = 0;

pub fn fn_key_physically_down() -> bool {
    unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) & SECONDARY_FN_MASK != 0 }
}

/// Watches the Globe/Fn modifier globally with a listen-only CoreGraphics event tap.
pub fn spawn(ui: UnboundedSender<UiEvent>) {
    std::thread::Builder::new()
        .name("fn-monitor".into())
        .spawn(move || run(ui))
        .expect("spawn fn-monitor thread");
}

fn run(ui: UnboundedSender<UiEvent>) {
    let fn_down = Arc::new(AtomicBool::new(false));

    // Watchdog: if the release event is missed (e.g. tap briefly disabled),
    // fall back to polling the real hardware modifier state.
    {
        let fn_down = fn_down.clone();
        let ui = ui.clone();
        std::thread::Builder::new()
            .name("fn-watchdog".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(300));
                if fn_down.load(Ordering::Relaxed) && !fn_key_physically_down() {
                    fn_down.store(false, Ordering::Relaxed);
                    let _ = ui.unbounded_send(UiEvent::FnUp);
                }
            })
            .ok();
    }

    let callback_fn_down = fn_down.clone();
    let callback_ui = ui.clone();
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
        move |_proxy, event_type, event| {
            match event_type {
                CGEventType::FlagsChanged => {
                    let down = event
                        .get_flags()
                        .contains(CGEventFlags::CGEventFlagSecondaryFn);
                    let was = callback_fn_down.swap(down, Ordering::Relaxed);
                    if down != was {
                        let _ = callback_ui.unbounded_send(if down {
                            UiEvent::FnDown
                        } else {
                            UiEvent::FnUp
                        });
                    }
                }
                CGEventType::KeyDown if callback_fn_down.load(Ordering::Relaxed) => {
                    let _ = callback_ui.unbounded_send(UiEvent::FnChord);
                }
                _ => {}
            }
            None
        },
    );

    match tap {
        Ok(tap) => {
            let source = match tap.mach_port.create_runloop_source(0) {
                Ok(source) => source,
                Err(_) => {
                    let _ = ui.unbounded_send(UiEvent::MonitorFailed);
                    return;
                }
            };
            unsafe {
                CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
            }
            tap.enable();
            CFRunLoop::run_current();
        }
        Err(_) => {
            let _ = ui.unbounded_send(UiEvent::MonitorFailed);
        }
    }
}
