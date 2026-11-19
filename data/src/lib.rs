//! Magnitor's measuring: app.contract's `usage` (every folder's size, for the pie) and
//! `contents` (what one folder holds), answered by walking the home folder on a thread per
//! core (`walk`); and Activity Monitor's `tasks`, `process` and `endProcess` (`tasks`, reading
//! the machine with `sample`); and `vitals`, the machine's load alone, for the menu bar, the
//! alerts and the widget (`vitals`). app.ts answers the rest; app.json places this source on a worker
//! thread of its own (`rust.placement`), so a walk of many gigabytes never holds up the view.
//!
//! A walk is kept for its `generation`, the view's count of Measure Again and Clear Cache: the
//! overview's walk already holds each folder's breakdown, so opening a folder answers at once,
//! and a new generation measures afresh.
use exact_runner::{Answer, DataError, DataSource, Store, Value};
use std::collections::HashMap;
use std::path::PathBuf;

pub mod icons;
pub mod sample;
pub mod sensors;
pub mod tasks;
pub mod vitals;
pub mod walk;

#[allow(dead_code, clippy::all)]
mod shapes {
    include!(concat!(env!("OUT_DIR"), "/shapes.rs"));
}
use shapes::{AppIcon, Contents, ContractValue, Item, Slice, Usage};

/// The sources this crate answers; app.ts answers the others.
pub const RUST_SOURCES: &[&str] = &["usage", "contents", "tasks", "process", "endProcess", "vitals", "appIcon", "appGroup", "endApp"];

/// A measured folder: its id in the view, its name, its group, and where it is under home.
struct Measured {
    id: &'static str,
    name: &'static str,
    group: &'static str,
    path: &'static str,
}

/// The pie's order, and so its colours (app.contract's `sliceColor`).
const FOLDERS: &[Measured] = &[
    Measured { id: "caches", name: "App caches", group: "cache", path: "Library/Caches" },
    Measured { id: "derived", name: "Xcode build data", group: "cache", path: "Library/Developer/Xcode/DerivedData" },
    Measured { id: "documents", name: "Documents", group: "device", path: "Documents" },
    Measured { id: "downloads", name: "Downloads", group: "device", path: "Downloads" },
    Measured { id: "desktop", name: "Desktop", group: "device", path: "Desktop" },
    Measured { id: "pictures", name: "Pictures", group: "device", path: "Pictures" },
    Measured { id: "movies", name: "Movies", group: "device", path: "Movies" },
    Measured { id: "music", name: "Music", group: "device", path: "Music" },
];

/// The source: each folder's last walk, by id, with the generation it was made for.
#[derive(Default)]
pub struct Magnitor {
    walked: HashMap<&'static str, (i64, walk::Folder)>,
    tasks: tasks::TaskManager,
    vitals: vitals::Monitor,
}

