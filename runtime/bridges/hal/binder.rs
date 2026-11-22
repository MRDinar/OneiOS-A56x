// SPDX-License-Identifier: MIT
//! The kernel's binder protocol (include/uapi/linux/android/binder.h) with no Android library: xnuhost is a static
//! binary. One layer for both kinds of parcel that cross it: the stable-AIDL ones of /dev/binder (the Wi-Fi
//! supplicant, the HALs: `hal::aidl`) and the HIDL ones of /dev/hwbinder (the Bluetooth HAL: `hal::hidl`).
//!
//! A [`Binder`] is the device: its fd and its read-only mapping (where the driver puts what it hands us). Calls into
//! our objects and death notifications go to a [`Handler`]; a [`Looper`] thread reads them while nobody waits on a
//! call, and a thread that waits on its call's reply ([`Binder::transact`]) handles what comes to it meanwhile (a
//! call the callee makes back into us before it answers comes down the same transaction stack). Written from the
//! UAPI and libbinder's IPCThreadState.cpp (AOSP, Apache 2.0). C: the halves of wifi/aidl_binder.c and
//! bt/hci_hidl.c that were the same code twice.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use xh_core::bytes::ne;

/// The numbers of the binder UAPI (64-bit, protocol version 8). Checked against the C header by a test.
pub mod uapi {
    const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
        dir << 30 | (size as u32) << 16 | (ty as u32) << 8 | nr as u32
    }
    const W: u32 = 1;
    const R: u32 = 2;

    /// The size field of an ioctl number: what follows a BR_/BC_ command word.
    pub const fn ioc_size(cmd: u32) -> usize {
        ((cmd >> 16) & 0x3fff) as usize
    }

    pub const TRANSACTION_DATA: usize = 64; // struct binder_transaction_data
    pub const TRANSACTION_DATA_SG: usize = 72; // struct binder_transaction_data_sg
    pub const TRANSACTION_DATA_SECCTX: usize = 72; // struct binder_transaction_data_secctx
    const PTR_COOKIE: usize = 16; // struct binder_ptr_cookie
    const HANDLE_COOKIE: usize = 12; // struct binder_handle_cookie (packed)

    pub const CURRENT_PROTOCOL_VERSION: i32 = 8;
    pub const BINDER_WRITE_READ: u32 = ioc(R | W, b'b', 1, 48);
    pub const BINDER_SET_MAX_THREADS: u32 = ioc(W, b'b', 5, 4);
    pub const BINDER_SET_CONTEXT_MGR: u32 = ioc(W, b'b', 7, 4);
    pub const BINDER_VERSION: u32 = ioc(R | W, b'b', 9, 4);
    pub const BINDER_CTL_ADD: u32 = ioc(R | W, b'b', 1, 264); // binderfs.h: struct binderfs_device

    pub const BC_TRANSACTION: u32 = ioc(W, b'c', 0, TRANSACTION_DATA);
    pub const BC_REPLY: u32 = ioc(W, b'c', 1, TRANSACTION_DATA);
    pub const BC_FREE_BUFFER: u32 = ioc(W, b'c', 3, 8);
    pub const BC_INCREFS: u32 = ioc(W, b'c', 4, 4);
    pub const BC_ACQUIRE: u32 = ioc(W, b'c', 5, 4);
    pub const BC_RELEASE: u32 = ioc(W, b'c', 6, 4);
    pub const BC_DECREFS: u32 = ioc(W, b'c', 7, 4);
    pub const BC_INCREFS_DONE: u32 = ioc(W, b'c', 8, PTR_COOKIE);
    pub const BC_ACQUIRE_DONE: u32 = ioc(W, b'c', 9, PTR_COOKIE);
    pub const BC_ENTER_LOOPER: u32 = ioc(0, b'c', 12, 0);
    pub const BC_EXIT_LOOPER: u32 = ioc(0, b'c', 13, 0);
    pub const BC_REQUEST_DEATH_NOTIFICATION: u32 = ioc(W, b'c', 14, HANDLE_COOKIE);
    pub const BC_CLEAR_DEATH_NOTIFICATION: u32 = ioc(W, b'c', 15, HANDLE_COOKIE);
    pub const BC_DEAD_BINDER_DONE: u32 = ioc(W, b'c', 16, 8);
    pub const BC_TRANSACTION_SG: u32 = ioc(W, b'c', 17, TRANSACTION_DATA_SG);
    pub const BC_REPLY_SG: u32 = ioc(W, b'c', 18, TRANSACTION_DATA_SG);

    pub const BR_TRANSACTION_SEC_CTX: u32 = ioc(R, b'r', 2, TRANSACTION_DATA_SECCTX);
    pub const BR_TRANSACTION: u32 = ioc(R, b'r', 2, TRANSACTION_DATA);
    pub const BR_REPLY: u32 = ioc(R, b'r', 3, TRANSACTION_DATA);
    pub const BR_DEAD_REPLY: u32 = ioc(0, b'r', 5, 0);
    pub const BR_TRANSACTION_COMPLETE: u32 = ioc(0, b'r', 6, 0);
    pub const BR_INCREFS: u32 = ioc(R, b'r', 7, PTR_COOKIE);
    pub const BR_ACQUIRE: u32 = ioc(R, b'r', 8, PTR_COOKIE);
    pub const BR_NOOP: u32 = ioc(0, b'r', 12, 0);
    pub const BR_DEAD_BINDER: u32 = ioc(R, b'r', 15, 8);
    pub const BR_FAILED_REPLY: u32 = ioc(0, b'r', 17, 0);
    pub const BR_FROZEN_REPLY: u32 = ioc(0, b'r', 18, 0);

    pub const TF_ONE_WAY: u32 = 0x01;
    pub const TF_STATUS_CODE: u32 = 0x08;
    pub const TF_ACCEPT_FDS: u32 = 0x10;

    /// Object types of flat_binder_object / binder_buffer_object: B_PACK_CHARS(c1, c2, '*', 0x85).
    pub const TYPE_BINDER: u32 = 0x7362_2a85; // 's','b': an object of the sender
    pub const TYPE_HANDLE: u32 = 0x7368_2a85; // 's','h': a reference to someone's object
    pub const TYPE_PTR: u32 = 0x7074_2a85; // 'p','t': a scatter-gather buffer (HIDL)
    pub const BUFFER_FLAG_HAS_PARENT: u32 = 0x01;
}

