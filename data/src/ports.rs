//! app.contract's `ports`: the TCP ports this user's processes listen on (a dev server's
//! `localhost:3000`, Postgres's 5432), each with the process, the folder it runs in and the
//! command that started it; and `stopPort`, which ends the process that holds one.
//!
//! On macOS it is libproc, in this process, with no `lsof`: every process's descriptors
//! (`PROC_PIDLISTFDS`), and for each socket its state and local address
//! (`PROC_PIDFDSOCKETINFO`). The kernel describes only this user's processes, so the ports the
//! system's daemons hold (root's) aren't listed, and the view says so. A process's folder is its
//! working directory (`PROC_PIDVNODEPATHINFO`), its command its arguments (`KERN_PROCARGS2`).
//!
//! A server is named for what a person started, not its executable: `node …/node_modules/.bin/vite`
//! is "vite", `python3 manage.py runserver` is "manage.py runserver" (`server_name`). One is
//! "development" unless it is part of macOS or of an app that isn't a developer's tool
//! (`is_dev`), or listens on a port the system chose: AirPlay's 7000, an app's own helpers and
//! an editor's bridge go under Other Ports.
//!
//! A port a container runtime holds (Docker's `com.docker.backend`, OrbStack's helper) is named
//! for the container that publishes it, asked of the Engine (`docker`): "shop-db-1", its image
//! and compose project; Stop then stops that container, never the runtime, which would take
//! every container down with it.
//!
//! Under the agent (`--home`), a `.ports` file in that home holds the servers instead, one a
//! line, tab-separated: `pid port local|all seconds-running executable folder command…`, the
//! folder relative to that home or absolute. Stopping one there only takes it off the list: a
//! test's pid is no real process's.
use crate::docker;
use crate::shapes::{Ended, Ports, Server};
use crate::tasks::{app_bundle, app_name, format_duration};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One listening socket as read: who holds it, on which port, and whether it is reachable from
/// other machines (any address but loopback).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawListener {
    pub pid: i32,
    pub port: u16,
    pub exposed: bool,
}

/// What is known of a process that listens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawServer {
    pub pid: i32,
    pub port: u16,
    pub exposed: bool,
    /// The executable's path.
    pub path: String,
    /// Its working directory, absolute ("" when unknown).
    pub cwd: String,
    /// Its arguments, the first the command as typed.
    pub args: Vec<String>,
    /// Seconds it has run (`None` when unknown).
    pub running: Option<u64>,
}

#[derive(Default)]
pub struct PortWatch {
    /// The last reading, for `stopPort`'s message and a container's id.
    last: Vec<Server>,
    /// The Engine's containers, asked at most every `CONTAINERS_EVERY`.
    containers: Option<(std::time::Instant, Vec<docker::Container>)>,
    /// The agent's servers, read once from `.ports`; stopping one takes it off.
    fixture: Option<Vec<RawServer>>,
}

impl PortWatch {
    pub fn ports(&mut self, _tick: i64) -> Ports {
        let home = crate::home();
        let raw = match self.fixture_servers(&home) {
            Some(servers) => Ok(servers),
            None => read(),
        };
        match raw {
            Ok(raw) => {
                let containers = if raw.iter().any(|s| is_runtime(&s.path)) { self.containers(&home) } else { Vec::new() };
                let servers = describe(&raw, &home, &containers);
                self.last = servers.clone();
                Ports { servers, error: String::new() }
            }
            Err(error) => Ports { servers: Vec::new(), error },
        }
    }

    /// The containers, from the last ask while it is fresh: a tick every few seconds asks the
    /// Engine no oftener than this.
    fn containers(&mut self, home: &Path) -> Vec<docker::Container> {
        match &self.containers {
            Some((at, list)) if at.elapsed() < CONTAINERS_EVERY => list.clone(),
            _ => {
                let list = docker::containers(home);
                self.containers = Some((std::time::Instant::now(), list.clone()));
                list
            }
        }
    }

