//! app.contract's `devFiles`: what a developer's Mac fills up with and can rebuild, for Storage's
//! Developer Files. Two kinds:
//!
//! - **Project folders**: a project's dependencies and build output, found by walking the home
//!   folder for a folder beside the file that makes it one (`node_modules` beside
//!   `package.json`, `target` beside `Cargo.toml`, `Pods` beside `Podfile`: `RULES`). The walk
//!   skips Library, the media folders, hidden folders, bundles and Go's module cache, and never
//!   enters what it found, so a `node_modules` inside another is not counted twice. Settings
//!   adds folders to walk (projects on another volume) and folders to leave out; a project
//!   folder outside home is named by its full path.
//! - **Caches**: the tools' own (`CACHES`): Xcode's build data and device support, npm's,
//!   Homebrew's, Gradle's.
//!
//! Each is measured as Storage's folders are (`walk::measure`, bytes on disk) and comes with what
//! the view needs to choose: when its project last changed, and whether a process is running in
//! that project now (a dev server, a watcher), which would break if its folder went. Suggested,
//! the view's first choice, is a cache that is rebuilt as needed, or a project folder untouched
//! for a month and not in use. The view moves what is chosen to the Trash (app.ts's
//! `trashItems`); this only reads.
use crate::shapes::{DevFiles, DevItem};
use crate::{format_bytes, walk};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

/// A project folder: its name (`cmake-build-*`: any name with that start), the files beside it
/// that make it one (any of them), and what it is.
pub struct Rule {
    pub dir: &'static str,
    pub markers: &'static [&'static str],
    pub kind: &'static str,
}

const NODE: &[&str] = &["package.json"];
const PYTHON: &[&str] = &["pyproject.toml", "requirements.txt", "setup.py", "Pipfile"];
const GRADLE: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];

/// The first rule whose folder and marker match names a folder: `build` is Gradle's or Flutter's.
pub const RULES: &[Rule] = &[
    Rule { dir: "node_modules", markers: NODE, kind: "Node modules" },
    Rule { dir: ".next", markers: NODE, kind: "Next.js build" },
    Rule { dir: ".nuxt", markers: NODE, kind: "Nuxt build" },
    Rule { dir: ".svelte-kit", markers: NODE, kind: "SvelteKit build" },
    Rule { dir: ".turbo", markers: NODE, kind: "Turborepo cache" },
    Rule { dir: ".parcel-cache", markers: NODE, kind: "Parcel cache" },
    Rule { dir: ".angular", markers: &["angular.json"], kind: "Angular cache" },
    Rule { dir: "target", markers: &["Cargo.toml", "rust-toolchain.toml", "rust-toolchain"], kind: "Rust build" },
    Rule { dir: "target", markers: &["pom.xml"], kind: "Maven build" },
    Rule { dir: ".build", markers: &["Package.swift"], kind: "Swift build" },
    Rule { dir: "Pods", markers: &["Podfile"], kind: "CocoaPods" },
    Rule { dir: "build", markers: GRADLE, kind: "Gradle build" },
    Rule { dir: "build", markers: &["pubspec.yaml"], kind: "Flutter build" },
    Rule { dir: ".gradle", markers: GRADLE, kind: "Gradle cache" },
    Rule { dir: ".dart_tool", markers: &["pubspec.yaml"], kind: "Dart tool cache" },
    Rule { dir: ".venv", markers: PYTHON, kind: "Python environment" },
    Rule { dir: "venv", markers: PYTHON, kind: "Python environment" },
    Rule { dir: ".zig-cache", markers: &["build.zig"], kind: "Zig cache" },
    Rule { dir: "_build", markers: &["mix.exs"], kind: "Elixir build" },
    Rule { dir: "deps", markers: &["mix.exs"], kind: "Elixir dependencies" },
    Rule { dir: ".stack-work", markers: &["stack.yaml"], kind: "Haskell build" },
    Rule { dir: "dist-newstyle", markers: &["cabal.project"], kind: "Haskell build" },
    Rule { dir: "cmake-build-*", markers: &["CMakeLists.txt"], kind: "CMake build" },
];

impl Rule {
    pub fn names(&self, name: &str) -> bool {
        match self.dir.strip_suffix('*') {
            Some(start) => name.starts_with(start) && name.len() > start.len(),
            None => self.dir == name,
        }
    }
}

