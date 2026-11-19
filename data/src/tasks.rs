//! Activity Monitor: app.contract's `tasks` (the machine's CPU and memory, and its processes
//! sorted and searched), `process` (one process, as Activity Monitor's inspector shows it) and
//! `endProcess` (Quit and Force Quit); and, as Raycast's Kill Process does, each app's processes
//! as one row (`group_apps`), opened as `appGroup` and quit or restarted together (`endApp`).
//!
//! A rate is two readings apart: each `tick` of the view's timer takes a reading (`sample`), and a
//! process's % CPU is the CPU time it used since the last one over the time between them, per
//! core, so a process busy on two cores shows 200%, as Activity Monitor's does. The first reading
//! (or one after the tab was away a while) takes a second a moment later, so there is always a
//! rate. A reading is kept for its tick: sorting, searching and opening a process ask no new one.
use crate::sample::{self, RawProc, RawSample, RawSystem};
use crate::shapes::{AppInfo, Ended, Proc, ProcInfo, Tasks};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime};

/// Rows the list shows; a search finds the rest.
pub const SHOWN: usize = 50;
/// CPU readings the graph keeps: two minutes at the view's three seconds.
const HISTORY: usize = 40;
/// A reading older than this is no base for a rate.
const STALE: Duration = Duration::from_secs(10);
/// The gap between the first reading and the second. It falls while the popover opens, the
/// app's own busiest moment: a short gap would show Magnitor's own % CPU many times its average.
const SETTLE: Duration = Duration::from_millis(1000);

/// A process with its rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub raw: RawProc,
    /// % CPU, per core.
    pub cpu: f64,
}

/// The system's CPU over a reading: user and system shares of every core's time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Load {
    pub user: f64,
    pub system: f64,
}

struct Reading {
    tick: i64,
    at: Instant,
    sample: RawSample,
    rows: Vec<Row>,
    load: Load,
}

#[derive(Default)]
pub struct TaskManager {
    last: Option<Reading>,
    history: VecDeque<Load>,
    error: String,
}

impl TaskManager {
    /// The reading for `tick`, taken unless it already was.
    fn read(&mut self, tick: i64) {
        if self.last.as_ref().is_some_and(|r| r.tick == tick) {
            return;
        }
        let base = match self.last.take() {
            Some(r) if r.at.elapsed() < STALE => Ok((r.at, r.sample)),
            _ => sample::take().map(|s| {
                std::thread::sleep(SETTLE);
                (Instant::now() - SETTLE, s)
            }),
        };
        let reading = base.and_then(|(then, before)| {
            let now = sample::take()?;
            Ok((then, before, now))
        });
        match reading {
            Ok((then, before, now)) => {
                let at = Instant::now();
                let rows = rates(&before.procs, &now.procs, (at - then).as_nanos() as f64);
                let load = load(&before.system, &now.system);
                if self.history.len() == HISTORY {
                    self.history.pop_front();
                }
                self.history.push_back(load);
                self.error.clear();
                self.last = Some(Reading { tick, at, sample: now, rows, load });
            }
            Err(e) => self.error = e,
        }
    }

    pub fn tasks(&mut self, tick: i64, sort: &str, query: &str, grouped: bool) -> Tasks {
        self.read(tick);
        let Some(r) = &self.last else {
            return Tasks { error: self.error.clone(), ..empty_tasks() };
        };
        let me = std::process::id() as i32;
        // Grouped, each app's processes are one row, which a search finds by the app's name or
        // any of its processes'.
        let (entries, groups) = if grouped { group_apps(&r.rows) } else { (Vec::new(), HashMap::new()) };
        let found = if grouped { find(&entries, &groups, &r.rows, query, me) } else { search(&r.rows, query, me) };
        let size = |row: &Row| groups.get(&row.raw.pid).map_or(1, |g| g.members.len());
        let (match_cpu, match_memory) = if query.trim().is_empty() { (String::new(), String::new()) } else { totals(&found) };
        let match_count = if query.trim().is_empty() { 0.0 } else { found.iter().map(|r| size(r)).sum::<usize>() as f64 };
        let more = found.len().saturating_sub(SHOWN);
        let shown: Vec<_> = exact_first(sorted(found, sort), query).into_iter().take(SHOWN).collect();
        // Each row names its app; the view asks for an app's icon once (`appIcon`). The ones not
        // yet drawn are drawn now, together, so those asks find them ready.
        let apps: Vec<String> = shown.iter().map(|row| groups.get(&row.raw.pid).map_or_else(|| app_bundle(&row.raw.path), |g| g.app.clone())).collect();
        crate::icons::prepare(&apps);
        let processes = shown
            .into_iter()
            .zip(apps)
            .map(|(row, app)| match groups.get(&row.raw.pid) {
                Some(g) => Proc {
                    detail: processes_text(g.members.len()),
                    mine: row.raw.detailed && !g.members.contains(&me),
                    group: true,
                    count: g.members.len() as f64,
                    app,
                    ..proc_row(row, me)
                },
                None => Proc { app, ..proc_row(row, me) },
            })
            .collect();
        let s = &r.sample.system;
        let (user_path, system_path) = graph(&self.history);
        Tasks {
            cpu_user: percent(r.load.user * 100.0),
            cpu_system: percent(r.load.system * 100.0),
            cpu_idle: percent((1.0 - r.load.user - r.load.system).max(0.0) * 100.0),
            user_path,
            system_path,
            memory_used: format_memory(s.memory_used),
            memory_total: format_memory(s.memory_total),
            memory_share: if s.memory_total > 0 { (s.memory_used as f64 / s.memory_total as f64).min(1.0) } else { 0.0 },
            pressure: pressure(s.pressure).into(),
            thermal: thermal(s.thermal).into(),
            temperature: temperature(s.cpu_temperature),
            cached: format_memory(s.memory_cached),
            compressed: format_memory(s.memory_compressed),
            swap: format_memory(s.swap_used),
            count: r.rows.len() as f64,
            processes,
            more: more as f64,
            match_count,
            match_cpu,
            match_memory,
            error: String::new(),
        }
    }