    /// Stop (SIGTERM) or force stop (SIGKILL) the process that holds `port`, refusing Magnitor
    /// and the system's first processes; a container's port stops its container.
    pub fn stop(&mut self, pid: i64, port: i64, force: bool) -> Ended {
        if let Some(s) = self.last.iter().find(|s| s.pid as i64 == pid && s.port as i64 == port && !s.container.is_empty()) {
            let (id, name) = (s.container.clone(), s.name.clone());
            self.containers = None;
            return match docker::stop(&crate::home(), &id, force) {
                Ok(()) => Ended { ok: true, message: format!("{} container {name}.", if force { "Killed" } else { "Stopped" }) },
                Err(e) => Ended { ok: false, message: format!("Couldn’t stop {name}: {e}") },
            };
        }
        let pid = pid as i32;
        let name = self.last.iter().find(|s| s.pid as i32 == pid).map_or_else(|| format!("Process {pid}"), |s| s.name.clone());
        let what = format!("{name} on port {port}");
        if pid <= 1 {
            return Ended { ok: false, message: format!("{name} is part of the system and can’t be stopped.") };
        }
        if pid == std::process::id() as i32 {
            return Ended { ok: false, message: "To quit Magnitor, use Quit Magnitor.".into() };
        }
        if let Some(servers) = &mut self.fixture {
            servers.retain(|s| s.pid != pid);
            return Ended { ok: true, message: if force { format!("Force stopped {what}.") } else { format!("Stopped {what}.") } };
        }
        match crate::sample::end(pid, force) {
            Ok(()) => Ended { ok: true, message: if force { format!("Force stopped {what}.") } else { format!("Stopped {what}.") } },
            // ESRCH
            Err(3) => Ended { ok: true, message: format!("{name} has already stopped.") },
            // EPERM
            Err(1) => Ended { ok: false, message: format!("You don’t have permission to stop {name}.") },
            Err(e) => Ended { ok: false, message: format!("Couldn’t stop {name} (error {e}).") },
        }
    }

    fn fixture_servers(&mut self, home: &Path) -> Option<Vec<RawServer>> {
        if self.fixture.is_none() {
            std::env::var_os("EXACT_AGENT_HOME")?;
            let text = std::fs::read_to_string(home.join(".ports")).ok()?;
            self.fixture = Some(parse_fixture(&text, home));
        }
        self.fixture.clone()
    }
}

/// `.ports`' lines (the module's comment).
pub fn parse_fixture(text: &str, home: &Path) -> Vec<RawServer> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(7, '\t').collect();
            let [pid, port, scope, secs, path, cwd, command] = f[..] else { return None };
            let cwd = if cwd.starts_with('/') { cwd.to_string() } else { home.join(cwd).to_string_lossy().into_owned() };
            Some(RawServer {
                pid: pid.parse().ok()?,
                port: port.parse().ok()?,
                exposed: scope == "all",
                path: path.into(),
                cwd,
                args: command.split(' ').map(str::to_owned).collect(),
                running: secs.parse().ok(),
            })
        })
        .collect()
}

/// How long the Engine's list of containers stands.
const CONTAINERS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// A container runtime's own process, which holds the ports its containers publish.
pub fn is_runtime(path: &str) -> bool {
    let exe = crate::sample::base_name(path);
    let app = app_bundle(path);
    ["com.docker.backend", "com.docker.vpnkit", "vpnkit-bridge", "docker-proxy", "OrbStack Helper", "limactl", "gvproxy"].contains(&exe)
        || (!app.is_empty() && ["Docker", "OrbStack", "Rancher Desktop", "Podman Desktop"].contains(&app_name(&app).as_str()))
}

/// The servers as the view lists them: development first, each section by port. A runtime's
/// port that a container publishes is named for the container.
pub fn describe(raw: &[RawServer], home: &Path, containers: &[docker::Container]) -> Vec<Server> {
    let home = home.to_string_lossy();
    let mut servers: Vec<Server> = raw
        .iter()
        .map(|s| {
            let exe = crate::sample::base_name(&s.path);
            let exe = if exe.is_empty() { s.args.first().map_or("", |a| crate::sample::base_name(a)) } else { exe };
            let app = app_bundle(&s.path);
            // An app's helper by its app's name (Docker's `com.docker.backend` is "Docker").
            let name = if app.is_empty() { server_name(exe, &s.args) } else { app_name(&app) };
            let folder = if s.cwd.is_empty() || s.cwd == "/" { String::new() } else { tilde(&s.cwd, &home) };
            let project = if folder.is_empty() || s.cwd == *home { String::new() } else { crate::sample::base_name(&s.cwd).to_owned() };
            // A port the system handed out (49152 and up) is a tool's own bridge (an editor's,
            // a debugger's), not a server a person started on a port they chose.
            let dev = is_dev(&s.path, &app) && s.port < EPHEMERAL;
            let container = if is_runtime(&s.path) { containers.iter().find(|c| c.ports.contains(&s.port)) } else { None };
            let (name, project) = match container {
                Some(c) => (c.name.clone(), c.project.clone()),
                None => (name, project),
            };
            Server {
                pid: s.pid as f64,
                port: s.port as f64,
                name,
                process: exe.to_owned(),
                command: tilde(&s.args.join(" "), &home),
                folder,
                project,
                exposed: s.exposed,
                url: if is_web(exe, s.port) { format!("http://localhost:{}", s.port) } else { String::new() },
                running: s.running.map_or(String::new(), format_duration),
                dev,
                path: s.path.clone(),
                dir: if s.cwd == "/" { String::new() } else { s.cwd.clone() },
                container: container.map_or(String::new(), |c| c.id.clone()),
                image: container.map_or(String::new(), |c| c.image.clone()),
            }
        })
        .collect();
    servers.sort_by(|a, b| b.dev.cmp(&a.dev).then(a.port.total_cmp(&b.port)).then(a.pid.total_cmp(&b.pid)));
    servers
}