/// Whether some rule's folder goes by this name, beside any marker.
pub fn is_artifact(name: &str) -> bool {
    RULES.iter().any(|r| r.names(name))
}

/// A tool's cache: where it is under home, its name, what removing it means, and whether it is
/// suggested (rebuilt without a person's help).
pub struct Cache {
    pub path: &'static str,
    pub name: &'static str,
    pub detail: &'static str,
    pub suggested: bool,
}

const REBUILT: &str = "Rebuilt the next time you build";
const DOWNLOADED: &str = "Downloaded again when needed";
const DEVICE: &str = "Copied again when you next connect a device";

/// Kept in step with app.ts's `TRASHABLE`, which refuses any other path.
pub const CACHES: &[Cache] = &[
    Cache { path: "Library/Developer/Xcode/DerivedData", name: "Xcode build data", detail: REBUILT, suggested: true },
    Cache { path: "Library/Caches/com.apple.dt.Xcode", name: "Xcode cache", detail: "Rebuilt by Xcode as needed", suggested: true },
    Cache { path: "Library/Developer/CoreSimulator/Caches", name: "Simulator caches", detail: "Rebuilt when a simulator starts", suggested: true },
    Cache { path: "Library/Developer/Xcode/iOS DeviceSupport", name: "iOS device support", detail: DEVICE, suggested: false },
    Cache { path: "Library/Developer/Xcode/watchOS DeviceSupport", name: "watchOS device support", detail: DEVICE, suggested: false },
    Cache { path: "Library/Developer/Xcode/tvOS DeviceSupport", name: "tvOS device support", detail: DEVICE, suggested: false },
    Cache { path: "Library/Developer/Xcode/visionOS DeviceSupport", name: "visionOS device support", detail: DEVICE, suggested: false },
    Cache { path: "Library/Developer/Xcode/Archives", name: "Xcode archives", detail: "Your archived builds and their crash symbols", suggested: false },
    Cache { path: ".npm/_cacache", name: "npm cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/Yarn", name: "Yarn cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/pnpm/store", name: "pnpm store", detail: "Shared with your projects: frees only what none of them uses", suggested: false },
    Cache { path: ".bun/install/cache", name: "Bun cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/Homebrew", name: "Homebrew downloads", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/CocoaPods", name: "CocoaPods cache", detail: DOWNLOADED, suggested: true },
    Cache { path: ".gradle/caches", name: "Gradle caches", detail: DOWNLOADED, suggested: true },
    Cache { path: ".m2/repository", name: "Maven repository", detail: DOWNLOADED, suggested: true },
    Cache { path: ".cargo/registry", name: "Cargo registry", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/pip", name: "pip cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/go-build", name: "Go build cache", detail: REBUILT, suggested: true },
    Cache { path: "Library/Caches/node-gyp", name: "node-gyp headers", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/typescript", name: "TypeScript cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/deno", name: "Deno cache", detail: DOWNLOADED, suggested: true },
    Cache { path: "Library/Caches/JetBrains", name: "JetBrains caches", detail: "Indexes rebuilt when you next open a project", suggested: true },
    Cache { path: "Library/Caches/ms-playwright", name: "Playwright browsers", detail: "Installed again with playwright install", suggested: false },
    Cache { path: "Library/Caches/Cypress", name: "Cypress", detail: "Installed again with npm install", suggested: false },
    Cache { path: ".cache/puppeteer", name: "Puppeteer browsers", detail: "Installed again with npm install", suggested: false },
];

/// Home's folders the walk leaves out: an app's data, and media, where no project lives.
const SKIP_TOP: &[&str] = &["Library", "Applications", "Pictures", "Music", "Movies", "Public"];
/// Folders under home the walk never enters: Go's module cache, whose read-only sources hold
/// other people's projects (`devtools` offers `go clean -modcache` for it).
const SKIP_UNDER_HOME: &[&str] = &["go/pkg"];
/// Folders that are one thing to the Finder (an app, a library, a project's file): never entered.
const BUNDLES: &[&str] = &[
    ".app", ".photoslibrary", ".musiclibrary", ".tvlibrary", ".xcarchive", ".bundle", ".framework", ".xcodeproj", ".xcworkspace", ".playground", ".noindex", ".sparsebundle",
    ".vmwarevm", ".pvm", ".utm", ".fcpbundle", ".logicx", ".band",
];
/// How deep the walk goes under home: `~/Documents/GitHub/org/repo/packages/app/node_modules`
/// is seven.
const MAX_DEPTH: usize = 9;
/// The most folders one walk reads, so a home of millions of folders ends in seconds.
const MAX_DIRS: usize = 400_000;
/// The project folders the view lists, the largest; the rest are counted (`more`).
const MAX_PROJECT_ITEMS: usize = 200;
/// A project untouched this long is suggested.
const STALE: Duration = Duration::from_secs(30 * 86_400);

/// A project folder the walk found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The folder to remove.
    pub dir: PathBuf,
    /// The project it belongs to (the folder holding it).
    pub project: PathBuf,
    pub rule: usize,
}

/// Where the walk goes beyond home, and what it leaves out, as Settings has them: absolute paths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folders {
    pub extra: Vec<PathBuf>,
    pub skip: Vec<PathBuf>,
}

impl Folders {
    pub fn new(extra: &[String], skip: &[String]) -> Folders {
        let absolute = |v: &[String]| v.iter().filter(|p| p.starts_with('/')).map(PathBuf::from).collect();
        Folders { extra: absolute(extra), skip: absolute(skip) }
    }
}

#[derive(Default)]
pub struct DevCleaner {
    last: Option<((i64, Folders), DevFiles)>,
}

impl DevCleaner {
    pub fn dev_files(&mut self, generation: i64, folders: Folders, threads: usize) -> DevFiles {
        let key = (generation, folders);
        if let Some((k, files)) = &self.last {
            if *k == key {
                return files.clone();
            }
        }
        let home = crate::home();
        let files = scan(&home, &key.1, threads, SystemTime::now(), &crate::ports::working_dirs());
        self.last = Some((key, files.clone()));
        files
    }
}

/// Find, measure and describe; `now` and `running` (each process's pid, name and working
/// directory) are passed in so a test fixes them.
pub fn scan(home: &Path, folders: &Folders, threads: usize, now: SystemTime, running: &[(i32, String, PathBuf)]) -> DevFiles {
    let found = discover(home, folders, threads);
    let caches: Vec<&Cache> = CACHES.iter().filter(|c| home.join(c.path).is_dir()).collect();
    let roots: Vec<PathBuf> = found.iter().map(|f| f.dir.clone()).chain(caches.iter().map(|c| home.join(c.path))).collect();
    let sizes = walk::measure(&roots, threads);
    let (project_sizes, cache_sizes) = sizes.split_at(found.len());

    let relative = |p: &Path| p.strip_prefix(home).unwrap_or(p).to_string_lossy().into_owned();
    let mut projects: Vec<DevItem> = found
        .iter()
        .zip(project_sizes)
        .filter(|(_, size)| size.total > 0)
        .map(|(f, size)| {
            let rule = &RULES[f.rule];
            let changed = last_changed(&f.project);
            let user = running.iter().find(|(_, _, cwd)| cwd.starts_with(&f.project));
            let stale = changed.and_then(|c| now.duration_since(c).ok()).is_some_and(|age| age >= STALE);
            let place = if f.project.starts_with(home) { format!("~/{}", relative(&f.project)) } else { f.project.to_string_lossy().into_owned() };
            DevItem {
                path: relative(&f.dir),
                name: project_name(&f.project, home),
                kind: rule.kind.into(),
                place,
                age: changed.map_or(String::new(), |c| changed_text(now.duration_since(c).unwrap_or_default())),
                group: "project".into(),
                bytes: size.total as f64,
                size: format_bytes(size.total),
                suggested: stale && user.is_none(),
                note: user.map_or(String::new(), |(_, name, _)| format!("In use by {name}")),
            }
        })
        .collect();
    projects.sort_by(|a, b| b.bytes.total_cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    let more = projects.len().saturating_sub(MAX_PROJECT_ITEMS);
    let hidden: u64 = projects.iter().skip(MAX_PROJECT_ITEMS).map(|i| i.bytes as u64).sum();
    projects.truncate(MAX_PROJECT_ITEMS);

    let mut cache_items: Vec<DevItem> = caches
        .iter()
        .zip(cache_sizes)
        .filter(|(_, size)| size.total > 0)
        .map(|(c, size)| DevItem {
            path: c.path.into(),
            name: c.name.into(),
            kind: "Cache".into(),
            place: c.detail.into(),
            age: String::new(),
            group: "cache".into(),
            bytes: size.total as f64,
            size: format_bytes(size.total),
            suggested: c.suggested,
            note: String::new(),
        })
        .collect();
    cache_items.sort_by(|a, b| b.bytes.total_cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));

    let items: Vec<DevItem> = projects.into_iter().chain(cache_items).collect();
    let total = items.iter().map(|i| i.bytes as u64).sum::<u64>() + hidden;
    DevFiles {
        count: items.len() as f64 + more as f64,
        items,
        total: format_bytes(total),
        total_bytes: total as f64,
        more: more as f64,
        error: String::new(),
    }
}

/// A project as a person knows it: its repository's name, and for a folder inside one, the path
/// from there (`open-chat/android/app`, not `app`). Outside a repository, the folder's own name.
pub fn project_name(project: &Path, home: &Path) -> String {
    let own = || project.file_name().map_or_else(|| project.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let root = project.ancestors().take_while(|a| *a != home && a.starts_with(home)).find(|a| a.join(".git").exists());
    match root {
        Some(root) if root != project => {
            let repo = root.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            format!("{repo}/{}", project.strip_prefix(root).unwrap_or(project).to_string_lossy())
        }
        _ => own(),
    }
}

/// "Changed 3 months ago", as a person counts.
pub fn changed_text(age: Duration) -> String {
    let days = age.as_secs() / 86_400;
    let plural = |n: u64, unit: &str| format!("Changed {n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match days {
        0 => "Changed today".into(),
        1 => "Changed yesterday".into(),
        2..=13 => plural(days, "day"),
        14..=59 => plural(days / 7, "week"),
        60..=364 => plural(days / 30, "month"),
        _ => plural(days / 365, "year"),
    }
}

/// When a project last changed: the newest of what is directly in it, but for the folders this
/// would remove (an install touches `node_modules`, not the project's own files).
fn last_changed(project: &Path) -> Option<SystemTime> {
    std::fs::read_dir(project)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !is_artifact(&name) && name != ".DS_Store"
        })
        .filter_map(|e| std::fs::symlink_metadata(e.path()).and_then(|m| m.modified()).ok())
        .max()
}

