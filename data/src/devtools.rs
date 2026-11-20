//! app.contract's `devCommands` and `runDevCommand`: Developer Files' space that the Trash can't
//! take, freed by the tool that owns it, each run only when the person confirms.
//!
//! - **Go's module cache** (`~/go/pkg/mod`): its files are read-only, so moving it to the Trash
//!   fails part way. `go clean -modcache` clears it.
//! - **Docker**: everything lives in one disk image (`Docker.raw`), which must never be trashed.
//!   A prune (`docker system prune`: stopped containers, unused networks, dangling images, build
//!   cache) frees space inside it, asked of the Engine (`docker`).
//! - **Unavailable simulators**: ones whose runtime is gone. `xcrun simctl delete unavailable`.
//!
//! Each is listed only when there is something to free, with the command it stands for, and
//! whether Magnitor can run it here (`available`: the tool found, Docker running); otherwise the
//! person can copy the command.
//!
//! Under the agent (`--home`) nothing is run and the system is not asked: only Go's cache in that
//! home is measured, and running it answers as if it had.
use crate::shapes::{DevCommand, DevCommands, Ended};
use crate::{docker, format_bytes, walk};
use std::path::{Path, PathBuf};
use std::process::Command;

const GO_CACHE: &str = "go/pkg/mod";
const GO: &[&str] = &["/opt/homebrew/bin/go", "/usr/local/go/bin/go", "/usr/local/bin/go"];
const SIMULATORS: &str = "Library/Developer/CoreSimulator/Devices";
const XCRUN: &str = "/usr/bin/xcrun";

#[derive(Default)]
pub struct DevTools {
    last: Option<(i64, DevCommands)>,
}

fn agent() -> bool {
    std::env::var_os("EXACT_AGENT_HOME").is_some()
}

impl DevTools {
    pub fn commands(&mut self, generation: i64, threads: usize) -> DevCommands {
        if let Some((g, answer)) = &self.last {
            if *g == generation {
                return answer.clone();
            }
        }
        let home = crate::home();
        let mut commands = Vec::new();
        commands.extend(go(&home, threads));
        if !agent() {
            commands.extend(docker_prune(&home));
            commands.extend(simulators(&home, threads));
        }
        let answer = DevCommands { commands, error: String::new() };
        self.last = Some((generation, answer.clone()));
        answer
    }

    pub fn run(&mut self, id: &str) -> Ended {
        let home = crate::home();
        let size = self.last.as_ref().and_then(|(_, a)| a.commands.iter().find(|c| c.id == id)).map(|c| c.size.clone()).unwrap_or_default();
        self.last = None;
        let freed = |what: &str| if size.is_empty() { format!("{what}.") } else { format!("{what}, freeing about {size}.") };
        if agent() {
            return Ended { ok: true, message: freed(&done(id)) };
        }
        let result = match id {
            "go" => match go_binary() {
                Some(go) => run(Command::new(go).args(["clean", "-modcache"]).current_dir(&home)),
                None => Err("Go isn’t installed where Magnitor looks. Run go clean -modcache in Terminal.".into()),
            },
            "docker" => match docker::socket(&home) {
                // Docker's own count of what it freed is the better figure.
                Some(socket) => docker::prune(&socket).map(|bytes| Some(format!("{}, freeing {}.", done(id), format_bytes(bytes)))),
                None => Err("Docker isn’t running.".into()),
            },
            "simulators" => run(Command::new(XCRUN).args(["simctl", "delete", "unavailable"])),
            _ => Err("Magnitor doesn’t know that command.".into()),
        };
        match result {
            Ok(Some(message)) => Ended { ok: true, message },
            Ok(None) => Ended { ok: true, message: freed(&done(id)) },
            Err(e) => Ended { ok: false, message: e },
        }
    }
}

fn done(id: &str) -> String {
    match id {
        "go" => "Cleared Go’s module cache".into(),
        "docker" => "Docker pruned what no container uses".into(),
        "simulators" => "Deleted the unavailable simulators".into(),
        _ => "Done".into(),
    }
}

/// A command run to its end: nothing on success, its error output otherwise.
fn run(command: &mut Command) -> Result<Option<String>, String> {
    let out = command.output().map_err(|e| format!("Couldn’t start it: {e}"))?;
    if out.status.success() {
        return Ok(None);
    }
    let error = String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("").to_owned();
    Err(if error.is_empty() { format!("It stopped with status {}.", out.status.code().unwrap_or(-1)) } else { error })
}

fn go_binary() -> Option<&'static str> {
    GO.iter().copied().find(|p| Path::new(p).is_file())
}