use uapi::*;

/// android::UNKNOWN_TRANSACTION (-EBADMSG): the status a call nobody knows is answered with.
pub const UNKNOWN_TRANSACTION: i32 = -74;

/// The driver's mapping: a little less than 1 MiB, as libbinder maps it.
const MAP_SIZE: usize = 1024 * 1024 - 2 * 4096;
/// A call that has no reply in this long is given up (-ETIMEDOUT).
pub const CALL_TIMEOUT_MS: i32 = 5000;

/// The calling thread's errno as the C code returned it: negative.
pub(crate) fn last_errno() -> i32 {
    -io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// strerror(errno) of a positive or negative errno (C's `strerror(rc < 0 ? -rc : rc)`).
pub fn strerror(rc: i32) -> String {
    xh_core::sys::strerror(rc.saturating_abs())
}

/// struct binder_transaction_data (64 bytes), with `data.ptr` as its two words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionData {
    pub target: u64, // a handle (outgoing) or our object's pointer (incoming)
    pub cookie: u64,
    pub code: u32,
    pub flags: u32,
    pub sender_pid: i32,
    pub sender_euid: u32,
    pub data_size: u64,
    pub offsets_size: u64,
    pub buffer: u64,
    pub offsets: u64,
}

impl TransactionData {
    pub fn parse(b: &[u8]) -> TransactionData {
        TransactionData {
            target: ne::u64(b, 0),
            cookie: ne::u64(b, 8),
            code: ne::u32(b, 16),
            flags: ne::u32(b, 20),
            sender_pid: ne::u32(b, 24) as i32,
            sender_euid: ne::u32(b, 28),
            data_size: ne::u64(b, 32),
            offsets_size: ne::u64(b, 40),
            buffer: ne::u64(b, 48),
            offsets: ne::u64(b, 56),
        }
    }
}

/// A batch of commands for one BINDER_WRITE_READ (C: the packed structs written with bwrite).
#[derive(Clone)]
pub struct Commands {
    bytes: [u8; 128],
    len: usize,
}

impl Default for Commands {
    fn default() -> Self {
        Commands { bytes: [0; 128], len: 0 }
    }
}

