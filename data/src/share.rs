//! app.contract's `share`: Activity Monitor's reading, served to Magnitor Remote on an iPhone on
//! the same network, and `newShareCode`, the pairing code it asks for.
//!
//! While sharing is on, a thread listens on `PORT` (or a port the system picks when that one is
//! taken). Each request is read on a thread of its own (at most `CONNECTIONS` at once, so a slow
//! client can't hold up the phone) and answered one at a time. Each is signed by a phone the
//! person allowed (`guard`: `Authorization: Magnitor id=…,ch=…,seq=…,sig=…`), except two:
//!
//! - `POST /v1/pair?name=…&x=…&y=…&proof=…`: a phone that proves it knows the pairing code
//!   (`guard::pair_proof`) waits for the person to allow it in Settings (`Gate::pending`); the
//!   code is then used up. `{ status: "pending", check }`, or `"paired"` for a phone already
//!   allowed; a wrong or spent code: 401 with `status: "code"`.
//! - `GET /v1/hello?id=<device id>`: `{ status: "paired", challenge, seq }` for an allowed phone,
//!   its session's challenge and the highest sequence number used; else `status` is `"pending"`
//!   (with `check`), `"denied"` or `"unpaired"`.
//!
//! Signed requests:
//!
//! - `GET /v1/activity?sort=cpu|memory&q=<search>`: the machine's load, its recent CPU and
//!   memory for the phone's graphs, and its busiest processes grouped by app (`activity_json`).
//! - `GET /v1/process?pid=<pid>`: one process, as Activity Monitor's inspector shows it.
//! - `GET /v1/app?app=<bundle path>`: an app's processes together.
//! - `POST /v1/end?pid=<pid>&force=0|1`: Quit or Force Quit a process (`TaskManager::end`).
//! - `POST /v1/end-app?app=<bundle path>&how=quit|force`: Quit or Force Quit an app.
//! - Once Settings › iPhone › Allow Control is on (`control`), else 403: `GET /v1/controls`, the
//!   sound's volume and the apps the phone can open; `POST /v1/volume?level=0…100`,
//!   `POST /v1/mute?on=0|1`, `POST /v1/open?app=<bundle path>`, `POST /v1/display-off` and
//!   `POST /v1/lock`, each answered as an end is (`{ ok, message }`); and
//!   `POST /v1/pointer?ops=…`, the trackpad's moves, clicks and scrolls (`pointer`); and
//!   `GET /v1/icons?apps=<bundle paths, one a line>`, those apps' icons as base64 PNGs, for the
//!   phone's Apps grid (only apps `/v1/controls` lists; at most `ICONS_MAX` a request).
//! - Anything else: 404, or 405 for the wrong method. A request not signed by an allowed phone:
//!   401 with `reason` `"unpaired"` (a phone the Mac doesn't know, or an old Remote's code),
//!   `"stale"` (a session the server no longer has, or a sequence number already used: say hello
//!   again) or `"signature"`. An address that failed too often: 429 (`guard::Lockout`).
//!   Only peers on the local network are answered (`guard::is_local`).
//!
//! The Mac is announced by Bonjour as `_magnitor._tcp` under its computer name
//! (`DNSServiceRegister`, in libSystem), so the phone finds it without an address. The phone can
//! also be given the address `local_address` reads.
//!
//! The traffic is plain HTTP on the local network: the reading (process names, the load) can be
//! read by anyone on the same network, but no secret crosses it, and a request can be neither
//! made nor replayed without an allowed phone's key. An allowed phone can quit this user's apps
//! (never Magnitor, nor the system's first processes, as Activity Monitor's Quit refuses them)
//! and, with control allowed, move the pointer, open apps, change the volume and lock the
//! screen. Settings shows which phone acted last (`Sharing::controller`) and how long control
//! has gone unused (`Sharing::control_idle`), which app.contract turns off after a while.
use crate::guard::{self, Device, Lockout, Session};
use crate::{control, pointer};
use crate::shapes::{AppInfo, Ended, Proc, ProcInfo, ShareCode, Sharing, Tasks};
use crate::tasks::{Load, TaskManager};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The port tried first, so an address typed on the phone keeps working across launches.
pub const PORT: u16 = 47820;
pub const SERVICE: &str = "_magnitor._tcp";
/// Letters and digits that can't be mistaken for each other (no 0/O, 1/I/L).
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// How long a request may take to arrive, and the most of it read.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_MAX: usize = 8 * 1024;
/// Connections read at once; more are closed unanswered.
const CONNECTIONS: usize = 16;
/// A phone that acted this recently is named in Settings as the one controlling the Mac.
const ACTING: Duration = Duration::from_secs(15);
/// Requests closer together than this share one reading: a search as it is typed, or a
/// process's details, take no new reading of the Mac.
const FRESH: Duration = Duration::from_millis(1000);
/// Memory readings kept for the phone's graph, as many as `tasks` keeps of CPU's.
const HISTORY: usize = 40;

/// A new pairing code, `ABCD-EFGH`, from the system's randomness.
pub fn new_code() -> ShareCode {
    let mut bytes = [0u8; 8];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        return ShareCode { code: String::new(), error: "Couldn’t make a code.".into() };
    }
    // 256 isn't a multiple of 31: the first few letters are a little likelier, which costs a
    // fraction of a bit of the code's 39.
    let letters: String = bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect();
    ShareCode { code: format!("{}-{}", &letters[..4], &letters[4..]), error: String::new() }
}

/// The code as typed on a phone: case and the dash don't matter.
fn normal(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// This Mac's address on the network its default route leaves by: a UDP socket's `connect`
/// chooses the interface and sends nothing.
pub fn local_address() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_loopback() && !a.ip().is_unspecified() => Some(a.ip().to_string()),
        _ => None,
    }
}

/// The Mac's name as Sharing settings show it.
fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

/// A share as a percentage to a tenth, for the phone's graphs.
fn tenth(share: f64) -> f64 {
    (share.clamp(0.0, 1.0) * 1000.0).round() / 10.0
}

fn proc_json(p: &Proc) -> Value {
    json!({
        "pid": p.pid,
        "name": p.name,
        "detail": p.detail,
        "cpu": p.cpu,
        "memory": p.memory,
        "group": p.group,
        "count": p.count,
        "app": p.app,
        "mine": p.mine,
    })
}