    pub fn process(&mut self, pid: i64, tick: i64) -> ProcInfo {
        self.read(tick);
        let rows = self.last.as_ref().map_or(&[][..], |r| &r.rows[..]);
        let Some(row) = rows.iter().find(|r| r.raw.pid as i64 == pid) else {
            return ProcInfo { pid: pid as f64, found: false, ..empty_info() };
        };
        let p = &row.raw;
        let parent = rows.iter().find(|r| r.raw.pid == p.ppid).map_or_else(|| format!("{}", p.ppid), |r| format!("{} ({})", r.raw.name, p.ppid));
        let running = if p.started > 0 {
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(String::new(), |now| format_duration(now.as_secs().saturating_sub(p.started)))
        } else {
            String::new()
        };
        ProcInfo {
            pid: pid as f64,
            found: true,
            name: p.name.clone(),
            path: p.path.clone(),
            user: p.user.clone(),
            parent,
            cpu: percent(row.cpu),
            cpu_time: format_cpu_time(p.cpu_ns),
            threads: p.threads.map_or("—".into(), |t| t.to_string()),
            memory: format_memory(p.memory),
            real_memory: format_memory(p.resident),
            running,
            mine: p.detailed,
            own: p.pid == std::process::id() as i32,
        }
    }

    /// The processes of the app at `app` (its bundle's path), as its row's inspector shows them.
    pub fn app_group(&mut self, app: &str, tick: i64) -> AppInfo {
        self.read(tick);
        let me = std::process::id() as i32;
        let rows = self.last.as_ref().map_or(&[][..], |r| &r.rows[..]);
        let members = members_of(rows, app);
        if members.is_empty() {
            return AppInfo { app: app.into(), name: app_name(app), found: false, ..empty_app() };
        }
        let main = main_process(app, &members).raw.pid;
        let (cpu, memory) = totals(&members);
        AppInfo {
            app: app.into(),
            found: true,
            name: app_name(app),
            cpu,
            memory,
            count: members.len() as f64,
            main: main as f64,
            mine: members.iter().all(|m| m.raw.detailed),
            own: members.iter().any(|m| m.raw.pid == me),
            processes: sorted(members, "cpu").into_iter().map(|r| Proc { app: app.into(), ..proc_row(r, me) }).collect(),
        }
    }