impl Commands {
    pub fn new() -> Commands {
        Commands::default()
    }
    fn put(mut self, b: &[u8]) -> Commands {
        self.bytes[self.len..self.len + b.len()].copy_from_slice(b);
        self.len += b.len();
        self
    }
    pub fn word(self, v: u32) -> Commands {
        self.put(&v.to_ne_bytes())
    }
    pub fn quad(self, v: u64) -> Commands {
        self.put(&v.to_ne_bytes())
    }
    pub fn transaction(self, t: &TransactionData) -> Commands {
        self.quad(t.target)
            .quad(t.cookie)
            .word(t.code)
            .word(t.flags)
            .word(t.sender_pid as u32)
            .word(t.sender_euid)
            .quad(t.data_size)
            .quad(t.offsets_size)
            .quad(t.buffer)
            .quad(t.offsets)
    }
    /// Our references to someone's object: BC_INCREFS and BC_ACQUIRE.
    pub fn acquire(self, handle: u32) -> Commands {
        self.word(BC_INCREFS).word(handle).word(BC_ACQUIRE).word(handle)
    }
    /// BC_RELEASE and BC_DECREFS.
    pub fn release(self, handle: u32) -> Commands {
        self.word(BC_RELEASE).word(handle).word(BC_DECREFS).word(handle)
    }
    /// A death notification: `cookie` comes back when the object's process dies.
    pub fn watch(self, handle: u32, cookie: u64) -> Commands {
        self.word(BC_REQUEST_DEATH_NOTIFICATION).word(handle).quad(cookie)
    }
    pub fn unwatch(self, handle: u32, cookie: u64) -> Commands {
        self.word(BC_CLEAR_DEATH_NOTIFICATION).word(handle).quad(cookie)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// struct binder_write_read
#[repr(C)]
#[derive(Default)]
struct WriteRead {
    write_size: u64,
    write_consumed: u64,
    write_buffer: u64,
    read_size: u64,
    read_consumed: u64,
    read_buffer: u64,
}

/// What a transaction sends: a parcel's data and the offsets of its objects, and for a scatter-gather (HIDL)
/// transaction the size of its buffers. Every pointer in it must stay valid until the transaction is written.
pub trait Outgoing {
    fn data(&self) -> &[u8];
    fn offsets(&self) -> &[u64];
    /// Some: a BC_TRANSACTION_SG / BC_REPLY_SG with this buffers_size.
    fn buffers_size(&self) -> Option<u64> {
        None
    }
    /// The parcel did not fit: it is never sent.
    fn overflow(&self) -> bool;
}

/// Calls into our objects and death notifications, from the looper or from a thread waiting on its call.
pub trait Handler: Send + Sync {
    /// A call into one of our objects (`tr.target()`: its pointer, 0 for a context manager's handle 0). Answer it
    /// with [`Incoming::finish`]; one dropped unanswered is given back and answered UNKNOWN_TRANSACTION.
    fn transaction(&self, tr: Incoming<'_>);
    /// The process of an object we watch died (BC_DEAD_BINDER_DONE is already sent).
    fn dead(&self, cookie: u64) {
        let _ = cookie;
    }
}

/// A handler for a binder that serves no object (a lookup only).
pub struct NoObjects;
impl Handler for NoObjects {
    fn transaction(&self, _tr: Incoming<'_>) {}
}

/// Why a binder device did not open.
#[derive(Debug)]
pub enum OpenError {
    /// open(2) failed.
    Open(io::Error),
    /// BINDER_VERSION, the mapping or BINDER_SET_MAX_THREADS failed; `protocol` is what the driver reported.
    Setup { protocol: i32, error: io::Error },
}

impl OpenError {
    pub fn errno(&self) -> i32 {
        match self {
            OpenError::Open(e) | OpenError::Setup { error: e, .. } => e.raw_os_error().unwrap_or(libc::EIO),
        }
    }
}

/// An open binder device.
pub struct Binder {
    fd: OwnedFd,
    map: *mut libc::c_void,
}

// SAFETY: the fd is used through ioctls, which the driver serialises per thread; the mapping is only kept to be
// unmapped (the driver's buffers in it are read through slices whose lifetime ends with BC_FREE_BUFFER).
unsafe impl Send for Binder {}
// SAFETY: as above: nothing in Binder is mutated through &self.
unsafe impl Sync for Binder {}

/// What a waiting call's reply is so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    Pending,
    Replied(TransactionData),
    Failed(i32),
}

/// What one read gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Nothing,
    Some,
}

impl Binder {
    /// Opens the device as libbinder does: the protocol version checked, the mapping made, no threads of ours
    /// spawned on the driver's request (BINDER_SET_MAX_THREADS 0).
    pub fn open(path: &Path) -> Result<Binder, OpenError> {
        let c = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| OpenError::Open(io::Error::from_raw_os_error(libc::EINVAL)))?;
        // SAFETY: a NUL-terminated path; the fd returned is owned below.
        let raw = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if raw < 0 {
            return Err(OpenError::Open(io::Error::last_os_error()));
        }
        // SAFETY: raw is a new fd of ours.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut protocol: i32 = 0;
        let setup = |protocol: i32, error: Option<io::Error>| OpenError::Setup {
            protocol,
            error: error.unwrap_or_else(|| io::Error::from_raw_os_error(libc::EPROTO)),
        };
        // SAFETY: BINDER_VERSION writes one i32 (struct binder_version).
        if unsafe { libc::ioctl(raw, BINDER_VERSION as _, &mut protocol as *mut i32) } < 0 {
            return Err(setup(protocol, Some(io::Error::last_os_error())));
        }
        if protocol != CURRENT_PROTOCOL_VERSION {
            return Err(setup(protocol, None));
        }
        // SAFETY: a new private read-only mapping of the device; unmapped in Drop.
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), MAP_SIZE, libc::PROT_READ, libc::MAP_PRIVATE | libc::MAP_NORESERVE, raw, 0)
        };
        if map == libc::MAP_FAILED {
            return Err(setup(protocol, Some(io::Error::last_os_error())));
        }
        let binder = Binder { fd, map };
        let threads: u32 = 0;
        // SAFETY: BINDER_SET_MAX_THREADS reads one u32.
        if unsafe { libc::ioctl(raw, BINDER_SET_MAX_THREADS as _, &threads as *const u32) } < 0 {
            return Err(setup(protocol, Some(io::Error::last_os_error())));
        }
        Ok(binder)
    }

    /// The device named by `env` (ONEIOS_BINDER, ONEIOS_HWBINDER: the PC test's binderfs), else `default`.
    pub fn path_from_env(env: &str, default: &str) -> String {
        match std::env::var(env) {
            Ok(v) if !v.is_empty() => v,
            _ => default.to_owned(),
        }
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    fn write_read(&self, bwr: &mut WriteRead) -> io::Result<()> {
        // SAFETY: bwr's buffers point at memory of the caller that lives through the call; the driver reads
        // write_size bytes and writes at most read_size.
        if unsafe { libc::ioctl(self.fd(), BINDER_WRITE_READ as _, bwr as *mut WriteRead) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Writes commands. 0, or -errno.
    pub fn write(&self, bytes: &[u8]) -> Result<(), i32> {
        let mut bwr =
            WriteRead { write_size: bytes.len() as u64, write_buffer: bytes.as_ptr() as u64, ..Default::default() };
        while bwr.write_consumed < bwr.write_size {
            if let Err(e) = self.write_read(&mut bwr) {
                let errno = e.raw_os_error().unwrap_or(libc::EIO);
                if errno != libc::EINTR && errno != libc::EAGAIN {
                    return Err(-errno);
                }
            }
        }
        Ok(())
    }

    pub fn write_cmds(&self, c: &Commands) -> Result<(), i32> {
        self.write(c.as_bytes())
    }

    pub fn acquire(&self, handle: u32) -> Result<(), i32> {
        self.write_cmds(&Commands::new().acquire(handle))
    }
    pub fn release(&self, handle: u32) {
        let _ = self.write_cmds(&Commands::new().release(handle));
    }
    pub fn watch(&self, handle: u32, cookie: u64) -> Result<(), i32> {
        self.write_cmds(&Commands::new().watch(handle, cookie))
    }

    pub fn free_buffer(&self, buffer: u64) {
        let _ = self.write_cmds(&Commands::new().word(BC_FREE_BUFFER).quad(buffer));
    }

    pub fn enter_looper(&self) {
        let _ = self.write_cmds(&Commands::new().word(BC_ENTER_LOOPER));
    }
    pub fn exit_looper(&self) {
        let _ = self.write_cmds(&Commands::new().word(BC_EXIT_LOOPER));
    }

    /// A transaction (`reply` false: BC_TRANSACTION[_SG] to `handle`) or a reply (BC_REPLY[_SG]).
    fn send(&self, reply: bool, handle: u32, code: u32, flags: u32, out: &dyn Outgoing) -> Result<(), i32> {
        let tr = TransactionData {
            target: handle as u64,
            code,
            flags,
            data_size: out.data().len() as u64,
            offsets_size: (out.offsets().len() * 8) as u64,
            buffer: out.data().as_ptr() as u64,
            offsets: out.offsets().as_ptr() as u64,
            ..Default::default()
        };
        let c = match out.buffers_size() {
            Some(buffers) => {
                Commands::new().word(if reply { BC_REPLY_SG } else { BC_TRANSACTION_SG }).transaction(&tr).quad(buffers)
            }
            None => Commands::new().word(if reply { BC_REPLY } else { BC_TRANSACTION }).transaction(&tr),
        };
        self.write_cmds(&c)
    }

    /// A reply that is a status code alone (TF_STATUS_CODE).
    pub fn reply_status(&self, status: i32) {
        let tr = TransactionData {
            flags: TF_STATUS_CODE,
            data_size: 4,
            buffer: &status as *const i32 as u64,
            ..Default::default()
        };
        let _ = self.write_cmds(&Commands::new().word(BC_REPLY).transaction(&tr));
    }

    /// A one-way transaction. 0 or -errno (-E2BIG: the parcel did not fit).
    pub fn oneway(&self, handle: u32, code: u32, out: &dyn Outgoing) -> Result<(), i32> {
        if out.overflow() {
            return Err(-libc::E2BIG);
        }
        self.send(false, handle, code, TF_ONE_WAY | TF_ACCEPT_FDS, out)
    }

    /// Reads what the driver has for this thread. `wait`: a call waits (its reply, or why it failed).
    fn process(&self, handler: &dyn Handler, mut wait: Option<&mut Wait>) -> Result<Read, i32> {
        let mut buffer = [0u8; 512];
        let mut bwr =
            WriteRead { read_size: buffer.len() as u64, read_buffer: buffer.as_mut_ptr() as u64, ..Default::default() };
        if let Err(e) = self.write_read(&mut bwr) {
            let errno = e.raw_os_error().unwrap_or(libc::EIO);
            return if errno == libc::EAGAIN || errno == libc::EINTR { Ok(Read::Nothing) } else { Err(-errno) };
        }
        if bwr.read_consumed == 0 {
            return Ok(Read::Nothing);
        }
        let got = &buffer[..bwr.read_consumed as usize];
        let mut at = 0;
        while at + 4 <= got.len() {
            let cmd = ne::u32(got, at);
            at += 4;
            let size = ioc_size(cmd);
            if size > got.len() - at {
                break;
            }
            let body = &got[at..at + size];
            match cmd {
                BR_INCREFS | BR_ACQUIRE => {
                    let done = if cmd == BR_INCREFS { BC_INCREFS_DONE } else { BC_ACQUIRE_DONE };
                    let _ = self.write_cmds(&Commands::new().word(done).quad(ne::u64(body, 0)).quad(ne::u64(body, 8)));
                }
                BR_TRANSACTION_SEC_CTX | BR_TRANSACTION => {
                    handler.transaction(Incoming { binder: self, tr: TransactionData::parse(body), done: false });
                }
                BR_REPLY => {
                    let tr = TransactionData::parse(body);
                    match wait.as_deref_mut() {
                        Some(w @ Wait::Pending) => *w = Wait::Replied(tr),
                        _ => self.free_buffer(tr.buffer),
                    }
                }
                BR_DEAD_REPLY | BR_FAILED_REPLY | BR_FROZEN_REPLY => {
                    if let Some(w @ Wait::Pending) = wait.as_deref_mut() {
                        *w = Wait::Failed(if cmd == BR_DEAD_REPLY { -libc::EPIPE } else { -libc::EIO });
                    }
                }
                BR_DEAD_BINDER => {
                    let cookie = ne::u64(body, 0);
                    let _ = self.write_cmds(&Commands::new().word(BC_DEAD_BINDER_DONE).quad(cookie));
                    handler.dead(cookie);
                }
                _ => {} // BR_NOOP, BR_TRANSACTION_COMPLETE, BR_SPAWN_LOOPER, BR_RELEASE, BR_DECREFS, ...
            }
            at += size;
        }
        Ok(Read::Some)
    }

    /// Waits up to `ms` for something to read; false on the timeout.
    fn wait_readable(&self, ms: i32) -> bool {
        let mut p = libc::pollfd { fd: self.fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one pollfd of ours.
        unsafe { libc::poll(&mut p, 1, ms) != 0 }
    }

    /// A two-way call and its reply (calls into our objects that come meanwhile go to `handler`). The reply's
    /// buffer goes back to the driver when the [`Reply`] is dropped. -errno: -E2BIG (the parcel did not fit),
    /// -ETIMEDOUT (no reply in 5 s), -EPIPE (the object's process died), -EIO (a failed reply).
    pub fn transact(
        &self,
        handle: u32,
        code: u32,
        out: &dyn Outgoing,
        handler: &dyn Handler,
    ) -> Result<Reply<'_>, i32> {
        if out.overflow() {
            return Err(-libc::E2BIG);
        }
        self.send(false, handle, code, TF_ACCEPT_FDS, out)?;
        let mut wait = Wait::Pending;
        loop {
            let read = self.process(handler, Some(&mut wait))?;
            match wait {
                Wait::Pending => {}
                Wait::Replied(tr) => return Ok(Reply { binder: self, tr }),
                Wait::Failed(rc) => return Err(rc),
            }
            if read == Read::Nothing && !self.wait_readable(CALL_TIMEOUT_MS) {
                return Err(-libc::ETIMEDOUT);
            }
        }
    }

    /// Becomes the context manager (handle 0: the tests' service manager). A context manager that just died is
    /// freed in deferred work: EBUSY is tried again for 2 s.
    pub fn context_manager(&self) -> Result<(), i32> {
        for _ in 0..40 {
            // SAFETY: BINDER_SET_CONTEXT_MGR with no argument the driver reads.
            if unsafe { libc::ioctl(self.fd(), BINDER_SET_CONTEXT_MGR as _, 0) } == 0 {
                return Ok(());
            }
            let rc = last_errno();
            if rc != -libc::EBUSY {
                return Err(rc);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(-libc::EBUSY)
    }
}

impl Drop for Binder {
    fn drop(&mut self) {
        // SAFETY: the mapping made in open; nothing refers to it any more (Reply and Incoming borrow the Binder).
        unsafe { libc::munmap(self.map, MAP_SIZE) };
        // the fd closes after: the driver drops our objects and references
    }
}

/// The parts of a transaction the driver handed over, in our mapping until BC_FREE_BUFFER.
fn kernel_slices(tr: &TransactionData) -> (&[u8], &[u8]) {
    let slice = |ptr: u64, len: u64| -> &[u8] {
        if ptr == 0 || len == 0 {
            return &[];
        }
        // SAFETY: the driver put len bytes at ptr in our read-only mapping and keeps them until we free the buffer,
        // which happens only when the Reply or Incoming that owns tr (and so this slice's borrow) ends.
        unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
    };
    (slice(tr.buffer, tr.data_size), slice(tr.offsets, tr.offsets_size))
}

/// The reply of a call; its buffer goes back to the driver when this is dropped.
pub struct Reply<'b> {
    binder: &'b Binder,
    tr: TransactionData,
}

impl Reply<'_> {
    /// A reply that is a status code alone: Some(status) (-EIO when it carries none).
    pub fn status_code(&self) -> Option<i32> {
        if self.tr.flags & TF_STATUS_CODE == 0 {
            return None;
        }
        let (data, _) = self.parts();
        Some(if data.len() >= 4 { ne::u32(data, 0) as i32 } else { -libc::EIO })
    }
    /// The data and the offsets array (as bytes: 8 per object).
    pub fn parts(&self) -> (&[u8], &[u8]) {
        kernel_slices(&self.tr)
    }
}

impl Drop for Reply<'_> {
    fn drop(&mut self) {
        self.binder.free_buffer(self.tr.buffer);
    }
}

/// A call into one of our objects. [`Incoming::finish`] gives its buffer back and answers it.
pub struct Incoming<'b> {
    binder: &'b Binder,
    tr: TransactionData,
    done: bool,
}