/// Whether the walk enters a folder by this name.
fn enters(name: &str) -> bool {
    !name.starts_with('.') && name != "node_modules" && !BUNDLES.iter().any(|b| name.ends_with(b))
}

/// The project folders under `home` and the extra folders, less those skipped, walked on
/// `threads` threads sharing one stack of folders.
pub fn discover(home: &Path, folders: &Folders, threads: usize) -> Vec<Found> {
    let skip: HashSet<PathBuf> = folders.skip.iter().cloned().chain(SKIP_UNDER_HOME.iter().map(|p| home.join(p))).collect();
    let mut tasks: Vec<(PathBuf, usize)> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (enters(&name) && !SKIP_TOP.contains(&name.as_str()) && !skip.contains(&e.path())).then(|| (e.path(), 1))
        })
        .collect();
    // An extra folder is walked as home's own folders are, from its top; one inside home that the
    // walk reaches anyway is found twice and kept once.
    tasks.extend(folders.extra.iter().filter(|p| p.is_dir() && !skip.contains(*p)).map(|p| (p.clone(), 1)));
    let queue = Mutex::new((tasks, 0usize));
    let ready = Condvar::new();
    let read = AtomicUsize::new(0);
    let mut found: Vec<Found> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.max(1)).map(|_| scope.spawn(|| discover_work(&queue, &ready, &read, &skip))).collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found.dedup_by(|a, b| a.dir == b.dir);
    found
}

