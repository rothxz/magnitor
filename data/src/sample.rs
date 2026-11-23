//! One reading of the machine for Activity Monitor: every process's CPU time and memory, and the
//! system's CPU ticks and memory, as Activity Monitor reads them.
//!
//! On macOS a process this user owns is read with libproc (`proc_pidinfo`, `proc_pid_rusage`):
//! its CPU time, threads, start, and its memory footprint, the figure Activity Monitor's Memory
//! column shows. The kernel answers those only for the user's own processes; for the rest
//! (root's, the system daemons') `/bin/ps`, which is set-uid root, gives the CPU time and resident
//! memory. `ps` is asked about those processes alone and names no account: resolving every
//! process's command and owner is most of its cost, and the uid is enough to name the owner
//! here, once per run. The system's figures are mach's `host_statistics` and a few `sysctl`s,
//! and the thermal pressure is libnotify's state for `com.apple.system.thermalpressurelevel`,
//! the public figure `ProcessInfo.thermalState` reports: a read of shared memory, no process.
//!
//! Starting the set-uid `ps` is most of a reading's cost (about 13 of its 20 ms), so `take` is
//! told when to ask it (`tasks`'s `OTHERS_EVERY`); between, the other users' processes are
//! `ps`'s last answer. The die temperature is read again every `TEMPERATURE_EVERY`.
//!
//! On Linux every process is in `/proc`, readable to any user and cheap to read: its CPU time,
//! threads and start from `stat`, its memory from `statm` (resident less shared: the private
//! memory GNOME's System Monitor shows), its owner from the directory's. Kernel threads are left
//! out, as Activity Monitor shows none. The system's figures are `/proc/stat`, `/proc/meminfo`
//! and the kernel's memory pressure (`/proc/pressure/memory`); the temperature is hwmon's.
//!
//! Elsewhere there is no reading: `take` says so, and the view shows the error.

/// How long a die temperature reading stands before the sensors are asked again.
pub const TEMPERATURE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// One process as read: no rates yet, those come from two readings (`tasks`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawProc {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub user: String,
    pub name: String,
    pub path: String,
    /// User and system CPU time together, in nanoseconds.
    pub cpu_ns: u64,
    /// The memory footprint (Activity Monitor's Memory); the resident size where it can't be read.
    pub memory: u64,
    /// The resident size (Activity Monitor's Real Memory).
    pub resident: u64,
    /// `None` for a process the kernel won't describe to this user.
    pub threads: Option<u32>,
    /// Seconds since 1970 it started at; 0 when unknown.
    pub started: u64,
    /// Read in full with libproc (the user's own), rather than from `ps`.
    pub detailed: bool,
}

/// The whole machine at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawSystem {
    /// mach's CPU ticks since boot, all cores: user, system, idle, nice.
    pub ticks: [u64; 4],
    pub memory_total: u64,
    /// Activity Monitor's Memory Used: app memory, wired and compressed.
    pub memory_used: u64,
    /// Cached files: file-backed and purgeable pages.
    pub memory_cached: u64,
    /// Memory the compressor holds (Activity Monitor's Compressed): what macOS squeezed before
    /// it swaps.
    pub memory_compressed: u64,
    pub swap_used: u64,
    /// The kernel's pressure: 1 normal, 2 warning, 4 critical (`kern.memorystatus_vm_pressure_level`).
    pub pressure: i32,
    /// The thermal pressure: 0 nominal, 1 moderate, 2 heavy, 3 trapping, 4 sleeping
    /// (`OSThermalPressureLevel` on macOS); `None` when it can't be read.
    pub thermal: Option<u64>,
    /// The CPU dies' average temperature in °C (`sensors`), read for Activity Monitor's
    /// readings alone, not the idle watch's; `None` there or when the Mac has no die sensors.
    pub cpu_temperature: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawSample {
    pub procs: Vec<RawProc>,
    pub system: RawSystem,
}

#[cfg(target_os = "macos")]
pub use mac::{alive, end, system, take};

#[cfg(target_os = "linux")]
pub use linux::{alive, end, system, take};

