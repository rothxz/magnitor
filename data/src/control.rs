//! What Magnitor Remote may do to this Mac once Settings › iPhone › Allow Control is on
//! (share.rs's `/v1/controls`, `/v1/volume`, `/v1/mute`, `/v1/open`, `/v1/display-off` and
//! `/v1/lock`): set the sound's volume or mute it, open an app, turn the display off, and lock
//! the screen.
//!
//! The volume is AppleScript's (`osascript`'s Standard Additions, which ask no permission). An
//! app is opened only when it is one `apps` lists, the `.app` bundles directly in the
//! Applications folders, so the phone can't have any other path opened (`open` would run a
//! `.command` file). The display sleeps by `pmset`; the screen locks by `SACLockScreenImmediate`,
//! private to macOS's `login.framework`, looked up when asked, so a macOS without it only says so.
use std::path::{Path, PathBuf};

/// The sound's output volume (0 to 100) and whether it is muted, or `None` when the output
/// has no volume of its own (an HDMI display sets its own).
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Volume {
    pub level: u8,
    pub muted: bool,
}

/// An app the phone can open: its name as Finder shows it without `.app`, and its bundle's path.
#[derive(Debug, PartialEq, Clone)]
pub struct App {
    pub name: String,
    pub path: String,
}

/// `get volume settings` as AppleScript prints it:
/// `output volume:40, input volume:75, alert volume:100, output muted:false`, where an output
/// with no volume says `missing value`.
pub fn parse_volume(text: &str) -> Option<Volume> {
    let field = |name: &str| {
        text.split(',').filter_map(|p| p.split_once(':')).find(|(k, _)| k.trim() == name).map(|(_, v)| v.trim())
    };
    let level = field("output volume")?.parse::<f64>().ok()?;
    let muted = field("output muted") == Some("true");
    Some(Volume { level: level.clamp(0.0, 100.0).round() as u8, muted })
}

/// The `.app` bundles directly in each of `roots`, by name; a name found twice is the first
/// root's (`/Applications` before the system's own).
pub fn apps_in(roots: &[PathBuf]) -> Vec<App> {
    let mut apps: Vec<App> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".app")) else { continue };
            if name.starts_with('.') || !path.is_dir() || apps.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
                continue;
            }
            apps.push(App { name: name.to_owned(), path: path.to_string_lossy().into_owned() });
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// The Applications folders, the person's own last.
fn roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> =
        ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities"].iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join("Applications"));
    }
    roots
}

pub fn apps() -> Vec<App> {
    apps_in(&roots())
}

