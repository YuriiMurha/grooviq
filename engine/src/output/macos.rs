//! CoreAudio details cpal does not cover: how an output is connected
//! (`kAudioDevicePropertyTransportType`: two property reads, nothing opened), and which thread
//! CoreAudio's notifications arrive on.

use super::Transport;
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertySelector, AudioObjectSetPropertyData, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE,
    kAudioDeviceTransportTypeBuiltIn, kAudioDeviceTransportTypeDisplayPort,
    kAudioDeviceTransportTypeHDMI, kAudioDeviceTransportTypeUSB, kAudioHardwareNoError,
    kAudioHardwarePropertyRunLoop, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioObjectUnknown,
};
use objc2_core_foundation::CFString;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::sync::Once;

/// Has CoreAudio deliver its notifications on a thread of its own rather than on the main
/// thread's run loop, which the app's UI may keep busy (cpal waits for one of them when it
/// switches a device's rate). Once per process, before any device is listed. Best effort: if
/// CoreAudio refuses, notifications stay on the main run loop.
pub(super) fn notify_on_own_thread() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // A null CFRunLoopRef means CoreAudio's own thread.
        let run_loop: *const c_void = ptr::null();
        // SAFETY: the property is one CFRunLoopRef, matching the size; there is no qualifier.
        let _status = unsafe {
            AudioObjectSetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address(kAudioHardwarePropertyRunLoop)),
                0,
                ptr::null(),
                size_of::<*const c_void>() as u32,
                NonNull::from(&run_loop).cast(),
            )
        };
    });
}

/// The transport of the device whose UID (cpal's device id on macOS) is `uid`, or `None` if
/// CoreAudio does not know the device.
pub(super) fn transport(uid: &str) -> Option<Transport> {
    let device = device_for_uid(uid)?;
    #[allow(non_upper_case_globals)] // CoreAudio's constant names
    let transport = match read_u32(device, kAudioDevicePropertyTransportType)? {
        kAudioDeviceTransportTypeBuiltIn => Transport::BuiltIn,
        kAudioDeviceTransportTypeBluetooth | kAudioDeviceTransportTypeBluetoothLE => {
            Transport::Bluetooth
        }
        kAudioDeviceTransportTypeUSB => Transport::Usb,
        kAudioDeviceTransportTypeHDMI => Transport::Hdmi,
        kAudioDeviceTransportTypeDisplayPort => Transport::DisplayPort,
        _ => Transport::Other,
    };
    Some(transport)
}

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// The device with this UID, from the system object (the UID is the property's qualifier).
fn device_for_uid(uid: &str) -> Option<AudioObjectID> {
    let uid = CFString::from_str(uid);
    let qualifier: *const CFString = &*uid;
    let mut device: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is one CFStringRef, which `uid` keeps alive for the call, and the
    // output is one AudioObjectID, matching `size`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address(kAudioHardwarePropertyTranslateUIDToDevice)),
            size_of::<*const CFString>() as u32,
            ptr::from_ref(&qualifier).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut device).cast(),
        )
    };
    (status == kAudioHardwareNoError && device != kAudioObjectUnknown).then_some(device)
}

/// A `UInt32` property of `object`.
fn read_u32(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the property is one UInt32, matching `size`; there is no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address(selector)),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == kAudioHardwareNoError && size as usize == size_of::<u32>()).then_some(value)
}