/// `/Users/me/Projects/web` as `~/Projects/web`.
fn tilde(text: &str, home: &str) -> String {
    if home.len() <= 1 {
        text.to_owned()
    } else if text == home {
        "~".into()
    } else {
        text.replace(&format!("{home}/"), "~/")
    }
}

/// The runtimes whose first argument names what runs.
const RUNTIMES: &[&str] = &["node", "bun", "deno", "python", "python3", "ruby", "php", "java", "dotnet", "perl", "uv", "npx", "bunx", "pnpm", "yarn", "npm"];

/// What a person started: the tool a runtime runs and its subcommand (`vite`, `next dev`,
/// `http.server`), or the executable's own name.
pub fn server_name(exe: &str, args: &[String]) -> String {
    let runtime = exe.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !RUNTIMES.contains(&exe) && !RUNTIMES.contains(&runtime) {
        return exe.to_owned();
    }
    let mut rest = args.iter().skip(1).map(String::as_str).peekable();
    let mut script = None;
    while let Some(a) = rest.next() {
        if a == "-m" {
            script = rest.next().map(str::to_owned);
            break;
        }
        // An option's value follows it (`--require x`, `-r x`); `--inspect` stands alone.
        if a.starts_with('-') {
            if matches!(a, "-r" | "--require" | "--import" | "--loader" | "-e" | "--eval" | "-c") {
                rest.next();
            }
            continue;
        }
        script = Some(tool_name(a));
        break;
    }
    let Some(script) = script.filter(|s| !s.is_empty()) else { return exe.to_owned() };
    match rest.next() {
        Some(sub) if is_word(sub) => format!("{script} {sub}"),
        _ => script,
    }
}

/// A script's path as its tool's name. In `node_modules`, a package's `bin` script names the
/// command people type (`.bin/vite`, `vite/bin/vite.js`, `next/dist/bin/next` and
/// `@angular/cli/bin/ng` are `vite`, `vite`, `next` and `ng`); another script, its package. Else
/// the file without its extension (`manage.py` keeps it: Django's is the name people know).
fn tool_name(script: &str) -> String {
    if let Some((_, after)) = script.rsplit_once("node_modules/") {
        let parts: Vec<&str> = after.split('/').collect();
        if parts.len() >= 2 && (parts[0] == ".bin" || parts[parts.len() - 2] == "bin") {
            return strip_script(parts[parts.len() - 1]);
        }
        let package = if parts[0].starts_with('@') { parts.get(1).copied().unwrap_or(parts[0]) } else { parts[0] };
        return package.to_owned();
    }
    let base = crate::sample::base_name(script);
    if base.ends_with(".py") || base.ends_with(".rb") {
        return base.to_owned();
    }
    strip_script(base)
}

fn strip_script(name: &str) -> String {
    [".js", ".mjs", ".cjs", ".ts"].iter().find_map(|e| name.strip_suffix(e)).unwrap_or(name).to_owned()
}

/// A subcommand: a plain word (`dev`, `runserver`, `serve`), not a path, option or value.
fn is_word(a: &str) -> bool {
    !a.is_empty() && a.len() <= 20 && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':') && a.starts_with(|c: char| c.is_ascii_alphabetic())
}

/// Apps whose ports are a developer's servers: containers and databases.
const DEV_APPS: &[&str] = &["Docker", "OrbStack", "Postgres", "Rancher Desktop", "DBngin", "Herd", "Local", "MAMP", "Podman Desktop", "Redis"];

fn is_dev_app(app: &str) -> bool {
    DEV_APPS.contains(&app_name(app).as_str())
}

/// A developer's server: not part of macOS, and not inside an app unless that app is a
/// developer's tool.
pub fn is_dev(path: &str, app: &str) -> bool {
    let system = ["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/", "/Library/Apple/", "/usr/lib/systemd/", "/lib/systemd/"].iter().any(|p| path.starts_with(p));
    !system && (app.is_empty() || is_dev_app(app))
}