impl Incoming<'_> {
    pub fn binder(&self) -> &Binder {
        self.binder
    }
    pub fn code(&self) -> u32 {
        self.tr.code
    }
    pub fn one_way(&self) -> bool {
        self.tr.flags & TF_ONE_WAY != 0
    }
    /// The object's pointer as we sent it (0: a call to the context manager).
    pub fn target(&self) -> u64 {
        self.tr.target
    }
    pub fn cookie(&self) -> u64 {
        self.tr.cookie
    }
    pub fn parts(&self) -> (&[u8], &[u8]) {
        kernel_slices(&self.tr)
    }

    /// Gives the buffer back (which also lets the driver hand over the object's next one-way call), then answers a
    /// two-way call: the reply parcel, or a status (Err; also for a reply that did not fit: UNKNOWN_TRANSACTION).
    pub fn finish(mut self, answer: Result<&dyn Outgoing, i32>) {
        self.done = true;
        self.binder.free_buffer(self.tr.buffer);
        if self.one_way() {
            return;
        }
        match answer {
            Ok(reply) if !reply.overflow() => {
                let _ = self.binder.send(true, 0, 0, 0, reply);
            }
            Ok(_) => self.binder.reply_status(UNKNOWN_TRANSACTION),
            Err(status) => self.binder.reply_status(if status != 0 { status } else { UNKNOWN_TRANSACTION }),
        }
    }
}

