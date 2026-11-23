// SPDX-License-Identifier: MIT
//! Audio backends for the virtio-snd device (C: audio/backend.c): the AAudio helper process and a null one.
//!
//! Why a helper: AAudio is a shared library (libaaudio.so) and xnuhost is a static binary. The helper (bin/oneios-audio,
//! audio/aaudio_helper.rs) is a small dynamic executable that plays what it is sent through One UI's audioserver,
//! which keeps running in a session, and sends back what the microphone records.

use super::proto::{
    au_message, samples, AuHeader, AU_HEADER, AU_LATENCY, AU_MAX_FRAMES, AU_PCM, AU_PROBE, AU_START, AU_STATE, AU_STOP,
    AU_TOLD,
};
use crate::proto::Wire;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};
use xh_core::sys::{errno, strerror};

/// PCM sink and source for the guest's virtio-snd card: 48 kHz S16LE, stereo out, mono in. Errors are positive errnos.
pub trait AudioBackend: Send + Sync {
    fn start(&self, output: bool, rate: u32, channels: u32) -> Result<(), i32>;
    fn stop(&self, output: bool) -> Result<(), i32>;
    /// Output: queues `pcm` (interleaved stereo); returns the frames taken.
    fn write(&self, pcm: &[i16]) -> usize;
    /// Input: fills `pcm` (mono) up to its length; returns the frames given, 0 none yet, or -errno (-EPIPE: the
    /// helper is gone).
    fn read(&self, pcm: &mut [i16]) -> i32;
    /// The speaker latency, measured before the guest boots (AU_PROBE: the stream is opened in silence and lingers):
    /// frames at 48 kHz from a guest period's arrival to the speaker. Err(ENOTSUP): not the helper; Err(ETIMEDOUT): no
    /// answer in `timeout_ms`. Blocks the caller up to `timeout_ms`.
    fn probe_latency(&self, _timeout_ms: u32) -> Result<u32, i32> {
        Err(libc::ENOTSUP)
    }
    /// Tells the helper the output latency the guest publishes (frames), for its end-to-end line. No-op otherwise.
    fn told(&self, _frames: u32) {}
}

// ---- null ---------------------------------------------------------------------------------------
/// A backend that drops output and gives silence (no helper, tests).
#[derive(Debug, Default)]
pub struct NullAudio;

impl AudioBackend for NullAudio {
    fn start(&self, _output: bool, _rate: u32, _channels: u32) -> Result<(), i32> {
        Ok(())
    }
    fn stop(&self, _output: bool) -> Result<(), i32> {
        Ok(())
    }
    fn write(&self, pcm: &[i16]) -> usize {
        pcm.len() / 2
    }
    fn read(&self, pcm: &mut [i16]) -> i32 {
        // silence in real time
        std::thread::sleep(Duration::from_micros(pcm.len() as u64 * 1_000_000 / 48000));
        pcm.fill(0);
        pcm.len() as i32
    }
}

// ---- the helper ---------------------------------------------------------------------------------
/// The AAudio helper process, fed over a socket pair.
#[derive(Debug)]
pub struct AudioHelper {
    fd: OwnedFd,
    pid: libc::pid_t,
}

/// What the helper sends back.
struct Packet {
    header: AuHeader,
    body: Vec<u8>,
}

impl AudioHelper {
    /// Starts the helper at `path` (`path 3`, its end of the pair as descriptor 3). None: the reason is logged.
    pub fn spawn(path: &str) -> Option<AudioHelper> {
        let (mine, theirs) = match xh_core::fd::socket_pair(libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC) {
            Ok(pair) => pair,
            Err(e) => {
                crate::br_error!("audio: off: socketpair: {}", xh_core::sys::error_text(&e));
                return None;
            }
        };
        let rc = match spawn_with_fd3(path, &theirs) {
            Ok(pid) => {
                drop(theirs);
                crate::br_info!("audio: on: helper {path} (pid {pid})");
                return Some(AudioHelper { fd: mine, pid });
            }
            Err(rc) => rc,
        };
        crate::br_error!("audio: off: {path}: {}", strerror(rc));
        None
    }

    /// The helper's process id.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    fn send(&self, header: AuHeader, pcm: &[i16]) -> Result<(), i32> {
        let message = au_message(header, pcm);
        // Never block a vCPU on a helper that is behind: a full socket drops the period.
        // SAFETY: a valid buffer of its length.
        let n = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                message.as_ptr().cast(),
                message.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }

    /// One packet without waiting: Ok(None) when there is none, Err(EPIPE) when the helper closed its end.
    fn recv(&self) -> Result<Option<Packet>, i32> {
        let mut buf = vec![0u8; AU_HEADER + AU_MAX_FRAMES as usize * 2];
        // SAFETY: a valid buffer of its length.
        let got = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
        if got == 0 {
            return Err(libc::EPIPE);
        }
        if got < AU_HEADER as isize {
            return Ok(None);
        }
        buf.truncate(got as usize);
        let header = AuHeader::read_from(&buf);
        Ok(Some(Packet { header, body: buf.split_off(AU_HEADER) }))
    }

    fn wait(&self, timeout_ms: i32) -> bool {
        let mut p = libc::pollfd { fd: self.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(&mut p, 1, timeout_ms) > 0 }
    }

    fn state_line(h: &AuHeader) {
        crate::br_error!(
            "audio: helper: {} stream: {}",
            if h.output != 0 { "speaker" } else { "microphone" },
            strerror(h.value as i32)
        );
    }
}

