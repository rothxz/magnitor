//! app.contract's `unusual`: the processes behaving unusually, for the process alerts. The
//! view's watch asks every ten seconds or so while those alerts are on (`procPulse`), with its
//! own clock (`at`, its `performanceNow()` in ms), so time held over a limit is the view's time.
//!
//! Two signals, each judged per process run (a pid and its start), for this user's processes
//! alone: the ones the kernel describes in full, and the ones a person can quit.
//!
//! - **Busy**: over `cpu_limit` % of one core (Activity Monitor's % CPU) on every reading for
//!   at least `busy_for` seconds: a runaway build, a stuck `node`, a spinning helper.
//! - **Growing**: a footprint that rose by at least `grow_gb` GB and by half again over the last
//!   `WINDOW`, rising on most readings: a leak, not a file loaded once.
//!
//! A finding's `id` names its kind, its process's name and a serial that moves on only once
//! `COOLDOWN` has passed since the last finding of that kind for that name. The view posts each
//! id once, so a process that stays busy, or one that restarts and spins again, alerts once in
//! that time, however often it is asked.
//!
//! Under the agent (`--home`), a `.processes` file in that home stands in for the readings, one
//! process a line, tab-separated: `pid name path cpu memoryMB growthMBperMinute`. Its footprint at
//! the view's time `t` is `memory + growth × t / 60 s`.
use crate::sample::{self, RawProc};
use crate::shapes::{Unusual, UnusualProc};
use crate::tasks::{app_bundle, app_name, format_duration, format_memory, rates, Row};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

/// How far back a footprint's growth is judged.
pub const WINDOW: f64 = 600.0;
/// How long after a finding the same kind for the same name stays quiet.
pub const COOLDOWN: f64 = 1800.0;
/// A reading older than this is no base for a rate: the next one starts afresh.
const STALE: f64 = 60.0;
/// The share of readings over the window a footprint must have risen on.
const RISING: f64 = 0.6;
/// A rise smaller than this between two readings is no rise.
const STEP: u64 = 1 << 20;

/// What the view asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// % of one core; 0 for never.
    pub cpu: f64,
    /// Seconds over `cpu` before a finding.
    pub busy_for: f64,
    /// GB a footprint grows by over `WINDOW`; 0 for never.
    pub grow_gb: f64,
}

/// One process run as followed across readings.
#[derive(Debug, Default)]
struct Track {
    started: u64,
    /// The view's time it went over the CPU limit, while it stays over.
    busy_since: Option<f64>,
    /// (time, footprint), oldest first, back to the newest one at least `WINDOW` old.
    memory: VecDeque<(f64, u64)>,
}

#[derive(Default)]
pub struct Watchdog {
    /// The last reading: its pulse, real time, view time and processes.
    last: Option<(i64, Instant, f64, Vec<RawProc>)>,
    tracks: HashMap<i32, Track>,
    /// Per kind and name: the serial of its last finding and when it was made.
    noted: HashMap<(String, String), (u32, f64)>,
    answer: Option<(i64, Unusual)>,
    fixture: Option<Vec<FixtureProc>>,
}

impl Watchdog {
    pub fn unusual(&mut self, pulse: i64, at_ms: f64, limits: Limits) -> Unusual {
        if let Some((p, answer)) = &self.answer {
            if *p == pulse {
                return answer.clone();
            }
        }
        let at = at_ms / 1000.0;
        let answer = match self.read(pulse, at) {
            Ok(rows) => Unusual { findings: self.observe(&rows, at, limits, std::process::id() as i32), error: String::new() },
            Err(error) => Unusual { findings: Vec::new(), error },
        };
        self.answer = Some((pulse, answer.clone()));
        answer
    }

    /// This user's processes with their % CPU since the last reading; none on the first.
    fn read(&mut self, pulse: i64, at: f64) -> Result<Vec<Row>, String> {
        if let Some(fixed) = self.fixture_rows(at) {
            return Ok(fixed);
        }
        let now = sample::take(false)?.procs.into_iter().filter(|p| p.detailed).collect::<Vec<_>>();
        let taken = Instant::now();
        let rows = match self.last.take() {
            Some((_, then, _, before)) if then.elapsed().as_secs_f64() < STALE => rates(&before, &now, (taken - then).as_nanos() as f64, None, &[]),
            // No base: every process is followed from here, none busy yet.
            _ => now.iter().map(|p| Row { raw: p.clone(), cpu: 0.0 }).collect(),
        };
        self.last = Some((pulse, taken, at, now));
        Ok(rows)
    }