impl Drop for Incoming<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.binder.free_buffer(self.tr.buffer);
            if !self.one_way() {
                self.binder.reply_status(UNKNOWN_TRANSACTION);
            }
        }
    }
}

/// The thread that takes calls into our objects and death notifications while no call of ours waits.
pub struct Looper {
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 writes two fds, owned below.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: two new fds of ours.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

impl Looper {
    pub fn start(binder: Arc<Binder>, handler: Arc<dyn Handler>) -> io::Result<Looper> {
        let (read, wake) = pipe()?;
        let thread = std::thread::Builder::new().spawn(move || {
            binder.enter_looper();
            let mut fds = [
                libc::pollfd { fd: binder.fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: read.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            loop {
                // SAFETY: two pollfds of ours.
                if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 && last_errno() != -libc::EINTR {
                    break;
                }
                if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                    break;
                }
                if fds[0].revents & libc::POLLIN != 0 {
                    while binder.process(&*handler, None) == Ok(Read::Some) {}
                }
            }
            binder.exit_looper();
        })?;
        Ok(Looper { wake, thread: Some(thread) })
    }
}

impl Drop for Looper {
    fn drop(&mut self) {
        // SAFETY: one byte to our pipe's write end.
        let _ = unsafe { libc::write(self.wake.as_raw_fd(), c"".as_ptr().cast(), 1) };
        if let Some(t) = self.thread.take() {
            // dropped from its own thread (a handler held the last reference to the owner): it ends by itself
            if t.thread().id() != std::thread::current().id() {
                let _ = t.join();
            }
        }
    }
}

/// The offsets of a parcel's objects as the bytes the driver reads (8 per object).
pub fn offsets_bytes(offsets: &[u64]) -> &[u8] {
    // SAFETY: u64 has no padding and every byte of it is a valid u8; the length is the same memory's.
    unsafe { std::slice::from_raw_parts(offsets.as_ptr().cast::<u8>(), offsets.len() * 8) }
}

/// The i-th offset of an offsets array given as bytes.
pub fn offset_at(offsets: &[u8], i: usize) -> Option<u64> {
    offsets.get(i * 8..i * 8 + 8).map(|b| u64::from_ne_bytes(b.try_into().expect("8 bytes")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every number above against the kernel's header, when the build host has it and a C compiler.
    #[test]
    fn uapi_matches_the_c_header() {
        let v = |x: u32| i64::from(x);
        #[rustfmt::skip]
        let pairs: &[(&str, i64)] = &[
            ("BINDER_WRITE_READ", v(BINDER_WRITE_READ)), ("BINDER_SET_MAX_THREADS", v(BINDER_SET_MAX_THREADS)),
            ("BINDER_SET_CONTEXT_MGR", v(BINDER_SET_CONTEXT_MGR)), ("BINDER_VERSION", v(BINDER_VERSION)),
            ("BINDER_CTL_ADD", v(BINDER_CTL_ADD)), ("BC_TRANSACTION", v(BC_TRANSACTION)), ("BC_REPLY", v(BC_REPLY)),
            ("BC_FREE_BUFFER", v(BC_FREE_BUFFER)), ("BC_INCREFS", v(BC_INCREFS)), ("BC_ACQUIRE", v(BC_ACQUIRE)),
            ("BC_RELEASE", v(BC_RELEASE)), ("BC_DECREFS", v(BC_DECREFS)), ("BC_INCREFS_DONE", v(BC_INCREFS_DONE)),
            ("BC_ACQUIRE_DONE", v(BC_ACQUIRE_DONE)), ("BC_ENTER_LOOPER", v(BC_ENTER_LOOPER)), ("BC_EXIT_LOOPER", v(BC_EXIT_LOOPER)),
            ("BC_REQUEST_DEATH_NOTIFICATION", v(BC_REQUEST_DEATH_NOTIFICATION)),
            ("BC_CLEAR_DEATH_NOTIFICATION", v(BC_CLEAR_DEATH_NOTIFICATION)), ("BC_DEAD_BINDER_DONE", v(BC_DEAD_BINDER_DONE)),
            ("BC_TRANSACTION_SG", v(BC_TRANSACTION_SG)), ("BC_REPLY_SG", v(BC_REPLY_SG)),
            ("BR_TRANSACTION_SEC_CTX", v(BR_TRANSACTION_SEC_CTX)), ("BR_TRANSACTION", v(BR_TRANSACTION)), ("BR_REPLY", v(BR_REPLY)),
            ("BR_DEAD_REPLY", v(BR_DEAD_REPLY)), ("BR_TRANSACTION_COMPLETE", v(BR_TRANSACTION_COMPLETE)),
            ("BR_INCREFS", v(BR_INCREFS)), ("BR_ACQUIRE", v(BR_ACQUIRE)), ("BR_NOOP", v(BR_NOOP)), ("BR_DEAD_BINDER", v(BR_DEAD_BINDER)),
            ("BR_FAILED_REPLY", v(BR_FAILED_REPLY)), ("BR_FROZEN_REPLY", v(BR_FROZEN_REPLY)),
            ("TF_ONE_WAY", v(TF_ONE_WAY)), ("TF_STATUS_CODE", v(TF_STATUS_CODE)), ("TF_ACCEPT_FDS", v(TF_ACCEPT_FDS)),
            ("BINDER_TYPE_BINDER", v(TYPE_BINDER)), ("BINDER_TYPE_HANDLE", v(TYPE_HANDLE)), ("BINDER_TYPE_PTR", v(TYPE_PTR)),
            ("BINDER_BUFFER_FLAG_HAS_PARENT", v(BUFFER_FLAG_HAS_PARENT)),
            ("BINDER_CURRENT_PROTOCOL_VERSION", CURRENT_PROTOCOL_VERSION.into()),
            ("sizeof(struct binder_transaction_data)", TRANSACTION_DATA as i64),
            ("sizeof(struct binder_transaction_data_sg)", TRANSACTION_DATA_SG as i64),
            ("sizeof(struct binder_transaction_data_secctx)", TRANSACTION_DATA_SECCTX as i64),
            ("sizeof(struct binder_write_read)", std::mem::size_of::<WriteRead>() as i64),
            ("sizeof(struct flat_binder_object)", 24), ("sizeof(struct binder_buffer_object)", 40),
            ("sizeof(struct binderfs_device)", 264),
        ];
        crate::hal::test_c::check("binder_uapi", &["linux/android/binder.h", "linux/android/binderfs.h"], pairs);
    }

    #[test]
    fn strerror_is_c_text() {
        assert_eq!(strerror(-libc::ENOENT), "No such file or directory");
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
    }

    #[test]
    fn commands_pack_as_the_c_structs() {
        let c = Commands::new().acquire(7).watch(7, 0x1122_3344_5566_7788);
        let b = c.as_bytes();
        assert_eq!(b.len(), 16 + 16);
        assert_eq!(ne::u32(b, 0), BC_INCREFS);
        assert_eq!(ne::u32(b, 4), 7);
        assert_eq!(ne::u32(b, 8), BC_ACQUIRE);
        assert_eq!(ne::u32(b, 16), BC_REQUEST_DEATH_NOTIFICATION);
        assert_eq!(ne::u64(b, 24), 0x1122_3344_5566_7788);
    }
}
