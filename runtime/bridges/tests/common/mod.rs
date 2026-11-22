// SPDX-License-Identifier: MIT
//! What the binder tests share: the checks' count, a child process, its end, the log on stdout.
#![allow(dead_code)]

use onebridges::log::{set_sink, Level, Sink};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub static CHECKS: AtomicU32 = AtomicU32::new(0);
pub static FAILURES: AtomicU32 = AtomicU32::new(0);

/// C's CHECK: counted, a failure printed with its line, the test goes on.
#[macro_export]
macro_rules! check {
    ($cond:expr) => {{
        common::CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !($cond) {
            common::FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            println!("FAIL {}:{}: {}", file!(), line!(), stringify!($cond));
        }
    }};
}

/// The bridges' lines on stdout as "  [E] line", as the C tests' log_line.
pub fn log_to_stdout() {
    set_sink(Sink::Custom(Arc::new(|level: Level, line: &str| {
        let l = match level {
            Level::Error => "E",
            Level::Info => "I",
            _ => "D",
        };
        println!("  [{l}] {line}");
    })));
}

/// The summary line and the exit code.
pub fn finish(name: &str) -> i32 {
    let (checks, failures) = (CHECKS.load(Ordering::Relaxed), FAILURES.load(Ordering::Relaxed));
    println!("{name}: {checks} checks, {failures} failed");
    i32::from(failures != 0)
}

/// Forks; the child runs `child`, then ends with _exit.
pub fn fork(child: impl FnOnce()) -> libc::pid_t {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // SAFETY: fork; the child only runs `child` and ends with _exit.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        child();
        exit(0);
    }
    pid
}

pub fn exit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // SAFETY: ends the process at once (a forked child must not run the parent's exit handlers).
    unsafe { libc::_exit(code) }
}

/// The child's exit code when it ends within `ms` (128 + signal for a killed one), else -1 and it is killed.
pub fn child_exit(pid: libc::pid_t, ms: u64) -> i32 {
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut status = 0;
        // SAFETY: waits for our own child.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        let _ = tx.send(status);
    });
    let status = match rx.recv_timeout(Duration::from_millis(ms)) {
        Ok(status) => status,
        Err(_) => {
            kill(pid);
            let _ = waiter.join();
            return -1;
        }
    };
    let _ = waiter.join();
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}

/// SIGKILL and the wait for it.
pub fn kill(pid: libc::pid_t) {
    // SAFETY: our own child.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}