/// posix_spawn of `path 3` with `fd` as the child's descriptor 3 and this process's environment.
fn spawn_with_fd3(path: &str, fd: &OwnedFd) -> Result<libc::pid_t, i32> {
    // Declared here: libc has no posix_spawn for Android (bionic has it since API 28). posix_spawn_file_actions_t is a
    // pointer in bionic and 80 bytes in glibc: 128 bytes, aligned, hold either.
    #[repr(C, align(8))]
    struct FileActions([u8; 128]);
    extern "C" {
        static environ: *const *mut libc::c_char;
        fn posix_spawn_file_actions_init(a: *mut FileActions) -> i32;
        fn posix_spawn_file_actions_adddup2(a: *mut FileActions, fd: i32, to: i32) -> i32;
        fn posix_spawn_file_actions_destroy(a: *mut FileActions) -> i32;
        fn posix_spawn(
            pid: *mut libc::pid_t,
            path: *const libc::c_char,
            actions: *const FileActions,
            attr: *const libc::c_void,
            argv: *const *mut libc::c_char,
            envp: *const *mut libc::c_char,
        ) -> i32;
    }
    let cpath = CString::new(path).map_err(|_| libc::EINVAL)?;
    let three = c"3";
    let argv = [cpath.as_ptr().cast_mut(), three.as_ptr().cast_mut(), std::ptr::null_mut()];
    let mut pid = 0;
    // A zeroed file-actions object for posix_spawn_file_actions_init to set up.
    let mut actions = FileActions([0; 128]);
    // SAFETY: init then adddup2 on our actions object; argv is NULL-terminated and lives across the call; environ is
    // the process's environment; destroy after.
    let rc = unsafe {
        posix_spawn_file_actions_init(&mut actions);
        posix_spawn_file_actions_adddup2(&mut actions, fd.as_raw_fd(), 3); // the helper's end is its fd 3
        let rc = posix_spawn(&mut pid, cpath.as_ptr(), &actions, std::ptr::null(), argv.as_ptr(), environ);
        posix_spawn_file_actions_destroy(&mut actions);
        rc
    };
    if rc != 0 {
        Err(rc)
    } else {
        Ok(pid)
    }
}

impl AudioBackend for AudioHelper {
    fn start(&self, output: bool, rate: u32, channels: u32) -> Result<(), i32> {
        if !output {
            // What the last recording left in the socket (and the helper's reports) is old: drop it.
            while let Ok(Some(_)) = self.recv() {}
        }
        let header = AuHeader {
            kind: AU_START,
            output: output.into(),
            channels: channels as u8,
            value: rate,
            ..Default::default()
        };
        self.send(header, &[])
    }

    fn stop(&self, output: bool) -> Result<(), i32> {
        self.send(AuHeader { kind: AU_STOP, output: output.into(), ..Default::default() }, &[])
    }

    fn write(&self, pcm: &[i16]) -> usize {
        let frames = (pcm.len() / 2).min(AU_MAX_FRAMES as usize);
        let header = AuHeader { kind: AU_PCM, output: 1, channels: 2, value: frames as u32, ..Default::default() };
        if self.send(header, &pcm[..frames * 2]).is_ok() {
            frames
        } else {
            0
        }
    }

    fn read(&self, pcm: &mut [i16]) -> i32 {
        loop {
            if !self.wait(100) {
                return 0;
            }
            let p = match self.recv() {
                Ok(Some(p)) => p,
                Ok(None) => return 0,
                Err(e) => return -e,
            };
            if p.header.kind == AU_STATE {
                if p.header.value != 0 {
                    AudioHelper::state_line(&p.header);
                }
                continue;
            }
            if p.header.kind != AU_PCM || p.header.output != 0 {
                continue;
            }
            let have = (p.body.len() / 2).min(p.header.value as usize).min(pcm.len());
            for (out, s) in pcm.iter_mut().zip(samples(&p.body)).take(have) {
                *out = s;
            }
            return have as i32;
        }
    }

    fn probe_latency(&self, timeout_ms: u32) -> Result<u32, i32> {
        self.send(AuHeader { kind: AU_PROBE, output: 1, ..Default::default() }, &[])?;
        let t0 = Instant::now();
        loop {
            let spent = t0.elapsed().as_millis() as u64;
            if spent >= u64::from(timeout_ms) {
                return Err(libc::ETIMEDOUT);
            }
            if !self.wait((u64::from(timeout_ms) - spent) as i32) {
                continue;
            }
            let Some(p) = self.recv()? else { continue };
            if p.header.kind == AU_STATE && p.header.value != 0 {
                AudioHelper::state_line(&p.header);
            }
            if p.header.kind == AU_LATENCY {
                return if p.header.value != 0 { Ok(p.header.value) } else { Err(libc::EIO) };
            }
        }
    }

    fn told(&self, frames: u32) {
        let _ = self.send(AuHeader { kind: AU_TOLD, output: 1, value: frames, ..Default::default() }, &[]);
    }
}
// Dropping the helper closes its socket: the helper exits when its socket closes.
