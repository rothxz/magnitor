//! A parallel `du`: the bytes on disk under each folder, by its top-level entries, measured by
//! a few threads sharing one queue of directories, at utility QoS on macOS, where each
//! directory is read in bulk (`getattrlistbulk`).
//!
//! A file counts its allocated blocks (`st_blocks` × 512), as `du` and Finder's "on disk" do,
//! so a sparse or compressed file counts what it takes. Symbolic links are not followed, and
//! a directory that cannot be read (another user's, or one macOS's privacy protection keeps
//! from this app) is left out, as `du` leaves it out after saying so.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

/// One measured folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folder {
    /// Whether the folder exists and could be read.
    pub found: bool,
    /// Every byte under it.
    pub total: u64,
    /// The bytes under each folder directly in it, by name.
    pub children: Vec<(String, u64)>,
    /// The files directly in it.
    pub loose: u64,
}

/// A directory to read: where it is, which root it is under, and which of that root's own
/// folders it is under (`None` for the root itself).
struct Task {
    dir: PathBuf,
    root: usize,
    child: Option<usize>,
}

/// The work left: directories not yet read, and how many are being read now.
struct Queue {
    tasks: Vec<Task>,
    busy: usize,
}

/// What one thread found, merged once at the end so no thread waits on another's totals.
#[derive(Default)]
struct Tally {
    loose: Vec<u64>,
    children: HashMap<(usize, usize), u64>,
}

/// Measure each of `roots` with up to `threads` threads.
pub fn measure(roots: &[PathBuf], threads: usize) -> Vec<Folder> {
    let mut folders: Vec<Folder> = vec![Folder::default(); roots.len()];
    // Each root's own folders, by index, named once here so the threads share indexes only.
    let mut names: Vec<Vec<String>> = vec![Vec::new(); roots.len()];
    let mut tasks = Vec::new();
    let mut tally = Tally { loose: vec![0; roots.len()], ..Tally::default() };
    let mut scratch = Scratch::new();
    for (root, path) in roots.iter().enumerate() {
        if !path.is_dir() {
            continue;
        }
        folders[root].found = true;
        tally.loose[root] = list(path, &mut scratch, |name| {
            names[root].push(name.to_string_lossy().into_owned());
            tasks.push(Task { dir: path.join(name), root, child: Some(names[root].len() - 1) });
        });
    }
    // The largest folders first: the queue is a stack, so they are taken last otherwise.
    tasks.reverse();

    let queue = Mutex::new(Queue { tasks, busy: 0 });
    let ready = Condvar::new();
    let tallies: Vec<Tally> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.max(1))
            .map(|_| scope.spawn(|| work(&queue, &ready, roots.len())))
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or_default()).collect()
    });

    for t in std::iter::once(tally).chain(tallies) {
        for (root, bytes) in t.loose.iter().enumerate() {
            folders[root].loose += bytes;
        }
        for ((root, child), bytes) in t.children {
            folders[root].children.push((names[root][child].clone(), bytes));
        }
    }
    for (root, folder) in folders.iter_mut().enumerate() {
        // A child's bytes arrive from each thread that read some of it: add them up.
        let mut by_child: HashMap<String, u64> = names[root].iter().map(|n| (n.clone(), 0)).collect();
        for (name, bytes) in folder.children.drain(..) {
            *by_child.entry(name).or_default() += bytes;
        }
        folder.children = by_child.into_iter().collect();
        folder.children.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        folder.total = folder.loose + folder.children.iter().map(|c| c.1).sum::<u64>();
    }
    folders
}

/// One thread: take a directory, add up its files, queue its directories; stop once no
/// directory is left and no thread is reading one (which could queue more).
fn work(queue: &Mutex<Queue>, ready: &Condvar, roots: usize) -> Tally {
    lower_priority();
    let mut tally = Tally { loose: vec![0; roots], ..Tally::default() };
    let mut found = Vec::new();
    let mut scratch = Scratch::new();
    let mut reading = false;
    loop {
        let task = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            // What this thread found and that it is done, in one step: a thread that saw
            // neither could take the queue for empty and stop while more was coming.
            q.tasks.append(&mut found);
            if reading {
                q.busy -= 1;
            }
            if !q.tasks.is_empty() || q.busy == 0 {
                ready.notify_all();
            }
            loop {
                if let Some(task) = q.tasks.pop() {
                    q.busy += 1;
                    break Some(task);
                }
                if q.busy == 0 {
                    ready.notify_all();
                    break None;
                }
                q = ready.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(task) = task else { return tally };
        reading = true;
        let bytes = read(&task.dir, &task, &mut found, &mut scratch);
        match task.child {
            Some(child) => *tally.children.entry((task.root, child)).or_default() += bytes,
            None => tally.loose[task.root] += bytes,
        }
    }
}

/// The files directly in `dir`; its directories go to `found` for any thread to read.
fn read(dir: &Path, task: &Task, found: &mut Vec<Task>, scratch: &mut Scratch) -> u64 {
    list(dir, scratch, |name| found.push(Task { dir: dir.join(name), root: task.root, child: task.child }))
}

/// The bytes the files directly in `dir` take on disk; each directory in it named to `on_dir`.
/// A directory that cannot be read takes 0.
fn list(dir: &Path, scratch: &mut Scratch, on_dir: impl FnMut(&OsStr)) -> u64 {
    #[cfg(target_os = "macos")]
    return bulk::list(dir, scratch, on_dir);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = scratch;
        list_each(dir, on_dir)
    }
}

