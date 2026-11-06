//! How far behind the ring the listener hears, read without playing anything (macOS only).
//!
//! `output_device_latency` reads the HAL's own figures for every output device: the terms that
//! sit between the sample an output callback writes and the DAC. It is ignored because it reports
//! this machine rather than checks the code.
//!
//! ```text
//! cargo nextest run -p onicode-desktop-codec --release --run-ignored only -E 'binary(audio_latency)' --no-capture
//! ```

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "test fixture arithmetic on small, bounded values"
    )]

    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{self, NonNull};

    use objc2_core_audio::{
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
        AudioObjectPropertyAddress, kAudioDevicePropertyBufferFrameSize,
        kAudioDevicePropertyBufferFrameSizeRange, kAudioDevicePropertyLatency,
        kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
        kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
        kAudioStreamPropertyLatency,
    };
    use objc2_core_audio_types::AudioValueRange;
    use objc2_core_foundation::{CFRetained, CFString};

    const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

    fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    /// One fixed-size property, `None` when the object does not have it.
    fn get<T: Copy>(object: AudioObjectID, selector: u32, scope: u32, zero: T) -> Option<T> {
        let mut value = zero;
        let mut size = size_of::<T>() as u32;
        let mut addr = address(selector, scope);
        // SAFETY: CoreAudio HAL rule: the address and the out buffer of `size` bytes are valid
        // for the call; `T` is the plain C type the selector is documented to return.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut addr),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        (status == 0).then_some(value)
    }

    /// A property that is an array of object IDs.
    fn ids(object: AudioObjectID, selector: u32, scope: u32) -> Vec<AudioObjectID> {
        let mut addr = address(selector, scope);
        let mut size = 0_u32;
        // SAFETY: CoreAudio HAL rule: valid address and out pointer for the size query.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                object,
                NonNull::from(&mut addr),
                0,
                ptr::null(),
                NonNull::from(&mut size),
            )
        };
        if status != 0 || size == 0 {
            return Vec::new();
        }
        let mut out = vec![0; size as usize / size_of::<AudioObjectID>()];
        // SAFETY: as above; the buffer holds `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut addr),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(out.as_mut_ptr().cast::<c_void>()).unwrap(),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        out.truncate(size as usize / size_of::<AudioObjectID>());
        out
    }

    fn name(object: AudioObjectID) -> String {
        let raw = get::<*const CFString>(
            object,
            kAudioObjectPropertyName,
            kAudioObjectPropertyScopeGlobal,
            ptr::null(),
        );
        let Some(raw) = raw.and_then(|r| NonNull::new(r.cast_mut())) else {
            return String::from("?");
        };
        // SAFETY: CoreAudio HAL rule: `kAudioObjectPropertyName` hands a CFString the caller
        // owns (+1), which `CFRetained` releases.
        let name = unsafe { CFRetained::from_raw(raw) };
        name.to_string()
    }

    /// The HAL's latency terms for every device with output streams. What a sample written by an
    /// output callback waits before the DAC is the I/O buffer the callback filled, the safety
    /// offset the HAL keeps ahead of the hardware's read position, and the device's and its
    /// stream's own latency.
    #[test]
    #[ignore = "measurement: reports this machine's output devices"]
    fn output_device_latency() {
        let default = get(
            SYSTEM,
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
            0_u32,
        )
        .unwrap_or(0);
        for device in ids(
            SYSTEM,
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
        ) {
            let streams = ids(
                device,
                kAudioDevicePropertyStreams,
                kAudioObjectPropertyScopeOutput,
            );
            if streams.is_empty() {
                continue;
            }
            let global = kAudioObjectPropertyScopeGlobal;
            let output = kAudioObjectPropertyScopeOutput;
            let rate = get(
                device,
                kAudioDevicePropertyNominalSampleRate,
                global,
                0.0_f64,
            )
            .unwrap_or(0.0);
            let buffer = get(device, kAudioDevicePropertyBufferFrameSize, global, 0_u32);
            let range = get(
                device,
                kAudioDevicePropertyBufferFrameSizeRange,
                global,
                AudioValueRange {
                    mMinimum: 0.0,
                    mMaximum: 0.0,
                },
            );
            let latency = get(device, kAudioDevicePropertyLatency, output, 0_u32);
            let safety = get(device, kAudioDevicePropertySafetyOffset, output, 0_u32);
            let stream_latency: Vec<u32> = streams
                .iter()
                .filter_map(|&s| get(s, kAudioStreamPropertyLatency, global, 0_u32))
                .collect();
            let frames = buffer.unwrap_or(0)
                + safety.unwrap_or(0)
                + latency.unwrap_or(0)
                + stream_latency.first().copied().unwrap_or(0);
            let ms = |f: u32| {
                if rate > 0.0 {
                    f64::from(f) * 1000.0 / rate
                } else {
                    0.0
                }
            };
            eprintln!(
                "{}{} (id {device}): {rate} Hz; I/O buffer {buffer:?} frames ({:.2} ms), range {:?}; \
                 device latency {latency:?}, safety offset {safety:?}, stream latency {stream_latency:?}; \
                 buffer + safety + latency + stream = {frames} frames = {:.2} ms",
                if device == default { "DEFAULT " } else { "" },
                name(device),
                ms(buffer.unwrap_or(0)),
                range.map(|r| (r.mMinimum, r.mMaximum)),
                ms(frames),
            );
        }
    }
}