fn discover_work(queue: &Mutex<(Vec<(PathBuf, usize)>, usize)>, ready: &Condvar, read: &AtomicUsize, skip: &HashSet<PathBuf>) -> Vec<Found> {
    let mut found = Vec::new();
    let mut more: Vec<(PathBuf, usize)> = Vec::new();
    let mut reading = false;
    loop {
        let task = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            q.0.append(&mut more);
            if reading {
                q.1 -= 1;
            }
            if !q.0.is_empty() || q.1 == 0 {
                ready.notify_all();
            }
            loop {
                if let Some(task) = q.0.pop() {
                    q.1 += 1;
                    break Some(task);
                }
                if q.1 == 0 {
                    ready.notify_all();
                    break None;
                }
                q = ready.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some((dir, depth)) = task else { return found };
        reading = true;
        if read.fetch_add(1, Ordering::Relaxed) >= MAX_DIRS {
            continue;
        }
        look(&dir, depth, &mut found, &mut more);
        more.retain(|(p, _)| !skip.contains(p));
    }
}

/// One folder: the project folders in it, and the folders to read next.
fn look(dir: &Path, depth: usize, found: &mut Vec<Found>, more: &mut Vec<(PathBuf, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files = HashSet::new();
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        // A link is not followed, as `walk` follows none.
        if kind.is_dir() {
            dirs.push(name);
        } else if kind.is_file() {
            files.insert(name);
        }
    }
    for name in dirs {
        let rule = RULES.iter().position(|r| r.names(&name) && r.markers.iter().any(|m| files.contains(*m)));
        if let Some(rule) = rule {
            found.push(Found { dir: dir.join(&name), project: dir.to_path_buf(), rule });
        } else if depth < MAX_DEPTH && enters(&name) {
            more.push((dir.join(&name), depth + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("magnitor-dev-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(root: &Path, path: &str, bytes: usize) {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn finds_project_folders_beside_their_marker_and_skips_the_rest() {
        let home = tree("find");
        write(&home, "Projects/web/package.json", 10);
        write(&home, "Projects/web/node_modules/vite/index.js", 8192);
        write(&home, "Projects/web/node_modules/inner/node_modules/x.js", 4096);
        write(&home, "Projects/web/.next/cache.bin", 4096);
        write(&home, "Projects/web/packages/ui/package.json", 10);
        write(&home, "Projects/web/packages/ui/node_modules/x.js", 4096);
        write(&home, "Projects/engine/Cargo.toml", 10);
        write(&home, "Projects/engine/target/debug/app", 4096);
        // No marker: a folder by that name alone is not one.
        write(&home, "Projects/notes/target/a.txt", 4096);
        write(&home, "Projects/notes/build/a.txt", 4096);
        // Never walked: Library, hidden folders, bundles.
        write(&home, "Library/x/package.json", 10);
        write(&home, "Library/x/node_modules/a.js", 4096);
        write(&home, ".hidden/package.json", 10);
        write(&home, ".hidden/node_modules/a.js", 4096);
        write(&home, "Projects/Tool.app/Contents/package.json", 10);
        write(&home, "Projects/Tool.app/Contents/node_modules/a.js", 4096);
        let found = discover(&home, &Folders::default(), 3);
        let rel: Vec<String> = found.iter().map(|f| f.dir.strip_prefix(&home).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(rel, vec!["Projects/engine/target", "Projects/web/.next", "Projects/web/node_modules", "Projects/web/packages/ui/node_modules"]);
        assert_eq!(RULES[found[0].rule].kind, "Rust build");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn suggests_stale_projects_not_in_use_and_rebuilt_caches() {
        let home = tree("scan");
        write(&home, "Code/old/package.json", 10);
        write(&home, "Code/old/node_modules/a.js", 16384);
        write(&home, "Code/busy/package.json", 10);
        write(&home, "Code/busy/node_modules/a.js", 8192);
        write(&home, ".npm/_cacache/index", 8192);
        write(&home, "Library/Developer/Xcode/Archives/a.xcarchive/x", 4096);
        // A month and a day from now, both projects are stale; one has a server running in it.
        let now = SystemTime::now() + Duration::from_secs(31 * 86_400);
        let running = vec![(42, "node".to_string(), home.join("Code/busy/src"))];
        let files = scan(&home, &Folders::default(), 2, now, &running);
        let summary: Vec<(&str, &str, bool, &str)> = files.items.iter().map(|i| (i.path.as_str(), i.group.as_str(), i.suggested, i.note.as_str())).collect();
        assert_eq!(
            summary,
            vec![
                ("Code/old/node_modules", "project", true, ""),
                ("Code/busy/node_modules", "project", false, "In use by node"),
                (".npm/_cacache", "cache", true, ""),
                ("Library/Developer/Xcode/Archives", "cache", false, ""),
            ]
        );
        assert_eq!(files.items[0].name, "old");
        assert_eq!(files.items[0].place, "~/Code/old");
        assert_eq!(files.items[0].age, "Changed 4 weeks ago");
        assert_eq!(files.items[0].size, "16 KB");
        assert_eq!(files.total, "37 KB");
        assert_eq!(files.count, 4.0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn finds_elixir_haskell_and_cmake_builds() {
        let home = tree("kinds");
        write(&home, "Code/api/mix.exs", 10);
        write(&home, "Code/api/_build/dev/x", 4096);
        write(&home, "Code/api/deps/phoenix/x", 4096);
        write(&home, "Code/hs/stack.yaml", 10);
        write(&home, "Code/hs/.stack-work/x", 4096);
        write(&home, "Code/engine/CMakeLists.txt", 10);
        write(&home, "Code/engine/cmake-build-debug/x", 4096);
        write(&home, "Code/engine/cmake-build-release/x", 4096);
        // No marker beside it, or only the start of the pattern: not one.
        write(&home, "Code/notes/deps/x", 4096);
        write(&home, "Code/engine2/CMakeLists.txt", 10);
        write(&home, "Code/engine2/cmake-build-/x", 4096);
        let found = discover(&home, &Folders::default(), 2);
        let kinds: Vec<(String, &str)> = found.iter().map(|f| (f.dir.strip_prefix(&home).unwrap().to_string_lossy().into_owned(), RULES[f.rule].kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("Code/api/_build".into(), "Elixir build"),
                ("Code/api/deps".into(), "Elixir dependencies"),
                ("Code/engine/cmake-build-debug".into(), "CMake build"),
                ("Code/engine/cmake-build-release".into(), "CMake build"),
                ("Code/hs/.stack-work".into(), "Haskell build"),
            ]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn walks_extra_folders_and_leaves_out_skipped_ones_and_gos_module_cache() {
        let home = tree("folders");
        let elsewhere = tree("elsewhere");
        write(&home, "Code/keep/package.json", 10);
        write(&home, "Code/keep/node_modules/a.js", 4096);
        write(&home, "Code/private/package.json", 10);
        write(&home, "Code/private/node_modules/a.js", 4096);
        write(&home, "go/pkg/mod/github.com/x/y@v1/package.json", 10);
        write(&home, "go/pkg/mod/github.com/x/y@v1/node_modules/a.js", 4096);
        write(&elsewhere, "shop/package.json", 10);
        write(&elsewhere, "shop/node_modules/a.js", 4096);
        let folders = Folders::new(&[elsewhere.to_string_lossy().into_owned(), "relative/ignored".into()], &[home.join("Code/private").to_string_lossy().into_owned()]);
        let files = scan(&home, &folders, 2, SystemTime::now(), &[]);
        let paths: Vec<&str> = files.items.iter().map(|i| i.path.as_str()).collect();
        let outside = elsewhere.join("shop/node_modules").to_string_lossy().into_owned();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"Code/keep/node_modules"));
        assert!(paths.contains(&outside.as_str()));
        let item = files.items.iter().find(|i| i.path == outside).unwrap();
        assert_eq!(item.place, elsewhere.join("shop").to_string_lossy());
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn names_a_folder_inside_a_repository_from_its_root() {
        let home = tree("names");
        std::fs::create_dir_all(home.join("Code/chat/.git")).unwrap();
        std::fs::create_dir_all(home.join("Code/chat/android/app")).unwrap();
        std::fs::create_dir_all(home.join("Code/loose")).unwrap();
        assert_eq!(project_name(&home.join("Code/chat/android/app"), &home), "chat/android/app");
        assert_eq!(project_name(&home.join("Code/chat"), &home), "chat");
        assert_eq!(project_name(&home.join("Code/loose"), &home), "loose");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn words_an_age() {
        let d = |n: u64| changed_text(Duration::from_secs(n * 86_400 + 5));
        assert_eq!(d(0), "Changed today");
        assert_eq!(d(1), "Changed yesterday");
        assert_eq!(d(5), "Changed 5 days ago");
        assert_eq!(d(14), "Changed 2 weeks ago");
        assert_eq!(d(90), "Changed 3 months ago");
        assert_eq!(d(400), "Changed 1 year ago");
    }
}