/// The first port of the range macOS hands out when a program asks for any.
const EPHEMERAL: u16 = 49152;

/// Executables and ports that speak something other than HTTP, which a browser can't open.
const NOT_WEB_NAMES: &[&str] = &["postgres", "postmaster", "mysqld", "mariadbd", "redis-server", "valkey-server", "mongod", "memcached", "sshd", "beam.smp", "rapportd", "ControlCenter", "sharingd", "adb"];
// 5037 is adb's server; 9229 Node's inspector, which DevTools opens, not a page.
const NOT_WEB_PORTS: &[u16] = &[22, 25, 53, 1433, 1521, 3306, 5037, 5432, 5672, 6379, 9092, 9229, 11211, 27017];

pub fn is_web(exe: &str, port: u16) -> bool {
    !NOT_WEB_NAMES.contains(&exe) && !NOT_WEB_PORTS.contains(&port)
}

/// Each process's listening sockets, read with libproc, as servers: one per process and port
/// (a server listening on IPv4 and IPv6 is one).
#[cfg(target_os = "macos")]
fn read() -> Result<Vec<RawServer>, String> {
    let listeners = mac::listeners();
    let mut by: BTreeMap<(i32, u16), RawServer> = BTreeMap::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for l in listeners {
        if let Some(s) = by.get_mut(&(l.pid, l.port)) {
            s.exposed |= l.exposed;
            continue;
        }
        let started = mac::started(l.pid);
        by.insert(
            (l.pid, l.port),
            RawServer {
                pid: l.pid,
                port: l.port,
                exposed: l.exposed,
                path: mac::pid_path(l.pid),
                cwd: mac::cwd(l.pid),
                args: mac::args(l.pid),
                running: started.filter(|&s| s > 0 && s <= now).map(|s| now - s),
            },
        );
    }
    Ok(by.into_values().collect())
}

