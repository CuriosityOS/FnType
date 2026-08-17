//! Core Audio helpers so capture does not flip AirPods into headset mode.
use std::ffi::c_void;
use std::mem;
use std::ptr::{null, NonNull};

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use objc2_core_audio::{
    kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBluetooth,
    kAudioDeviceTransportTypeBluetoothLE, kAudioHardwareNoError,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, AudioDeviceID, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData,
};

pub struct DefaultInputGuard {
    previous: Option<AudioDeviceID>,
}

impl DefaultInputGuard {
    /// If the system default input is a Bluetooth headset, point it at `preferred`
    /// for the lifetime of this guard so playback can stay on A2DP.
    pub fn protect_playback(preferred: &str) -> Self {
        if crate::audio::is_bluetooth_input(preferred) {
            return Self { previous: None };
        }
        let Some(current) = default_input_id() else {
            return Self { previous: None };
        };
        if !device_is_bluetooth(current) {
            return Self { previous: None };
        }
        let Some(preferred_id) = input_id_by_name(preferred) else {
            return Self { previous: None };
        };
        if preferred_id == current {
            return Self { previous: None };
        }
        if !set_default_input_id(preferred_id) {
            return Self { previous: None };
        }
        eprintln!(
            "fntype: temporarily set default input to {preferred} so Bluetooth playback stays high quality"
        );
        Self {
            previous: Some(current),
        }
    }
}

impl Drop for DefaultInputGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            if set_default_input_id(previous) {
                if let Some(name) = device_name(previous) {
                    eprintln!("fntype: restored default input to {name}");
                }
            }
        }
    }
}

fn default_input_id() -> Option<AudioDeviceID> {
    audio_object_get(
        kAudioObjectSystemObject as AudioObjectID,
        default_input_address(),
    )
}

fn set_default_input_id(id: AudioDeviceID) -> bool {
    let address = default_input_address();
    let status = unsafe {
        AudioObjectSetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            null(),
            mem::size_of::<AudioDeviceID>() as u32,
            NonNull::from(&id).cast(),
        )
    };
    status == kAudioHardwareNoError
}

fn input_id_by_name(name: &str) -> Option<AudioDeviceID> {
    device_ids()?
        .into_iter()
        .find(|id| device_name(*id).as_deref() == Some(name))
}

fn device_is_bluetooth(id: AudioDeviceID) -> bool {
    if device_name(id)
        .as_deref()
        .is_some_and(crate::audio::is_bluetooth_input)
    {
        return true;
    }
    matches!(
        transport_type(id),
        Some(kind)
            if kind == kAudioDeviceTransportTypeBluetooth
                || kind == kAudioDeviceTransportTypeBluetoothLE
    )
}

fn device_name(id: AudioDeviceID) -> Option<String> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioObjectPropertyName,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut cf_name: CFStringRef = std::ptr::null();
    let mut data_size = mem::size_of::<CFStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
            NonNull::from(&mut cf_name).cast(),
        )
    };
    if status != kAudioHardwareNoError || cf_name.is_null() {
        return None;
    }
    let name = unsafe { CFString::wrap_under_create_rule(cf_name) };
    Some(name.to_string())
}

fn transport_type(id: AudioDeviceID) -> Option<u32> {
    audio_object_get(
        id,
        AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyTransportType,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        },
    )
}

fn device_ids() -> Option<Vec<AudioDeviceID>> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut data_size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
        )
    };
    if status != kAudioHardwareNoError || data_size == 0 {
        return None;
    }
    let count = data_size as usize / mem::size_of::<AudioDeviceID>();
    let mut ids = vec![0u32; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
            NonNull::new(ids.as_mut_ptr())?.cast(),
        )
    };
    if status != kAudioHardwareNoError {
        return None;
    }
    Some(ids)
}

fn default_input_address() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultInputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn audio_object_get<T: Copy + Default>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
) -> Option<T> {
    let mut value = T::default();
    let mut data_size = mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    (status == kAudioHardwareNoError).then_some(value)
}