fn item(id: &str, name: &str, detail: &str, command: &str, bytes: u64, available: bool, note: &str) -> DevCommand {
    DevCommand {
        id: id.into(),
        name: name.into(),
        detail: detail.into(),
        command: command.into(),
        bytes: bytes as f64,
        size: if bytes > 0 { format_bytes(bytes) } else { String::new() },
        available,
        note: note.into(),
    }
}

fn go(home: &Path, threads: usize) -> Option<DevCommand> {
    let dir = home.join(GO_CACHE);
    if !dir.is_dir() {
        return None;
    }
    let bytes = walk::measure(&[dir], threads).first().map_or(0, |f| f.total);
    if bytes == 0 {
        return None;
    }
    let available = agent() || go_binary().is_some();
    Some(item(
        "go",
        "Go module cache",
        "Downloaded again when a project builds. Its files are read-only, so it can’t go to the Trash.",
        "go clean -modcache",
        bytes,
        available,
        if available { "" } else { "Go isn’t installed where Magnitor looks. Copy the command and run it in Terminal." },
    ))
}

fn docker_prune(home: &Path) -> Option<DevCommand> {
    let socket = docker::socket(home)?;
    const DETAIL: &str = "Removes stopped containers, unused networks, untagged images and build cache. Docker keeps it all in one disk image: never move that to the Trash.";
    match docker::reclaimable(&socket) {
        Ok(0) => None,
        Ok(bytes) => Some(item("docker", "Docker", DETAIL, "docker system prune", bytes, true, "")),
        Err(_) => Some(item("docker", "Docker", DETAIL, "docker system prune", 0, false, "Docker isn’t running. Open it, then measure again to see what a prune frees.")),
    }
}

/// `simctl list devices unavailable -j`'s devices: each one's folder (its `dataPath`'s parent).
pub fn parse_unavailable(json: &str) -> Vec<PathBuf> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    v["devices"]
        .as_object()
        .into_iter()
        .flat_map(|runtimes| runtimes.values())
        .flat_map(|list| list.as_array().into_iter().flatten())
        .filter(|d| d["isAvailable"].as_bool() == Some(false))
        .filter_map(|d| d["dataPath"].as_str().and_then(|p| Path::new(p).parent()).map(Path::to_path_buf))
        .collect()
}

fn simulators(home: &Path, threads: usize) -> Option<DevCommand> {
    // Without Xcode's simulators, `xcrun` would offer to install the command line tools.
    if !home.join(SIMULATORS).is_dir() {
        return None;
    }
    let out = Command::new(XCRUN).args(["simctl", "list", "devices", "unavailable", "-j"]).output().ok()?;
    let dirs: Vec<PathBuf> = parse_unavailable(&String::from_utf8_lossy(&out.stdout)).into_iter().filter(|d| d.starts_with(home.join(SIMULATORS))).collect();
    if dirs.is_empty() {
        return None;
    }
    let bytes: u64 = walk::measure(&dirs, threads).iter().map(|f| f.total).sum();
    let n = dirs.len();
    Some(item(
        "simulators",
        &format!("{n} unavailable {}", if n == 1 { "simulator" } else { "simulators" }),
        "Simulators whose iOS version is no longer installed. Xcode can’t start them.",
        "xcrun simctl delete unavailable",
        bytes,
        true,
        "",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_unavailable_simulators_folders() {
        let json = r#"{"devices":{
          "com.apple.CoreSimulator.SimRuntime.iOS-16-4":[
            {"udid":"A","name":"iPhone 14","isAvailable":false,"dataPath":"/Users/me/Library/Developer/CoreSimulator/Devices/A/data"},
            {"udid":"B","name":"iPhone 15","isAvailable":true,"dataPath":"/Users/me/Library/Developer/CoreSimulator/Devices/B/data"}],
          "com.apple.CoreSimulator.SimRuntime.iOS-15-0":[]}}"#;
        assert_eq!(parse_unavailable(json), vec![PathBuf::from("/Users/me/Library/Developer/CoreSimulator/Devices/A")]);
        assert!(parse_unavailable("xcrun: error").is_empty());
    }

    #[test]
    fn describes_a_command_with_its_size() {
        let c = item("go", "Go module cache", "d", "go clean -modcache", 1_200_000_000, false, "n");
        assert_eq!((c.size.as_str(), c.available, c.note.as_str()), ("1.2 GB", false, "n"));
        assert_eq!(item("x", "", "", "", 0, true, "").size, "");
    }
}
