//! Magnitor Remote's trackpad (share.rs's `POST /v1/pointer?ops=…`, once control is allowed):
//! the pointer moved, clicked, scrolled and held down for a drag, by CoreGraphics' events as a
//! mouse would post them.
//!
//! A request carries the moves and presses the phone gathered since its last one, in order,
//! as `ops`: `m:dx,dy` (move by points), `s:dx,dy` (scroll by pixels, content following the
//! finger), `c:l` or `c:r` (click left or right), `d` and `u` (the left button down and up,
//! for a drag). The pointer stays on a display: past every display's edge it stops at the
//! edge of the one it was on.
//!
//! Posting events needs Accessibility access (System Settings › Privacy & Security ›
//! Accessibility): `allowed` says whether Magnitor has it, and `ask_access` asks once.

/// One thing the phone's trackpad did.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Op {
    Move(f64, f64),
    Scroll(i32, i32),
    Click(Button),
    Down,
    Up,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Button {
    Left,
    Right,
}

/// The most ops one request may carry.
pub const MAX_OPS: usize = 256;
/// The furthest one move or scroll may go, so a garbled request can't fling the pointer.
const MAX_STEP: f64 = 4000.0;

/// `ops` as the phone writes them, or `None` when any is malformed or there are too many.
pub fn parse_ops(text: &str) -> Option<Vec<Op>> {
    let pair = |v: &str| {
        let (x, y) = v.split_once(',')?;
        let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
        (x.is_finite() && y.is_finite()).then(|| (x.clamp(-MAX_STEP, MAX_STEP), y.clamp(-MAX_STEP, MAX_STEP)))
    };
    let ops: Option<Vec<Op>> = text
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|op| match op.split_once(':') {
            Some(("m", v)) => pair(v).map(|(x, y)| Op::Move(x, y)),
            Some(("s", v)) => pair(v).map(|(x, y)| Op::Scroll(x.round() as i32, y.round() as i32)),
            Some(("c", "l")) => Some(Op::Click(Button::Left)),
            Some(("c", "r")) => Some(Op::Click(Button::Right)),
            None if op == "d" => Some(Op::Down),
            None if op == "u" => Some(Op::Up),
            _ => None,
        })
        .collect();
    ops.filter(|o| !o.is_empty() && o.len() <= MAX_OPS)
}

/// How many clicks a click makes, as macOS counts a double-click: the same button again
/// within the interval counts on (up to three), else it starts at one.
pub fn click_count(last: Option<(std::time::Instant, Button, i64)>, button: Button, now: std::time::Instant) -> i64 {
    const DOUBLE: std::time::Duration = std::time::Duration::from_millis(500);
    match last {
        Some((at, b, n)) if b == button && now.duration_since(at) <= DOUBLE => (n % 3) + 1,
        _ => 1,
    }
}

