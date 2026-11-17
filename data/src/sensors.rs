//! The CPU's temperature in °C, from Apple silicon's die sensors (`PMU tdie0`, `tdie1`, …).
//!
//! macOS has no public temperature API: these are read through IOKit's HID event system
//! (`IOHIDEventSystemClient`), which is private, as Stats and iStat Menus read them. So it may
//! go in a later macOS, and an Intel Mac or a chip without `tdie` sensors has none: `None`, and
//! the view shows the public thermal state alone (`sample`'s `thermal`).
//!
//! Each sensor read is a round trip to the power manager, about 1 ms of waiting and almost no
//! CPU. The sensors are found once; each reading asks a few of them, spread across the dies,
//! and averages them. Only Activity Monitor's tick asks, never the idle watch.

/// Die sensors read each time: their average is within a tenth of a degree of all of them.
#[cfg(target_os = "macos")]
const READ: usize = 6;

/// The die sensors' average, rounded to a tenth of a degree; `None` when there are none.
#[cfg(target_os = "macos")]
pub fn cpu_temperature() -> Option<f64> {
    mac::cpu_temperature()
}

#[cfg(not(target_os = "macos"))]
pub fn cpu_temperature() -> Option<f64> {
    None
}

/// Up to `n` of `len` items, evenly spaced, so the readings span every die.
pub fn spread(len: usize, n: usize) -> Vec<usize> {
    if len <= n {
        return (0..len).collect();
    }
    (0..n).map(|i| i * len / n).collect()
}

/// The average of plausible readings (a sensor can answer 0 or garbage while it wakes).
pub fn average(readings: &[f64]) -> Option<f64> {
    let ok: Vec<f64> = readings.iter().copied().filter(|t| *t > 0.0 && *t < 150.0).collect();
    (!ok.is_empty()).then(|| (ok.iter().sum::<f64>() / ok.len() as f64 * 10.0).round() / 10.0)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{average, spread, READ};
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::OnceLock;

    type Ref = *const c_void;

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Opaque;
        static kCFTypeDictionaryValueCallBacks: Opaque;
        fn CFNumberCreate(allocator: Ref, kind: isize, value: *const c_void) -> Ref;
        fn CFDictionaryCreate(allocator: Ref, keys: *const Ref, values: *const Ref, count: isize, key_callbacks: *const Opaque, value_callbacks: *const Opaque) -> Ref;
        fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
        fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFStringGetTypeID() -> usize;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFRelease(value: Ref);
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDEventSystemClientCreate(allocator: Ref) -> Ref;
        fn IOHIDEventSystemClientSetMatching(client: Ref, matching: Ref) -> c_int;
        fn IOHIDEventSystemClientCopyServices(client: Ref) -> Ref;
        fn IOHIDServiceClientCopyProperty(service: Ref, key: Ref) -> Ref;
        fn IOHIDServiceClientCopyEvent(service: Ref, kind: i64, options: i32, timestamp: i64) -> Ref;
        fn IOHIDEventGetFloatValue(event: Ref, field: i32) -> f64;
    }

    const UTF8: u32 = 0x0800_0100;
    const INT: isize = 9;
    /// The vendor page and usage of a temperature sensor, and `kIOHIDEventTypeTemperature`.
    const SENSOR_PAGE: i32 = 0xff00;
    const SENSOR_USAGE: i32 = 5;
    const TEMPERATURE: i64 = 15;

    /// The client and the die sensors to read, kept for the run. They are CoreFoundation
    /// objects held, never released, and only the data source's worker thread reads them.
    struct Sensors {
        dies: Vec<Ref>,
    }
    unsafe impl Send for Sensors {}
    unsafe impl Sync for Sensors {}

    fn cf_string(text: &std::ffi::CStr) -> Ref {
        unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) }
    }

    fn product(service: Ref, key: Ref) -> String {
        let value = unsafe { IOHIDServiceClientCopyProperty(service, key) };
        if value.is_null() {
            return String::new();
        }
        let mut buffer = [0 as c_char; 128];
        let ok = unsafe { CFGetTypeID(value) == CFStringGetTypeID() && CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) != 0 };
        unsafe { CFRelease(value) };
        if !ok {
            return String::new();
        }
        unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy().into_owned()
    }

    fn find() -> Option<Sensors> {
        unsafe {
            let client = IOHIDEventSystemClientCreate(std::ptr::null());
            if client.is_null() {
                return None;
            }
            let page = CFNumberCreate(std::ptr::null(), INT, &SENSOR_PAGE as *const i32 as *const c_void);
            let usage = CFNumberCreate(std::ptr::null(), INT, &SENSOR_USAGE as *const i32 as *const c_void);
            let keys = [cf_string(c"PrimaryUsagePage"), cf_string(c"PrimaryUsage")];
            let values = [page, usage];
            let matching = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            IOHIDEventSystemClientSetMatching(client, matching);
            for r in [matching, page, usage, keys[0], keys[1]] {
                CFRelease(r);
            }
            let services = IOHIDEventSystemClientCopyServices(client);
            if services.is_null() {
                CFRelease(client);
                return None;
            }
            let key = cf_string(c"Product");
            let dies: Vec<Ref> = (0..CFArrayGetCount(services))
                .map(|i| CFArrayGetValueAtIndex(services, i))
                .filter(|&s| product(s, key).contains("tdie"))
                .collect();
            CFRelease(key);
            if dies.is_empty() {
                CFRelease(services);
                CFRelease(client);
                return None;
            }
            // The services array holds the sensors, and the client the services: both are kept.
            let chosen = spread(dies.len(), READ).into_iter().map(|i| dies[i]).collect();
            Some(Sensors { dies: chosen })
        }
    }

    fn sensors() -> Option<&'static Sensors> {
        static SENSORS: OnceLock<Option<Sensors>> = OnceLock::new();
        SENSORS.get_or_init(find).as_ref()
    }

    pub fn cpu_temperature() -> Option<f64> {
        let readings: Vec<f64> = sensors()?
            .dies
            .iter()
            .filter_map(|&sensor| unsafe {
                let event = IOHIDServiceClientCopyEvent(sensor, TEMPERATURE, 0, 0);
                if event.is_null() {
                    return None;
                }
                let t = IOHIDEventGetFloatValue(event, (TEMPERATURE as i32) << 16);
                CFRelease(event);
                Some(t)
            })
            .collect();
        average(&readings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_the_reads_across_every_die() {
        assert_eq!(spread(26, 6), [0, 4, 8, 13, 17, 21]);
        assert_eq!(spread(3, 6), [0, 1, 2]);
        assert_eq!(spread(0, 6), Vec::<usize>::new());
    }

    #[test]
    fn averages_plausible_readings_alone() {
        assert_eq!(average(&[47.04, 47.36]), Some(47.2));
        assert_eq!(average(&[0.0, 48.0, 900.0]), Some(48.0));
        assert_eq!(average(&[]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_mac_when_it_has_die_sensors() {
        // An Intel Mac or a chip without `tdie` sensors has none; one that has them reads sense.
        if let Some(t) = cpu_temperature() {
            assert!(t > 0.0 && t < 150.0, "{t}");
        }
    }
}
