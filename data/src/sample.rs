//! One reading of the machine for Activity Monitor: every process's CPU time and memory, and the
//! system's CPU ticks and memory, as Activity Monitor reads them.
//!
//! On macOS a process this user owns is read with libproc (`proc_pidinfo`, `proc_pid_rusage`):
//! its CPU time, threads, start, and its memory footprint, the figure Activity Monitor's Memory
//! column shows. The kernel answers those only for the user's own processes; for the rest
//! (root's, the system daemons') `/bin/ps`, which is set-uid root, gives the CPU time and resident
//! memory. The system's figures are mach's `host_statistics` and a few `sysctl`s.
//!
//! Elsewhere there is no reading: `take` says so, and the view shows the error.

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
    pub swap_used: u64,
    /// The kernel's pressure: 1 normal, 2 warning, 4 critical (`kern.memorystatus_vm_pressure_level`).
    pub pressure: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawSample {
    pub procs: Vec<RawProc>,
    pub system: RawSystem,
}

#[cfg(target_os = "macos")]
pub use mac::{end, take};

#[cfg(not(target_os = "macos"))]
pub fn take() -> Result<RawSample, String> {
    Err("Activity Monitor reads processes on macOS only.".into())
}

#[cfg(not(target_os = "macos"))]
pub fn end(_pid: i32, _force: bool) -> Result<(), i32> {
    Err(1)
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

/// `ps -axo pid=,ppid=,uid=,rss=,time=,user=,comm=`: each process `ps` sees, by pid. `comm` is
/// last since it can hold spaces.
pub fn parse_ps(out: &str) -> Vec<RawProc> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let uid = fields.next()?.parse().ok()?;
            let rss: u64 = fields.next()?.parse().ok()?;
            let cpu_ns = parse_cpu_time(fields.next()?)?;
            let user = fields.next()?.to_owned();
            let comm = fields.collect::<Vec<_>>().join(" ");
            Some(RawProc {
                pid,
                ppid,
                uid,
                user,
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
    fn detailed(pid: i32, names: &mut HashMap<u32, String>) -> Option<RawProc> {
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
        let path = pid_path(pid);
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

    /// Every process `ps` sees, for those libproc won't describe.
    fn from_ps() -> HashMap<i32, RawProc> {
        let Ok(out) = std::process::Command::new("/bin/ps").args(["-axo", "pid=,ppid=,uid=,rss=,time=,user=,comm="]).output() else {
            return HashMap::new();
        };
        parse_ps(&String::from_utf8_lossy(&out.stdout)).into_iter().map(|p| (p.pid, p)).collect()
    }

    fn system() -> RawSystem {
        let mut ticks = [0u32; 4];
        let mut count = 4u32;
        unsafe { host_statistics(host(), HOST_CPU_LOAD_INFO, ticks.as_mut_ptr() as *mut c_int, &mut count) };
        let mut vm = VmStatistics64::default();
        let mut count = (std::mem::size_of::<VmStatistics64>() / 4) as u32;
        unsafe { host_statistics64(host(), HOST_VM_INFO64, &mut vm as *mut _ as *mut c_int, &mut count) };
        let page = sysctl::<u64>(c"hw.pagesize").unwrap_or(16_384);
        let pages = |n: u32| n as u64 * page;
        let app = (vm.internal_page_count as u64).saturating_sub(vm.purgeable_count as u64);
        RawSystem {
            ticks: ticks.map(u64::from),
            memory_total: sysctl::<u64>(c"hw.memsize").unwrap_or(0),
            memory_used: (app + vm.wire_count as u64 + vm.compressor_page_count as u64) * page,
            memory_cached: pages(vm.external_page_count) + pages(vm.purgeable_count),
            swap_used: sysctl::<SwapUsage>(c"vm.swapusage").map_or(0, |s| s.used),
            pressure: sysctl::<i32>(c"kern.memorystatus_vm_pressure_level").unwrap_or(1),
        }
    }

    pub fn take() -> Result<RawSample, String> {
        let pids = all_pids();
        if pids.is_empty() {
            return Err("Couldn’t list the processes.".into());
        }
        let mut names = HashMap::new();
        let mut procs = Vec::with_capacity(pids.len());
        let mut others = Vec::new();
        for pid in pids {
            match detailed(pid, &mut names) {
                Some(p) => procs.push(p),
                None => others.push(pid),
            }
        }
        if !others.is_empty() {
            let mut seen = from_ps();
            for pid in others {
                if let Some(mut p) = seen.remove(&pid) {
                    // libproc names the executable in full where ps's `comm` may be cut short.
                    let path = pid_path(pid);
                    if !path.is_empty() {
                        p.name = base_name(&path).to_owned();
                        p.path = path;
                    }
                    procs.push(p);
                }
            }
        }
        Ok(RawSample { procs, system: system() })
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
    fn reads_ps_cpu_times() {
        assert_eq!(parse_cpu_time("0:00.05"), Some(50_000_000));
        assert_eq!(parse_cpu_time("12:34.50"), Some(754_500_000_000));
        assert_eq!(parse_cpu_time("1:00:00"), Some(3_600_000_000_000));
        assert_eq!(parse_cpu_time("2-00:00:01"), Some(172_801_000_000_000));
        assert_eq!(parse_cpu_time("soon"), None);
    }

    #[test]
    fn reads_ps_lines_with_spaces_in_the_command() {
        let out = "    1     0     0  13264   9:51.23 root             /sbin/launchd\n  412     1   501   2048   0:00.10 anna /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n";
        let procs = parse_ps(out);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].name, "launchd");
        assert_eq!(procs[0].user, "root");
        assert_eq!(procs[0].memory, 13264 * 1024);
        assert_eq!(procs[0].cpu_ns, 591_230_000_000);
        assert_eq!(procs[1].name, "Google Chrome");
        assert!(!procs[1].detailed);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_machine() {
        let sample = take().expect("a reading");
        let me = std::process::id() as i32;
        let own = sample.procs.iter().find(|p| p.pid == me).expect("this process");
        assert!(own.detailed && own.memory > 0 && own.threads.unwrap_or(0) >= 1);
        // Root's launchd, read through ps.
        assert!(sample.procs.iter().any(|p| p.pid == 1 && p.name == "launchd"));
        assert!(sample.system.memory_total > 0 && sample.system.memory_used > 0);
        assert!(sample.system.ticks.iter().sum::<u64>() > 0);
    }
}