/// `list` by `readdir` and an `lstat` per file: everywhere but macOS, and the tests' reference.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn list_each(dir: &Path, mut on_dir: impl FnMut(&OsStr)) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            on_dir(&entry.file_name());
        } else if let Ok(meta) = entry.metadata() {
            // `DirEntry::metadata` does not follow a symbolic link: a link counts its own size.
            bytes += meta.blocks() * 512;
        }
    }
    bytes
}

/// A thread's buffer for `getattrlistbulk`, 8-byte aligned as its entries are; kept across
/// directories so a walk allocates it once a thread.
pub struct Scratch(#[cfg_attr(not(target_os = "macos"), allow(dead_code))] Vec<u64>);

impl Scratch {
    fn new() -> Scratch {
        // 64 KiB: some hundreds of entries a call, so most directories take one.
        Scratch(vec![0; 64 * 1024 / 8])
    }
}

/// macOS's `getattrlistbulk`: a directory's entries with their type and size on disk, a buffer
/// of them a call, where `readdir` takes an `lstat` more for each file. Nearly all a walk's time
/// is the kernel's, so this is most of what it costs.
#[cfg(target_os = "macos")]
mod bulk {
    use super::Scratch;
    use std::ffi::{c_int, c_void, OsStr};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    /// `struct attrlist` (sys/attr.h).
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    extern "C" {
        fn getattrlistbulk(dirfd: c_int, list: *mut AttrList, buf: *mut c_void, size: usize, options: u64) -> c_int;
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    // Every fork's bytes on disk: what `st_blocks` counts.
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    // Every requested attribute packed, valid or not, so each sits at a fixed offset.
    const FSOPT_PACK_INVAL_ATTRS: u64 = 0x0000_0008;
    const O_DIRECTORY: i32 = 0x0010_0000;
    const VDIR: u32 = 2;
    const EINTR: i32 = 4;

    // An entry, packed in the order sys/attr.h gives: its length; the attributes returned
    // (five groups); the error, which follows them; the name's reference (offset from itself,
    // length with the NUL); the type; the size on disk. Each 4-byte aligned.
    const RETURNED_FILE: usize = 4 + 12;
    const ERROR: usize = 24;
    const NAME: usize = 28;
    const TYPE: usize = 36;
    const SIZE: usize = 40;
    const FIXED: usize = 48;

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
    }

    pub fn list(dir: &Path, scratch: &mut Scratch, mut on_dir: impl FnMut(&OsStr)) -> u64 {
        let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(O_DIRECTORY).open(dir) else { return 0 };
        let mut attrs = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR | ATTR_CMN_OBJTYPE,
            volattr: 0,
            dirattr: 0,
            fileattr: ATTR_FILE_ALLOCSIZE,
            forkattr: 0,
        };
        let size = scratch.0.len() * 8;
        let mut bytes = 0;
        loop {
            // SAFETY: `attrs` is a filled attrlist and the buffer is `size` bytes, 8-aligned.
            let n = unsafe { getattrlistbulk(file.as_raw_fd(), &mut attrs, scratch.0.as_mut_ptr().cast(), size, FSOPT_PACK_INVAL_ATTRS) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(EINTR) {
                continue;
            }
            if n <= 0 {
                return bytes;
            }
            // SAFETY: the u64 buffer read as the bytes it holds.
            let buf: &[u8] = unsafe { std::slice::from_raw_parts(scratch.0.as_ptr().cast(), size) };
            let mut at = 0;
            for _ in 0..n {
                let Some(entry) = buf.get(at..).filter(|e| e.len() >= FIXED) else { return bytes };
                let len = u32_at(entry, 0) as usize;
                if len < FIXED || len > entry.len() {
                    return bytes;
                }
                let entry = &entry[..len];
                at += len;
                if u32_at(entry, ERROR) != 0 {
                    continue;
                }
                if u32_at(entry, TYPE) == VDIR {
                    let start = NAME as i64 + i64::from(u32_at(entry, NAME) as i32);
                    let name_len = u32_at(entry, NAME + 4) as usize;
                    let Some(name) = usize::try_from(start).ok().and_then(|s| entry.get(s..s + name_len)) else { continue };
                    // Without its NUL.
                    let name = name.strip_suffix(&[0]).unwrap_or(name);
                    if !name.is_empty() {
                        on_dir(OsStr::from_bytes(name));
                    }
                } else if u32_at(entry, RETURNED_FILE) & ATTR_FILE_ALLOCSIZE != 0 {
                    let size = i64::from_ne_bytes(entry[SIZE..SIZE + 8].try_into().unwrap());
                    bytes += size.max(0) as u64;
                }
            }
        }
    }
}

/// The walk's threads, on macOS, at utility QoS: the scheduler favours the efficiency cores
/// and gives way to what the person is doing, as Spotlight's indexing does.
fn lower_priority() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative: std::ffi::c_int) -> std::ffi::c_int;
        }
        const QOS_CLASS_UTILITY: u32 = 0x11;
        // SAFETY: it sets only the calling thread's class.
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_each_child_and_the_loose_files_on_many_threads() {
        let dir = std::env::temp_dir().join(format!("magnitor-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/deep/deeper")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        let write = |p: &str, n: usize| std::fs::write(dir.join(p), vec![1u8; n]).unwrap();
        write("a/one", 8192);
        write("a/deep/deeper/two", 16384);
        write("b/three", 4096);
        write("loose", 4096);
        let folders = measure(&[dir.clone(), dir.join("missing")], 4);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(folders[0].children, vec![("a".into(), 24576), ("b".into(), 4096), ("empty".into(), 0)]);
        assert_eq!(folders[0].loose, 4096);
        assert_eq!(folders[0].total, 32768);
        assert!(!folders[1].found);
    }

    /// The bulk read counts what `lstat` does, and names the same directories: a link its own
    /// size, a sparse file its blocks, the system's compressed files what they take.
    #[cfg(target_os = "macos")]
    #[test]
    fn bulk_matches_lstat() {
        let dir = std::env::temp_dir().join(format!("magnitor-bulk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("empty"), b"").unwrap();
        std::fs::write(dir.join("small"), b"hi").unwrap();
        std::fs::write(dir.join("block"), vec![7u8; 10_000]).unwrap();
        let sparse = std::fs::File::create(dir.join("sparse")).unwrap();
        sparse.set_len(50_000_000).unwrap();
        std::os::unix::fs::symlink("small", dir.join("link")).unwrap();
        std::os::unix::fs::symlink("sub", dir.join("dir-link")).unwrap();
        // Many entries: more than one buffer's worth.
        for i in 0..3000 {
            std::fs::create_dir(dir.join("sub").join(format!("d{i:05}-{}", "x".repeat(40)))).unwrap();
        }
        let roots = [dir.clone(), dir.join("sub"), "/usr/bin".into(), "/usr/lib".into(), "/System/Library/CoreServices".into(), "/nonexistent".into()];
        for root in &roots {
            let mut scratch = Scratch::new();
            let (mut a, mut b) = (Vec::new(), Vec::new());
            let bulk = bulk::list(root, &mut scratch, |n| a.push(n.to_owned()));
            let each = list_each(root, |n| b.push(n.to_owned()));
            a.sort();
            b.sort();
            assert_eq!(bulk, each, "bytes in {}", root.display());
            assert_eq!(a, b, "directories in {}", root.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CPU and wall time of a walk of real folders, `MAGNITOR_BENCH` (colon-separated):
    /// `MAGNITOR_BENCH=~/Library/Caches:… cargo test -p magnitor-data --release bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench() {
        let Some(roots) = std::env::var_os("MAGNITOR_BENCH") else { return };
        let roots: Vec<PathBuf> = std::env::split_paths(&roots).collect();
        let threads = std::env::var("MAGNITOR_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(4);
        let cpu = || {
            let mut usage = std::mem::MaybeUninit::<[i64; 18]>::zeroed();
            extern "C" {
                fn getrusage(who: i32, usage: *mut [i64; 18]) -> i32;
            }
            // RUSAGE_SELF; ru_utime and ru_stime are the first two timevals.
            unsafe { getrusage(0, usage.as_mut_ptr()) };
            let u = unsafe { usage.assume_init() };
            (u[0] as f64 + u[1] as f64 / 1e6, u[2] as f64 + u[3] as f64 / 1e6)
        };
        let (user0, sys0) = cpu();
        let start = std::time::Instant::now();
        let folders = measure(&roots, threads);
        let wall = start.elapsed().as_secs_f64();
        let (user1, sys1) = cpu();
        let total: u64 = folders.iter().map(|f| f.total).sum();
        println!(
            "threads {threads}: {total} bytes · wall {wall:.2}s · user {:.2}s · system {:.2}s · CPU {:.0}%",
            user1 - user0,
            sys1 - sys0,
            (user1 - user0 + sys1 - sys0) / wall * 100.0
        );
    }
}
