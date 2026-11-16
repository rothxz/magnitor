//! A parallel `du`: the bytes on disk under each folder, by its top-level entries, measured by
//! a thread per core sharing one queue of directories.
//!
//! A file counts its allocated blocks (`st_blocks` × 512), as `du` and Finder's "on disk" do,
//! so a sparse or compressed file counts what it takes. Symbolic links are not followed, and
//! a directory that cannot be read (another user's, or one macOS's privacy protection keeps
//! from this app) is left out, as `du` leaves it out after saying so.
use std::collections::HashMap;
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
    for (root, path) in roots.iter().enumerate() {
        let Ok(entries) = std::fs::read_dir(path) else { continue };
        folders[root].found = true;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                names[root].push(entry.file_name().to_string_lossy().into_owned());
                tasks.push(Task { dir: entry.path(), root, child: Some(names[root].len() - 1) });
            } else if let Ok(meta) = entry.metadata() {
                tally.loose[root] += on_disk(&meta);
            }
        }
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
    let mut tally = Tally { loose: vec![0; roots], ..Tally::default() };
    let mut found = Vec::new();
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
        let bytes = read(&task.dir, &task, &mut found);
        match task.child {
            Some(child) => *tally.children.entry((task.root, child)).or_default() += bytes,
            None => tally.loose[task.root] += bytes,
        }
    }
}

/// The files directly in `dir`; its directories go to `found` for any thread to read.
fn read(dir: &Path, task: &Task, found: &mut Vec<Task>) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            found.push(Task { dir: entry.path(), root: task.root, child: task.child });
        } else if let Ok(meta) = entry.metadata() {
            // `DirEntry::metadata` does not follow a symbolic link: a link counts its own size.
            bytes += on_disk(&meta);
        }
    }
    bytes
}

fn on_disk(meta: &std::fs::Metadata) -> u64 {
    meta.blocks() * 512
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_each_child_and_the_loose_files_on_many_threads() {
        let dir = std::env::temp_dir().join(format!("storage-walk-{}", std::process::id()));
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
}
