//! app.contract's `vitals`: the machine's load alone (CPU, memory and its pressure), for the menu
//! bar's readout, the alerts and the widget. It reads the system's counters and no process, so the
//! app can ask every few seconds while its popover is closed.
//!
//! CPU is a rate, as in `tasks`: the share of every core's ticks spent busy between this `pulse`'s
//! reading and the last one. The first reading (or one after a long gap) takes a second a moment
//! later, so there is always a rate. A reading is kept for its pulse.
//!
//! Under the agent (`--home`), a `.vitals` file in that home holds the reading instead, so a test
//! can put the machine under load: `cpu memory pressure`, such as `97 62 1`.
use crate::sample::{self, RawSystem};
use crate::shapes::Vitals;
use crate::tasks::{format_memory, load, pressure};
use std::time::{Duration, Instant};

/// A reading older than this is no base for a rate.
const STALE: Duration = Duration::from_secs(120);
/// The gap between the first reading and the second.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct Monitor {
    last: Option<(i64, Instant, RawSystem)>,
    answer: Option<Vitals>,
}

impl Monitor {
    pub fn vitals(&mut self, pulse: i64) -> Vitals {
        if let Some(fixed) = fixture() {
            return fixed;
        }
        if let (Some((p, _, _)), Some(answer)) = (&self.last, &self.answer) {
            if *p == pulse {
                return answer.clone();
            }
        }
        let before = match self.last.take() {
            Some((_, at, s)) if at.elapsed() < STALE => Ok(s),
            _ => sample::system().inspect(|_| std::thread::sleep(SETTLE)),
        };
        match before.and_then(|b| sample::system().map(|now| (b, now))) {
            Ok((before, now)) => {
                let answer = reading(&before, &now);
                self.last = Some((pulse, Instant::now(), now));
                self.answer = Some(answer.clone());
                answer
            }
            Err(error) => Vitals { error, ..empty() },
        }
    }
}

fn reading(before: &RawSystem, now: &RawSystem) -> Vitals {
    let l = load(before, now);
    let share = if now.memory_total > 0 { now.memory_used as f64 / now.memory_total as f64 } else { 0.0 };
    Vitals {
        cpu: one_decimal((l.user + l.system) * 100.0),
        memory: one_decimal(share * 100.0),
        pressure: pressure(now.pressure).into(),
        memory_used: format_memory(now.memory_used),
        error: String::new(),
    }
}

fn one_decimal(v: f64) -> f64 {
    (v.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

fn empty() -> Vitals {
    Vitals { cpu: 0.0, memory: 0.0, pressure: String::new(), memory_used: String::new(), error: String::new() }
}

/// The agent's stand-in reading: `<home>/.vitals`, under a drive only.
fn fixture() -> Option<Vitals> {
    let home = std::env::var_os("EXACT_AGENT_HOME")?;
    let text = std::fs::read_to_string(std::path::Path::new(&home).join(".vitals")).ok()?;
    let n: Vec<f64> = text.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    let (cpu, memory) = (*n.first()?, *n.get(1)?);
    Some(Vitals {
        cpu: one_decimal(cpu),
        memory: one_decimal(memory),
        pressure: pressure(n.get(2).copied().unwrap_or(1.0) as i32).into(),
        memory_used: String::new(),
        error: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_share_of_every_tick() {
        let before = RawSystem { ticks: [100, 50, 800, 50], memory_total: 16, memory_used: 4, ..Default::default() };
        let now = RawSystem { ticks: [160, 70, 820, 50], memory_total: 16, memory_used: 4, ..Default::default() };
        let v = reading(&before, &now);
        assert_eq!(v.cpu, 80.0);
        assert_eq!(v.memory, 25.0);
        assert_eq!(v.pressure, "Normal");
    }
}
