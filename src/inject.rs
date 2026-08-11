use std::ffi::{c_char, c_void, CStr};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEvent, CGEventFlags};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use foreign_types::ForeignType;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::transcript::is_terminal_app;

pub const KEY_V: u16 = 9;
pub const KEY_RETURN: u16 = 36;

#[derive(Clone, Debug)]
pub struct Target {
    pub pid: i32,
    pub name: String,
    pub bundle_id: String,
}

impl Target {
    pub fn is_terminal(&self) -> bool {
        is_terminal_app(&self.name, &self.bundle_id)
    }
}

unsafe fn ns_string_to_string(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

pub fn frontmost_app() -> Option<Target> {
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        let name: *mut AnyObject = msg_send![app, localizedName];
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        Some(Target {
            pid,
            name: {
                let name = ns_string_to_string(name);
                if name.is_empty() {
                    "Current app".into()
                } else {
                    name
                }
            },
            bundle_id: ns_string_to_string(bundle),
        })
    }
}

/// Re-activates the app that was frontmost when dictation started.
pub fn activate(pid: i32) -> bool {
    unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return false;
        }
        let terminated: bool = msg_send![app, isTerminated];
        if terminated {
            return false;
        }
        // NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps
        let _: bool = msg_send![app, activateWithOptions: 3usize];
        true
    }
}

extern "C" {
    fn CGEventPostToPid(pid: i32, event: *mut c_void);
}

pub fn post_key(pid: i32, keycode: u16, command: bool) {
    let Ok(source) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
        return;
    };
    for down in [true, false] {
        let Ok(event) = CGEvent::new_keyboard_event(source.clone(), keycode, down) else {
            continue;
        };
        if command {
            event.set_flags(CGEventFlags::CGEventFlagCommand);
        }
        unsafe { CGEventPostToPid(pid, event.as_ptr().cast()) };
    }
}

// ---- Accessibility: secure-field detection + permission prompts ----

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: CFStringRef,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef)
        -> bool;
    fn AXIsProcessTrusted() -> bool;
}

extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CFRelease(cf: *const c_void);
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
}

/// True when keyboard focus is on a password (secure) text field.
pub fn focused_element_is_secure() -> bool {
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return false;
        }
        let mut focused: *mut c_void = std::ptr::null_mut();
        let focused_attr = CFString::new("AXFocusedUIElement");
        let status =
            AXUIElementCopyAttributeValue(system, focused_attr.as_concrete_TypeRef(), &mut focused);
        CFRelease(system.cast());
        if status != 0 || focused.is_null() {
            return false;
        }

        let mut subrole: *mut c_void = std::ptr::null_mut();
        let subrole_attr = CFString::new("AXSubrole");
        let status = AXUIElementCopyAttributeValue(
            focused,
            subrole_attr.as_concrete_TypeRef(),
            &mut subrole,
        );
        CFRelease(focused.cast());
        if status != 0 || subrole.is_null() {
            return false;
        }

        let is_secure = if CFGetTypeID(subrole.cast()) == CFStringGetTypeID() {
            let value = CFString::wrap_under_get_rule(subrole.cast());
            value == "AXSecureTextField"
        } else {
            false
        };
        CFRelease(subrole.cast());
        is_secure
    }
}

pub struct Permissions {
    pub input_monitoring: bool,
    pub accessibility: bool,
}

pub fn permissions() -> Permissions {
    unsafe {
        Permissions {
            input_monitoring: CGPreflightListenEventAccess(),
            accessibility: AXIsProcessTrusted(),
        }
    }
}

pub fn request_permissions() {
    unsafe {
        if !CGPreflightListenEventAccess() {
            CGRequestListenEventAccess();
        }
        if !CGPreflightPostEventAccess() {
            CGRequestPostEventAccess();
        }
        let key = CFString::new("AXTrustedCheckOptionPrompt");
        let options = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
    }
}
