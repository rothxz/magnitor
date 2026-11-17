//! Activity Monitor: app.contract's `tasks` (the machine's CPU and memory, and its processes
//! sorted and searched), `process` (one process, as Activity Monitor's inspector shows it) and
//! `endProcess` (Quit and Force Quit).
//!
//! A rate is two readings apart: each `tick` of the view's timer takes a reading (`sample`), and a
//! process's % CPU is the CPU time it used since the last one over the time between them, per
//! core, so a process busy on two cores shows 200%, as Activity Monitor's does. The first reading
//! (or one after the tab was away a while) takes a second a moment later, so there is always a
//! rate. A reading is kept for its tick: sorting, searching and opening a process ask no new one.
use crate::sample::{self, RawProc, RawSample, RawSystem};
use crate::shapes::{Ended, Proc, ProcInfo, Tasks};
use std::collections::{HashMap, VecDeque};
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

    pub fn tasks(&mut self, tick: i64, sort: &str, query: &str) -> Tasks {
        self.read(tick);
        let Some(r) = &self.last else {
            return Tasks { error: self.error.clone(), ..empty_tasks() };
        };
        let me = std::process::id() as i32;
        let found = search(&r.rows, query);
        let more = found.len().saturating_sub(SHOWN);
        let shown: Vec<_> = exact_first(sorted(found, sort), query).into_iter().take(SHOWN).collect();
        let apps: Vec<String> = shown.iter().map(|row| app_bundle(&row.raw.path)).collect();
        let processes = shown.into_iter().zip(crate::icons::icons(&apps)).map(|(row, icon)| Proc { icon, ..proc_row(row, me) }).collect();
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
        error: String::new(),
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
        icon: String::new(),
    }
}

/// The outermost app bundle a path lies in (a helper's is its app's), or "" for none.
pub fn app_bundle(path: &str) -> String {
    path.find(".app/").map_or(String::new(), |i| path[..i + 4].to_owned())
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

/// The rows whose name holds `query` (any case), or whose pid it is.
pub fn search<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.iter().collect();
    }
    rows.iter().filter(|r| r.raw.name.to_lowercase().contains(&q) || r.raw.pid.to_string() == q).collect()
}

/// Largest first by `"cpu"` or `"memory"`; by name for `"name"`. Ties go by name, then pid.
pub fn sorted<'a>(mut rows: Vec<&'a Row>, sort: &str) -> Vec<&'a Row> {
    let by_name = |a: &Row, b: &Row| a.raw.name.to_lowercase().cmp(&b.raw.name.to_lowercase()).then(a.raw.pid.cmp(&b.raw.pid));
    match sort {
        "memory" => rows.sort_by(|a, b| b.raw.memory.cmp(&a.raw.memory).then_with(|| by_name(a, b))),
        "name" => rows.sort_by(|a, b| by_name(a, b)),
        _ => rows.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then_with(|| b.raw.memory.cmp(&a.raw.memory)).then_with(|| by_name(a, b))),
    }
    rows
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
        let rows: Vec<Row> = [(1, "launchd", 1.0, 10), (42, "Safari", 30.0, 500), (7, "safari helper", 30.0, 900), (420, "Mail", 0.5, 100)]
            .into_iter()
            .map(|(pid, name, cpu, mem)| Row { raw: proc(pid, name, 0, mem), cpu })
            .collect();
        let names = |rs: Vec<&Row>| rs.iter().map(|r| r.raw.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(sorted(search(&rows, " SAFARI "), "cpu")), ["safari helper", "Safari"]);
        assert_eq!(names(search(&rows, "42")), ["Safari"]);
        assert_eq!(names(sorted(search(&rows, ""), "memory")), ["safari helper", "Safari", "Mail", "launchd"]);
        assert_eq!(names(sorted(search(&rows, ""), "name")), ["launchd", "Mail", "Safari", "safari helper"]);
        assert_eq!(names(exact_first(sorted(search(&rows, "safari"), "cpu"), "safari")), ["Safari", "safari helper"]);
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