/// Each listening socket in `/proc/net/tcp` and `tcp6` whose inode one of this user's processes
/// holds, as servers: one per process and port.
#[cfg(target_os = "linux")]
fn read() -> Result<Vec<RawServer>, String> {
    let listeners = linux::listeners()?;
    let mut by: BTreeMap<(i32, u16), RawServer> = BTreeMap::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for l in listeners {
        if let Some(s) = by.get_mut(&(l.pid, l.port)) {
            s.exposed |= l.exposed;
            continue;
        }
        let started = linux::started(l.pid);
        by.insert(
            (l.pid, l.port),
            RawServer {
                pid: l.pid,
                port: l.port,
                exposed: l.exposed,
                path: linux::pid_path(l.pid),
                cwd: linux::cwd(l.pid),
                args: linux::args(l.pid),
                running: started.filter(|&s| s > 0 && s <= now).map(|s| now - s),
            },
        );
    }
    Ok(by.into_values().collect())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read() -> Result<Vec<RawServer>, String> {
    Err("Ports are read on macOS and Linux.".into())
}

/// Each process's working directory, for the cleanup's "in use" (`devclean`): this user's
/// processes, by pid, with the executable's name.
#[cfg(target_os = "macos")]
pub fn working_dirs() -> Vec<(i32, String, PathBuf)> {
    let me = std::process::id() as i32;
    mac::pids()
        .into_iter()
        .filter(|&pid| pid > 1 && pid != me)
        .filter_map(|pid| {
            let cwd = mac::cwd(pid);
            (!cwd.is_empty() && cwd != "/").then(|| (pid, crate::sample::base_name(&mac::pid_path(pid)).to_owned(), PathBuf::from(cwd)))
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub fn working_dirs() -> Vec<(i32, String, PathBuf)> {
    let me = std::process::id() as i32;
    linux::pids()
        .into_iter()
        .filter(|&pid| pid > 1 && pid != me)
        .filter_map(|pid| {
            let cwd = linux::cwd(pid);
            (!cwd.is_empty() && cwd != "/").then(|| (pid, crate::sample::base_name(&linux::pid_path(pid)).to_owned(), PathBuf::from(cwd)))
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn working_dirs() -> Vec<(i32, String, PathBuf)> {
    Vec::new()
}

/// `/proc/net/tcp` (and `tcp6`), one socket a line: `sl local_address rem_address st … uid
/// timeout inode`, the address hex `ADDR:PORT`, the state `0A` for LISTEN. Each listening
/// socket's inode, its port, and whether it is reachable from other machines.
pub fn parse_proc_net_tcp(text: &str) -> Vec<(u64, u16, bool)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                return None;
            }
            let (addr, port) = f[1].split_once(':')?;
            let port = u16::from_str_radix(port, 16).ok()?;
            let inode = f[9].parse::<u64>().ok()?;
            // IPv4 is one 32-bit word in host (little-endian) order: 127.x.x.x ends in `7F`.
            // IPv6 is four such words: `::1` is three zero words and `01000000`;
            // `::ffff:127.x.x.x` is `0000…FFFF0000` and a word ending in `7F`.
            let loopback = match addr.len() {
                8 => addr.ends_with("7F"),
                32 => addr == "00000000000000000000000001000000" || (addr.starts_with("0000000000000000FFFF0000") && addr.ends_with("7F")),
                _ => false,
            };
            (port > 0 && inode > 0).then_some((inode, port, !loopback))
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::RawListener;
    use std::collections::HashMap;
    use std::fs;

    unsafe extern "C" {
        fn sysconf(name: i32) -> i64;
    }
    const SC_CLK_TCK: i32 = 2;

    pub fn pids() -> Vec<i32> {
        fs::read_dir("/proc")
            .map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok()).collect())
            .unwrap_or_default()
    }

    /// Every listening TCP socket whose holder this user can see: the kernel lists every
    /// socket, but only one's own processes' descriptors name their inodes.
    pub fn listeners() -> Result<Vec<RawListener>, String> {
        let mut sockets: HashMap<u64, (u16, bool)> = HashMap::new();
        let mut read_any = false;
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(text) = fs::read_to_string(table) {
                read_any = true;
                for (inode, port, exposed) in super::parse_proc_net_tcp(&text) {
                    sockets.insert(inode, (port, exposed));
                }
            }
        }
        if !read_any {
            return Err("Couldn’t read the ports.".into());
        }
        let mut out = Vec::new();
        if sockets.is_empty() {
            return Ok(out);
        }
        for pid in pids() {
            let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            for fd in fds.flatten() {
                let Ok(target) = fs::read_link(fd.path()) else { continue };
                let target = target.to_string_lossy();
                let Some(inode) = target.strip_prefix("socket:[").and_then(|t| t.strip_suffix(']')).and_then(|t| t.parse::<u64>().ok()) else { continue };
                if let Some(&(port, exposed)) = sockets.get(&inode) {
                    out.push(RawListener { pid, port, exposed });
                }
            }
        }
        Ok(out)
    }

    pub fn pid_path(pid: i32) -> String {
        fs::read_link(format!("/proc/{pid}/exe")).map_or_else(
            |_| args(pid).into_iter().next().unwrap_or_default(),
            |p| p.to_string_lossy().trim_end_matches(" (deleted)").to_owned(),
        )
    }

    pub fn cwd(pid: i32) -> String {
        fs::read_link(format!("/proc/{pid}/cwd")).map_or(String::new(), |p| p.to_string_lossy().into_owned())
    }

    pub fn args(pid: i32) -> Vec<String> {
        let raw = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        raw.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
    }

    /// Seconds since 1970 the process started at: boot time plus `stat`'s starttime.
    pub fn started(pid: i32) -> Option<u64> {
        let line = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, f) = crate::sample::parse_proc_stat(&line)?;
        let ticks = f.get(19)?.parse::<u64>().ok()?;
        let stat = fs::read_to_string("/proc/stat").ok()?;
        let boot = stat.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse::<u64>().ok())?;
        // SAFETY: sysconf reads a constant.
        let clock = unsafe { sysconf(SC_CLK_TCK) }.max(1) as u64;
        Some(boot + ticks / clock)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::RawListener;
    use std::ffi::{c_int, c_void};

    extern "C" {
        fn proc_listallpids(buffer: *mut c_void, size: c_int) -> c_int;
        fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
        fn proc_pidfdinfo(pid: c_int, fd: c_int, flavor: c_int, buffer: *mut c_void, size: c_int) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
        fn sysctl(name: *mut c_int, namelen: u32, old: *mut c_void, oldlen: *mut usize, new: *mut c_void, newlen: usize) -> c_int;
    }

    // <sys/proc_info.h>
    const PROC_PIDLISTFDS: c_int = 1;
    const PROC_PIDTBSDINFO: c_int = 3;
    const PROC_PIDVNODEPATHINFO: c_int = 9;
    const PROC_PIDFDSOCKETINFO: c_int = 3;
    const PROX_FDTYPE_SOCKET: u32 = 2;
    const SOCKINFO_TCP: i32 = 2;
    const TSI_S_LISTEN: i32 = 1;
    const INI_IPV4: u8 = 1;
    const INI_IPV6: u8 = 2;
    const AF_INET: i32 = 2;
    const AF_INET6: i32 = 30;
    // <sys/sysctl.h>
    const CTL_KERN: c_int = 1;
    const KERN_ARGMAX: c_int = 8;
    const KERN_PROCARGS2: c_int = 49;

    // Offsets into `struct socket_fdinfo` (792 bytes), as the SDK's headers lay it out:
    // `psi` (`struct socket_info`) at 24; in it `soi_family` at 160, `soi_kind` at 232 and the
    // protocol's union at 240, whose `tcp_sockinfo` starts with `in_sockinfo` (`insi_lport` at
    // 4, `insi_vflag` at 24, `insi_laddr` at 48) and holds `tcpsi_state` at 80. An IPv4
    // address is the last four bytes of `insi_laddr`'s 16.
    const SOCKET_FDINFO: usize = 792;
    const SOI_FAMILY: usize = 24 + 160;
    const SOI_KIND: usize = 24 + 232;
    const INSI: usize = 24 + 240;
    const INSI_LPORT: usize = INSI + 4;
    const INSI_VFLAG: usize = INSI + 24;
    const INSI_LADDR: usize = INSI + 48;
    const TCPSI_STATE: usize = INSI + 80;
    // `struct proc_vnodepathinfo` (2352 bytes): the working directory's path at 152.
    const VNODEPATHINFO: usize = 2352;
    const CDIR_PATH: usize = 152;
    const MAXPATHLEN: usize = 1024;
    // `struct proc_bsdinfo` (136 bytes): `pbi_start_tvsec` at 120.
    const BSDINFO: usize = 136;
    const START_TVSEC: usize = 120;

    fn i32_at(b: &[u8], at: usize) -> i32 {
        i32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn c_text(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    pub fn pids() -> Vec<i32> {
        // SAFETY: a null buffer asks for the count.
        let n = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        let mut pids = vec![0i32; n as usize + 64];
        // SAFETY: the buffer holds `pids.len()` pids.
        let got = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as c_int) };
        pids.truncate(got.max(0) as usize);
        pids.retain(|&p| p > 0);
        pids
    }

    /// The descriptors of `pid` that are sockets; none for another user's process.
    fn socket_fds(pid: i32) -> Vec<i32> {
        // SAFETY: a null buffer asks for the size.
        let size = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if size <= 0 {
            return Vec::new();
        }
        // `struct proc_fdinfo`: the fd, then its type, each 4 bytes.
        let mut buf = vec![0u32; size as usize / 4 + 32];
        // SAFETY: the buffer is `buf.len() * 4` bytes.
        let got = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), (buf.len() * 4) as c_int) };
        if got <= 0 {
            return Vec::new();
        }
        buf[..got as usize / 4].as_chunks::<2>().0.iter().filter(|e| e[1] == PROX_FDTYPE_SOCKET).map(|e| e[0] as i32).collect()
    }

    /// Every listening TCP socket of this user's processes.
    pub fn listeners() -> Vec<RawListener> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; SOCKET_FDINFO];
        for pid in pids() {
            for fd in socket_fds(pid) {
                // SAFETY: the buffer is a whole `struct socket_fdinfo`.
                let n = unsafe { proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, buf.as_mut_ptr().cast(), SOCKET_FDINFO as c_int) };
                if n as usize != SOCKET_FDINFO {
                    continue;
                }
                let family = i32_at(&buf, SOI_FAMILY);
                if (family != AF_INET && family != AF_INET6) || i32_at(&buf, SOI_KIND) != SOCKINFO_TCP || i32_at(&buf, TCPSI_STATE) != TSI_S_LISTEN {
                    continue;
                }
                // The port in network order, in the int's low 16 bits.
                let port = u16::from_be(i32_at(&buf, INSI_LPORT) as u16);
                let vflag = buf[INSI_VFLAG];
                let addr = &buf[INSI_LADDR..INSI_LADDR + 16];
                let exposed = if vflag & INI_IPV6 != 0 && vflag & INI_IPV4 == 0 {
                    // `::` is every address; `::1` is loopback.
                    let loopback = addr[..15].iter().all(|&b| b == 0) && addr[15] == 1;
                    !loopback
                } else {
                    addr[12] != 127
                };
                if port > 0 {
                    out.push(RawListener { pid, port, exposed });
                }
            }
        }
        out
    }

    pub fn pid_path(pid: i32) -> String {
        let mut buffer = vec![0u8; 4 * MAXPATHLEN];
        // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes.
        let n = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        if n <= 0 { String::new() } else { c_text(&buffer[..n as usize]) }
    }

    pub fn cwd(pid: i32) -> String {
        let mut buf = vec![0u8; VNODEPATHINFO];
        // SAFETY: the buffer is a whole `struct proc_vnodepathinfo`.
        let n = unsafe { proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, buf.as_mut_ptr().cast(), VNODEPATHINFO as c_int) };
        if n as usize != VNODEPATHINFO { String::new() } else { c_text(&buf[CDIR_PATH..CDIR_PATH + MAXPATHLEN]) }
    }

    /// Seconds since 1970 the process started at.
    pub fn started(pid: i32) -> Option<u64> {
        let mut buf = vec![0u8; BSDINFO];
        // SAFETY: the buffer is a whole `struct proc_bsdinfo`.
        let n = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, buf.as_mut_ptr().cast(), BSDINFO as c_int) };
        (n as usize == BSDINFO).then(|| u64::from_ne_bytes(buf[START_TVSEC..START_TVSEC + 8].try_into().unwrap()))
    }

    /// The arguments the process was started with: `KERN_PROCARGS2` is argc, the executable's
    /// path and its padding, then each argument, NUL-terminated.
    pub fn args(pid: i32) -> Vec<String> {
        let mut max: c_int = 0;
        let mut size = std::mem::size_of::<c_int>();
        let mut mib = [CTL_KERN, KERN_ARGMAX];
        // SAFETY: `max` is a c_int, `size` its size.
        if unsafe { sysctl(mib.as_mut_ptr(), 2, (&mut max as *mut c_int).cast(), &mut size, std::ptr::null_mut(), 0) } != 0 || max <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; max as usize];
        let mut size = buf.len();
        let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
        // SAFETY: the buffer is `size` bytes.
        if unsafe { sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
            return Vec::new();
        }
        parse_procargs(&buf[..size])
    }

    pub fn parse_procargs(buf: &[u8]) -> Vec<String> {
        let argc = i32::from_ne_bytes(buf[..4].try_into().unwrap()).max(0) as usize;
        let mut at = 4;
        // The executable's path, then the NULs that pad it.
        while at < buf.len() && buf[at] != 0 {
            at += 1;
        }
        while at < buf.len() && buf[at] == 0 {
            at += 1;
        }
        let mut args = Vec::with_capacity(argc);
        while args.len() < argc && at < buf.len() {
            let end = buf[at..].iter().position(|&b| b == 0).map_or(buf.len(), |e| at + e);
            args.push(String::from_utf8_lossy(&buf[at..end]).into_owned());
            at = end + 1;
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn names_what_a_runtime_runs() {
        assert_eq!(server_name("node", &args("node /Users/me/web/node_modules/.bin/vite --port 3000")), "vite");
        assert_eq!(server_name("node", &args("node /Users/me/web/node_modules/next/dist/bin/next dev")), "next dev");
        assert_eq!(server_name("node", &args("node --inspect -r ts-node/register server.ts")), "server");
        assert_eq!(server_name("node", &args("node /Users/me/app/node_modules/@angular/cli/bin/ng serve")), "ng serve");
        assert_eq!(server_name("node", &args("node /Users/me/app/node_modules/@nestjs/core/main.js")), "core");
        assert_eq!(server_name("python3.12", &args("python3 -m http.server 8000")), "http.server");
        assert_eq!(server_name("python3", &args("python3 manage.py runserver 0.0.0.0:8000")), "manage.py runserver");
        assert_eq!(server_name("ruby", &args("ruby bin/rails server")), "rails server");
        assert_eq!(server_name("postgres", &args("/opt/homebrew/bin/postgres -D /var")), "postgres");
        assert_eq!(server_name("node", &args("node")), "node");
    }

    #[test]
    fn sorts_development_first_and_hides_non_web_urls() {
        let home = Path::new("/Users/me");
        let raw = parse_fixture(
            "1\t7000\tall\t5\t/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter\t/\t/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter\n\
             2\t5432\tlocal\t60\t/opt/homebrew/bin/postgres\t/\tpostgres -D x\n\
             3\t3000\tlocal\t8040\t/usr/local/bin/node\tProjects/web\tnode /Users/me/Projects/web/node_modules/.bin/vite\n\
             bad line\n",
            home,
        );
        assert_eq!(raw.len(), 3);
        let s = describe(&raw, home, &[]);
        assert_eq!(s.iter().map(|s| s.port as u16).collect::<Vec<_>>(), vec![3000, 5432, 7000]);
        assert_eq!((s[0].name.as_str(), s[0].folder.as_str(), s[0].project.as_str()), ("vite", "~/Projects/web", "web"));
        assert_eq!(s[0].url, "http://localhost:3000");
        assert_eq!(s[0].running, "2 h 14 min");
        assert_eq!(s[0].command, "node ~/Projects/web/node_modules/.bin/vite");
        assert_eq!(s[1].url, "");
        assert!(s[1].dev);
        assert!(!s[2].dev && s[2].exposed);
        assert_eq!(s[2].name, "ControlCenter");
        // A port the system chose is a helper's, under the others.
        let raw = parse_fixture("5\t63334\tlocal\t5\t/usr/local/bin/node\t/\tnode bridge.js\n", home);
        assert!(!describe(&raw, home, &[])[0].dev);
    }

    #[test]
    fn docker_is_a_developers_app() {
        assert!(is_dev("/Applications/Docker.app/Contents/MacOS/com.docker.backend", "/Applications/Docker.app"));
        assert!(!is_dev("/Applications/Spotify.app/Contents/MacOS/Spotify", "/Applications/Spotify.app"));
        assert!(!is_dev("/usr/libexec/rapportd", ""));
        let raw = vec![RawServer { pid: 9, port: 8080, path: "/Applications/Docker.app/Contents/MacOS/com.docker.backend".into(), ..Default::default() }];
        assert_eq!(describe(&raw, Path::new("/Users/me"), &[])[0].name, "Docker");
    }

    #[test]
    fn a_containers_port_is_named_for_the_container() {
        let raw = vec![
            RawServer { pid: 9, port: 5433, exposed: true, path: "/Applications/Docker.app/Contents/MacOS/com.docker.backend".into(), ..Default::default() },
            RawServer { pid: 9, port: 8080, path: "/Applications/Docker.app/Contents/MacOS/com.docker.backend".into(), ..Default::default() },
            RawServer { pid: 12, port: 5433, path: "/usr/local/bin/node".into(), args: vec!["node".into()], ..Default::default() },
        ];
        let containers = vec![docker::Container { id: "abc".into(), name: "shop-db-1".into(), image: "postgres:16".into(), project: "shop".into(), ports: vec![5433] }];
        let servers = describe(&raw, Path::new("/Users/me"), &containers);
        let db = servers.iter().find(|s| s.pid == 9.0 && s.port == 5433.0).unwrap();
        assert_eq!((db.name.as_str(), db.project.as_str(), db.image.as_str(), db.container.as_str()), ("shop-db-1", "shop", "postgres:16", "abc"));
        // A port no container publishes stays Docker's; another process on the same port is its own.
        assert_eq!(servers.iter().find(|s| s.port == 8080.0).unwrap().name, "Docker");
        assert_eq!(servers.iter().find(|s| s.pid == 12.0).unwrap().container, "");
        assert!(is_runtime("/Applications/OrbStack.app/Contents/MacOS/OrbStack Helper"));
        assert!(!is_runtime("/usr/local/bin/node"));
    }

    /// A socket this test opens is read back with its port, from this process.
    #[test]
    fn reads_proc_net_tcp() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0 100 0 0 10 0
   1: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1 0 100 0 0 10 0
   2: 0100007F:0BB8 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 333 1 0 100 0 0 10 0
";
        assert_eq!(parse_proc_net_tcp(v4), vec![(111, 3000, false), (222, 5432, true)]);
        let v6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 444 1 0 100 0 0 10 0
   1: 00000000000000000000000000000000:1F91 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1 0 100 0 0 10 0
";
        assert_eq!(parse_proc_net_tcp(v6), vec![(444, 8080, false), (555, 8081, true)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_a_listening_socket_on_linux() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = std::process::id() as i32;
        let found = linux::listeners().unwrap();
        assert!(found.iter().any(|l| l.pid == me && l.port == port && !l.exposed), "{found:?}");
        assert!(linux::started(me).is_some());
        assert!(!linux::args(me).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_a_listening_socket() {
        let local = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let (lp, op) = (local.local_addr().unwrap().port(), open.local_addr().unwrap().port());
        let me = std::process::id() as i32;
        let found = mac::listeners();
        let mine: Vec<&RawListener> = found.iter().filter(|l| l.pid == me).collect();
        assert!(mine.iter().any(|l| l.port == lp && !l.exposed), "{mine:?}");
        assert!(mine.iter().any(|l| l.port == op && l.exposed), "{mine:?}");
        let servers = read().unwrap();
        let s = servers.iter().find(|s| s.pid == me && s.port == lp).unwrap();
        assert!(!s.cwd.is_empty() && !s.args.is_empty() && s.running.is_some(), "{s:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_procargs() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/local/bin/node\0\0\0\0node\0server.js\0PATH=/bin\0");
        assert_eq!(mac::parse_procargs(&buf), vec!["node", "server.js"]);
    }
}