/// The machine's counters alone, without a process: the menu bar's readout and the alerts read
/// these every few seconds.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn system() -> Result<RawSystem, String> {
    Err("Magnitor reads the CPU and memory on macOS and Linux only.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn take(_others: bool) -> Result<RawSample, String> {
    Err("Activity Monitor reads processes on macOS and Linux only.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn end(_pid: i32, _force: bool) -> Result<(), i32> {
    Err(1)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn alive(_pid: i32) -> bool {
    false
}

/// The kernel's memory pressure in macOS's levels (1 normal, 2 warning, 4 critical) from the
/// share of memory still available and the PSI `some avg10` (the percent of the last ten
/// seconds some task waited on memory).
pub fn linux_pressure(available_share: f64, waiting: Option<f64>) -> i32 {
    let waiting = waiting.unwrap_or(0.0);
    if available_share < 0.05 || waiting >= 30.0 {
        4
    } else if available_share < 0.15 || waiting >= 10.0 {
        2
    } else {
        1
    }
}

/// A thermal level in macOS's terms (0 nominal … 3 critical) from the CPU's temperature, for
/// Linux, which reports no thermal pressure: Intel and AMD parts throttle near 95–100 °C.
pub fn linux_thermal(celsius: f64) -> u64 {
    match celsius {
        t if t >= 95.0 => 3,
        t if t >= 90.0 => 2,
        t if t >= 80.0 => 1,
        _ => 0,
    }
}

/// `/proc/meminfo` as kB by field name.
pub fn parse_meminfo(text: &str) -> std::collections::HashMap<&str, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            Some((key.trim(), rest.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}

/// `/proc/stat`'s `cpu` line as macOS's four ticks: user, system (with interrupts and steal),
/// idle (with I/O wait), nice.
pub fn parse_cpu_ticks(stat: &str) -> Option<[u64; 4]> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let n: Vec<u64> = line.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    let at = |i: usize| n.get(i).copied().unwrap_or(0);
    (n.len() >= 4).then(|| [at(0), at(2) + at(5) + at(6) + at(7), at(3) + at(4), at(1)])
}

/// One `/proc/<pid>/stat` line: its `comm`, and the fields after it (state first), or `None`.
pub fn parse_proc_stat(line: &str) -> Option<(&str, Vec<&str>)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let comm = line.get(open + 1..close)?;
    Some((comm, line.get(close + 1..)?.split_whitespace().collect()))
}

/// `ps`'s `time`, `[[dd-]hh:]mm:ss.ss`, in nanoseconds.
pub fn parse_cpu_time(text: &str) -> Option<u64> {
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, text),
    };
    let mut seconds = 0.0;
    for part in rest.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(((days * 86_400.0 + seconds) * 1e9).round() as u64)
}

/// `ps -o pid=,ppid=,uid=,rss=,time=,comm= -p …`: each process `ps` sees, by pid, its owner
/// unnamed (the caller names it from `uid`). `comm` is last since it can hold spaces.
pub fn parse_ps(out: &str) -> Vec<RawProc> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let uid = fields.next()?.parse().ok()?;
            let rss: u64 = fields.next()?.parse().ok()?;
            let cpu_ns = parse_cpu_time(fields.next()?)?;
            let comm = fields.collect::<Vec<_>>().join(" ");
            Some(RawProc {
                pid,
                ppid,
                uid,
                name: base_name(&comm).to_owned(),
                path: comm,
                cpu_ns,
                memory: rss * 1024,
                resident: rss * 1024,
                ..RawProc::default()
            })
        })
        .collect()
}