    /// The app at `app` ended, as `how` says: `quit` or `force` its main process (its helpers end
    /// with it), `quitAll` or `forceAll` every one of its processes, `restart` (its main process
    /// quit, then the app opened again once that has gone) or `forceRestart` (every process
    /// force quit, then opened again). Never this app, nor the system's first processes.
    pub fn end_app(&mut self, app: &str, how: &str) -> Ended {
        let me = std::process::id() as i32;
        let rows = self.last.as_ref().map_or(&[][..], |r| &r.rows[..]);
        let members = members_of(rows, app);
        let name = app_name(app);
        if members.is_empty() {
            return Ended { ok: true, message: format!("{name} has already quit.") };
        }
        if members.iter().any(|m| m.raw.pid == me) {
            return Ended { ok: false, message: "To quit Magnitor, use Quit Magnitor.".into() };
        }
        if members.iter().any(|m| m.raw.pid <= 1) {
            return Ended { ok: false, message: format!("{name} is part of macOS and can’t be quit.") };
        }
        let main = main_process(app, &members).raw.pid;
        let all = matches!(how, "quitAll" | "forceAll" | "forceRestart");
        let force = matches!(how, "force" | "forceAll" | "forceRestart");
        let targets: Vec<i32> = if all { members.iter().map(|m| m.raw.pid).collect() } else { vec![main] };
        let (mut refused, mut failed) = (0, None);
        for &pid in &targets {
            match sample::end(pid, force) {
                // ESRCH: gone already.
                Ok(()) | Err(3) => {}
                // EPERM: another user's.
                Err(1) => refused += 1,
                Err(e) => failed = Some(e),
            }
        }
        if refused == targets.len() {
            return Ended { ok: false, message: format!("You don’t have permission to quit {name}.") };
        }
        if let Some(e) = failed {
            return Ended { ok: false, message: format!("Couldn’t quit all of {name} (error {e}).") };
        }
        if matches!(how, "restart" | "forceRestart") {
            // Opened again once its main process has gone, off this worker's thread: an app
            // may take seconds to quit, and readings go on meanwhile.
            let app = app.to_owned();
            std::thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(15);
                while sample::alive(main) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = std::process::Command::new("/usr/bin/open").arg(&app).status();
            });
        }
        let n = targets.len();
        let mut message = match how {
            "force" => format!("Force quit {name}."),
            "quitAll" => format!("Asked all {n} of {name}’s processes to quit."),
            "forceAll" => format!("Force quit all {n} of {name}’s processes."),
            "restart" | "forceRestart" => format!("Restarting {name}…"),
            _ => format!("Asked {name} to quit."),
        };
        if refused > 0 {
            message.push_str(&format!(" {refused} of them belong to another user and kept running."));
        }
        Ended { ok: true, message }
    }

    /// Quit (SIGTERM) or Force Quit (SIGKILL), refusing this app and the system's first processes.
    pub fn end(&mut self, pid: i64, force: bool) -> Ended {
        let pid = pid as i32;
        let name = self.last.as_ref().and_then(|r| r.rows.iter().find(|row| row.raw.pid == pid)).map_or_else(|| format!("Process {pid}"), |r| r.raw.name.clone());
        if pid <= 1 {
            return Ended { ok: false, message: format!("{name} is part of macOS and can’t be quit.") };
        }
        if pid == std::process::id() as i32 {
            return Ended { ok: false, message: "To quit Magnitor, use Quit Magnitor.".into() };
        }
        match sample::end(pid, force) {
            Ok(()) => Ended { ok: true, message: if force { format!("Force quit {name}.") } else { format!("Asked {name} to quit.") } },
            // ESRCH
            Err(3) => Ended { ok: true, message: format!("{name} has already quit.") },
            // EPERM
            Err(1) => Ended { ok: false, message: format!("You don’t have permission to quit {name}.") },
            Err(e) => Ended { ok: false, message: format!("Couldn’t quit {name} (error {e}).") },
        }
    }
}

fn empty_tasks() -> Tasks {
    Tasks {
        cpu_user: String::new(),
        cpu_system: String::new(),
        cpu_idle: String::new(),
        user_path: String::new(),
        system_path: String::new(),
        memory_used: String::new(),
        memory_total: String::new(),
        memory_share: 0.0,
        pressure: String::new(),
        thermal: String::new(),
        temperature: String::new(),
        cached: String::new(),
        compressed: String::new(),
        swap: String::new(),
        count: 0.0,
        processes: Vec::new(),
        more: 0.0,
        match_count: 0.0,
        match_cpu: String::new(),
        match_memory: String::new(),
        error: String::new(),
    }
}

fn empty_app() -> AppInfo {
    AppInfo {
        app: String::new(),
        found: false,
        name: String::new(),
        cpu: String::new(),
        memory: String::new(),
        count: 0.0,
        main: 0.0,
        mine: false,
        own: false,
        processes: Vec::new(),
    }
}

fn empty_info() -> ProcInfo {
    ProcInfo {
        pid: 0.0,
        found: false,
        name: String::new(),
        path: String::new(),
        user: String::new(),
        parent: String::new(),
        cpu: String::new(),
        cpu_time: String::new(),
        threads: String::new(),
        memory: String::new(),
        real_memory: String::new(),
        running: String::new(),
        mine: false,
        own: false,
    }
}

fn proc_row(row: &Row, me: i32) -> Proc {
    let p = &row.raw;
    Proc {
        pid: p.pid as f64,
        name: p.name.clone(),
        detail: if p.detailed { format!("{}", p.pid) } else { format!("{} · {}", p.pid, p.user) },
        cpu: percent(row.cpu),
        memory: format_memory(p.memory),
        mine: p.detailed && p.pid != me,
        app: String::new(),
        group: false,
        count: 1.0,
    }
}

fn processes_text(n: usize) -> String {
    format!("{n} {}", if n == 1 { "process" } else { "processes" })
}

/// An app's processes, made one row: under its main process's pid, named by the app, its % CPU
/// and memory its processes' added up. `members` holds each app's processes by row pid.
pub struct AppGroup {
    pub app: String,
    pub members: Vec<i32>,
}

