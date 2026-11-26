//! app.contract's `share`: Activity Monitor's reading, served to Magnitor Remote on an iPhone on
//! the same network, and `newShareCode`, the pairing code it asks for.
//!
//! While sharing is on, a thread listens on `PORT` (or a port the system picks when that one is
//! taken) and answers one request at a time, each with `Authorization: Bearer <code>`:
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
//!   `POST /v1/pointer?ops=…`, the trackpad's moves, clicks and scrolls (`pointer`).
//! - Anything else: 404, or 405 for the wrong method; a missing or wrong code: 401, a second
//!   later, so a guess costs a second (codes are 8 of 31 letters and digits: about 850 billion).
//!
//! The Mac is announced by Bonjour as `_magnitor._tcp` under its computer name
//! (`DNSServiceRegister`, in libSystem), so the phone finds it without an address. The phone can
//! also be given the address `local_address` reads.
//!
//! The traffic is plain HTTP on the local network: the code and the reading can be read by
//! anyone on the same network, and whoever has the code can quit this user's apps (never
//! Magnitor, nor the system's first processes, as Activity Monitor's Quit refuses them) and,
//! with control allowed, open apps, change the volume and lock the screen.
use crate::{control, pointer};
use crate::shapes::{AppInfo, Ended, Proc, ProcInfo, ShareCode, Sharing, Tasks};
use crate::tasks::{Load, TaskManager};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The port tried first, so an address typed on the phone keeps working across launches.
pub const PORT: u16 = 47820;
pub const SERVICE: &str = "_magnitor._tcp";
/// Letters and digits that can't be mistaken for each other (no 0/O, 1/I/L).
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// How long a request may take to arrive, and the most of it read.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_MAX: usize = 8 * 1024;
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

/// Whether `given` is `code`, comparing every byte whatever differs first.
fn same(given: &str, code: &str) -> bool {
    let (a, b) = (normal(given), normal(code));
    !b.is_empty() && a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
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
        "apps": apps.iter().map(|a| json!({ "name": a.name, "app": a.path })).collect::<Vec<_>>(),
    })
}

/// A request's path and query, and its bearer code.
#[derive(Debug, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub code: String,
}

pub fn parse_request(text: &str) -> Option<Request> {
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let code = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("authorization"))
        .and_then(|(_, v)| v.trim().strip_prefix("Bearer ").map(|c| c.trim().to_owned()))
        .unwrap_or_default();
    Some(Request { method, path: path.into(), query: query.into(), code })
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
}

/// The paths that act on the Mac beyond Activity Monitor: refused unless control is allowed.
const CONTROLS: &[&str] = &["/v1/controls", "/v1/volume", "/v1/mute", "/v1/open", "/v1/display-off", "/v1/lock", "/v1/pointer"];

/// What the server answers `request` with: a status and a JSON body. Whether control is
/// allowed is told only to a phone with the code.
pub fn respond(request: &Request, code: &str, control: bool, mut answer: impl FnMut(Ask) -> Value) -> (u16, Value) {
    let method = match request.path.as_str() {
        "/v1/activity" | "/v1/process" | "/v1/app" | "/v1/controls" => "GET",
        "/v1/end" | "/v1/end-app" | "/v1/volume" | "/v1/mute" | "/v1/open" | "/v1/display-off" | "/v1/lock" | "/v1/pointer" => "POST",
        _ => return (404, json!({ "error": "Not found." })),
    };
    if request.method != method {
        return (405, json!({ "error": format!("Only {method}.") }));
    }
    if !same(&request.code, code) {
        return (401, json!({ "error": "The code doesn’t match the one in Magnitor’s Settings on your Mac." }));
    }
    if !control && CONTROLS.contains(&request.path.as_str()) {
        return (403, json!({ "error": "Control is off in Magnitor’s Settings on your Mac." }));
    }
    let q = &request.query;
    let pid = param(q, "pid").and_then(|p| p.parse::<i64>().ok());
    let app = decode(param(q, "app").unwrap_or(""));
    let bad = (400, json!({ "error": "Bad request." }));
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
        ("/v1/volume", _) => match param(q, "level").and_then(|l| l.parse::<u8>().ok()).filter(|l| *l <= 100) {
            Some(level) => Ask::Volume { level },
            None => return bad,
        },
        ("/v1/mute", _) => Ask::Mute { on: param(q, "on") == Some("1") },
        ("/v1/open", _) if !app.is_empty() => Ask::Open { app },
        ("/v1/display-off", _) => Ask::DisplayOff,
        ("/v1/lock", _) => Ask::Lock,
        ("/v1/pointer", _) => match pointer::parse_ops(&decode(param(q, "ops").unwrap_or(""))) {
            Some(ops) => Ask::Pointer { ops },
            None => return bad,
        },
        _ => return bad,
    };
    (200, answer(ask))
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
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
}