/// A path's last part: an executable's name, which for an app is the app's
/// (`/Applications/Safari.app/Contents/MacOS/Safari`).
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{base_name, parse_ps, RawProc, RawSample, RawSystem};
    use std::collections::HashMap;
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::sync::OnceLock;

    // <sys/proc_info.h>
    #[repr(C)]
    #[derive(Default)]
    struct BsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        rfu_1: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        nice: i32,
        start_tvsec: u64,
        start_tvusec: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct TaskInfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct TaskAllInfo {
        bsd: BsdInfo,
        task: TaskInfo,
    }

    // <mach/vm_statistics.h>
    #[repr(C)]
    #[derive(Default)]
    struct VmStatistics64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }

    // <sys/sysctl.h>'s `vm.swapusage`.
    #[repr(C)]
    #[derive(Default)]
    struct SwapUsage {
        total: u64,
        avail: u64,
        used: u64,
        pagesize: u32,
        encrypted: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    #[repr(C)]
    struct Passwd {
        name: *const c_char,
    }

    const PROC_PIDTASKALLINFO: c_int = 2;
    const RUSAGE_INFO_V2: c_int = 2;
    const HOST_CPU_LOAD_INFO: c_int = 3;
    const HOST_VM_INFO64: c_int = 4;

    extern "C" {
        fn proc_listallpids(buffer: *mut c_void, size: c_int) -> c_int;
        fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
        fn mach_host_self() -> u32;
        fn host_statistics(host: u32, flavor: c_int, info: *mut c_int, count: *mut u32) -> c_int;
        fn host_statistics64(host: u32, flavor: c_int, info: *mut c_int, count: *mut u32) -> c_int;
        fn mach_timebase_info(info: *mut Timebase) -> c_int;
        fn sysctlbyname(name: *const c_char, old: *mut c_void, old_len: *mut usize, new: *mut c_void, new_len: usize) -> c_int;
        fn getpwuid(uid: u32) -> *const Passwd;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        // <notify.h>
        fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
        fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    }

    /// <libkern/OSThermalNotification.h>'s `kOSThermalNotificationPressureLevelName`.
    const THERMAL_PRESSURE: &CStr = c"com.apple.system.thermalpressurelevel";

    /// The thermal pressure level. The token is registered once; each read after is a read of
    /// shared memory, so it costs nothing to ask on every reading.
    fn thermal() -> Option<u64> {
        static TOKEN: OnceLock<Option<c_int>> = OnceLock::new();
        let token = (*TOKEN.get_or_init(|| {
            let mut token = 0;
            (unsafe { notify_register_check(THERMAL_PRESSURE.as_ptr(), &mut token) } == 0).then_some(token)
        }))?;
        let mut state = 0u64;
        (unsafe { notify_get_state(token, &mut state) } == 0).then_some(state)
    }

    /// The host port, asked once: each `mach_host_self` takes a send right.
    fn host() -> u32 {
        static HOST: OnceLock<u32> = OnceLock::new();
        *HOST.get_or_init(|| unsafe { mach_host_self() })
    }

    /// libproc's CPU times are mach ticks (on Apple silicon 125/3 ns each): to nanoseconds.
    fn ticks_to_ns(ticks: u64) -> u64 {
        static BASE: OnceLock<Timebase> = OnceLock::new();
        let base = *BASE.get_or_init(|| {
            let mut t = Timebase::default();
            unsafe { mach_timebase_info(&mut t) };
            if t.denom == 0 { Timebase { numer: 1, denom: 1 } } else { t }
        });
        (ticks as u128 * base.numer as u128 / base.denom as u128) as u64
    }

    fn sysctl<T: Default>(name: &CStr) -> Option<T> {
        let mut value = T::default();
        let mut len = std::mem::size_of::<T>();
        let ok = unsafe { sysctlbyname(name.as_ptr(), &mut value as *mut T as *mut c_void, &mut len, std::ptr::null_mut(), 0) };
        (ok == 0).then_some(value)
    }

    fn c_text(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    /// Account names by uid, kept for the run: each `getpwuid` may ask opendirectoryd, and the
    /// few accounts that own processes don't change while Magnitor runs.
    fn user_names() -> &'static std::sync::Mutex<HashMap<u32, String>> {
        static NAMES: OnceLock<std::sync::Mutex<HashMap<u32, String>>> = OnceLock::new();
        NAMES.get_or_init(Default::default)
    }

    /// Each uid's account name, looked up once.
    fn user_name(uid: u32, names: &mut HashMap<u32, String>) -> String {
        names
            .entry(uid)
            .or_insert_with(|| {
                // Only this worker thread calls getpwuid, so its static buffer is not shared.
                let entry = unsafe { getpwuid(uid) };
                if entry.is_null() || unsafe { (*entry).name.is_null() } {
                    uid.to_string()
                } else {
                    unsafe { CStr::from_ptr((*entry).name) }.to_string_lossy().into_owned()
                }
            })
            .clone()
    }

    /// Each process's executable path from the last reading, by pid, with the tag it was read
    /// for (its start time, or its `ps` command): a path never changes while a process runs, so
    /// a reading looks up only the processes that are new since the last.
    fn paths() -> &'static std::sync::Mutex<HashMap<i32, (u64, String)>> {
        static PATHS: std::sync::OnceLock<std::sync::Mutex<HashMap<i32, (u64, String)>>> = std::sync::OnceLock::new();
        PATHS.get_or_init(Default::default)
    }

    /// `pid`'s path, from `last` when it was read for the same `tag`; kept in `next`.
    fn known_path(pid: i32, tag: u64, last: &HashMap<i32, (u64, String)>, next: &mut HashMap<i32, (u64, String)>) -> String {
        let path = match last.get(&pid) {
            Some((t, path)) if *t == tag => path.clone(),
            _ => pid_path(pid),
        };
        next.insert(pid, (tag, path.clone()));
        path
    }

    fn pid_path(pid: i32) -> String {
        let mut buffer = [0u8; 4096];
        let n = unsafe { proc_pidpath(pid, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32) };
        if n > 0 { c_text(&buffer[..n as usize]) } else { String::new() }
    }

    fn all_pids() -> Vec<i32> {
        let mut pids = vec![0i32; 4096];
        loop {
            let bytes = (pids.len() * 4) as c_int;
            let n = unsafe { proc_listallpids(pids.as_mut_ptr() as *mut c_void, bytes) };
            if n < 0 {
                return Vec::new();
            }
            // A full buffer may have cut the list short: ask again with room to spare.
            if (n as usize) < pids.len() {
                pids.truncate(n as usize);
                return pids;
            }
            pids.resize(pids.len() * 2, 0);
        }
    }

    /// A process this user may read in full; `None` when the kernel refuses (another user's).
    fn detailed(pid: i32, names: &mut HashMap<u32, String>, last: &HashMap<i32, (u64, String)>, next: &mut HashMap<i32, (u64, String)>) -> Option<RawProc> {
        let mut all = TaskAllInfo::default();
        let size = std::mem::size_of::<TaskAllInfo>() as c_int;
        if unsafe { proc_pidinfo(pid, PROC_PIDTASKALLINFO, 0, &mut all as *mut _ as *mut c_void, size) } != size {
            return None;
        }
        // rusage_info_v2, as words: ri_resident_size is the 9th, ri_phys_footprint the 10th.
        let mut usage = [0u64; 32];
        let footprint = if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, usage.as_mut_ptr() as *mut c_void) } == 0 {
            usage[9]
        } else {
            all.task.resident_size
        };
        let path = known_path(pid, all.bsd.start_tvsec, last, next);
        let name = if !path.is_empty() {
            base_name(&path).to_owned()
        } else if all.bsd.name[0] != 0 {
            c_text(&all.bsd.name)
        } else {
            c_text(&all.bsd.comm)
        };
        Some(RawProc {
            pid,
            ppid: all.bsd.ppid as i32,
            uid: all.bsd.uid,
            user: user_name(all.bsd.uid, names),
            name,
            path,
            cpu_ns: ticks_to_ns(all.task.total_user + all.task.total_system),
            memory: footprint,
            resident: all.task.resident_size,
            threads: Some(all.task.threadnum.max(0) as u32),
            started: all.bsd.start_tvsec,
            detailed: true,
        })
    }

    /// A `ps` process's tag for its path: its command, which a new process on a reused pid changes.
    fn ps_tag(p: &RawProc) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&p.name, &p.path, p.ppid, p.uid).hash(&mut h);
        h.finish()
    }

    /// `pids` as `ps` sees them, for those libproc won't describe. Asking for these alone, not
    /// `-ax`, spares `ps` reading every other process's command: a quarter of the cost. A pid
    /// gone since it was listed is left out (and `ps` exits 1, which changes nothing here).
    fn from_ps(pids: &[i32]) -> HashMap<i32, RawProc> {
        let list = pids.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
        let Ok(out) = std::process::Command::new("/bin/ps").args(["-o", "pid=,ppid=,uid=,rss=,time=,comm=", "-p", &list]).output() else {
            return HashMap::new();
        };
        parse_ps(&String::from_utf8_lossy(&out.stdout)).into_iter().map(|p| (p.pid, p)).collect()
    }

    pub fn system() -> Result<RawSystem, String> {
        Ok(counters())
    }

    /// Figures fixed while the machine runs, asked once.
    fn page_size() -> u64 {
        static PAGE: OnceLock<u64> = OnceLock::new();
        *PAGE.get_or_init(|| sysctl::<u64>(c"hw.pagesize").unwrap_or(16_384))
    }

    fn memory_size() -> u64 {
        static SIZE: OnceLock<u64> = OnceLock::new();
        *SIZE.get_or_init(|| sysctl::<u64>(c"hw.memsize").unwrap_or(0))
    }

    fn counters() -> RawSystem {
        let mut ticks = [0u32; 4];
        let mut count = 4u32;
        unsafe { host_statistics(host(), HOST_CPU_LOAD_INFO, ticks.as_mut_ptr() as *mut c_int, &mut count) };
        let mut vm = VmStatistics64::default();
        let mut count = (std::mem::size_of::<VmStatistics64>() / 4) as u32;
        unsafe { host_statistics64(host(), HOST_VM_INFO64, &mut vm as *mut _ as *mut c_int, &mut count) };
        let page = page_size();
        let pages = |n: u32| n as u64 * page;
        let app = (vm.internal_page_count as u64).saturating_sub(vm.purgeable_count as u64);
        RawSystem {
            ticks: ticks.map(u64::from),
            memory_total: memory_size(),
            memory_used: (app + vm.wire_count as u64 + vm.compressor_page_count as u64) * page,
            memory_cached: pages(vm.external_page_count) + pages(vm.purgeable_count),
            memory_compressed: pages(vm.compressor_page_count),
            swap_used: sysctl::<SwapUsage>(c"vm.swapusage").map_or(0, |s| s.used),
            pressure: sysctl::<i32>(c"kern.memorystatus_vm_pressure_level").unwrap_or(1),
            thermal: thermal(),
            cpu_temperature: None,
        }
    }

    /// `ps`'s last answer, named as `take` named it: what a reading that leaves `ps` out shows.
    fn last_others() -> &'static std::sync::Mutex<HashMap<i32, RawProc>> {
        static OTHERS: OnceLock<std::sync::Mutex<HashMap<i32, RawProc>>> = OnceLock::new();
        OTHERS.get_or_init(Default::default)
    }

    /// The die sensors' average, read again only once `TEMPERATURE_EVERY` has passed: a reading
    /// is a few IOKit round trips, about 5 ms of each tick, for a figure that moves slowly.
    fn temperature() -> Option<f64> {
        static LAST: std::sync::Mutex<Option<(std::time::Instant, Option<f64>)>> = std::sync::Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some((at, t)) if at.elapsed() < super::TEMPERATURE_EVERY => t,
            _ => {
                let t = crate::sensors::cpu_temperature();
                *last = Some((std::time::Instant::now(), t));
                t
            }
        }
    }

    pub fn take(others: bool) -> Result<RawSample, String> {
        let pids = all_pids();
        if pids.is_empty() {
            return Err("Couldn’t list the processes.".into());
        }
        let mut names = user_names().lock().unwrap_or_else(|e| e.into_inner());
        let mut paths = paths().lock().unwrap_or_else(|e| e.into_inner());
        let last = std::mem::take(&mut *paths);
        let mut procs = Vec::with_capacity(pids.len());
        let mut refused = Vec::new();
        for pid in pids {
            match detailed(pid, &mut names, &last, &mut paths) {
                Some(p) => procs.push(p),
                None => refused.push(pid),
            }
        }
        let mut kept = last_others().lock().unwrap_or_else(|e| e.into_inner());
        if others {
            let mut seen = from_ps(&refused);
            kept.clear();
            for pid in refused {
                if let Some(mut p) = seen.remove(&pid) {
                    p.user = user_name(p.uid, &mut names);
                    // libproc names the executable in full where ps's `comm` may be cut short.
                    let path = known_path(pid, ps_tag(&p), &last, &mut paths);
                    if !path.is_empty() {
                        p.name = base_name(&path).to_owned();
                        p.path = path;
                    }
                    kept.insert(pid, p.clone());
                    procs.push(p);
                }
            }
        } else {
            // `ps`'s last answer for the processes still listed; one started since waits for
            // the next. Their paths are kept for that answer.
            for pid in refused {
                if let Some(p) = kept.get(&pid) {
                    if let Some(entry) = last.get(&pid) {
                        paths.insert(pid, entry.clone());
                    }
                    procs.push(p.clone());
                }
            }
        }
        let system = RawSystem { cpu_temperature: temperature(), ..counters() };
        Ok(RawSample { procs, system })
    }

    /// Whether `pid` still runs (another user's too: the kernel refuses the signal, not the pid).
    pub fn alive(pid: i32) -> bool {
        let signalled = unsafe { kill(pid, 0) };
        signalled == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
    }

    /// SIGTERM, or SIGKILL when forced; the errno when refused.
    pub fn end(pid: i32, force: bool) -> Result<(), i32> {
        let signal = if force { 9 } else { 15 };
        if unsafe { kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{base_name, linux_pressure, linux_thermal, parse_cpu_ticks, parse_meminfo, parse_proc_stat, RawProc, RawSample, RawSystem};
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::sync::OnceLock;

    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn getuid() -> u32;
        fn sysconf(name: i32) -> i64;
    }

    // <unistd.h> on Linux.
    const SC_CLK_TCK: i32 = 2;
    const SC_PAGESIZE: i32 = 30;
    /// `PF_KTHREAD`, in `stat`'s flags: a kernel thread.
    const KTHREAD: u64 = 0x0020_0000;

    fn clock_ticks() -> u64 {
        static TICKS: OnceLock<u64> = OnceLock::new();
        *TICKS.get_or_init(|| u64::try_from(unsafe { sysconf(SC_CLK_TCK) }).ok().filter(|t| *t > 0).unwrap_or(100))
    }

    fn page_size() -> u64 {
        static PAGE: OnceLock<u64> = OnceLock::new();
        *PAGE.get_or_init(|| u64::try_from(unsafe { sysconf(SC_PAGESIZE) }).ok().filter(|p| *p > 0).unwrap_or(4096))
    }

    /// Seconds since 1970 the machine booted at (`/proc/stat`'s `btime`).
    fn boot_time() -> u64 {
        static BOOT: OnceLock<u64> = OnceLock::new();
        *BOOT.get_or_init(|| {
            let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
            stat.lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok()).unwrap_or(0)
        })
    }

    /// The accounts' names by uid, from `/etc/passwd`, read once.
    fn user_names() -> &'static HashMap<u32, String> {
        static NAMES: OnceLock<HashMap<u32, String>> = OnceLock::new();
        NAMES.get_or_init(|| {
            let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
            passwd
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split(':');
                    let name = fields.next()?;
                    let uid = fields.nth(1)?.parse().ok()?;
                    Some((uid, name.to_owned()))
                })
                .collect()
        })
    }

    fn user_name(uid: u32) -> String {
        user_names().get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }

    pub fn system() -> Result<RawSystem, String> {
        counters()
    }

    fn counters() -> Result<RawSystem, String> {
        let stat = fs::read_to_string("/proc/stat").map_err(|e| format!("Couldn’t read the CPU: {e}"))?;
        let ticks = parse_cpu_ticks(&stat).ok_or("Couldn’t read the CPU.")?;
        let text = fs::read_to_string("/proc/meminfo").map_err(|e| format!("Couldn’t read the memory: {e}"))?;
        let info = parse_meminfo(&text);
        let kb = |key: &str| info.get(key).copied().unwrap_or(0) * 1024;
        let total = kb("MemTotal");
        let available = info.get("MemAvailable").map_or_else(|| kb("MemFree") + kb("Cached") + kb("Buffers"), |a| a * 1024);
        let waiting = fs::read_to_string("/proc/pressure/memory").ok().and_then(|psi| {
            psi.lines().find(|l| l.starts_with("some "))?.split_whitespace().find_map(|f| f.strip_prefix("avg10=")?.parse().ok())
        });
        let share = if total > 0 { available as f64 / total as f64 } else { 1.0 };
        Ok(RawSystem {
            ticks,
            memory_total: total,
            memory_used: total.saturating_sub(available),
            memory_cached: kb("Cached") + kb("Buffers"),
            memory_compressed: kb("Zswap"),
            swap_used: kb("SwapTotal").saturating_sub(kb("SwapFree")),
            pressure: linux_pressure(share, waiting),
            thermal: None,
            cpu_temperature: None,
        })
    }

    /// The temperature, read again only once `TEMPERATURE_EVERY` has passed.
    fn temperature() -> Option<f64> {
        static LAST: std::sync::Mutex<Option<(std::time::Instant, Option<f64>)>> = std::sync::Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some((at, t)) if at.elapsed() < super::TEMPERATURE_EVERY => t,
            _ => {
                let t = crate::sensors::cpu_temperature();
                *last = Some((std::time::Instant::now(), t));
                t
            }
        }
    }

    /// One process, or `None` for a kernel thread or one that ended while it was read.
    fn read(pid: i32, me: u32) -> Option<RawProc> {
        let dir = format!("/proc/{pid}");
        let line = fs::read_to_string(format!("{dir}/stat")).ok()?;
        let (comm, f) = parse_proc_stat(&line)?;
        // After `comm`: state, ppid, pgrp, session, tty, tpgid, flags, minflt, cminflt, majflt,
        // cmajflt, utime, stime, cutime, cstime, priority, nice, num_threads, itrealvalue,
        // starttime.
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        if num(6) & KTHREAD != 0 || pid == 2 {
            return None;
        }
        let uid = fs::metadata(&dir).ok()?.uid();
        let statm = fs::read_to_string(format!("{dir}/statm")).unwrap_or_default();
        let pages: Vec<u64> = statm.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        let resident = pages.get(1).copied().unwrap_or(0) * page_size();
        let shared = pages.get(2).copied().unwrap_or(0) * page_size();
        let exe = fs::read_link(format!("{dir}/exe")).ok().map(|p| p.to_string_lossy().trim_end_matches(" (deleted)").to_owned());
        let path = exe.unwrap_or_else(|| {
            let cmdline = fs::read(format!("{dir}/cmdline")).unwrap_or_default();
            let first = cmdline.split(|b| *b == 0).next().unwrap_or_default();
            String::from_utf8_lossy(first).into_owned()
        });
        // `comm` is cut at 15 bytes: the executable's whole name where it begins with it.
        let whole = base_name(&path);
        let name = if comm.len() >= 15 && whole.starts_with(comm) { whole.to_owned() } else { comm.to_owned() };
        let clock = clock_ticks();
        Some(RawProc {
            pid,
            ppid: num(1) as i32,
            uid,
            user: user_name(uid),
            name,
            path: if path.is_empty() { comm.to_owned() } else { path },
            cpu_ns: (num(11) + num(12)) * 1_000_000_000 / clock,
            memory: resident.saturating_sub(shared),
            resident,
            threads: u32::try_from(num(17)).ok(),
            started: boot_time() + num(19) / clock,
            detailed: uid == me,
        })
    }

    /// Every process `/proc` lists. Another user's is read in full too, so `others` changes
    /// nothing here.
    pub fn take(_others: bool) -> Result<RawSample, String> {
        let me = unsafe { getuid() };
        let entries = fs::read_dir("/proc").map_err(|e| format!("Couldn’t list the processes: {e}"))?;
        let procs: Vec<RawProc> = entries
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| read(pid, me))
            .collect();
        if procs.is_empty() {
            return Err("Couldn’t list the processes.".into());
        }
        let celsius = temperature();
        let system = RawSystem { cpu_temperature: celsius, thermal: celsius.map(linux_thermal), ..counters()? };
        Ok(RawSample { procs, system })
    }

    pub fn alive(pid: i32) -> bool {
        let signalled = unsafe { kill(pid, 0) };
        signalled == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
    }

    /// SIGTERM, or SIGKILL when forced; the errno when refused.
    pub fn end(pid: i32, force: bool) -> Result<(), i32> {
        let signal = if force { 9 } else { 15 };
        if unsafe { kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_linux_proc_files() {
        let stat = "cpu  1599739 4221 94041 4691081 103801 0 3052 0 0 0\ncpu0 1 2 3 4\n";
        assert_eq!(parse_cpu_ticks(stat), Some([1_599_739, 97_093, 4_794_882, 4221]));
        let info = parse_meminfo("MemTotal:  8000 kB\nMemAvailable:    4000 kB\n");
        assert_eq!(info.get("MemAvailable"), Some(&4000));
        let (comm, fields) = parse_proc_stat("42 (Web Content (x)) S 1 42 42 0 -1 4194560 10 0 0 0 7 3").unwrap();
        assert_eq!(comm, "Web Content (x)");
        assert_eq!(fields[0], "S");
        assert_eq!(fields[1], "1");
        assert_eq!(fields[11], "7");
    }

    #[test]
    fn names_linux_memory_pressure_in_macos_levels() {
        assert_eq!(linux_pressure(0.5, Some(0.1)), 1);
        assert_eq!(linux_pressure(0.1, None), 2);
        assert_eq!(linux_pressure(0.5, Some(12.0)), 2);
        assert_eq!(linux_pressure(0.03, None), 4);
        assert_eq!(linux_thermal(46.0), 0);
        assert_eq!(linux_thermal(85.0), 1);
        assert_eq!(linux_thermal(97.0), 3);
    }

    #[test]
    fn reads_ps_cpu_times() {
        assert_eq!(parse_cpu_time("0:00.05"), Some(50_000_000));
        assert_eq!(parse_cpu_time("12:34.50"), Some(754_500_000_000));
        assert_eq!(parse_cpu_time("1:00:00"), Some(3_600_000_000_000));
        assert_eq!(parse_cpu_time("2-00:00:01"), Some(172_801_000_000_000));
        assert_eq!(parse_cpu_time("soon"), None);
    }

    #[test]
    fn reads_ps_lines_with_spaces_in_the_command() {
        let out = "    1     0     0  13264   9:51.23 /sbin/launchd\n  412     1   501   2048   0:00.10 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n";
        let procs = parse_ps(out);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].name, "launchd");
        assert_eq!(procs[0].uid, 0);
        assert_eq!(procs[0].memory, 13264 * 1024);
        assert_eq!(procs[0].cpu_ns, 591_230_000_000);
        assert_eq!(procs[1].name, "Google Chrome");
        assert!(!procs[1].detailed);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_machine() {
        let sample = take(true).expect("a reading");
        let me = std::process::id() as i32;
        let own = sample.procs.iter().find(|p| p.pid == me).expect("this process");
        assert!(own.detailed && own.memory > 0 && own.threads.unwrap_or(0) >= 1);
        // Root's launchd, read through ps.
        assert!(sample.procs.iter().any(|p| p.pid == 1 && p.name == "launchd" && p.user == "root"));
        assert!(sample.system.thermal.is_some_and(|t| t <= 4));
        assert!(sample.system.memory_total > 0 && sample.system.memory_used > 0);
        assert!(sample.system.ticks.iter().sum::<u64>() > 0);
    }
}