/// The rows with each app that runs two or more processes as one (Raycast's app grouping): a
/// helper lies in its app's bundle (`app_bundle`), so VS Code's main process and its twenty
/// helpers are one row. A process outside an app, or an app's only one, stays as it is.
pub fn group_apps(rows: &[Row]) -> (Vec<Row>, HashMap<i32, AppGroup>) {
    let mut by_app: HashMap<String, Vec<&Row>> = HashMap::new();
    for r in rows {
        let app = app_bundle(&r.raw.path);
        if !app.is_empty() {
            by_app.entry(app).or_default().push(r);
        }
    }
    let mut entries = Vec::new();
    let mut groups = HashMap::new();
    for r in rows {
        let app = app_bundle(&r.raw.path);
        match by_app.get(&app) {
            Some(members) if members.len() >= 2 => {
                let main = main_process(&app, members);
                if r.raw.pid != main.raw.pid {
                    continue;
                }
                entries.push(app_row(&app, main, members));
                groups.insert(main.raw.pid, AppGroup { app, members: members.iter().map(|m| m.raw.pid).collect() });
            }
            _ => entries.push(r.clone()),
        }
    }
    (entries, groups)
}

/// The processes running from the app at `app`.
fn members_of<'a>(rows: &'a [Row], app: &str) -> Vec<&'a Row> {
    if app.is_empty() {
        return Vec::new();
    }
    rows.iter().filter(|r| app_bundle(&r.raw.path) == app).collect()
}

/// The app's main process: one whose parent is outside the app, its bundle's own executable
/// first (`Contents/MacOS`), the oldest pid then.
pub fn main_process<'a>(app: &str, members: &[&'a Row]) -> &'a Row {
    let pids: HashSet<i32> = members.iter().map(|m| m.raw.pid).collect();
    let executable = format!("{app}/Contents/MacOS/");
    members
        .iter()
        .filter(|m| !pids.contains(&m.raw.ppid))
        .min_by_key(|m| (!m.raw.path.starts_with(&executable), m.raw.pid))
        .or_else(|| members.iter().min_by_key(|m| m.raw.pid))
        .copied()
        .expect("an app's row has a process")
}

/// An app's name from its bundle's path: `/Applications/Visual Studio Code.app` is "Visual
/// Studio Code".
pub fn app_name(app: &str) -> String {
    sample::base_name(app).trim_end_matches(".app").to_owned()
}

fn app_row(app: &str, main: &Row, members: &[&Row]) -> Row {
    Row {
        raw: RawProc {
            name: app_name(app),
            path: app.to_owned(),
            cpu_ns: members.iter().map(|m| m.raw.cpu_ns).sum(),
            memory: members.iter().map(|m| m.raw.memory).sum(),
            resident: members.iter().map(|m| m.raw.resident).sum(),
            threads: None,
            detailed: members.iter().all(|m| m.raw.detailed),
            ..main.raw.clone()
        },
        cpu: members.iter().map(|m| m.cpu).sum(),
    }
}

/// Grouped rows that `query` finds: by the row's own name or pid, or, for an app's row, any of
/// its processes'. With no query, every row but `me`'s (`search`).
fn find<'a>(entries: &'a [Row], groups: &HashMap<i32, AppGroup>, rows: &[Row], query: &str, me: i32) -> Vec<&'a Row> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return entries.iter().filter(|e| e.raw.pid != me).collect();
    }
    let by_pid: HashMap<i32, &Row> = rows.iter().map(|r| (r.raw.pid, r)).collect();
    entries
        .iter()
        .filter(|e| matches(e, &q) || groups.get(&e.raw.pid).is_some_and(|g| g.members.iter().any(|m| by_pid.get(m).is_some_and(|r| matches(r, &q)))))
        .collect()
}

fn matches(r: &Row, q: &str) -> bool {
    r.raw.name.to_lowercase().contains(q) || r.raw.pid.to_string() == q
}

/// The outermost app bundle a path lies in (a helper's is its app's), or "" for none. A bundle
/// is a `.app` with its `Contents`: a folder that only ends in `.app` (a build's
/// `…/com.magnitor.app/macos/Magnitor.app`) is no app of its own.
pub fn app_bundle(path: &str) -> String {
    path.find(".app/Contents/").map_or(String::new(), |i| path[..i + 4].to_owned())
}

