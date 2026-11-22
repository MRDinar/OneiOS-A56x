// SPDX-License-Identifier: MIT
//! Named threads (the C runtime's `pthread_create` + `pthread_setname_np`, e.g. "xh-blk-flush", "xnuhost-ram"): the
//! name shows in /proc/PID/task/*/comm, which `oneios-tool phone procs` and the `oneios-top` lines read. Plain
//! threads: never realtime (the owner's rule: the way back to One UI must always work).
use std::io;
use std::thread::{Builder, JoinHandle};

/// Linux keeps 15 bytes of a thread's name.
pub const NAME_MAX: usize = 15;

fn short(name: &str) -> String {
    crate::text::truncate(name, NAME_MAX).to_owned()
}

/// Starts `f` on a thread named `name` (cut to [`NAME_MAX`] bytes).
pub fn spawn<T: Send + 'static>(name: &str, f: impl FnOnce() -> T + Send + 'static) -> io::Result<JoinHandle<T>> {
    Builder::new().name(short(name)).spawn(f)
}

/// [`spawn`] with a stack of `stack` bytes (virtio_blk.c's flush thread: 64 KiB).
pub fn spawn_with_stack<T: Send + 'static>(
    name: &str,
    stack: usize,
    f: impl FnOnce() -> T + Send + 'static,
) -> io::Result<JoinHandle<T>> {
    Builder::new().name(short(name)).stack_size(stack).spawn(f)
}

/// Names the calling thread (prctl PR_SET_NAME, as task.c's "xnuhost-tasks").
pub fn set_name(name: &str) {
    let Ok(c) = std::ffi::CString::new(short(name)) else { return };
    // SAFETY: a valid NUL-terminated name of at most 15 bytes; PR_SET_NAME copies it.
    unsafe { libc::prctl(libc::PR_SET_NAME, c.as_ptr()) };
}

/// The calling thread's name as Linux keeps it.
pub fn name() -> String {
    let mut buf = [0u8; 16];
    // SAFETY: PR_GET_NAME writes at most 16 bytes, NUL-terminated, into the buffer.
    unsafe { libc::prctl(libc::PR_GET_NAME, buf.as_mut_ptr()) };
    crate::text::nul_text(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let h = spawn("xnuhost-a-very-long-name", name).unwrap();
        assert_eq!(h.join().unwrap(), "xnuhost-a-very-");
        let h = spawn_with_stack("xh-blk-flush", 64 << 10, || {
            set_name("renamed");
            name()
        })
        .unwrap();
        assert_eq!(h.join().unwrap(), "renamed");
    }
}
