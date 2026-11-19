//! Each app's icon for the process list, as a PNG data URL: Info.plist names its .icns (AppIcon
//! when it doesn't), which `sips` draws as a 64-pixel PNG. An app with no .icns has the generic
//! app icon. Each app's is made once and kept: in memory for this run, and on disk in
//! Magnitor's own cache folder for the next, so opening the popover after a launch spawns no
//! `plutil` or `sips` for an app already drawn. "" when none could be drawn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const GENERIC: &str = "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericApplicationIcon.icns";
/// Magnitor's own folder in Library/Caches.
const OWN_CACHE: &str = "com.magnitor.app";

fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// One app's icon, "" for none: the `appIcon` source. Kept after the first ask.
pub fn icon(app: &str) -> String {
    if app.is_empty() {
        return String::new();
    }
    if let Some(known) = cache().lock().unwrap().get(app) {
        return known.clone();
    }
    let drawn = app_icon(app);
    cache().lock().unwrap().insert(app.to_owned(), drawn.clone());
    drawn
}

/// The icons of `apps` not yet known, read from disk or drawn together, each on its own
/// thread, so the view's asks for them (`icon`) answer from memory. Known ones cost a lookup.
pub fn prepare(apps: &[String]) {
    let missing: Vec<&String> = {
        let known = cache().lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        apps.iter().filter(|a| !a.is_empty() && !known.contains_key(*a) && seen.insert(*a)).collect()
    };
    if missing.is_empty() {
        return;
    }
    let drawn: Vec<(String, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = missing.iter().map(|app| s.spawn(move || ((*app).clone(), app_icon(app)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    cache().lock().unwrap().extend(drawn);
}

/// The app's icon as a data URL: the one kept on disk, or drawn now and kept.
fn app_icon(app: &str) -> String {
    let kept = kept_path(app);
    let png = match kept.as_ref().and_then(|p| std::fs::read(p).ok()).filter(|b| is_png(b)) {
        Some(png) => Some(png),
        None => {
            let png = draw_app(app);
            if let (Some(path), Some(png)) = (&kept, &png) {
                keep(path, png);
            }
            png
        }
    };
    png.map(|b| format!("data:image/png;base64,{}", base64(&b))).unwrap_or_default()
}

/// Where the app's icon is kept: named by its path and its Info.plist's modification time, so
/// an updated app is drawn afresh. The person's home always, never the agent's stand-in.
fn kept_path(app: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let modified = std::fs::metadata(format!("{app}/Contents/Info.plist")).ok()?.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(PathBuf::from(home).join("Library/Caches").join(OWN_CACHE).join("icons").join(format!("{:016x}-{secs}.png", fnv1a(app.as_bytes()))))
}

/// Written whole, then renamed into place, so a reader never sees half an icon. Best effort:
/// an icon that can't be kept is drawn again next launch.
fn keep(path: &PathBuf, png: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let partial = path.with_extension(format!("{}.part", std::process::id()));
    if std::fs::write(&partial, png).is_ok() && std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

fn draw_app(app: &str) -> Option<Vec<u8>> {
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
    draw(&format!("{app}/Contents/Resources/{file}")).or_else(|| draw(GENERIC))
}

fn draw(icns: &str) -> Option<Vec<u8>> {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let out = std::env::temp_dir().join(format!("magnitor-icon-{}-{}.png", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)));
    let ok = Command::new("/usr/bin/sips")
        .args(["-s", "format", "png", "-Z", "64", icns, "--out"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let bytes = if ok { std::fs::read(&out).ok() } else { None };
    let _ = std::fs::remove_file(&out);
    bytes.filter(|b| is_png(b))
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
}

/// FNV-1a: the same name for the same app on every launch and Rust version.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
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

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(super::fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(super::fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