    /// Follow each process in `rows`, read at `at`, and say which are unusual now.
    fn observe(&mut self, rows: &[Row], at: f64, limits: Limits, me: i32) -> Vec<UnusualProc> {
        self.tracks.retain(|pid, _| rows.iter().any(|r| r.raw.pid == *pid));
        let mut found = Vec::new();
        for row in rows.iter().filter(|r| r.raw.pid != me) {
            let p = &row.raw;
            let track = self.tracks.entry(p.pid).or_default();
            // A pid reused by another process starts afresh.
            if track.started != p.started {
                *track = Track { started: p.started, ..Track::default() };
            }
            if limits.cpu > 0.0 && row.cpu >= limits.cpu {
                let since = *track.busy_since.get_or_insert(at);
                if at - since >= limits.busy_for {
                    found.push((p.clone(), Kind::Busy { cpu: row.cpu, limit: limits.cpu, secs: at - since }));
                }
            } else {
                track.busy_since = None;
            }
            track.memory.push_back((at, p.memory));
            while track.memory.len() > 2 && track.memory[1].0 <= at - WINDOW {
                track.memory.pop_front();
            }
            if limits.grow_gb > 0.0 {
                if let Some((from, to, secs)) = growing(&track.memory, limits.grow_gb) {
                    found.push((p.clone(), Kind::Growing { from, to, secs }));
                }
            }
        }
        found.into_iter().map(|(p, kind)| self.finding(&p, kind, at)).collect()
    }

    fn finding(&mut self, p: &RawProc, kind: Kind, at: f64) -> UnusualProc {
        let name = display_name(p);
        let key = (kind.id().to_owned(), name.clone());
        let serial = match self.noted.get(&key) {
            Some(&(serial, when)) if at - when < COOLDOWN => serial,
            last => {
                let next = last.map_or(1, |&(serial, _)| serial + 1);
                self.noted.insert(key, (next, at));
                next
            }
        };
        let (title, body) = kind.text(&name);
        UnusualProc {
            id: format!("{}:{name}:{serial}", kind.id()),
            kind: kind.id().into(),
            pid: p.pid as f64,
            name,
            title,
            body,
        }
    }

    /// The agent's stand-in readings: `<home>/.processes`, under a drive only.
    fn fixture_rows(&mut self, at: f64) -> Option<Vec<Row>> {
        if self.fixture.is_none() {
            let home = std::env::var_os("EXACT_AGENT_HOME")?;
            let text = std::fs::read_to_string(std::path::Path::new(&home).join(".processes")).ok()?;
            self.fixture = Some(parse_fixture(&text));
        }
        let fixed = self.fixture.as_ref()?;
        Some(fixed.iter().map(|f| f.row(at)).collect())
    }
}

enum Kind {
    Busy { cpu: f64, limit: f64, secs: f64 },
    Growing { from: u64, to: u64, secs: f64 },
}

impl Kind {
    fn id(&self) -> &'static str {
        match self {
            Kind::Busy { .. } => "cpu",
            Kind::Growing { .. } => "memory",
        }
    }

    fn text(&self, name: &str) -> (String, String) {
        match *self {
            Kind::Busy { cpu, limit, secs } => (
                format!("{name} is using a lot of CPU"),
                format!(
                    "It has used over {}% CPU for {}, and is at {}% now. Open Magnitor to see it or quit it.",
                    limit.round(),
                    format_duration(secs.round() as u64),
                    cpu.round()
                ),
            ),
            Kind::Growing { from, to, secs } => (
                format!("{name} keeps using more memory"),
                format!(
                    "It grew from {} to {} in {}. A steady rise like this can be a leak: quitting and reopening it frees the memory.",
                    format_memory(from),
                    format_memory(to),
                    format_duration(secs.round() as u64)
                ),
            ),
        }
    }
}