/// The home folder: the agent's stand-in (`--home`) under a drive, else the person's.
fn home() -> PathBuf {
    std::env::var_os("EXACT_AGENT_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

impl Magnitor {
    /// Walk `which` (indexes into `FOLDERS`) together, sharing the threads, and keep each.
    fn walk(&mut self, which: &[usize], generation: i64) {
        let home = home();
        let roots: Vec<PathBuf> = which.iter().map(|&i| home.join(FOLDERS[i].path)).collect();
        for (&i, folder) in which.iter().zip(walk::measure(&roots, threads())) {
            self.walked.insert(FOLDERS[i].id, (generation, folder));
        }
    }

    /// Every folder, walked for `generation` unless it already was.
    fn usage(&mut self, generation: i64) -> Usage {
        let stale: Vec<usize> = (0..FOLDERS.len())
            .filter(|&i| self.walked.get(FOLDERS[i].id).is_none_or(|(g, _)| *g != generation))
            .collect();
        if !stale.is_empty() {
            self.walk(&stale, generation);
        }
        let sizes: Vec<u64> = FOLDERS.iter().map(|f| self.walked.get(f.id).map_or(0, |(_, w)| w.total)).collect();
        summarize(&sizes)
    }

    /// One folder's entries, largest first, from the overview's walk when it was this generation's.
    fn contents(&mut self, id: &str, generation: i64) -> Contents {
        let Some(index) = FOLDERS.iter().position(|f| f.id == id) else {
            return Contents { id: id.into(), total: String::new(), items: Vec::new(), error: String::new() };
        };
        if self.walked.get(FOLDERS[index].id).is_none_or(|(g, _)| *g != generation) {
            self.walk(&[index], generation);
        }
        let folder = &FOLDERS[index];
        let walked = &self.walked[folder.id].1;
        if !walked.found {
            return Contents {
                id: id.into(),
                total: String::new(),
                items: Vec::new(),
                error: format!("Couldn’t read {}.", folder.name),
            };
        }
        listing(folder, walked)
    }
}

impl DataSource for Magnitor {
    /// Nothing is known before the walk or a reading: every resource is asked at launch.
    fn query(&mut self, source: &str, _: &[Value]) -> Result<Value, DataError> {
        match source {
            "usage" | "contents" | "tasks" | "process" | "endProcess" | "vitals" | "appIcon" | "appGroup" | "endApp" => Err(DataError::Unavailable("measured at run time".into())),
            _ => Err(DataError::UnknownSource(source.into())),
        }
    }

    fn answer(&mut self, _: &mut Store, source: &str, args: &[Value]) -> Result<Answer, DataError> {
        let number = |i: usize| match args.get(i) {
            Some(Value::Number(n)) => Ok(*n as i64),
            _ => Err(DataError::BadArguments(source.into())),
        };
        let text = |i: usize| args.get(i).and_then(Value::as_str).ok_or_else(|| DataError::BadArguments(source.into()));
        match source {
            "usage" => Ok(Answer::Now(self.usage(number(0)?).to_value())),
            "contents" => Ok(Answer::Now(self.contents(text(0)?, number(1)?).to_value())),
            // Grouped by app unless the view says otherwise.
            "tasks" => Ok(Answer::Now(self.tasks.tasks(number(0)?, text(1)?, text(2)?, !matches!(args.get(3), Some(Value::Bool(false)))).to_value())),
            "appGroup" => Ok(Answer::Now(self.tasks.app_group(text(0)?, number(1)?).to_value())),
            "endApp" => Ok(Answer::Now(self.tasks.end_app(text(0)?, text(1)?).to_value())),
            "process" => Ok(Answer::Now(self.tasks.process(number(0)?, number(1)?).to_value())),
            "vitals" => Ok(Answer::Now(self.vitals.vitals(number(0)?).to_value())),
            "appIcon" => Ok(Answer::Now(AppIcon { icon: icons::icon(text(0)?) }.to_value())),
            "endProcess" => {
                let force = matches!(args.get(1), Some(Value::Bool(true)));
                Ok(Answer::Now(self.tasks.end(number(0)?, force).to_value()))
            }
            _ => Err(DataError::UnknownSource(source.into())),
        }
    }
}

/// Finder's decimal units: 511 KB, 211.2 MB, 2 GB (app.ts's `formatBytes`).
pub fn format_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1e3 {
        return format!("{bytes} bytes");
    }
    if b < 1e6 {
        return format!("{} KB", (b / 1e3).round());
    }
    let (n, unit) = if b < 1e9 { (b / 1e6, "MB") } else if b < 1e12 { (b / 1e9, "GB") } else { (b / 1e12, "TB") };
    format!("{} {unit}", trim(n))
}

/// One decimal, without a trailing `.0`.
fn trim(n: f64) -> String {
    let s = format!("{:.1}", (n * 10.0).round() / 10.0);
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

pub fn format_percent(share: f64) -> String {
    if share > 0.0 && share < 0.001 {
        "<0.1%".into()
    } else {
        format!("{}%", trim(share * 100.0))
    }
}

/// A donut segment in a 200×200 box, angles in radians clockwise from twelve o'clock.
const OUTER: f64 = 100.0;
const INNER: f64 = 46.0;
const LABEL: f64 = (OUTER + INNER) / 2.0;

fn at(r: f64, a: f64) -> String {
    format!("{:.3} {:.3}", 100.0 + r * a.sin(), 100.0 - r * a.cos())
}

pub fn segment(from: f64, to: f64) -> String {
    // A whole ring is drawn as almost all of one.
    let end = to.min(from + 2.0 * std::f64::consts::PI - 1e-4);
    let large = if end - from > std::f64::consts::PI { 1 } else { 0 };
    format!(
        "M {} A {OUTER} {OUTER} 0 {large} 1 {} L {} A {INNER} {INNER} 0 {large} 0 {} Z",
        at(OUTER, from),
        at(OUTER, end),
        at(INNER, end),
        at(INNER, from)
    )
}

/// The pie and its totals from each folder's size, in `FOLDERS`' order; empty folders are left out.
fn summarize(sizes: &[u64]) -> Usage {
    let total: u64 = sizes.iter().sum();
    let mut angle = 0.0;
    let slices = FOLDERS
        .iter()
        .zip(sizes)
        .filter(|(_, &bytes)| bytes > 0)
        .map(|(f, &bytes)| {
            let share = if total > 0 { bytes as f64 / total as f64 } else { 0.0 };
            let (from, to) = (angle, angle + share * 2.0 * std::f64::consts::PI);
            let mid = (from + to) / 2.0;
            angle = to;
            Slice {
                id: f.id.into(),
                name: f.name.into(),
                group: f.group.into(),
                bytes: bytes as f64,
                size: format_bytes(bytes),
                percent: format_percent(share),
                path: segment(from, to),
                // The label's centre, as a percentage of the chart's box; thin slices go unlabelled.
                label_x: (100.0 + LABEL * mid.sin()) / 2.0,
                label_y: (100.0 - LABEL * mid.cos()) / 2.0,
                labelled: share >= 0.05,
            }
        })
        .collect();
    let cache: u64 = FOLDERS.iter().zip(sizes).filter(|(f, _)| f.group == "cache").map(|(_, b)| b).sum();
    Usage {
        total: format_bytes(total),
        total_bytes: total as f64,
        slices,
        cache_bytes: cache as f64,
        cache_size: format_bytes(cache),
        error: String::new(),
    }
}

/// What a folder's entry is called: a project's name for its Xcode build data
/// (`Rycap-hlbxgiedqbavjucurvqdmompqdbb`), else its own. An app cache's folder is passed on as
/// `folder`, for the view to name by the app that keeps it.
fn entry(folder: &Measured, name: &str) -> (String, String, String) {
    match folder.id {
        "caches" => (name.into(), String::new(), name.into()),
        "derived" => match name.rsplit_once('-') {
            Some((project, hash)) if hash.len() == 28 && hash.bytes().all(|b| b.is_ascii_lowercase()) => {
                (project.into(), "Xcode project".into(), String::new())
            }
            _ => (name.into(), String::new(), String::new()),
        },
        _ => (name.into(), String::new(), String::new()),
    }
}

fn listing(folder: &Measured, walked: &walk::Folder) -> Contents {
    let total = walked.total;
    let share = |bytes: u64| format_percent(if total > 0 { bytes as f64 / total as f64 } else { 0.0 });
    let mut items: Vec<Item> = walked
        .children
        .iter()
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(name, bytes)| {
            let (shown, detail, folder_name) = entry(folder, name);
            Item {
                path: format!("{}/{name}", folder.path),
                name: shown,
                detail,
                folder: folder_name,
                bytes: *bytes as f64,
                size: format_bytes(*bytes),
                percent: share(*bytes),
            }
        })
        .collect();
    if walked.loose > 0 {
        items.push(Item {
            path: folder.path.into(),
            name: "Other files".into(),
            detail: "Files directly in the folder".into(),
            folder: String::new(),
            bytes: walked.loose as f64,
            size: format_bytes(walked.loose),
            percent: share(walked.loose),
        });
    }
    items.sort_by(|a, b| b.bytes.total_cmp(&a.bytes));
    Contents { id: folder.id.into(), total: format_bytes(total), items, error: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_finder_does() {
        assert_eq!(format_bytes(511_000), "511 KB");
        assert_eq!(format_bytes(211_200_000), "211.2 MB");
        assert_eq!(format_bytes(2_000_000_000), "2 GB");
        assert_eq!(format_bytes(999), "999 bytes");
        assert_eq!(format_percent(0.0004), "<0.1%");
        assert_eq!(format_percent(0.104), "10.4%");
        assert_eq!(format_percent(0.09), "9%");
    }

    #[test]
    fn names_an_xcode_project_by_its_build_folder() {
        let derived = &FOLDERS[1];
        assert_eq!(entry(derived, "Rycap-hlbxgiedqbavjucurvqdmompqdbb").0, "Rycap");
        assert_eq!(entry(derived, "ModuleCache.noindex").0, "ModuleCache.noindex");
        assert_eq!(entry(&FOLDERS[0], "com.apple.Safari").2, "com.apple.Safari");
    }
}