impl Reader {
    fn new(name: String) -> Reader {
        Reader { name, tasks: TaskManager::default(), tick: 0, at: None, memory: VecDeque::new() }
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

/// One connection: its request read, answered, and closed.
fn serve(mut stream: TcpStream, code: &Mutex<String>, control: &AtomicBool, reader: &mut Reader) {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < REQUEST_MAX {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let (status, body) = match parse_request(&String::from_utf8_lossy(&buf)) {
        Some(request) => {
            let code = code.lock().unwrap_or_else(|e| e.into_inner()).clone();
            respond(&request, &code, control.load(Ordering::SeqCst), |ask| reader.answer(ask))
        }
        None => (400, json!({ "error": "Bad request." })),
    };
    // A wrong code waits: one guess a second, since connections are answered in turn.
    if status == 401 {
        std::thread::sleep(Duration::from_secs(1));
    }
    let body = body.to_string();
    let head = format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", status_text(status), body.len());
    let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body.as_bytes()));
}

/// The server while sharing is on.
struct Running {
    port: u16,
    stop: Arc<AtomicBool>,
    code: Arc<Mutex<String>>,
    control: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _announced: Option<bonjour::Announcement>,
}

impl Running {
    fn start(code: &str, control: bool) -> Result<Running, String> {
        let listener = TcpListener::bind(("0.0.0.0", PORT))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .map_err(|e| format!("Couldn’t open a port for your iPhone: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(code.to_owned()));
        let control = Arc::new(AtomicBool::new(control));
        let (stop_t, code_t, control_t) = (stop.clone(), shared.clone(), control.clone());
        let thread = std::thread::Builder::new()
            .name("magnitor-share".into())
            .spawn(move || {
                let mut reader = Reader::new(computer_name());
                for stream in listener.incoming() {
                    if stop_t.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve(stream, &code_t, &control_t, &mut reader);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Running { port, stop, code: shared, control, thread: Some(thread), _announced: bonjour::announce(port) })
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
    error: String,
    control: bool,
}

impl Share {
    /// Sharing on with `code`, or off: started, stopped or given the new code as asked; with
    /// `control`, the phone may also act on the Mac (`control`).
    pub fn share(&mut self, on: bool, code: &str, control: bool) -> Sharing {
        let on = on && !normal(code).is_empty();
        // Turning control on asks for the access the trackpad needs, so macOS's prompt shows
        // while the person is at the Mac; turning it off lets go of a drag the phone held.
        if on && control && !self.control {
            pointer::ask_access();
        }
        if !(on && control) {
            pointer::release();
        }
        self.control = on && control;
        if !on {
            self.running = None;
            self.error.clear();
        } else if let Some(r) = &self.running {
            *r.code.lock().unwrap_or_else(|e| e.into_inner()) = code.to_owned();
            r.control.store(control, Ordering::SeqCst);
        } else {
            match Running::start(code, control) {
                Ok(r) => {
                    self.running = Some(r);
                    self.error.clear();
                }
                Err(e) => self.error = e,
            }
        }
        let port = self.running.as_ref().map_or(0, |r| r.port);
        Sharing {
            on: self.running.is_some(),
            port: port as f64,
            address: match (port, local_address()) {
                (0, _) | (_, None) => String::new(),
                (p, Some(ip)) => format!("{ip}:{p}"),
            },
            error: self.error.clone(),
            pointer: pointer::allowed(),
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

    #[test]
    fn a_code_is_eight_unambiguous_letters() {
        let c = new_code().code;
        assert_eq!(c.len(), 9);
        assert_eq!(&c[4..5], "-");
        assert!(c.chars().filter(|&c| c != '-').all(|c| ALPHABET.contains(&(c as u8))));
        assert_ne!(new_code().code, c);
    }

    #[test]
    fn the_code_matches_as_typed() {
        assert!(same("abcd-efgh", "ABCD-EFGH"));
        assert!(same("ABCDEFGH", "ABCD-EFGH"));
        assert!(!same("ABCD-EFGX", "ABCD-EFGH"));
        assert!(!same("", ""));
    }

    #[test]
    fn reads_a_request() {
        let r = parse_request("GET /v1/activity?sort=memory HTTP/1.1\r\nHost: mac.local\r\nauthorization: Bearer ABCD-EFGH\r\n\r\n").unwrap();
        assert_eq!(r, Request { method: "GET".into(), path: "/v1/activity".into(), query: "sort=memory".into(), code: "ABCD-EFGH".into() });
    }

    #[test]
    fn answers_only_the_right_code() {
        let get = |path: &str, code: &str| Request { method: "GET".into(), path: path.into(), query: "sort=memory".into(), code: code.into() };
        let mut asked = Vec::new();
        assert_eq!(respond(&get("/v1/activity", "WRONG-CODE"), "ABCD-EFGH", false, |_| unreachable!()).0, 401);
        assert_eq!(respond(&get("/v1/other", "ABCD-EFGH"), "ABCD-EFGH", false, |_| unreachable!()).0, 404);
        let (status, _) = respond(&get("/v1/activity", "abcd-efgh"), "ABCD-EFGH", false, |ask| {
            asked.push(ask);
            json!({})
        });
        assert_eq!((status, asked), (200, vec![Ask::Activity { sort: "memory", query: String::new() }]));
    }

    #[test]
    fn reads_each_ask() {
        let ask = |method: &str, path: &str, query: &str| {
            let mut asked = None;
            let r = Request { method: method.into(), path: path.into(), query: query.into(), code: "ABCD-EFGH".into() };
            let status = respond(&r, "ABCD-EFGH", true, |a| {
                asked = Some(a);
                json!({})
            })
            .0;
            (status, asked)
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
    }

    #[test]
    fn controls_need_control_allowed_and_the_code() {
        let r = |path: &str, code: &str| Request { method: "POST".into(), path: path.into(), query: "level=10".into(), code: code.into() };
        for path in ["/v1/volume", "/v1/lock", "/v1/display-off", "/v1/pointer"] {
            assert_eq!(respond(&r(path, "ABCD-EFGH"), "ABCD-EFGH", false, |_| unreachable!()).0, 403);
            // A wrong code learns nothing of whether control is allowed.
            assert_eq!(respond(&r(path, "NOPE-NOPE"), "ABCD-EFGH", false, |_| unreachable!()).0, 401);
        }
        // Activity Monitor's reading is served either way.
        let get = Request { method: "GET".into(), path: "/v1/activity".into(), query: String::new(), code: "ABCD-EFGH".into() };
        assert_eq!(respond(&get, "ABCD-EFGH", false, |_| json!({})).0, 200);
    }

    #[test]
    fn controls_list_the_volume_and_apps() {
        let apps = [control::App { name: "Notes".into(), path: "/System/Applications/Notes.app".into() }];
        let j = controls_json(Some(control::Volume { level: 40, muted: true }), &apps);
        assert_eq!(j, json!({ "hasVolume": true, "volume": 40, "muted": true, "apps": [{ "name": "Notes", "app": "/System/Applications/Notes.app" }] }));
        assert_eq!(controls_json(None, &[])["hasVolume"], json!(false));
    }

    #[test]
    fn decodes_a_query_value() {
        assert_eq!(decode("a%20b+c%2Fd%E2%80%99s"), "a b c/d’s");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    /// Serves for real on a free port: a request with the code, one without, and a stop.
    #[test]
    fn serves_over_tcp_and_stops() {
        let mut share = Share::default();
        let s = share.share(true, "ABCD-EFGH", false);
        assert!(s.on, "{}", s.error);
        let send = |method: &str, path: &str, code: &str| {
            let mut c = TcpStream::connect(("127.0.0.1", s.port as u16)).unwrap();
            write!(c, "{method} {path} HTTP/1.1\r\nAuthorization: Bearer {code}\r\n\r\n").unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        let ask = |code: &str| send("GET", "/v1/activity", code);
        let ok = ask("ABCD-EFGH");
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("\"processes\""), "{ok}");
        assert!(ok.contains("\"memoryHistory\":["), "{ok}");
        assert!(ask("NOPE-NOPE").starts_with("HTTP/1.1 401"));
        // Control is refused until allowed, then allowed on the running server.
        assert!(send("GET", "/v1/controls", "ABCD-EFGH").starts_with("HTTP/1.1 403"));
        assert!(share.share(true, "ABCD-EFGH", true).on);
        let controls = send("GET", "/v1/controls", "ABCD-EFGH");
        assert!(controls.starts_with("HTTP/1.1 200") && controls.contains("\"apps\":["), "{controls}");
        assert!(!share.share(false, "ABCD-EFGH", false).on);
        assert!(TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], s.port as u16)), Duration::from_millis(300)).is_err());
    }
}