/// Each process in `now` with its % CPU since `before`, `elapsed_ns` earlier. A process new
/// since then, or a pid reused by another, starts at 0.
pub fn rates(before: &[RawProc], now: &[RawProc], elapsed_ns: f64) -> Vec<Row> {
    let then: HashMap<i32, &RawProc> = before.iter().map(|p| (p.pid, p)).collect();
    now.iter()
        .map(|p| {
            let cpu = match then.get(&p.pid) {
                Some(b) if b.started == p.started && p.cpu_ns >= b.cpu_ns && elapsed_ns > 0.0 => (p.cpu_ns - b.cpu_ns) as f64 / elapsed_ns * 100.0,
                _ => 0.0,
            };
            Row { raw: p.clone(), cpu }
        })
        .collect()
}

/// The machine's user and system shares between two readings of its ticks (nice counts as user).
pub fn load(before: &RawSystem, now: &RawSystem) -> Load {
    let d: Vec<f64> = (0..4).map(|i| now.ticks[i].saturating_sub(before.ticks[i]) as f64).collect();
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return Load::default();
    }
    Load { user: (d[0] + d[3]) / total, system: d[1] / total }
}

/// The rows whose name holds `query` (any case), or whose pid it is. With no query, every row
/// but `me`'s: Magnitor leaves itself out of the list, where reading the list it draws would
/// rank it among the busiest, and shows itself to a search that names it.
pub fn search<'a>(rows: &'a [Row], query: &str, me: i32) -> Vec<&'a Row> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.iter().filter(|r| r.raw.pid != me).collect();
    }
    rows.iter().filter(|r| matches(r, &q)).collect()
}

/// The % CPU and memory of `rows` added up, as the list shows each.
pub fn totals(rows: &[&Row]) -> (String, String) {
    let cpu: f64 = rows.iter().map(|r| r.cpu).sum();
    let memory: u64 = rows.iter().map(|r| r.raw.memory).sum();
    (percent(cpu), format_memory(memory))
}

/// Largest first by `"cpu"` or `"memory"`; by name for `"name"`. Ties go by name, then pid.
///
/// The figures are compared as the list shows them (`percent`, `format_memory`), not as read:
/// most processes idle at a "0.0" whose raw value differs at every reading, and ordering those
/// by noise reshuffled the list each tick, so the view moved, made and dropped dozens of rows
/// every three seconds. Compared as shown, a row moves only when what it shows changes.
pub fn sorted<'a>(mut rows: Vec<&'a Row>, sort: &str) -> Vec<&'a Row> {
    let by_name = |a: &Row, b: &Row| a.raw.name.to_lowercase().cmp(&b.raw.name.to_lowercase()).then(a.raw.pid.cmp(&b.raw.pid));
    match sort {
        "memory" => rows.sort_by(|a, b| shown_memory(b.raw.memory).cmp(&shown_memory(a.raw.memory)).then_with(|| by_name(a, b))),
        "name" => rows.sort_by(|a, b| by_name(a, b)),
        _ => rows.sort_by(|a, b| shown_cpu(b.cpu).cmp(&shown_cpu(a.cpu)).then_with(|| by_name(a, b))),
    }
    rows
}

/// % CPU in the tenths `percent` shows.
fn shown_cpu(value: f64) -> i64 {
    (value.max(0.0) * 10.0).round() as i64
}

/// Memory rounded to the unit `format_memory` shows it in (a KB, a tenth of a MB, a hundredth
/// of a GB), in bytes, so figures in different units still compare.
fn shown_memory(bytes: u64) -> u64 {
    const K: u64 = 1024;
    let unit = if bytes < K * K { K } else if bytes < K * K * K { K * K / 10 } else { K * K * K / 100 };
    (bytes + unit / 2) / unit * unit
}

/// A process the query names exactly leads, whatever the sort: typing "Finder" finds Finder
/// before FinderSyncExtension.
pub fn exact_first<'a>(rows: Vec<&'a Row>, query: &str) -> Vec<&'a Row> {
    let q = query.trim().to_lowercase();
    let (exact, rest): (Vec<&Row>, Vec<&Row>) = rows.into_iter().partition(|r| !q.is_empty() && r.raw.name.to_lowercase() == q);
    exact.into_iter().chain(rest).collect()
}

/// The CPU graph in a 140×44 box, as Activity Monitor draws its load: system in red at the
/// bottom, user in blue over it, the newest reading at the right edge.
pub const GRAPH_W: f64 = 140.0;
pub const GRAPH_H: f64 = 44.0;