pub const NO_ACCESS: &str = "Magnitor needs Accessibility access on your Mac to move the pointer: allow it in System Settings › Privacy & Security › Accessibility, then try again.";

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    type CGEventRef = *mut c_void;

    // <CoreGraphics/CGEventTypes.h> and <CGEvent.h>.
    const MOUSE_MOVED: u32 = 5;
    const LEFT_DOWN: u32 = 1;
    const LEFT_UP: u32 = 2;
    const RIGHT_DOWN: u32 = 3;
    const RIGHT_UP: u32 = 4;
    const LEFT_DRAGGED: u32 = 6;
    const BUTTON_LEFT: u32 = 0;
    const BUTTON_RIGHT: u32 = 1;
    const HID_TAP: u32 = 0;
    const FIELD_CLICK_STATE: u32 = 1;
    const FIELD_DELTA_X: u32 = 4;
    const FIELD_DELTA_Y: u32 = 5;
    const SCROLL_PIXEL: u32 = 0;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: *const c_void) -> CGEventRef;
        fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
        fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, at: CGPoint, button: u32) -> CGEventRef;
        fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, wheel1: i32, wheel2: i32, wheel3: i32) -> CGEventRef;
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CGGetDisplaysWithPoint(point: CGPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// Whether the left button is held for a drag, and the last click, for double-clicks.
    static HELD: AtomicBool = AtomicBool::new(false);
    static LAST: Mutex<Option<(Instant, Button, i64)>> = Mutex::new(None);

    pub fn allowed() -> bool {
        // SAFETY: no arguments; reads the process's own access.
        unsafe { CGPreflightPostEventAccess() }
    }

    /// Asks for Accessibility access unless Magnitor has it: macOS shows its prompt once, and
    /// lists Magnitor in the settings either way.
    pub fn ask_access() {
        // A test binary never asks: the prompt would name it, not Magnitor.
        if cfg!(test) {
            return;
        }
        // SAFETY: as above.
        unsafe {
            if !CGPreflightPostEventAccess() {
                CGRequestPostEventAccess();
            }
        }
    }

    fn display_of(p: CGPoint) -> Option<CGRect> {
        let (mut id, mut count) = (0u32, 0u32);
        // SAFETY: room for one display id; `count` says whether it was written.
        let err = unsafe { CGGetDisplaysWithPoint(p, 1, &mut id, &mut count) };
        // SAFETY: a display id the system just named.
        (err == 0 && count > 0).then(|| unsafe { CGDisplayBounds(id) })
    }

    fn location() -> CGPoint {
        // SAFETY: a null source makes a blank event, released once read.
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            if e.is_null() {
                return CGPoint { x: 0.0, y: 0.0 };
            }
            let p = CGEventGetLocation(e);
            CFRelease(e);
            p
        }
    }

    /// Posts `event` and releases it; `false` when it couldn't be made.
    fn post(event: CGEventRef, fields: &[(u32, i64)]) -> bool {
        if event.is_null() {
            return false;
        }
        // SAFETY: a valid event, released once after posting.
        unsafe {
            for &(field, value) in fields {
                CGEventSetIntegerValueField(event, field, value);
            }
            CGEventPost(HID_TAP, event);
            CFRelease(event);
        }
        true
    }

    fn mouse(kind: u32, at: CGPoint, button: u32, fields: &[(u32, i64)]) -> bool {
        // SAFETY: a null source; the event is posted and released by `post`.
        post(unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, at, button) }, fields)
    }

    fn move_by(dx: f64, dy: f64) -> bool {
        let from = location();
        let mut to = CGPoint { x: from.x + dx, y: from.y + dy };
        if display_of(to).is_none() {
            // SAFETY: no arguments.
            let b = display_of(from).unwrap_or_else(|| unsafe { CGDisplayBounds(CGMainDisplayID()) });
            to.x = to.x.clamp(b.origin.x, b.origin.x + b.size.width - 1.0);
            to.y = to.y.clamp(b.origin.y, b.origin.y + b.size.height - 1.0);
        }
        let kind = if HELD.load(Ordering::SeqCst) { LEFT_DRAGGED } else { MOUSE_MOVED };
        mouse(kind, to, BUTTON_LEFT, &[(FIELD_DELTA_X, (to.x - from.x).round() as i64), (FIELD_DELTA_Y, (to.y - from.y).round() as i64)])
    }

    fn count_click(button: Button) -> i64 {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let n = click_count(*last, button, now);
        *last = Some((now, button, n));
        n
    }

    fn click(button: Button) -> bool {
        let at = location();
        let n = count_click(button);
        let (down, up, b) = match button {
            Button::Left => (LEFT_DOWN, LEFT_UP, BUTTON_LEFT),
            Button::Right => (RIGHT_DOWN, RIGHT_UP, BUTTON_RIGHT),
        };
        mouse(down, at, b, &[(FIELD_CLICK_STATE, n)]) && mouse(up, at, b, &[(FIELD_CLICK_STATE, n)])
    }

    fn scroll(dx: i32, dy: i32) -> bool {
        // SAFETY: a null source; posted and released by `post`.
        post(unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null(), SCROLL_PIXEL, 2, dy, dx, 0) }, &[])
    }

    pub fn run(ops: &[Op]) -> crate::control::Done {
        if !allowed() {
            ask_access();
            return Err(NO_ACCESS.into());
        }
        for op in ops {
            let ok = match *op {
                Op::Move(dx, dy) => move_by(dx, dy),
                Op::Scroll(dx, dy) => scroll(dx, dy),
                Op::Click(b) => click(b),
                Op::Down => {
                    let n = count_click(Button::Left);
                    HELD.store(true, Ordering::SeqCst);
                    mouse(LEFT_DOWN, location(), BUTTON_LEFT, &[(FIELD_CLICK_STATE, n)])
                }
                Op::Up => {
                    let held = HELD.swap(false, Ordering::SeqCst);
                    !held || mouse(LEFT_UP, location(), BUTTON_LEFT, &[(FIELD_CLICK_STATE, 1)])
                }
            };
            if !ok {
                return Err("Couldn’t move the pointer.".into());
            }
        }
        Ok(String::new())
    }

    /// Lets go of a drag the phone left held (sharing stopped, control turned off).
    pub fn release() {
        if HELD.swap(false, Ordering::SeqCst) {
            mouse(LEFT_UP, location(), BUTTON_LEFT, &[(FIELD_CLICK_STATE, 1)]);
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::{allowed, ask_access, release, run};

#[cfg(not(target_os = "macos"))]
mod other {
    pub fn allowed() -> bool {
        false
    }
    pub fn ask_access() {}
    pub fn release() {}
    pub fn run(_: &[super::Op]) -> crate::control::Done {
        Err("Only a Mac’s pointer can be moved from your iPhone.".into())
    }
}

#[cfg(not(target_os = "macos"))]
pub use other::{allowed, ask_access, release, run};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn reads_the_phones_ops() {
        assert_eq!(
            parse_ops("m:3.5,-2;s:0,12;c:l;c:r;d;m:1,1;u"),
            Some(vec![Op::Move(3.5, -2.0), Op::Scroll(0, 12), Op::Click(Button::Left), Op::Click(Button::Right), Op::Down, Op::Move(1.0, 1.0), Op::Up])
        );
        assert_eq!(parse_ops("m:99999,0"), Some(vec![Op::Move(MAX_STEP, 0.0)]));
        assert_eq!(parse_ops(""), None);
        assert_eq!(parse_ops("m:1"), None);
        assert_eq!(parse_ops("m:NaN,1"), None);
        assert_eq!(parse_ops("c:x"), None);
        assert_eq!(parse_ops("k:a"), None);
        assert_eq!(parse_ops(&"c:l;".repeat(MAX_OPS + 1)), None);
    }

    #[test]
    fn counts_a_double_click_as_macos_does() {
        let t = Instant::now();
        assert_eq!(click_count(None, Button::Left, t), 1);
        assert_eq!(click_count(Some((t, Button::Left, 1)), Button::Left, t + Duration::from_millis(200)), 2);
        assert_eq!(click_count(Some((t, Button::Left, 2)), Button::Left, t + Duration::from_millis(200)), 3);
        assert_eq!(click_count(Some((t, Button::Left, 3)), Button::Left, t + Duration::from_millis(200)), 1);
        assert_eq!(click_count(Some((t, Button::Right, 1)), Button::Left, t + Duration::from_millis(200)), 1);
        assert_eq!(click_count(Some((t, Button::Left, 1)), Button::Left, t + Duration::from_millis(900)), 1);
    }
}
