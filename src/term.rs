//! Raw-mode terminal plumbing: termios, window size, poll/read, signals, local time.
//!
//! The picker drives the terminal with its own escape sequences (as the Node original
//! did), so this is deliberately thin: enough to put stdin in raw mode, learn the
//! window size, wait for input with a timeout, and always hand the terminal back.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);
static ORIG_TERMIOS: OnceLock<libc::termios> = OnceLock::new();
/// Set from a signal handler; the picker's event loop notices it on its next tick.
static SIGNALED: AtomicI32 = AtomicI32::new(0);

pub fn isatty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

pub fn write_out(s: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// (cols, rows), falling back to 80x24 like the JS `out.columns || 80`.
pub fn term_size() -> (usize, usize) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ as _, &mut ws) };
    let cols = if r == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    };
    let rows = if r == 0 && ws.ws_row > 0 {
        ws.ws_row as usize
    } else {
        24
    };
    (cols, rows)
}

/// Put stdin in raw mode, matching what Node's `setRawMode(true)` does: no echo, no
/// line buffering, and no signal generation — so Ctrl-C arrives as a 0x03 byte and the
/// picker decides what it means. Output post-processing is left alone, which is why
/// every rendered line ends in an explicit CRLF.
pub fn set_raw() -> bool {
    let fd = libc::STDIN_FILENO;
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
        return false;
    }
    let _ = ORIG_TERMIOS.set(t);
    let mut raw = t;
    raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
    raw.c_oflag |= libc::ONLCR;
    raw.c_cflag |= libc::CS8;
    raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return false;
    }
    RAW_ACTIVE.store(true, Ordering::SeqCst);
    true
}

/// Hand the terminal back: cooked mode, mouse reporting off, cursor on. Safe to call
/// repeatedly and from the panic hook; does nothing unless raw mode is actually on.
pub fn restore() {
    if RAW_ACTIVE.swap(false, Ordering::SeqCst) {
        if let Some(orig) = ORIG_TERMIOS.get() {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, orig);
            }
        }
        // Mouse off, clear our full-screen UI, cursor back on.
        write_out("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[2J\x1b[H\x1b[?25h");
    }
}

/// Wait up to `timeout_ms` for stdin. `Ok(true)` = readable, `Ok(false)` = timed out,
/// `Err(())` = interrupted (a signal arrived); the caller re-checks its state and loops.
pub fn poll_stdin(timeout_ms: i32) -> Result<bool, ()> {
    let mut fds = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
    if r < 0 {
        return Err(());
    }
    Ok(r > 0)
}

/// Bytes read, 0 on EOF, negative on error.
pub fn read_stdin(buf: &mut [u8]) -> isize {
    unsafe {
        libc::read(
            libc::STDIN_FILENO,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNALED.store(sig, Ordering::SeqCst);
}

/// Note SIGINT is only reachable from outside (raw mode delivers Ctrl-C as a byte).
/// Handling these at all is about leaving the terminal usable, not about the exit code.
pub fn install_signal_handlers() {
    // `sighandler_t` is an integer, so the handler goes through a pointer cast rather
    // than straight from the fn item.
    let handler = on_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// The signal that arrived, or 0.
pub fn pending_signal() -> i32 {
    SIGNALED.load(Ordering::SeqCst)
}

pub struct LocalTime {
    pub year: i32,
    pub mon: i32,
    pub mday: i32,
    pub hour: i32,
    pub min: i32,
    pub sec: i32,
}

/// Epoch milliseconds -> local broken-down time, via libc so the system's timezone
/// database (and DST rules) apply without pulling in a date crate.
pub fn local_time(ms: i64) -> Option<LocalTime> {
    let secs = ms.div_euclid(1000);
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::localtime_r(&t, &mut tm) };
    if r.is_null() {
        return None;
    }
    Some(LocalTime {
        year: tm.tm_year as i32 + 1900,
        mon: tm.tm_mon as i32 + 1,
        mday: tm.tm_mday as i32,
        hour: tm.tm_hour as i32,
        min: tm.tm_min as i32,
        sec: tm.tm_sec as i32,
    })
}