pub fn graph(history: &VecDeque<Load>) -> (String, String) {
    if history.is_empty() {
        return (String::new(), String::new());
    }
    let step = GRAPH_W / (HISTORY - 1) as f64;
    let first = HISTORY - history.len();
    let x = |i: usize| (first + i) as f64 * step;
    let y = |share: f64| GRAPH_H * (1.0 - share.clamp(0.0, 1.0));
    let point = |x: f64, y: f64| format!("{x:.1} {y:.1}");
    // One reading is a sliver, not a point.
    let (x0, xn) = if history.len() == 1 { (GRAPH_W - step, GRAPH_W) } else { (x(0), x(history.len() - 1)) };
    let xs: Vec<f64> = if history.len() == 1 { vec![x0, xn] } else { (0..history.len()).map(x).collect() };
    let loads: Vec<Load> = if history.len() == 1 { vec![history[0]; 2] } else { history.iter().copied().collect() };

    let system_line: Vec<String> = xs.iter().zip(&loads).map(|(&x, l)| point(x, y(l.system))).collect();
    let top_line: Vec<String> = xs.iter().zip(&loads).map(|(&x, l)| point(x, y(l.system + l.user))).collect();
    let system = format!("M {} L {} L {} Z", point(x0, GRAPH_H), system_line.join(" L "), point(xn, GRAPH_H));
    let back: Vec<String> = system_line.iter().rev().cloned().collect();
    let user = format!("M {} L {} Z", top_line.join(" L "), back.join(" L "));
    (user, system)
}

pub fn pressure(level: i32) -> &'static str {
    match level {
        4 => "Critical",
        2 => "Warning",
        _ => "Normal",
    }
}

/// The thermal pressure in `ProcessInfo.thermalState`'s words (nominal shown as "Normal", as
/// memory's is): how hard macOS is working to cool the Mac, not a temperature. "" when unread.
pub fn thermal(level: Option<u64>) -> &'static str {
    match level {
        None => "",
        Some(0) => "Normal",
        Some(1) => "Fair",
        Some(2) => "Serious",
        Some(_) => "Critical",
    }
}

/// The CPU's temperature in whole degrees, "47 °C"; "" when unread.
pub fn temperature(celsius: Option<f64>) -> String {
    celsius.map_or(String::new(), |t| format!("{} °C", t.round()))
}

/// % CPU as Activity Monitor shows it: one decimal.
pub fn percent(value: f64) -> String {
    format!("{:.1}", value.max(0.0))
}

/// Memory in Activity Monitor's binary units: 812 KB, 245.1 MB, 1.25 GB.
pub fn format_memory(bytes: u64) -> String {
    let b = bytes as f64;
    const K: f64 = 1024.0;
    if b < K * K {
        format!("{} KB", (b / K).round())
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.2} GB", b / (K * K * K))
    }
}

/// CPU time as Activity Monitor's column: 4.21, 12:03.50, 3:02:11.08.
pub fn format_cpu_time(ns: u64) -> String {
    let hundredths = ns / 10_000_000;
    let (h, m, s, c) = (hundredths / 360_000, hundredths / 6_000 % 60, hundredths / 100 % 60, hundredths % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{c:02}")
    } else if m > 0 {
        format!("{m}:{s:02}.{c:02}")
    } else {
        format!("{s}.{c:02}")
    }
}

