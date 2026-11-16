//! Each app's icon for the process list, as a PNG data URL: Info.plist names its .icns (AppIcon
//! when it doesn't), which `sips` draws as a 64-pixel PNG. An app with no .icns has the generic
//! app icon. Each app's is made once and kept; "" when none could be drawn.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const GENERIC: &str = "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericApplicationIcon.icns";

fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The icons of `apps` ("" for a process outside an app), in order. Apps not yet drawn are
/// drawn together, each on its own thread.
pub fn icons(apps: &[String]) -> Vec<String> {
    let missing: Vec<&String> = {
        let known = cache().lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        apps.iter().filter(|a| !a.is_empty() && !known.contains_key(*a) && seen.insert(*a)).collect()
    };
    if !missing.is_empty() {
        let drawn: Vec<(String, String)> = std::thread::scope(|s| {
            let handles: Vec<_> = missing.iter().map(|app| s.spawn(move || ((*app).clone(), app_icon(app)))).collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        cache().lock().unwrap().extend(drawn);
    }
    let known = cache().lock().unwrap();
    apps.iter().map(|a| known.get(a).cloned().unwrap_or_default()).collect()
}

fn app_icon(app: &str) -> String {
    let name = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIconFile", "raw", &format!("{app}/Contents/Info.plist")])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "AppIcon".into());
    let file = if name.ends_with(".icns") { name } else { format!("{name}.icns") };
    draw(&format!("{app}/Contents/Resources/{file}")).or_else(|| draw(GENERIC)).unwrap_or_default()
}

fn draw(icns: &str) -> Option<String> {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let out = std::env::temp_dir().join(format!("storage-icon-{}-{}.png", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)));
    let ok = Command::new("/usr/bin/sips")
        .args(["-s", "format", "png", "-Z", "64", icns, "--out"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let bytes = if ok { std::fs::read(&out).ok() } else { None };
    let _ = std::fs::remove_file(&out);
    bytes.filter(|b| b.starts_with(b"\x89PNG")).map(|b| format!("data:image/png;base64,{}", base64(&b)))
}

pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ABC[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ABC[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