/// When an app's bundle last changed (its `Info.plist`, else the bundle), in seconds since 1970,
/// or 0 when unknown: the phone keeps an app's icon until this changes.
pub fn stamp(path: &str) -> u64 {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let bundle = Path::new(path);
    modified(&bundle.join("Contents/Info.plist"))
        .or_else(|| modified(bundle))
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The side, in pixels, of the icons `icons` draws: sharp at the phone's 60-point tile on a 2×
/// screen, and small enough to send a dozen at once.
pub const ICON_SIZE: u32 = 144;

/// What a control did: whether it worked, and what to tell the phone when it didn't.
pub type Done = Result<String, String>;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use std::process::Command;

    fn osascript(script: &str) -> Result<String, String> {
        let out = Command::new("/usr/bin/osascript").args(["-e", script]).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    pub fn volume() -> Option<Volume> {
        osascript("get volume settings").ok().as_deref().and_then(parse_volume)
    }

    pub fn set_volume(level: u8) -> Done {
        if volume().is_none() {
            return Err("This Mac’s sound output sets its own volume.".into());
        }
        osascript(&format!("set volume output volume {}", level.min(100)))
            .map(|_| format!("Volume {level}%."))
            .map_err(|_| "Couldn’t change the volume.".into())
    }

    pub fn set_muted(on: bool) -> Done {
        osascript(&format!("set volume output muted {on}"))
            .map(|_| (if on { "Muted." } else { "Unmuted." }).into())
            .map_err(|_| "Couldn’t change the sound.".into())
    }

    /// Opens `path` when it is one of `apps`, or brings it to the front when it is running.
    pub fn open(path: &str, apps: &[App]) -> Done {
        let Some(app) = apps.iter().find(|a| a.path == path) else {
            return Err("Only apps in the Applications folders open from your iPhone.".into());
        };
        match Command::new("/usr/bin/open").args(["-a", &app.path]).status() {
            Ok(s) if s.success() => Ok(format!("Opened {}.", app.name)),
            _ => Err(format!("{} didn’t open.", app.name)),
        }
    }

    /// Draws each bundle's icon as Finder does (`NSWorkspace`'s `iconForFile:`, by JavaScript for
    /// Automation, so no AppKit is linked here) into an `ICON_SIZE` square PNG: one base64 line
    /// per path, in order, "" for one it couldn't draw.
    const ICON_SCRIPT: &str = r#"ObjC.import('AppKit');
function run(argv) {
  const size = Number(argv[0]);
  return argv.slice(1).map((p) => {
    try {
      const img = $.NSWorkspace.sharedWorkspace.iconForFile(p);
      const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(null, size, size, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
      $.NSGraphicsContext.saveGraphicsState;
      $.NSGraphicsContext.setCurrentContext($.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep));
      img.drawInRectFromRectOperationFraction($.NSMakeRect(0, 0, size, size), $.NSZeroRect, $.NSCompositingOperationSourceOver, 1);
      $.NSGraphicsContext.restoreGraphicsState;
      return rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $()).base64EncodedStringWithOptions(0).js;
    } catch (e) {
      return '';
    }
  }).join('\n');
}"#;

    pub fn icons(paths: &[&str]) -> Vec<String> {
        let mut drawn = vec![String::new(); paths.len()];
        if paths.is_empty() {
            return drawn;
        }
        let size = ICON_SIZE.to_string();
        // A bundle that is a link (Safari's, into the system's cryptex) is drawn as its target,
        // without Finder's alias arrow, as Launchpad draws it.
        let targets: Vec<String> = paths
            .iter()
            .map(|p| std::fs::canonicalize(p).map_or_else(|_| (*p).to_owned(), |t| t.to_string_lossy().into_owned()))
            .collect();
        let out = Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", ICON_SCRIPT, &size]).args(&targets).output();
        if let Some(out) = out.ok().filter(|o| o.status.success()) {
            for (slot, line) in drawn.iter_mut().zip(String::from_utf8_lossy(&out.stdout).split('\n')) {
                *slot = line.trim().to_owned();
            }
        }
        drawn
    }

    pub fn display_off() -> Done {
        match Command::new("/usr/bin/pmset").arg("displaysleepnow").status() {
            Ok(s) if s.success() => Ok("The display is off.".into()),
            _ => Err("Couldn’t turn the display off.".into()),
        }
    }

    extern "C" {
        // <dlfcn.h>, in libSystem.
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
        fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    }
    const RTLD_LAZY: i32 = 1;

    /// The screen locked as the Apple menu's Lock Screen does.
    pub fn lock() -> Done {
        let failed = || Err("This version of macOS can’t be locked from your iPhone.".into());
        // SAFETY: both strings are NUL-terminated; the framework stays loaded (never `dlclose`d),
        // and the symbol, when found, is `int SACLockScreenImmediate(void)`.
        unsafe {
            let handle = dlopen(c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login".as_ptr(), RTLD_LAZY);
            if handle.is_null() {
                return failed();
            }
            let symbol = dlsym(handle, c"SACLockScreenImmediate".as_ptr());
            if symbol.is_null() {
                return failed();
            }
            let lock: extern "C" fn() -> i32 = std::mem::transmute(symbol);
            lock();
        }
        Ok("Locked.".into())
    }
}

#[cfg(target_os = "macos")]
pub use mac::{display_off, icons, lock, open, set_muted, set_volume, volume};

#[cfg(not(target_os = "macos"))]
mod other {
    use super::*;
    const ONLY: &str = "Only a Mac can be controlled from your iPhone.";
    pub fn icons(paths: &[&str]) -> Vec<String> {
        vec![String::new(); paths.len()]
    }
    pub fn volume() -> Option<Volume> {
        None
    }
    pub fn set_volume(_: u8) -> Done {
        Err(ONLY.into())
    }
    pub fn set_muted(_: bool) -> Done {
        Err(ONLY.into())
    }
    pub fn open(_: &str, _: &[App]) -> Done {
        Err(ONLY.into())
    }
    pub fn display_off() -> Done {
        Err(ONLY.into())
    }
    pub fn lock() -> Done {
        Err(ONLY.into())
    }
}

#[cfg(not(target_os = "macos"))]
pub use other::{display_off, icons, lock, open, set_muted, set_volume, volume};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_applescripts_volume() {
        assert_eq!(parse_volume("output volume:40, input volume:75, alert volume:100, output muted:false"), Some(Volume { level: 40, muted: false }));
        assert_eq!(parse_volume("output volume:6, input volume:missing value, alert volume:100, output muted:true"), Some(Volume { level: 6, muted: true }));
        assert_eq!(parse_volume("output volume:missing value, input volume:75, alert volume:100, output muted:missing value"), None);
        assert_eq!(parse_volume(""), None);
    }

    #[test]
    fn lists_the_app_bundles_in_each_folder_once() {
        let base = std::env::temp_dir().join(format!("magnitor-apps-{}", std::process::id()));
        let (first, second) = (base.join("Applications"), base.join("System"));
        for dir in [first.join("Zed.app"), first.join("notes.app"), first.join("Folder"), second.join("Notes.app"), second.join("Calculator.app")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(first.join("Fake.app"), "a file, not a bundle").unwrap();
        let apps = apps_in(&[first.clone(), second.clone(), base.join("missing")]);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Calculator", "notes", "Zed"]);
        assert_eq!(apps[1].path, first.join("notes.app").to_string_lossy());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn opens_only_a_listed_app() {
        let apps = [App { name: "Notes".into(), path: "/System/Applications/Notes.app".into() }];
        assert!(open("/tmp/run-me.command", &apps).is_err());
        assert!(open("/System/Applications/Calculator.app", &apps).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn draws_an_apps_icon_as_a_png() {
        let drawn = icons(&["/System/Applications/Calculator.app", "/no/such.app"]);
        assert_eq!(drawn.len(), 2);
        // A PNG's signature, base64: 89 50 4E 47 0D 0A 1A 0A.
        assert!(drawn[0].starts_with("iVBORw0KGgo"), "{}", &drawn[0][..drawn[0].len().min(40)]);
        assert!(stamp("/System/Applications/Calculator.app") > 0);
        assert_eq!(stamp("/no/such.app"), 0);
    }
}