/// How long something has run: 45 sec, 12 min, 3 h 5 min, 2 days 4 h.
pub fn format_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d} {} {h} h", if d == 1 { "day" } else { "days" })
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{secs} sec")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_has_its_apps_bundle() {
        assert_eq!(app_bundle("/Applications/Google Chrome.app/Contents/Frameworks/G.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"), "/Applications/Google Chrome.app");
        assert_eq!(app_bundle("/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder"), "/System/Library/CoreServices/Finder.app");
        assert_eq!(app_bundle("/usr/libexec/WindowServer"), "");
        assert_eq!(app_bundle("launchd"), "");
        assert_eq!(app_bundle("/Users/me/target/clients/x/com.magnitor.app/macos/Magnitor.app/Contents/MacOS/Magnitor"), "/Users/me/target/clients/x/com.magnitor.app/macos/Magnitor.app");
    }

    fn proc(pid: i32, name: &str, cpu_ns: u64, memory: u64) -> RawProc {
        RawProc { pid, name: name.into(), cpu_ns, memory, started: 100, detailed: true, ..RawProc::default() }
    }

    #[test]
    fn a_rate_is_cpu_time_over_wall_time_per_core() {
        let before = [proc(10, "a", 1_000_000_000, 0), proc(11, "b", 5, 0), proc(12, "gone", 0, 0)];
        let mut reused = proc(11, "b2", 9, 0);
        reused.started = 200;
        let now = [proc(10, "a", 3_000_000_000, 0), reused, proc(13, "new", 7_000_000_000, 0)];
        let rows = rates(&before, &now, 1e9);
        // Two seconds of CPU in one: two cores busy.
        assert_eq!(rows[0].cpu, 200.0);
        assert_eq!(rows[1].cpu, 0.0, "a reused pid starts over");
        assert_eq!(rows[2].cpu, 0.0, "a new process starts at 0");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn load_shares_every_cores_ticks() {
        let before = RawSystem { ticks: [100, 50, 800, 0], ..RawSystem::default() };
        let now = RawSystem { ticks: [130, 60, 850, 10], ..RawSystem::default() };
        let l = load(&before, &now);
        assert_eq!(l.user, 0.4);
        assert_eq!(l.system, 0.1);
        assert_eq!(load(&now, &now), Load::default());
    }

    #[test]
    fn searches_by_name_or_pid_and_sorts() {
        const MB: u64 = 1 << 20;
        let rows: Vec<Row> = [(1, "launchd", 1.0, 10 * MB), (42, "Safari", 30.0, 500 * MB), (7, "safari helper", 30.0, 900 * MB), (420, "Mail", 0.5, 100 * MB)]
            .into_iter()
            .map(|(pid, name, cpu, mem)| Row { raw: proc(pid, name, 0, mem), cpu })
            .collect();
        let names = |rs: Vec<&Row>| rs.iter().map(|r| r.raw.name.clone()).collect::<Vec<_>>();
        // The same % CPU shown: by name, whatever their memory.
        assert_eq!(names(sorted(search(&rows, " SAFARI ", -1), "cpu")), ["Safari", "safari helper"]);
        assert_eq!(names(search(&rows, "42", -1)), ["Safari"]);
        assert_eq!(names(sorted(search(&rows, "", -1), "memory")), ["safari helper", "Safari", "Mail", "launchd"]);
        assert_eq!(names(sorted(search(&rows, "", -1), "name")), ["launchd", "Mail", "Safari", "safari helper"]);
        assert_eq!(names(exact_first(sorted(search(&rows, "safari", -1), "cpu"), "safari")), ["Safari", "safari helper"]);
    }

    #[test]
    fn adds_up_every_match() {
        const MB: u64 = 1 << 20;
        let rows: Vec<Row> = [(1, "Code", 12.5, 300 * MB), (2, "Code Helper (Renderer)", 30.0, 500 * MB), (3, "Safari", 5.0, 100 * MB)]
            .into_iter()
            .map(|(pid, name, cpu, mem)| Row { raw: proc(pid, name, 0, mem), cpu })
            .collect();
        assert_eq!(totals(&search(&rows, "code", -1)), ("42.5".to_string(), "800.0 MB".to_string()));
        assert_eq!(totals(&[]), ("0.0".to_string(), "0 KB".to_string()));
    }

    fn in_app(pid: i32, ppid: i32, name: &str, path: &str, cpu: f64, memory: u64) -> Row {
        let mut raw = proc(pid, name, 0, memory);
        raw.ppid = ppid;
        raw.path = path.into();
        Row { raw, cpu }
    }

    fn code_and_others() -> Vec<Row> {
        const MB: u64 = 1 << 20;
        let code = "/Applications/Visual Studio Code.app";
        vec![
            in_app(1, 0, "launchd", "/sbin/launchd", 0.5, 10 * MB),
            in_app(11, 10, "Code Helper (Renderer)", &format!("{code}/Contents/Frameworks/Code Helper (Renderer).app/Contents/MacOS/Code Helper (Renderer)"), 20.0, 500 * MB),
            in_app(10, 1, "Electron", &format!("{code}/Contents/MacOS/Electron"), 2.5, 300 * MB),
            in_app(12, 10, "Code Helper", &format!("{code}/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper"), 1.0, 100 * MB),
            in_app(20, 1, "Safari", "/Applications/Safari.app/Contents/MacOS/Safari", 3.0, 200 * MB),
        ]
    }

    #[test]
    fn groups_an_apps_processes_under_its_main_one() {
        let rows = code_and_others();
        let (entries, groups) = group_apps(&rows);
        let names: Vec<_> = entries.iter().map(|e| (e.raw.pid, e.raw.name.clone())).collect();
        assert_eq!(names, [(1, "launchd".to_string()), (10, "Visual Studio Code".to_string()), (20, "Safari".to_string())]);
        let code = &entries[1];
        assert_eq!(code.cpu, 23.5);
        assert_eq!(code.raw.memory, 900 << 20);
        assert_eq!(code.raw.path, "/Applications/Visual Studio Code.app");
        let g = &groups[&10];
        assert_eq!(g.app, "/Applications/Visual Studio Code.app");
        assert_eq!(g.members.len(), 3);
        // An app with one process is a row of its own.
        assert!(!groups.contains_key(&20));
        assert_eq!(app_name("/Applications/Visual Studio Code.app"), "Visual Studio Code");
    }

    #[test]
    fn finds_an_app_by_its_name_or_any_of_its_processes() {
        let rows = code_and_others();
        let (entries, groups) = group_apps(&rows);
        let names = |rs: Vec<&Row>| rs.iter().map(|r| r.raw.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(find(&entries, &groups, &rows, "code", -1)), ["Visual Studio Code"]);
        assert_eq!(names(find(&entries, &groups, &rows, "renderer", -1)), ["Visual Studio Code"]);
        assert_eq!(names(find(&entries, &groups, &rows, "12", -1)), ["Visual Studio Code"]);
        assert_eq!(names(find(&entries, &groups, &rows, "", 20)), ["launchd", "Visual Studio Code"]);
        // Added up, a search's total counts the app's every process.
        assert_eq!(totals(&find(&entries, &groups, &rows, "code", -1)), ("23.5".to_string(), "900.0 MB".to_string()));
    }

    #[test]
    fn the_main_process_is_the_one_its_app_did_not_start() {
        let rows = code_and_others();
        let members: Vec<&Row> = rows.iter().filter(|r| app_bundle(&r.raw.path) == "/Applications/Visual Studio Code.app").collect();
        assert_eq!(main_process("/Applications/Visual Studio Code.app", &members).raw.pid, 10);
    }

    #[test]
    fn leaves_itself_out_until_a_search_names_it() {
        let rows: Vec<Row> = [(1, "launchd"), (500, "Magnitor"), (42, "Safari")].into_iter().map(|(pid, name)| Row { raw: proc(pid, name, 0, 0), cpu: 0.0 }).collect();
        let names = |rs: Vec<&Row>| rs.iter().map(|r| r.raw.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(search(&rows, "", 500)), ["launchd", "Safari"]);
        assert_eq!(names(search(&rows, "  ", 500)), ["launchd", "Safari"]);
        assert_eq!(names(search(&rows, "magni", 500)), ["Magnitor"]);
        assert_eq!(names(search(&rows, "500", 500)), ["Magnitor"]);
    }

    #[test]
    fn a_difference_the_list_does_not_show_moves_no_row() {
        let row = |pid, name: &str, cpu, memory| Row { raw: proc(pid, name, 0, memory), cpu };
        let names = |rs: Vec<&Row>| rs.iter().map(|r| r.raw.name.clone()).collect::<Vec<_>>();
        // Both show 0.0 % and 12.3 MB: one reading's noise must not put "b" above "a".
        let before = [row(1, "a", 0.01, 12_900_000), row(2, "b", 0.04, 12_910_000)];
        let after = [row(1, "a", 0.03, 12_905_000), row(2, "b", 0.0, 12_895_000)];
        for rows in [&before, &after] {
            assert_eq!(names(sorted(rows.iter().collect(), "cpu")), ["a", "b"]);
            assert_eq!(names(sorted(rows.iter().collect(), "memory")), ["a", "b"]);
        }
        // A difference it shows still orders them.
        let busy = [row(1, "a", 0.0, 12_900_000), row(2, "b", 0.1, 13_000_000)];
        assert_eq!(names(sorted(busy.iter().collect(), "cpu")), ["b", "a"]);
        assert_eq!(names(sorted(busy.iter().collect(), "memory")), ["b", "a"]);
    }

    #[test]
    fn formats_as_activity_monitor_does() {
        assert_eq!(format_memory(812 * 1024), "812 KB");
        assert_eq!(format_memory(257_000_000), "245.1 MB");
        assert_eq!(format_memory(16 * 1024 * 1024 * 1024), "16.00 GB");
        assert_eq!(format_cpu_time(4_210_000_000), "4.21");
        assert_eq!(format_cpu_time(723_500_000_000), "12:03.50");
        assert_eq!(format_cpu_time(10_931_080_000_000), "3:02:11.08");
        assert_eq!(format_duration(45), "45 sec");
        assert_eq!(format_duration(3 * 3600 + 300), "3 h 5 min");
        assert_eq!(format_duration(86_400 + 4 * 3600), "1 day 4 h");
        assert_eq!(percent(12.345), "12.3");
    }

    #[test]
    fn names_the_thermal_pressure_as_thermal_state_does() {
        assert_eq!(thermal(None), "");
        assert_eq!(thermal(Some(0)), "Normal");
        assert_eq!(thermal(Some(1)), "Fair");
        assert_eq!(thermal(Some(2)), "Serious");
        assert_eq!(thermal(Some(3)), "Critical");
        assert_eq!(thermal(Some(4)), "Critical");
        assert_eq!(temperature(Some(47.4)), "47 °C");
        assert_eq!(temperature(Some(47.6)), "48 °C");
        assert_eq!(temperature(None), "");
    }

    #[test]
    fn graphs_system_under_user_with_the_newest_at_the_right() {
        let history: VecDeque<Load> = [Load { user: 0.5, system: 0.25 }, Load { user: 0.0, system: 0.0 }].into();
        let (user, system) = graph(&history);
        // Two readings: the last two of forty slots.
        assert_eq!(system, "M 136.4 44.0 L 136.4 33.0 L 140.0 44.0 L 140.0 44.0 Z");
        assert_eq!(user, "M 136.4 11.0 L 140.0 44.0 L 140.0 44.0 L 136.4 33.0 Z");
        assert_eq!(graph(&VecDeque::new()), (String::new(), String::new()));
    }
}