/// The reading the phone shows: the load and its recent history (oldest first, as percentages),
/// and the busiest processes.
pub fn activity_json(name: &str, t: &Tasks, loads: &[Load], memory: &VecDeque<f64>) -> Value {
    json!({
        "name": name,
        "cpuUser": t.cpu_user,
        "cpuSystem": t.cpu_system,
        "cpuIdle": t.cpu_idle,
        "userHistory": loads.iter().map(|l| tenth(l.user)).collect::<Vec<_>>(),
        "systemHistory": loads.iter().map(|l| tenth(l.system)).collect::<Vec<_>>(),
        "memoryHistory": memory.iter().map(|&m| tenth(m)).collect::<Vec<_>>(),
        "memoryUsed": t.memory_used,
        "memoryTotal": t.memory_total,
        "memoryShare": t.memory_share,
        "cached": t.cached,
        "compressed": t.compressed,
        "swap": t.swap,
        "pressure": t.pressure,
        "thermal": t.thermal,
        "temperature": t.temperature,
        "count": t.count,
        "more": t.more,
        "matchCount": t.match_count,
        "processes": t.processes.iter().map(proc_json).collect::<Vec<_>>(),
        "error": t.error,
    })
}

fn process_json(p: &ProcInfo) -> Value {
    json!({
        "pid": p.pid,
        "found": p.found,
        "name": p.name,
        "path": p.path,
        "user": p.user,
        "parent": p.parent,
        "cpu": p.cpu,
        "cpuTime": p.cpu_time,
        "threads": p.threads,
        "memory": p.memory,
        "realMemory": p.real_memory,
        "running": p.running,
        "mine": p.mine,
        "own": p.own,
    })
}

fn app_json(a: &AppInfo) -> Value {
    json!({
        "app": a.app,
        "found": a.found,
        "name": a.name,
        "cpu": a.cpu,
        "memory": a.memory,
        "count": a.count,
        "main": a.main,
        "mine": a.mine,
        "own": a.own,
        "processes": a.processes.iter().map(proc_json).collect::<Vec<_>>(),
    })
}

fn ended_json(e: &Ended) -> Value {
    json!({ "ok": e.ok, "message": e.message })
}

/// What the phone's Controls show: the volume (`hasVolume` false when the output sets its own)
/// and the apps it can open, by name.
pub fn controls_json(volume: Option<control::Volume>, apps: &[control::App]) -> Value {
    json!({
        "hasVolume": volume.is_some(),
        "volume": volume.map_or(0, |v| v.level),
        "muted": volume.is_some_and(|v| v.muted),
        "apps": apps.iter().map(|a| json!({ "name": a.name, "app": a.path, "stamp": control::stamp(&a.path) })).collect::<Vec<_>>(),
    })
}

/// A request's path and query, and its `Authorization` header.
#[derive(Debug, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub auth: String,
}

pub fn parse_request(text: &str) -> Option<Request> {
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let auth = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.trim().to_owned())
        .unwrap_or_default();
    Some(Request { method, path: path.into(), query: query.into(), auth })
}

fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// A query's value as written before `encodeURIComponent`: `%XX` is its byte, `+` a space.
fn decode(value: &str) -> String {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = value.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push(h << 4 | l);
                i += 3;
            }
            (b'+', ..) => {
                out.push(b' ');
                i += 1;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the phone asked for, once its code has matched.
#[derive(Debug, PartialEq)]
pub enum Ask {
    Activity { sort: &'static str, query: String },
    Process { pid: i64 },
    App { app: String },
    End { pid: i64, force: bool },
    EndApp { app: String, how: &'static str },
    Controls,
    Volume { level: u8 },
    Mute { on: bool },
    Open { app: String },
    DisplayOff,
    Lock,
    Pointer { ops: Vec<pointer::Op> },
    Icons { apps: Vec<String> },
}

/// The most icons one `/v1/icons` request asks for: drawing them holds up the phone's other
/// requests, a few hundredths of a second each.
pub const ICONS_MAX: usize = 12;

/// The paths that act on the Mac beyond Activity Monitor: refused unless control is allowed.
const CONTROLS: &[&str] = &["/v1/controls", "/v1/volume", "/v1/mute", "/v1/open", "/v1/display-off", "/v1/lock", "/v1/pointer", "/v1/icons"];

/// The controls that act on the Mac, as opposed to reading it: their use keeps control on.
const ACTS: &[&str] = &["/v1/volume", "/v1/mute", "/v1/open", "/v1/display-off", "/v1/lock", "/v1/pointer"];

/// A phone that proved the code, waiting for the person to allow it.
#[derive(Debug)]
pub struct Pending {
    pub device: Device,
    at: Instant,
}

/// Who may ask, as Settings last said and as the phones have proved: the pairing code (its
/// letters, when it stops working, whether it is spent), the phones allowed and refused, the
/// one waiting, each allowed phone's session, the addresses that failed, whether control is
/// allowed, and which phone last acted.
#[derive(Default)]
pub struct Gate {
    code: String,
    code_until: Option<Instant>,
    code_used: bool,
    code_tries: u32,
    devices: Vec<Device>,
    denied: Vec<String>,
    denied_arg: String,
    pub pending: Option<Pending>,
    sessions: HashMap<String, Session>,
    pub lockout: Lockout,
    control: bool,
    /// When control was allowed or last used, whichever is later.
    control_used: Option<Instant>,
    acted: Option<(Instant, String)>,
}

/// What `authorize` decided: an answer now, or what an allowed phone (by name) asked for.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Answer(u16, Value),
    Ask(Ask, String),
}

impl Gate {
    /// Settings' word: the code and how old it is, the phones allowed (app.contract's lines,
    /// `Device::line`), one just refused, and whether control is allowed.
    pub fn set(&mut self, code: &str, code_age: Duration, devices: &[String], denied: &str, control: bool, now: Instant) {
        let code = normal(code);
        if code != self.code {
            self.code_until = (!code.is_empty()).then(|| now + guard::CODE_LIFE.saturating_sub(code_age));
            self.code_used = false;
            self.code_tries = 0;
            self.code = code;
        }
        self.devices = devices.iter().filter_map(|l| Device::parse(l)).collect();
        let (devices, sessions) = (&self.devices, &mut self.sessions);
        sessions.retain(|id, _| devices.iter().any(|d| &d.id == id));
        // Settings keeps the last phone refused: it is refused once, so pairing it again later
        // with a new code works.
        if denied != self.denied_arg {
            self.denied_arg = denied.to_owned();
            if !denied.is_empty() && !self.denied.iter().any(|d| d == denied) {
                self.denied.push(denied.to_owned());
            }
        }
        let (devices, denied) = (&self.devices, &self.denied);
        // Allowed (now in the list) or refused: no longer waiting.
        if self.pending.as_ref().is_some_and(|p| devices.iter().any(|d| d.id == p.device.id) || denied.contains(&p.device.id)) {
            self.pending = None;
        }
        if control && (!self.control || self.control_used.is_none()) {
            self.control_used = Some(now);
        }
        if !control {
            self.control_used = None;
        }
        self.control = control;
    }

    pub fn code_valid(&self, now: Instant) -> bool {
        !self.code.is_empty() && !self.code_used && self.code_tries < guard::CODE_TRIES && self.code_until.is_some_and(|t| now < t)
    }

    /// The phone waiting to be allowed, unless it has waited too long.
    pub fn waiting(&mut self, now: Instant) -> Option<&Device> {
        if self.pending.as_ref().is_some_and(|p| now.duration_since(p.at) >= guard::PENDING_LIFE) {
            self.pending = None;
        }
        self.pending.as_ref().map(|p| &p.device)
    }

    /// The phone that acted in the last `ACTING`, by name.
    pub fn controller(&self, now: Instant) -> String {
        self.acted.as_ref().filter(|(at, _)| now.duration_since(*at) < ACTING).map(|(_, n)| n.clone()).unwrap_or_default()
    }

    /// Seconds since control was allowed or last used; 0 while it's off.
    pub fn control_idle(&self, now: Instant) -> f64 {
        self.control_used.map_or(0.0, |t| now.duration_since(t).as_secs_f64().floor())
    }

    fn pair(&mut self, q: &str, peer: IpAddr, now: Instant) -> (u16, Value) {
        let (x, y, proof) = (decode(param(q, "x").unwrap_or("")), decode(param(q, "y").unwrap_or("")), decode(param(q, "proof").unwrap_or("")));
        let Some(device) = Device::new(&decode(param(q, "name").unwrap_or("")), &x, &y) else {
            return (400, json!({ "error": "Bad request." }));
        };
        if !self.code_valid(now) {
            return (401, json!({ "status": "code", "error": "That code has expired or was already used. Make a new one in Magnitor’s Settings on your Mac." }));
        }
        if !guard::same(&proof, &guard::pair_proof(&self.code, &x, &y)) {
            self.code_tries += 1;
            self.lockout.fail(peer, now);
            return (401, json!({ "status": "code", "error": "That code doesn’t match the one in Magnitor’s Settings on your Mac." }));
        }
        self.code_used = true;
        self.denied.retain(|d| d != &device.id);
        if self.devices.iter().any(|d| d.id == device.id) {
            return (200, json!({ "status": "paired", "check": device.check() }));
        }
        let check = device.check();
        self.pending = Some(Pending { device, at: now });
        (200, json!({ "status": "pending", "check": check }))
    }

    fn hello(&mut self, q: &str, now: Instant) -> (u16, Value) {
        let id = param(q, "id").unwrap_or("");
        if self.devices.iter().any(|d| d.id == id) {
            let stale = self.sessions.get(id).is_none_or(|s| now.duration_since(s.used) >= guard::SESSION_IDLE);
            if stale {
                let Some(challenge) = guard::challenge() else {
                    return (500, json!({ "error": "Couldn’t start a session." }));
                };
                self.sessions.insert(id.to_owned(), Session::new(challenge, now));
            }
            let s = self.sessions.get_mut(id).expect("inserted above");
            s.used = now;
            return (200, json!({ "status": "paired", "challenge": s.challenge, "seq": s.top }));
        }
        if let Some(d) = self.waiting(now).filter(|d| d.id == id) {
            return (200, json!({ "status": "pending", "check": d.check() }));
        }
        let status = if self.denied.iter().any(|d| d == id) { "denied" } else { "unpaired" };
        (200, json!({ "status": status }))
    }

    /// The allowed phone that signed `request`, by name, or the refusal. Only a wrong signature
    /// counts against the address: the rest cost nothing to check and reveal nothing.
    fn check(&mut self, request: &Request, peer: IpAddr, now: Instant) -> Result<String, (u16, Value)> {
        let unpaired = || (401, json!({ "reason": "unpaired", "error": "This iPhone isn’t paired with this Mac. Pair it again in Magnitor Remote’s Settings." }));
        let stale = || (401, json!({ "reason": "stale", "error": "The session ended. Try again." }));
        let Some(signed) = guard::parse_signed(&request.auth) else { return Err(unpaired()) };
        let Some(device) = self.devices.iter().find(|d| d.id == signed.id) else { return Err(unpaired()) };
        let Some(session) = self.sessions.get_mut(&device.id) else { return Err(stale()) };
        if !guard::same(&signed.ch, &session.challenge) || !session.fresh(signed.seq) {
            return Err(stale());
        }
        let text = guard::signed_text(&session.challenge, signed.seq, &request.method, &request.path, &request.query);
        if !device.verify(&text, &signed.sig) {
            self.lockout.fail(peer, now);
            return Err((401, json!({ "reason": "signature", "error": "This request wasn’t signed by a paired iPhone." })));
        }
        session.accept(signed.seq, now);
        self.lockout.clear(peer);
        Ok(device.name.clone())
    }

    /// What `name` did once answered: a POST names it as the phone in control, a control that
    /// acts keeps control on.
    pub fn record(&mut self, path: &str, method: &str, name: &str, now: Instant) {
        if method == "POST" {
            self.acted = Some((now, name.to_owned()));
        }
        if ACTS.contains(&path) && self.control {
            self.control_used = Some(now);
        }
    }
}


/// What the server does with `request` from `peer`: an answer now (a refusal, a pairing, a
/// hello), or what an allowed phone asked for. Whether control is allowed is told only to an
/// allowed phone.
pub fn authorize(request: &Request, gate: &mut Gate, peer: IpAddr, now: Instant) -> Verdict {
    let method = match request.path.as_str() {
        "/v1/activity" | "/v1/process" | "/v1/app" | "/v1/controls" | "/v1/icons" | "/v1/hello" => "GET",
        "/v1/end" | "/v1/end-app" | "/v1/volume" | "/v1/mute" | "/v1/open" | "/v1/display-off" | "/v1/lock" | "/v1/pointer" | "/v1/pair" => "POST",
        _ => return Verdict::Answer(404, json!({ "error": "Not found." })),
    };
    if request.method != method {
        return Verdict::Answer(405, json!({ "error": format!("Only {method}.") }));
    }
    match request.path.as_str() {
        "/v1/pair" => {
            let (s, v) = gate.pair(&request.query, peer, now);
            return Verdict::Answer(s, v);
        }
        "/v1/hello" => {
            let (s, v) = gate.hello(&request.query, now);
            return Verdict::Answer(s, v);
        }
        _ => {}
    }
    let name = match gate.check(request, peer, now) {
        Ok(name) => name,
        Err((s, v)) => return Verdict::Answer(s, v),
    };
    if !gate.control && CONTROLS.contains(&request.path.as_str()) {
        return Verdict::Answer(403, json!({ "error": "Control is off in Magnitor’s Settings on your Mac." }));
    }
    match ask(request) {
        Some(ask) => Verdict::Ask(ask, name),
        None => Verdict::Answer(400, json!({ "error": "Bad request." })),
    }
}

/// What an allowed phone asked for, or `None` for a bad request.
fn ask(request: &Request) -> Option<Ask> {
    let q = &request.query;
    let pid = param(q, "pid").and_then(|p| p.parse::<i64>().ok());
    let app = decode(param(q, "app").unwrap_or(""));
    let ask = match (request.path.as_str(), pid) {
        ("/v1/activity", _) => Ask::Activity {
            sort: if param(q, "sort") == Some("memory") { "memory" } else { "cpu" },
            query: decode(param(q, "q").unwrap_or("")),
        },
        ("/v1/process", Some(pid)) => Ask::Process { pid },
        ("/v1/end", Some(pid)) => Ask::End { pid, force: param(q, "force") == Some("1") },
        ("/v1/app", _) if !app.is_empty() => Ask::App { app },
        ("/v1/end-app", _) if !app.is_empty() => Ask::EndApp { app, how: if param(q, "how") == Some("force") { "force" } else { "quit" } },
        ("/v1/controls", _) => Ask::Controls,
        ("/v1/icons", _) => {
            let apps: Vec<String> = decode(param(q, "apps").unwrap_or("")).split('\n').filter(|a| !a.is_empty()).map(str::to_owned).collect();
            if apps.is_empty() || apps.len() > ICONS_MAX {
                return None;
            }
            Ask::Icons { apps }
        }
        ("/v1/volume", _) => Ask::Volume { level: param(q, "level").and_then(|l| l.parse::<u8>().ok()).filter(|l| *l <= 100)? },
        ("/v1/mute", _) => Ask::Mute { on: param(q, "on") == Some("1") },
        ("/v1/open", _) if !app.is_empty() => Ask::Open { app },
        ("/v1/display-off", _) => Ask::DisplayOff,
        ("/v1/lock", _) => Ask::Lock,
        ("/v1/pointer", _) => Ask::Pointer { ops: pointer::parse_ops(&decode(param(q, "ops").unwrap_or("")))? },
        _ => return None,
    };
    Some(ask)
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Bad Request",
    }
}

/// The server's own Activity Monitor: its readings, and memory's kept for the phone's graph.
struct Reader {
    name: String,
    tasks: TaskManager,
    tick: i64,
    at: Option<Instant>,
    memory: VecDeque<f64>,
    /// Each app's icon as last drawn, by its bundle's path, with the bundle's `stamp` then.
    icons: HashMap<String, (u64, String)>,
}

impl Reader {
    fn new(name: String) -> Reader {
        Reader { name, tasks: TaskManager::default(), tick: 0, at: None, memory: VecDeque::new(), icons: HashMap::new() }
    }

    /// The icons of those of `asked` the Mac lists, drawn once for each version of the app.
    fn icons(&mut self, asked: &[String]) -> Value {
        let listed = control::apps();
        let wanted: Vec<(&str, u64)> = asked
            .iter()
            .filter(|a| listed.iter().any(|l| &l.path == *a))
            .map(|a| (a.as_str(), control::stamp(a)))
            .collect();
        let missing: Vec<&str> = wanted.iter().filter(|(a, s)| self.icons.get(*a).is_none_or(|(k, _)| k != s)).map(|(a, _)| *a).collect();
        for (app, png) in missing.iter().zip(control::icons(&missing)) {
            let stamp = wanted.iter().find(|(a, _)| a == app).map_or(0, |(_, s)| *s);
            self.icons.insert((*app).to_owned(), (stamp, png));
        }
        let icons: Vec<Value> = wanted
            .iter()
            .filter_map(|(a, s)| self.icons.get(*a).filter(|(_, png)| !png.is_empty()).map(|(_, png)| json!({ "app": a, "stamp": s, "png": png })))
            .collect();
        json!({ "icons": icons })
    }

    /// A new reading unless the last is fresh; whether one was taken.
    fn refresh(&mut self) -> bool {
        if self.at.is_some_and(|at| at.elapsed() < FRESH) {
            return false;
        }
        self.tick += 1;
        self.at = Some(Instant::now());
        self.tasks.read(self.tick);
        true
    }

    fn answer(&mut self, ask: Ask) -> Value {
        let done = |d: control::Done| match d {
            Ok(message) => json!({ "ok": true, "message": message }),
            Err(message) => json!({ "ok": false, "message": message }),
        };
        // Controls take no reading of the Mac.
        let ask = match ask {
            Ask::Controls => return controls_json(control::volume(), &control::apps()),
            Ask::Volume { level } => return done(control::set_volume(level)),
            Ask::Mute { on } => return done(control::set_muted(on)),
            Ask::Open { app } => return done(control::open(&app, &control::apps())),
            Ask::DisplayOff => return done(control::display_off()),
            Ask::Lock => return done(control::lock()),
            Ask::Pointer { ops } => return done(pointer::run(&ops)),
            Ask::Icons { apps } => return self.icons(&apps),
            ask => ask,
        };
        let new = self.refresh();
        match ask {
            Ask::Activity { sort, query } => {
                let t = self.tasks.tasks(self.tick, sort, &query, true);
                if new && t.error.is_empty() {
                    if self.memory.len() == HISTORY {
                        self.memory.pop_front();
                    }
                    self.memory.push_back(t.memory_share);
                }
                activity_json(&self.name, &t, &self.tasks.loads(), &self.memory)
            }
            Ask::Process { pid } => process_json(&self.tasks.process(pid, self.tick)),
            Ask::App { app } => app_json(&self.tasks.app_group(&app, self.tick)),
            Ask::End { pid, force } => ended_json(&self.tasks.end(pid, force)),
            Ask::EndApp { app, how } => ended_json(&self.tasks.end_app(&app, how)),
            _ => unreachable!("answered above"),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One connection from `peer`: its request read, checked, answered, and closed. The gate is held
/// only to check and to record, never while the Mac is read or acted on.
fn serve(mut stream: TcpStream, peer: IpAddr, gate: &Mutex<Gate>, reader: &Mutex<Reader>) {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let (status, body) = if lock(gate).lockout.locked(peer, Instant::now()) {
        (429, json!({ "error": "Too many failed attempts from this device. Try again in 15 minutes." }))
    } else {
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < REQUEST_MAX {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        match parse_request(&String::from_utf8_lossy(&buf)) {
            Some(request) => {
                let verdict = authorize(&request, &mut lock(gate), peer, Instant::now());
                match verdict {
                    Verdict::Answer(status, body) => (status, body),
                    Verdict::Ask(ask, name) => {
                        let body = lock(reader).answer(ask);
                        lock(gate).record(&request.path, &request.method, &name, Instant::now());
                        (200, body)
                    }
                }
            }
            None => (400, json!({ "error": "Bad request." })),
        }
    };
    let body = body.to_string();
    let head = format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", status_text(status), body.len());
    let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body.as_bytes()));
}

/// A connection being read, counted until its thread ends however it ends.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The server while sharing is on.
struct Running {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _announced: Option<bonjour::Announcement>,
}

impl Running {
    fn start(gate: Arc<Mutex<Gate>>) -> Result<Running, String> {
        let listener = TcpListener::bind(("0.0.0.0", PORT))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .map_err(|e| format!("Couldn’t open a port for your iPhone: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("magnitor-share".into())
            .spawn(move || {
                let reader = Arc::new(Mutex::new(Reader::new(computer_name())));
                let open = Arc::new(AtomicUsize::new(0));
                for stream in listener.incoming() {
                    if stop_t.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    // Off the local network, or one too many at once: closed unanswered.
                    let Some(peer) = stream.peer_addr().ok().map(|a| a.ip()).filter(|ip| guard::is_local(*ip)) else { continue };
                    if open.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS {
                        open.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let counted = Counted(open.clone());
                    let (gate, reader) = (gate.clone(), reader.clone());
                    let _ = std::thread::Builder::new().name("magnitor-share-request".into()).spawn(move || {
                        let _counted = counted;
                        serve(stream, peer, &gate, &reader);
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Running { port, stop, thread: Some(thread), _announced: bonjour::announce(port) })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The listener waits in `accept`: a connection of its own wakes it to see `stop`.
        let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], self.port)), Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub struct Share {
    running: Option<Running>,
    gate: Arc<Mutex<Gate>>,
    error: String,
    control: bool,
}

/// Milliseconds since 1970 now, as app.contract's clock gives a code's birth.
fn epoch_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
}

impl Share {
    /// Sharing on or off: started or stopped as asked, and told Settings' word (`Gate::set`): the
    /// pairing code and when it was made (milliseconds since 1970), the phones allowed, one just
    /// refused, and with `control`, whether they may also act on the Mac (`control`).
    pub fn share(&mut self, on: bool, code: &str, code_at: f64, control: bool, devices: &[String], denied: &str) -> Sharing {
        // Turning control on asks for the access the trackpad needs, so macOS's prompt shows
        // while the person is at the Mac; turning it off lets go of a drag the phone held.
        if on && control && !self.control {
            pointer::ask_access();
        }
        if !(on && control) {
            pointer::release();
        }
        self.control = on && control;
        let now = Instant::now();
        let age = Duration::from_millis((epoch_ms() - code_at).max(0.0) as u64);
        lock(&self.gate).set(code, age, devices, denied, on && control, now);
        if !on {
            self.running = None;
            self.error.clear();
        } else if self.running.is_none() {
            match Running::start(self.gate.clone()) {
                Ok(r) => {
                    self.running = Some(r);
                    self.error.clear();
                }
                Err(e) => self.error = e,
            }
        }
        let port = self.running.as_ref().map_or(0, |r| r.port);
        let mut gate = lock(&self.gate);
        let waiting = gate.waiting(now).cloned();
        Sharing {
            on: self.running.is_some(),
            port: port as f64,
            address: match (port, local_address()) {
                (0, _) | (_, None) => String::new(),
                (p, Some(ip)) => format!("{ip}:{p}"),
            },
            error: self.error.clone(),
            pointer: pointer::allowed(),
            code_valid: gate.code_valid(now),
            pending_id: waiting.as_ref().map(|d| d.id.clone()).unwrap_or_default(),
            pending_name: waiting.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
            pending_check: waiting.as_ref().map(Device::check).unwrap_or_default(),
            pending_line: waiting.as_ref().map(Device::line).unwrap_or_default(),
            controller: gate.controller(now),
            control_idle: gate.control_idle(now),
        }
    }
}

#[cfg(target_os = "macos")]
mod bonjour {
    use std::ffi::{c_char, c_void, CString};

    #[allow(non_camel_case_types)]
    type DNSServiceRef = *mut c_void;

    extern "C" {
        // <dns_sd.h>, in libSystem.
        fn DNSServiceRegister(
            sd_ref: *mut DNSServiceRef,
            flags: u32,
            interface_index: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port: u16,
            txt_len: u16,
            txt_record: *const c_void,
            callback: *const c_void,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefDeallocate(sd_ref: DNSServiceRef);
    }

    /// The registration, withdrawn when dropped. mDNSResponder keeps it while its connection
    /// is open; its reply (the name it settled on) isn't needed, so it is never read.
    pub struct Announcement(DNSServiceRef);

    // SAFETY: the reference is only deallocated, once, from whichever thread drops it.
    unsafe impl Send for Announcement {}

    pub fn announce(port: u16) -> Option<Announcement> {
        let service = CString::new(super::SERVICE).ok()?;
        // A TXT record of one string, `v=1`: a length byte, then the bytes.
        let txt = b"\x03v=1";
        let mut sd: DNSServiceRef = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call; a null name is the computer's name, a
        // null domain and host the defaults, a null callback no reply.
        let err = unsafe {
            DNSServiceRegister(&mut sd, 0, 0, std::ptr::null(), service.as_ptr(), std::ptr::null(), std::ptr::null(), port.to_be(), txt.len() as u16, txt.as_ptr().cast(), std::ptr::null(), std::ptr::null_mut())
        };
        (err == 0 && !sd.is_null()).then_some(Announcement(sd))
    }

    impl Drop for Announcement {
        fn drop(&mut self) {
            // SAFETY: registered above and deallocated once.
            unsafe { DNSServiceRefDeallocate(self.0) }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod bonjour {
    pub struct Announcement;
    pub fn announce(_port: u16) -> Option<Announcement> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::tests::Phone;

    const LAN: &str = "192.168.1.30";

    fn peer() -> IpAddr {
        LAN.parse().unwrap()
    }

    #[test]
    fn a_code_is_eight_unambiguous_letters() {
        let c = new_code().code;
        assert_eq!(c.len(), 9);
        assert_eq!(&c[4..5], "-");
        assert!(c.chars().filter(|&c| c != '-').all(|c| ALPHABET.contains(&(c as u8))));
        assert_ne!(new_code().code, c);
    }

    #[test]
    fn reads_a_request() {
        let r = parse_request("GET /v1/activity?sort=memory HTTP/1.1\r\nHost: mac.local\r\nauthorization: Magnitor id=ab,ch=cd,seq=1,sig=ef\r\n\r\n").unwrap();
        assert_eq!(r, Request { method: "GET".into(), path: "/v1/activity".into(), query: "sort=memory".into(), auth: "Magnitor id=ab,ch=cd,seq=1,sig=ef".into() });
    }

    /// A phone as Remote is: allowed by a gate, its session's challenge, and its next number.
    struct Client {
        phone: Phone,
        id: String,
        challenge: String,
        seq: u64,
    }

    impl Client {
        fn allowed(gate: &mut Gate, control: bool) -> Client {
            let phone = Phone::new();
            let d = phone.device("Ada’s iPhone");
            gate.set("", Duration::ZERO, &[d.line()], "", control, Instant::now());
            let mut c = Client { phone, id: d.id, challenge: String::new(), seq: 0 };
            c.hello(gate);
            c
        }

        fn hello(&mut self, gate: &mut Gate) -> Value {
            let r = Request { method: "GET".into(), path: "/v1/hello".into(), query: format!("id={}", self.id), auth: String::new() };
            let Verdict::Answer(200, v) = authorize(&r, gate, peer(), Instant::now()) else { panic!("hello") };
            if v["status"] == "paired" {
                self.challenge = v["challenge"].as_str().unwrap().to_owned();
                self.seq = v["seq"].as_u64().unwrap();
            }
            v
        }

        fn request(&mut self, method: &str, path: &str, query: &str) -> Request {
            self.seq += 1;
            let sig = self.phone.sign(&guard::signed_text(&self.challenge, self.seq, method, path, query));
            let auth = format!("Magnitor id={},ch={},seq={},sig={sig}", self.id, self.challenge, self.seq);
            Request { method: method.into(), path: path.into(), query: query.into(), auth }
        }

        fn ask(&mut self, gate: &mut Gate, method: &str, path: &str, query: &str) -> Verdict {
            let r = self.request(method, path, query);
            authorize(&r, gate, peer(), Instant::now())
        }
    }

    fn status(v: &Verdict) -> u16 {
        match v {
            Verdict::Answer(s, _) => *s,
            Verdict::Ask(..) => 200,
        }
    }

    fn reason(v: &Verdict) -> &str {
        match v {
            Verdict::Answer(_, b) => b["reason"].as_str().unwrap_or(""),
            Verdict::Ask(..) => "",
        }
    }

    #[test]
    fn answers_only_a_signed_request_from_an_allowed_phone() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, false);
        assert_eq!(c.ask(&mut gate, "GET", "/v1/activity", "sort=memory"), Verdict::Ask(Ask::Activity { sort: "memory", query: String::new() }, "Ada’s iPhone".into()));
        // The old Remote's code, no header, or a phone the Mac doesn't know.
        for auth in ["Bearer ABCD-EFGH", ""] {
            let r = Request { method: "GET".into(), path: "/v1/activity".into(), query: String::new(), auth: auth.into() };
            assert_eq!(reason(&authorize(&r, &mut gate, peer(), Instant::now())), "unpaired");
        }
        let mut stranger = Client { phone: Phone::new(), id: Phone::new().device("x").id, challenge: c.challenge.clone(), seq: 0 };
        assert_eq!(reason(&stranger.ask(&mut gate, "GET", "/v1/activity", "")), "unpaired");
        // Another key signing as this phone.
        let mut forger = Client { phone: Phone::new(), id: c.id.clone(), challenge: c.challenge.clone(), seq: 50 };
        assert_eq!(reason(&forger.ask(&mut gate, "POST", "/v1/end", "pid=501")), "signature");
        // The query is signed too: a request can't be changed on its way.
        let mut r = c.request("POST", "/v1/end", "pid=501&force=0");
        r.query = "pid=501&force=1".into();
        assert_eq!(reason(&authorize(&r, &mut gate, peer(), Instant::now())), "signature");
        assert_eq!(status(&c.ask(&mut gate, "GET", "/v1/other", "")), 404);
    }

    #[test]
    fn a_request_seen_on_the_network_cant_be_sent_again() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, true);
        let r = c.request("POST", "/v1/lock", "");
        assert_eq!(authorize(&r, &mut gate, peer(), Instant::now()), Verdict::Ask(Ask::Lock, "Ada’s iPhone".into()));
        assert_eq!(reason(&authorize(&r, &mut gate, peer(), Instant::now())), "stale");
        // Requests sent together may arrive in either order.
        let (first, second) = (c.request("GET", "/v1/controls", ""), c.request("GET", "/v1/controls", ""));
        assert_eq!(status(&authorize(&second, &mut gate, peer(), Instant::now())), 200);
        assert_eq!(status(&authorize(&first, &mut gate, peer(), Instant::now())), 200);
        // A phone told its session is stale says hello and goes on from the highest number.
        let seq = c.seq;
        c.seq = 0;
        assert_eq!(reason(&c.ask(&mut gate, "GET", "/v1/controls", "")), "stale");
        assert_eq!(c.hello(&mut gate)["seq"], json!(seq));
        assert_eq!(status(&c.ask(&mut gate, "GET", "/v1/controls", "")), 200);
    }

    #[test]
    fn a_session_ends_with_its_server_or_its_phone() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, false);
        // A restarted server has a new challenge: the old one is stale, not a forgery.
        let mut restarted = Gate::default();
        restarted.set("", Duration::ZERO, &[c.phone.device("Ada’s iPhone").line()], "", false, Instant::now());
        assert_eq!(reason(&c.ask(&mut restarted, "GET", "/v1/activity", "")), "stale");
        assert!(!restarted.lockout.locked(peer(), Instant::now()));
        // A phone removed in Settings is no longer answered, nor said hello to.
        gate.set("", Duration::ZERO, &[], "", false, Instant::now());
        assert_eq!(reason(&c.ask(&mut gate, "GET", "/v1/activity", "")), "unpaired");
        assert_eq!(c.hello(&mut gate)["status"], json!("unpaired"));
    }

    #[test]
    fn reads_each_ask() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, true);
        let mut ask = |method: &str, path: &str, query: &str| match c.ask(&mut gate, method, path, query) {
            Verdict::Ask(a, _) => (200, Some(a)),
            Verdict::Answer(s, _) => (s, None),
        };
        assert_eq!(ask("GET", "/v1/activity", "q=Google%20Chrome"), (200, Some(Ask::Activity { sort: "cpu", query: "Google Chrome".into() })));
        assert_eq!(ask("GET", "/v1/process", "pid=501"), (200, Some(Ask::Process { pid: 501 })));
        assert_eq!(ask("GET", "/v1/app", "app=%2FApplications%2FXcode.app"), (200, Some(Ask::App { app: "/Applications/Xcode.app".into() })));
        assert_eq!(ask("POST", "/v1/end", "pid=501&force=1"), (200, Some(Ask::End { pid: 501, force: true })));
        assert_eq!(ask("POST", "/v1/end-app", "app=%2FA.app&how=quit"), (200, Some(Ask::EndApp { app: "/A.app".into(), how: "quit" })));
        // Ending is a POST, reading a GET; a missing pid or app is a bad request.
        assert_eq!(ask("GET", "/v1/end", "pid=501"), (405, None));
        assert_eq!(ask("POST", "/v1/activity", ""), (405, None));
        assert_eq!(ask("POST", "/v1/end", "pid=x"), (400, None));
        assert_eq!(ask("GET", "/v1/app", ""), (400, None));
        assert_eq!(ask("GET", "/v1/controls", ""), (200, Some(Ask::Controls)));
        assert_eq!(ask("POST", "/v1/volume", "level=35"), (200, Some(Ask::Volume { level: 35 })));
        assert_eq!(ask("POST", "/v1/mute", "on=1"), (200, Some(Ask::Mute { on: true })));
        assert_eq!(ask("POST", "/v1/open", "app=%2FApplications%2FNotes.app"), (200, Some(Ask::Open { app: "/Applications/Notes.app".into() })));
        assert_eq!(ask("POST", "/v1/display-off", ""), (200, Some(Ask::DisplayOff)));
        assert_eq!(ask("POST", "/v1/lock", ""), (200, Some(Ask::Lock)));
        assert_eq!(ask("POST", "/v1/volume", "level=101"), (400, None));
        assert_eq!(ask("POST", "/v1/open", ""), (400, None));
        assert_eq!(ask("GET", "/v1/lock", ""), (405, None));
        assert_eq!(ask("POST", "/v1/pointer", "ops=m%3A3%2C-2%3Bc%3Al"), (200, Some(Ask::Pointer { ops: vec![pointer::Op::Move(3.0, -2.0), pointer::Op::Click(pointer::Button::Left)] })));
        assert_eq!(ask("POST", "/v1/pointer", "ops=jump"), (400, None));
        assert_eq!(ask("GET", "/v1/icons", "apps=%2FA.app%0A%2FB%20C.app"), (200, Some(Ask::Icons { apps: vec!["/A.app".into(), "/B C.app".into()] })));
        assert_eq!(ask("GET", "/v1/icons", ""), (400, None));
        assert_eq!(ask("GET", "/v1/icons", &format!("apps={}", vec!["%2FA.app"; ICONS_MAX + 1].join("%0A"))), (400, None));
        assert_eq!(ask("POST", "/v1/icons", "apps=%2FA.app"), (405, None));
    }

    #[test]
    fn controls_need_control_allowed_and_a_signature() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, false);
        for path in ["/v1/volume", "/v1/lock", "/v1/display-off", "/v1/pointer"] {
            assert_eq!(status(&c.ask(&mut gate, "POST", path, "level=10")), 403);
            // An unsigned request learns nothing of whether control is allowed.
            let r = Request { method: "POST".into(), path: path.into(), query: "level=10".into(), auth: String::new() };
            assert_eq!(status(&authorize(&r, &mut gate, peer(), Instant::now())), 401);
        }
        assert_eq!(status(&c.ask(&mut gate, "GET", "/v1/icons", "apps=%2FA.app")), 403);
        // Activity Monitor's reading is served either way.
        assert_eq!(status(&c.ask(&mut gate, "GET", "/v1/activity", "")), 200);
    }

    /// A pairing request for `phone` proving `code`.
    fn pair(gate: &mut Gate, phone: &Phone, code: &str, ip: IpAddr) -> (u16, Value) {
        let proof = guard::pair_proof(&normal(code), &phone.x, &phone.y);
        let query = format!("name=Ada%E2%80%99s%20iPhone&x={}&y={}&proof={proof}", phone.x, phone.y);
        let r = Request { method: "POST".into(), path: "/v1/pair".into(), query, auth: String::new() };
        match authorize(&r, gate, ip, Instant::now()) {
            Verdict::Answer(s, v) => (s, v),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn a_code_pairs_one_phone_once_and_the_person_allows_it() {
        let mut gate = Gate::default();
        gate.set("ABCD-EFGH", Duration::from_secs(30), &[], "", false, Instant::now());
        let phone = Phone::new();
        assert_eq!(pair(&mut gate, &phone, "abcd-efgh", peer()).1["status"], json!("pending"));
        let d = phone.device("Ada’s iPhone");
        // Both screens show the same six digits.
        let (id, name, check, line) = {
            let w = gate.waiting(Instant::now()).unwrap();
            (w.id.clone(), w.name.clone(), w.check(), w.line())
        };
        assert_eq!((id.as_str(), name.as_str(), check.as_str()), (d.id.as_str(), "Ada’s iPhone", d.check().as_str()));
        let mut c = Client { phone, id: d.id.clone(), challenge: String::new(), seq: 0 };
        assert_eq!(c.hello(&mut gate)["status"], json!("pending"));
        // Spent: another phone with the same code is refused.
        let (s, v) = pair(&mut gate, &Phone::new(), "ABCD-EFGH", peer());
        assert_eq!((s, &v["status"]), (401, &json!("code")));
        // Allowed in Settings: no longer waiting, and its hello opens a session.
        gate.set("ABCD-EFGH", Duration::from_secs(40), &[line], "", false, Instant::now());
        assert!(gate.waiting(Instant::now()).is_none());
        assert_eq!(c.hello(&mut gate)["status"], json!("paired"));
        assert_eq!(status(&c.ask(&mut gate, "GET", "/v1/activity", "")), 200);
    }

    #[test]
    fn a_refused_phone_is_told_so() {
        let mut gate = Gate::default();
        gate.set("ABCD-EFGH", Duration::ZERO, &[], "", false, Instant::now());
        let phone = Phone::new();
        pair(&mut gate, &phone, "ABCD-EFGH", peer());
        let id = phone.device("x").id;
        gate.set("ABCD-EFGH", Duration::ZERO, &[], &id, false, Instant::now());
        assert!(gate.waiting(Instant::now()).is_none());
        let mut c = Client { phone, id: id.clone(), challenge: String::new(), seq: 0 };
        assert_eq!(c.hello(&mut gate)["status"], json!("denied"));
        // A new code pairs it again, though Settings still names it as the last refused.
        gate.set("WXYZ-2345", Duration::ZERO, &[], &id, false, Instant::now());
        assert_eq!(pair(&mut gate, &c.phone, "WXYZ-2345", peer()).1["status"], json!("pending"));
        gate.set("WXYZ-2345", Duration::ZERO, &[], &id, false, Instant::now());
        assert_eq!(c.hello(&mut gate)["status"], json!("pending"));
    }

    #[test]
    fn a_code_expires_and_wrong_guesses_void_it_and_lock_the_guesser_out() {
        let mut gate = Gate::default();
        gate.set("ABCD-EFGH", guard::CODE_LIFE, &[], "", false, Instant::now());
        assert!(!gate.code_valid(Instant::now()));
        assert_eq!(pair(&mut gate, &Phone::new(), "ABCD-EFGH", peer()).0, 401);
        // A new code works; each wrong guess counts against it and its sender.
        gate.set("WXYZ-2345", Duration::ZERO, &[], "", false, Instant::now());
        let guesser: IpAddr = "192.168.1.66".parse().unwrap();
        for _ in 0..guard::CODE_TRIES {
            assert_eq!(pair(&mut gate, &Phone::new(), "NOPE-NOPE", guesser).0, 401);
        }
        assert!(gate.lockout.locked(guesser, Instant::now()));
        assert!(!gate.lockout.locked(peer(), Instant::now()));
        // The right code no longer works either: it was voided.
        assert_eq!(pair(&mut gate, &Phone::new(), "WXYZ-2345", peer()).0, 401);
        assert!(!gate.code_valid(Instant::now()));
    }

    #[test]
    fn settings_learn_who_acted_and_how_long_control_has_gone_unused() {
        let mut gate = Gate::default();
        let mut c = Client::allowed(&mut gate, true);
        let now = Instant::now();
        assert_eq!(gate.controller(now), "");
        assert_eq!(gate.control_idle(now + Duration::from_secs(90)), 90.0);
        let r = c.request("POST", "/v1/volume", "level=20");
        assert!(matches!(authorize(&r, &mut gate, peer(), now), Verdict::Ask(..)));
        let later = now + Duration::from_secs(60);
        gate.record(&r.path, &r.method, "Ada’s iPhone", later);
        assert_eq!(gate.controller(later + Duration::from_secs(5)), "Ada’s iPhone");
        assert_eq!(gate.controller(later + ACTING), "");
        assert_eq!(gate.control_idle(later + Duration::from_secs(10)), 10.0);
        // A read keeps nothing on; control off reads 0.
        gate.record("/v1/controls", "GET", "Ada’s iPhone", later + Duration::from_secs(30));
        assert_eq!(gate.control_idle(later + Duration::from_secs(40)), 40.0);
        gate.set("", Duration::ZERO, &[c.phone.device("Ada’s iPhone").line()], "", false, later);
        assert_eq!(gate.control_idle(later + Duration::from_secs(40)), 0.0);
    }

    #[test]
    fn controls_list_the_volume_and_apps() {
        let apps = [control::App { name: "Notes".into(), path: "/System/Applications/Notes.app".into() }];
        let j = controls_json(Some(control::Volume { level: 40, muted: true }), &apps);
        assert_eq!((&j["hasVolume"], &j["volume"], &j["muted"]), (&json!(true), &json!(40), &json!(true)));
        assert_eq!((&j["apps"][0]["name"], &j["apps"][0]["app"]), (&json!("Notes"), &json!("/System/Applications/Notes.app")));
        // When the bundle last changed, so the phone redraws an updated app's icon.
        assert_eq!(j["apps"][0]["stamp"], json!(control::stamp("/System/Applications/Notes.app")));
        assert_eq!(controls_json(None, &[])["hasVolume"], json!(false));
    }

    #[test]
    fn decodes_a_query_value() {
        assert_eq!(decode("a%20b+c%2Fd%E2%80%99s"), "a b c/d’s");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    /// The phone's own bytes (remote/tools/interop.ts, by remote/sign.ts and WebCrypto, as
    /// Magnitor Remote makes them): its key's id and digits, its proof of the code, and a signed
    /// request as it goes on the wire, read and allowed as the server reads them.
    #[test]
    fn a_phones_bytes_match_the_macs() {
        let v: Value = serde_json::from_str(include_str!("interop.json")).unwrap();
        let s = |k: &str| v[k].as_str().unwrap().to_owned();
        let d = Device::new("iPhone", &s("x"), &s("y")).unwrap();
        assert_eq!((d.id.as_str(), d.check().as_str()), (s("id").as_str(), s("check").as_str()));
        let mut gate = Gate::default();
        gate.set(&s("code"), Duration::ZERO, &[], "", false, Instant::now());
        assert!(guard::same(&guard::pair_proof(&normal(&s("code")), &s("x"), &s("y")), &s("proof")));
        // Its pairing request, as the server reads it.
        let target = format!("/v1/pair?name=iPhone&x={}&y={}&proof={}", s("x"), s("y"), s("proof"));
        let r = parse_request(&format!("POST {target} HTTP/1.1\r\n\r\n")).unwrap();
        assert_eq!(authorize(&r, &mut gate, peer(), Instant::now()), Verdict::Answer(200, json!({ "status": "pending", "check": s("check") })));
        // Its signed request, under the session it was signed for.
        gate.set(&s("code"), Duration::ZERO, &[d.line()], "", false, Instant::now());
        gate.sessions.insert(d.id.clone(), Session::new(s("challenge"), Instant::now()));
        let auth = format!("Magnitor id={},ch={},seq={},sig={}", d.id, s("challenge"), v["seq"], s("sig"));
        let r = parse_request(&format!("{} {} HTTP/1.1\r\nAuthorization: {auth}\r\n\r\n", s("method"), s("target"))).unwrap();
        assert_eq!(authorize(&r, &mut gate, peer(), Instant::now()), Verdict::Ask(Ask::Activity { sort: "cpu", query: "Ada's /tmp “x” (1)!".into() }, "iPhone".into()));
    }

    /// Serves for real on a free port: a pairing, its approval, signed requests, one unsigned,
    /// and a stop.
    #[test]
    fn serves_over_tcp_and_stops() {
        let mut share = Share::default();
        let s = share.share(true, "ABCD-EFGH", epoch_ms(), false, &[], "");
        assert!(s.on && s.code_valid, "{}", s.error);
        let send = |method: &str, target: &str, auth: &str| {
            let mut c = TcpStream::connect(("127.0.0.1", s.port as u16)).unwrap();
            write!(c, "{method} {target} HTTP/1.1\r\nAuthorization: {auth}\r\n\r\n").unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        let phone = Phone::new();
        let proof = guard::pair_proof("ABCDEFGH", &phone.x, &phone.y);
        let paired = send("POST", &format!("/v1/pair?name=Studio%20Phone&x={}&y={}&proof={proof}", phone.x, phone.y), "");
        assert!(paired.starts_with("HTTP/1.1 200") && paired.contains("\"pending\""), "{paired}");
        let s2 = share.share(true, "ABCD-EFGH", epoch_ms(), false, &[], "");
        assert_eq!((s2.pending_name.as_str(), s2.code_valid), ("Studio Phone", false));
        let id = phone.device("x").id;
        share.share(true, "ABCD-EFGH", epoch_ms(), false, std::slice::from_ref(&s2.pending_line), "");
        let hello = send("GET", &format!("/v1/hello?id={id}"), "");
        let body: Value = serde_json::from_str(hello.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let challenge = body["challenge"].as_str().unwrap().to_owned();
        let signed = |seq: u64, method: &str, path: &str| {
            let sig = phone.sign(&guard::signed_text(&challenge, seq, method, path, ""));
            send(method, path, &format!("Magnitor id={id},ch={challenge},seq={seq},sig={sig}"))
        };
        let ok = signed(1, "GET", "/v1/activity");
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("\"processes\"") && ok.contains("\"memoryHistory\":["), "{ok}");
        assert!(send("GET", "/v1/activity", "Bearer ABCD-EFGH").starts_with("HTTP/1.1 401"));
        // Control is refused until allowed, then allowed on the running server.
        assert!(signed(2, "GET", "/v1/controls").starts_with("HTTP/1.1 403"));
        let line = phone.device("Studio Phone").line();
        assert!(share.share(true, "", 0.0, true, std::slice::from_ref(&line), "").on);
        let controls = signed(3, "GET", "/v1/controls");
        assert!(controls.starts_with("HTTP/1.1 200") && controls.contains("\"apps\":["), "{controls}");
        assert!(!share.share(false, "", 0.0, false, &[line], "").on);
        assert!(TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], s.port as u16)), Duration::from_millis(300)).is_err());
    }
}