/// Over a history spanning at least `WINDOW`: the footprint it rose from and to, and over how
/// long, when it grew by `grow_gb` GB and half again, rising on most readings.
pub fn growing(memory: &VecDeque<(f64, u64)>, grow_gb: f64) -> Option<(u64, u64, f64)> {
    let (&(first_at, from), &(last_at, to)) = (memory.front()?, memory.back()?);
    let span = last_at - first_at;
    if span < WINDOW || to <= from {
        return None;
    }
    let grew = (to - from) as f64;
    if grew < grow_gb * 1e9 || grew < from as f64 * 0.5 {
        return None;
    }
    let steps = memory.len() - 1;
    let rising = memory.iter().zip(memory.iter().skip(1)).filter(|((_, a), (_, b))| *b >= a + STEP).count();
    (rising as f64 >= steps as f64 * RISING).then_some((from, to, span))
}

/// A process as a person knows it: its own name, and its app's where that says more
/// ("Code Helper (Plugin) in Visual Studio Code").
pub fn display_name(p: &RawProc) -> String {
    let bundle = app_bundle(&p.path);
    if bundle.is_empty() {
        return p.name.clone();
    }
    let app = app_name(&bundle);
    if p.name.starts_with(&app) {
        p.name.clone()
    } else {
        format!("{} in {app}", p.name)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct FixtureProc {
    pid: i32,
    name: String,
    path: String,
    cpu: f64,
    memory_mb: f64,
    growth_mb_per_minute: f64,
}

impl FixtureProc {
    fn row(&self, at: f64) -> Row {
        let mb = self.memory_mb + self.growth_mb_per_minute * at / 60.0;
        Row {
            raw: RawProc {
                pid: self.pid,
                name: self.name.clone(),
                path: self.path.clone(),
                memory: (mb * 1e6) as u64,
                started: 1,
                detailed: true,
                ..RawProc::default()
            },
            cpu: self.cpu,
        }
    }
}

fn parse_fixture(text: &str) -> Vec<FixtureProc> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [pid, name, path, cpu, memory, growth] = f[..] else { return None };
            Some(FixtureProc {
                pid: pid.parse().ok()?,
                name: name.into(),
                path: path.into(),
                cpu: cpu.parse().ok()?,
                memory_mb: memory.parse().ok()?,
                growth_mb_per_minute: growth.parse().ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;
    const ME: i32 = 1;

    fn row(pid: i32, name: &str, cpu: f64, memory: u64) -> Row {
        Row { raw: RawProc { pid, name: name.into(), memory, started: 7, detailed: true, ..RawProc::default() }, cpu }
    }

    fn limits() -> Limits {
        Limits { cpu: 100.0, busy_for: 120.0, grow_gb: 1.0 }
    }

    #[test]
    fn busy_only_once_held_over_the_limit_for_the_time() {
        let mut w = Watchdog::default();
        assert!(w.observe(&[row(10, "node", 180.0, GB)], 0.0, limits(), ME).is_empty());
        assert!(w.observe(&[row(10, "node", 180.0, GB)], 60.0, limits(), ME).is_empty());
        let found = w.observe(&[row(10, "node", 190.0, GB)], 120.0, limits(), ME);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "cpu:node:1");
        assert_eq!(found[0].title, "node is using a lot of CPU");
        assert_eq!(found[0].body, "It has used over 100% CPU for 2 min, and is at 190% now. Open Magnitor to see it or quit it.");
    }

    #[test]
    fn a_dip_under_the_limit_starts_the_time_again() {
        let mut w = Watchdog::default();
        w.observe(&[row(10, "node", 180.0, GB)], 0.0, limits(), ME);
        w.observe(&[row(10, "node", 20.0, GB)], 60.0, limits(), ME);
        assert!(w.observe(&[row(10, "node", 180.0, GB)], 130.0, limits(), ME).is_empty());
        assert_eq!(w.observe(&[row(10, "node", 180.0, GB)], 250.0, limits(), ME).len(), 1);
    }

    #[test]
    fn the_same_name_keeps_its_id_until_the_cooldown_passes() {
        let mut w = Watchdog::default();
        let l = Limits { busy_for: 0.0, ..limits() };
        assert_eq!(w.observe(&[row(10, "node", 180.0, GB)], 0.0, l, ME)[0].id, "cpu:node:1");
        // Restarted on another pid: the same alert, not a new one.
        assert_eq!(w.observe(&[row(11, "node", 180.0, GB)], 600.0, l, ME)[0].id, "cpu:node:1");
        assert_eq!(w.observe(&[row(11, "node", 180.0, GB)], COOLDOWN + 1.0, l, ME)[0].id, "cpu:node:2");
    }

    #[test]
    fn leaves_itself_out_and_honours_never() {
        let mut w = Watchdog::default();
        let never = Limits { cpu: 0.0, busy_for: 0.0, grow_gb: 0.0 };
        assert!(w.observe(&[row(ME, "Magnitor", 400.0, GB)], 0.0, Limits { busy_for: 0.0, ..limits() }, ME).is_empty());
        assert!(w.observe(&[row(10, "node", 400.0, GB)], 0.0, never, ME).is_empty());
    }

    #[test]
    fn a_footprint_rising_steadily_over_the_window_is_growing() {
        let mut w = Watchdog::default();
        let mut found = Vec::new();
        for i in 0..=60 {
            let at = i as f64 * 10.0;
            found = w.observe(&[row(10, "Leaky", 0.0, GB / 2 + i * 30_000_000)], at, limits(), ME);
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "memory:Leaky:1");
        assert_eq!(found[0].body, "It grew from 476.8 MB to 2.14 GB in 10 min. A steady rise like this can be a leak: quitting and reopening it frees the memory.");
    }

    #[test]
    fn a_file_loaded_once_is_not_growing() {
        let mut memory = VecDeque::new();
        for i in 0..=60 {
            memory.push_back((i as f64 * 10.0, if i < 5 { GB / 2 } else { 3 * GB }));
        }
        assert_eq!(growing(&memory, 1.0), None);
    }

    #[test]
    fn growth_needs_the_whole_window_the_size_and_half_again() {
        let steady = |from: u64, step: u64, n: u64| (0..=n).map(|i| (i as f64 * 10.0, from + i * step)).collect::<VecDeque<_>>();
        // Five minutes: too soon to say.
        assert_eq!(growing(&steady(GB / 2, 50_000_000, 30), 1.0), None);
        // 1.2 GB over ten minutes, but from 8 GB: not half again.
        assert_eq!(growing(&steady(8 * GB, 20_000_000, 60), 1.0), None);
        // Half again, but under the GB.
        assert_eq!(growing(&steady(GB / 10, 5_000_000, 60), 1.0), None);
        assert!(growing(&steady(GB / 2, 20_000_000, 60), 1.0).is_some());
    }

    #[test]
    fn keeps_only_the_window_of_footprints() {
        let mut w = Watchdog::default();
        for i in 0..200 {
            w.observe(&[row(10, "x", 0.0, GB)], i as f64 * 10.0, limits(), ME);
        }
        let m = &w.tracks[&10].memory;
        assert!(m.back().unwrap().0 - m.front().unwrap().0 <= WINDOW + 10.0);
        assert!(m.back().unwrap().0 - m.front().unwrap().0 >= WINDOW);
    }

    #[test]
    fn a_process_that_ends_is_forgotten() {
        let mut w = Watchdog::default();
        w.observe(&[row(10, "x", 0.0, GB), row(11, "y", 0.0, GB)], 0.0, limits(), ME);
        w.observe(&[row(11, "y", 0.0, GB)], 10.0, limits(), ME);
        assert!(!w.tracks.contains_key(&10));
    }

    #[test]
    fn names_a_helper_by_its_app() {
        let p = |name: &str, path: &str| RawProc { name: name.into(), path: path.into(), ..RawProc::default() };
        assert_eq!(display_name(&p("node", "/usr/local/bin/node")), "node");
        assert_eq!(display_name(&p("Slack", "/Applications/Slack.app/Contents/MacOS/Slack")), "Slack");
        assert_eq!(display_name(&p("Slack Helper (Renderer)", "/Applications/Slack.app/Contents/Frameworks/Slack Helper (Renderer).app/Contents/MacOS/Slack Helper (Renderer)")), "Slack Helper (Renderer)");
        assert_eq!(display_name(&p("Code Helper (Plugin)", "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)")), "Code Helper (Plugin) in Visual Studio Code");
    }

    #[test]
    fn reads_the_agents_fixture() {
        let f = parse_fixture("5150\tnode\t/usr/local/bin/node\t180\t300\t0\nbad line\n5151\tLeaky\t/Applications/Leaky.app/Contents/MacOS/Leaky\t2\t800\t150\n");
        assert_eq!(f.len(), 2);
        assert_eq!(f[1].row(120.0).raw.memory, 1_100_000_000);
        assert_eq!(f[0].row(0.0).cpu, 180.0);
    }
}
