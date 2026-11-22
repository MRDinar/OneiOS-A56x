// SPDX-License-Identifier: MIT
//! The Bluetooth HAL client (bt::hci_hidl) against a real binder driver: a private binderfs on the build host (the
//! test runs itself in a user namespace with one: hal::binderfs, no root). A child process plays hwservicemanager
//! (the context manager) and android.hardware.bluetooth@1.1::IBluetoothHci/default, and reads every parcel the way
//! libhwbinder does (readCString tokens, readBuffer/readEmbeddedBuffer with their parent checks), independently of
//! hal::hidl's reader. It calls back like the QTI HAL: from its own thread, two-way, and for a command before it
//! answers the command (a HAL that holds a lock across the callback).
//!
//! Scenarios: a 1.1 HAL (probe, open, interfaceChain on our callback, initializationComplete, HCI Reset and Read
//! BD_ADDR answered with Command Complete, ACL echoed, close); a 1.0-only HAL; no HAL registered; a HAL that never
//! completes initialisation (the timeout); a HAL that dies.
//! Not tested here: the real libhwbinder, SELinux, the QTI HAL itself. That is `bridgectl bt` on the phone.
//! C: tests/test_hidl_binder.c.

mod common;

use onebridges::bt::hci::{Hci, HciState};
use onebridges::bt::{hci_hidl, hidl_parcel::*};
use onebridges::hal::binder::uapi::*;
use onebridges::hal::binder::{Commands, TransactionData};
use onebridges::hal::binderfs::{self, Entered};
use onebridges::hal::hidl;
use std::ffi::CStr;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use xh_core::bytes::ne;

// ================================ the fake hwservicemanager + HAL (child) ================================
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ok,
    V10,
    Absent,
    Silent,
}
const HAL_PTR: u64 = 0x48_414c;

static SFD: OnceLock<i32> = OnceLock::new();
static SMODE: OnceLock<Mode> = OnceLock::new();
static SFAIL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

macro_rules! scheck {
    ($cond:expr) => {{
        if !($cond) {
            SFAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            println!("FAKE HAL FAIL {}:{}: {}", file!(), line!(), stringify!($cond));
        }
    }};
}

fn sfd() -> i32 {
    *SFD.get().expect("the fake's binder")
}

/// struct binder_write_read
#[repr(C)]
#[derive(Default)]
struct Bwr {
    write_size: u64,
    write_consumed: u64,
    write_buffer: u64,
    read_size: u64,
    read_consumed: u64,
    read_buffer: u64,
}

fn ioctl_bwr(b: &mut Bwr) -> bool {
    // SAFETY: the buffers in b live through the call.
    unsafe { xh_core::ioctl::ioctl(sfd(), BINDER_WRITE_READ, b) }.is_ok()
}

fn swrite(data: &[u8]) {
    let mut b = Bwr { write_size: data.len() as u64, write_buffer: data.as_ptr() as u64, ..Default::default() };
    while b.write_consumed < b.write_size {
        if !ioctl_bwr(&mut b) && xh_core::sys::errno() != libc::EINTR {
            println!("fake hal: write: {}", std::io::Error::last_os_error());
            common::exit(90);
        }
    }
}

/// Reads what the driver has: the bytes of the commands.
fn sread(buf: &mut [u8]) -> usize {
    loop {
        let mut b = Bwr { read_size: buf.len() as u64, read_buffer: buf.as_mut_ptr() as u64, ..Default::default() };
        if ioctl_bwr(&mut b) {
            return b.read_consumed as usize;
        }
        if xh_core::sys::errno() != libc::EINTR {
            println!("fake hal: read: {}", std::io::Error::last_os_error());
            common::exit(92);
        }
    }
}

/// Memory the driver gave or a pointer of ours: `len` bytes at `ptr`.
fn mem<'a>(ptr: u64, len: usize) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    // SAFETY: a pointer the driver fixed up into our mapping (or our own), valid while its transaction is held.
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }
}

/// A parcel being read, with libhwbinder's checks.
struct Rd<'a> {
    d: &'a [u8],
    pos: usize,
    off: Vec<u64>,
    next: usize,
    bad: bool,
}

impl<'a> Rd<'a> {
    fn new(tr: &TransactionData) -> Rd<'a> {
        let d = mem(tr.buffer, tr.data_size as usize);
        let o = mem(tr.offsets, tr.offsets_size as usize);
        Rd { d, pos: 0, off: (0..o.len() / 8).map(|i| ne::u64(o, i * 8)).collect(), next: 0, bad: false }
    }
    fn cstr(&mut self) -> &'a CStr {
        if self.bad || self.pos >= self.d.len() {
            self.bad = true;
            return c"";
        }
        match CStr::from_bytes_until_nul(&self.d[self.pos..]) {
            Ok(s) => {
                self.pos += (s.to_bytes().len() + 1 + 3) & !3;
                s
            }
            Err(_) => {
                self.bad = true;
                c""
            }
        }
    }
    fn i32(&mut self) -> i32 {
        if self.bad || self.pos + 4 > self.d.len() {
            self.bad = true;
            return 0;
        }
        let v = ne::u32(self.d, self.pos) as i32;
        self.pos += 4;
        v
    }
    /// readObject: the object at the read position must be the next one in the offsets.
    fn object(&mut self, size: usize) -> Option<&'a [u8]> {
        if self.bad
            || self.next >= self.off.len()
            || self.off[self.next] != self.pos as u64
            || self.pos + size > self.d.len()
        {
            self.bad = true;
            return None;
        }
        let o = &self.d[self.pos..self.pos + size];
        self.pos += size;
        self.next += 1;
        Some(o)
    }
    /// readBuffer (no parent) / readEmbeddedBuffer: type PTR, the length asked for, the parent link. Objects may sit
    /// at any 4-byte offset (the driver's rule), so they are copied out.
    fn buffer(&mut self, length: usize, parent: Option<(usize, usize)>) -> Option<(usize, hidl::BufferObject)> {
        let index = self.next;
        let b = hidl::BufferObject::parse(self.object(40)?);
        let mut ok = b.kind == TYPE_PTR && b.length == length as u64;
        match parent {
            None => ok = ok && b.flags & BUFFER_FLAG_HAS_PARENT == 0,
            Some((parent, parent_offset)) => {
                ok = ok
                    && b.flags & BUFFER_FLAG_HAS_PARENT != 0
                    && b.parent == parent as u64
                    && b.parent_offset == parent_offset as u64;
                // libhwbinder: the parent's pointer at that offset is this buffer (the driver fixed it up)
                let at = self.off[parent] as usize;
                let p = hidl::BufferObject::parse(&self.d[at..at + 40]);
                ok = ok && ne::u64(mem(p.buffer + parent_offset as u64, 8), 0) == b.buffer;
            }
        }
        if !ok {
            self.bad = true;
            return None;
        }
        Some((index, b))
    }
    fn vec(&mut self, string: bool) -> Option<&'a [u8]> {
        let (top_index, top) = self.buffer(16, None)?;
        let size = ne::u32(mem(top.buffer, 16), 8) as usize;
        let (_, child) = self.buffer(size + usize::from(string), Some((top_index, 0)))?;
        let data = mem(child.buffer, size + usize::from(string));
        if string && data[size] != 0 {
            self.bad = true;
            return None;
        }
        Some(&data[..size])
    }
}

/// A parcel being written. Boxed: the refs it points at keep their address.
#[derive(Default)]
struct Wr {
    d: Vec<u8>,
    off: Vec<u64>,
    buffers: u64,
    refs: Vec<[u8; 16]>,
}

impl Wr {
    fn new() -> Box<Wr> {
        Box::new(Wr { d: Vec::with_capacity(1024), refs: Vec::with_capacity(4), ..Default::default() })
    }
    fn bytes(&mut self, p: &[u8]) {
        self.d.extend_from_slice(p);
        self.d.resize((self.d.len() + 3) & !3, 0);
    }
    fn object(&mut self, o: &[u8]) {
        self.off.push(self.d.len() as u64);
        self.bytes(o);
    }
    fn i32(&mut self, v: i32) {
        self.bytes(&v.to_ne_bytes());
    }
    fn token(&mut self, t: &CStr) {
        self.bytes(t.to_bytes_with_nul());
    }
    fn vec(&mut self, data: &[u8]) {
        let mut r = [0u8; 16];
        r[..8].copy_from_slice(&(data.as_ptr() as u64).to_ne_bytes());
        r[8..12].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        self.refs.push(r);
        let rp = self.refs.last().unwrap().as_ptr() as u64;
        let parent = self.off.len() as u64;
        let top = hidl::BufferObject { kind: TYPE_PTR, buffer: rp, length: 16, ..Default::default() };
        self.object(&top.to_bytes());
        let child = hidl::BufferObject {
            kind: TYPE_PTR,
            flags: BUFFER_FLAG_HAS_PARENT,
            buffer: data.as_ptr() as u64,
            length: data.len() as u64,
            parent,
            parent_offset: 0,
        };
        self.object(&child.to_bytes());
        self.buffers += 16 + ((data.len() as u64 + 7) & !7);
    }
    fn sg(&self, cmd: u32, handle: u32, code: u32, flags: u32) -> Commands {
        let tr = TransactionData {
            target: handle as u64,
            code,
            flags,
            data_size: self.d.len() as u64,
            offsets_size: (self.off.len() * 8) as u64,
            buffer: self.d.as_ptr() as u64,
            offsets: self.off.as_ptr() as u64,
            ..Default::default()
        };
        Commands::new().word(cmd).transaction(&tr).quad(self.buffers)
    }
}

fn sreply(w: &Wr) {
    swrite(w.sg(BC_REPLY_SG, 0, 0, 0).as_bytes());
}
fn sfree(buffer: u64) {
    swrite(Commands::new().word(BC_FREE_BUFFER).quad(buffer).as_bytes());
}

// ---- the HAL's reader thread: callbacks into the client, two-way, one at a time ----
struct Job {
    code: u32,
    data: Vec<u8>,
}
#[derive(Default)]
struct Jobs {
    queue: std::collections::VecDeque<Job>,
    done: u32,
    posted: u32,
    cb_handle: Option<u32>,
}
static JOBS: Mutex<Jobs> =
    Mutex::new(Jobs { queue: std::collections::VecDeque::new(), done: 0, posted: 0, cb_handle: None });
static JCOND: Condvar = Condvar::new();

/// One two-way call to the client's callback; true when a normal reply came. `check` sees the reply.
fn scall(cb: u32, code: u32, w: &Wr, check: Option<fn(&mut Rd<'_>) -> bool>) -> bool {
    swrite(w.sg(BC_TRANSACTION_SG, cb, code, TF_ACCEPT_FDS).as_bytes());
    let mut buf = [0u8; 256];
    loop {
        let n = sread(&mut buf);
        let mut at = 0;
        while at + 4 <= n {
            let cmd = ne::u32(&buf, at);
            at += 4;
            if cmd == BR_REPLY {
                let tr = TransactionData::parse(&buf[at..at + 64]);
                let mut ok = tr.flags & TF_STATUS_CODE == 0;
                if ok {
                    let mut r = Rd::new(&tr);
                    ok = r.i32() == 0 && !r.bad && check.is_none_or(|c| c(&mut r));
                }
                sfree(tr.buffer);
                return ok;
            }
            if cmd == BR_DEAD_REPLY || cmd == BR_FAILED_REPLY {
                return false;
            }
            at += ioc_size(cmd);
        }
    }
}

fn check_chain(r: &mut Rd<'_>) -> bool {
    let Some((top, v)) = r.buffer(16, None) else { return false };
    if ne::u32(mem(v.buffer, 16), 8) != 3 {
        return false;
    }
    let Some((array, a)) = r.buffer(3 * 16, Some((top, 0))) else { return false };
    let items = mem(a.buffer, 48);
    let want = [BT_CALLBACKS_1_1, BT_CALLBACKS_1_0, HIDL_BASE];
    for (i, w) in want.iter().enumerate() {
        let size = ne::u32(items, i * 16 + 8) as usize;
        let Some((_, b)) = r.buffer(size + 1, Some((array, i * 16))) else { return false };
        if mem(b.buffer, size + 1) != w.to_bytes_with_nul() {
            return false;
        }
    }
    !r.bad
}

fn reader() {
    loop {
        let (job, cb) = {
            let mut j = JCOND.wait_while(JOBS.lock().unwrap(), |j| j.queue.is_empty()).unwrap();
            (j.queue.pop_front().unwrap(), j.cb_handle.unwrap_or(0))
        };
        let mut w = Wr::new();
        let ok = if job.code == hidl::INTERFACE_CHAIN {
            w.token(HIDL_BASE);
            scall(cb, job.code, &w, Some(check_chain))
        } else if job.code == BT_CB_INIT_COMPLETE {
            w.token(BT_CALLBACKS_1_0);
            w.i32(0);
            scall(cb, job.code, &w, None)
        } else {
            w.token(BT_CALLBACKS_1_0);
            w.vec(&job.data);
            scall(cb, job.code, &w, None)
        };
        scheck!(ok);
        println!("  fake HAL: callback {:#x} answered: {}", job.code, if ok { "yes" } else { "NO" });
        let mut j = JOBS.lock().unwrap();
        j.done += 1;
        JCOND.notify_all();
    }
}

fn post(code: u32, data: &[u8]) -> u32 {
    let mut j = JOBS.lock().unwrap();
    j.queue.push_back(Job { code, data: data.to_vec() });
    j.posted += 1;
    JCOND.notify_all();
    j.posted
}
fn wait_job(ticket: u32) {
    let _j = JCOND.wait_while(JOBS.lock().unwrap(), |j| j.done < ticket).unwrap();
}

fn serve(tr: &TransactionData) {
    let mode = *SMODE.get().unwrap();
    let mut r = Rd::new(tr);
    let mut w = Wr::new();
    let reply = tr.flags & TF_ONE_WAY == 0;
    if tr.target == 0 {
        // hwservicemanager
        scheck!(tr.code == 1);
        scheck!(r.cstr() == HIDL_MANAGER);
        let fq = r.vec(true).unwrap_or_default();
        let instance = r.vec(true).unwrap_or_default();
        scheck!(!r.bad);
        scheck!(instance == b"default");
        let (is11, is10) = (fq == BT_HCI_1_1.to_bytes(), fq == BT_HCI_1_0.to_bytes());
        scheck!(is11 || is10);
        let registered = match mode {
            Mode::Absent => false,
            Mode::V10 => is10,
            _ => true,
        };
        println!(
            "  fake hwservicemanager: get {}/{}: {}",
            String::from_utf8_lossy(fq),
            String::from_utf8_lossy(instance),
            if registered { "the HAL" } else { "null" }
        );
        w.i32(0);
        let ptr = if registered { HAL_PTR } else { 0 };
        let o =
            onebridges::hal::aidl::FlatObject { kind: TYPE_BINDER, flags: 0x17f, binder_or_handle: ptr, cookie: ptr };
        if registered {
            w.object(&o.to_bytes());
        } else {
            w.bytes(&o.to_bytes()); // libhwbinder: a null binder has no offset entry
        }
    } else {
        // IBluetoothHci
        scheck!(tr.target == HAL_PTR);
        let token = r.cstr();
        match tr.code {
            BT_INITIALIZE_1_1 | BT_INITIALIZE => {
                scheck!(token == if tr.code == BT_INITIALIZE_1_1 { BT_HCI_1_1 } else { BT_HCI_1_0 });
                scheck!(mode != Mode::V10 || tr.code == BT_INITIALIZE);
                let o = r.object(24).map(onebridges::hal::aidl::FlatObject::parse);
                scheck!(o.is_some_and(|o| o.kind == TYPE_HANDLE));
                let mut have_cb = false;
                if let Some(o) = o.filter(|o| o.kind == TYPE_HANDLE) {
                    let h = o.binder_or_handle as u32;
                    JOBS.lock().unwrap().cb_handle = Some(h);
                    have_cb = true;
                    swrite(Commands::new().acquire(h).as_bytes());
                }
                println!("  fake HAL: initialize{}", if tr.code == BT_INITIALIZE_1_1 { "_1_1" } else { "" });
                if mode != Mode::Silent && have_cb {
                    post(hidl::INTERFACE_CHAIN, &[]);
                    post(BT_CB_INIT_COMPLETE, &[]);
                }
            }
            BT_SEND_COMMAND => {
                let data = r.vec(false).unwrap_or_default();
                scheck!(token == BT_HCI_1_0 && data.len() >= 3 && data.len() == 3 + data[2] as usize);
                let opcode = u16::from(data[0]) | u16::from(data[1]) << 8;
                let mut event = vec![0x0e, 4, 1, data[0], data[1], 0];
                if opcode == 0x1009 {
                    event[1] = 10; // Read BD_ADDR
                    event.extend_from_slice(b"\x66\x55\x44\x33\x22\x11");
                }
                println!("  fake HAL: command {opcode:#06x}");
                wait_job(post(BT_CB_EVENT, &event)); // the event goes out before the command returns
            }
            BT_SEND_ACL => {
                let data = r.vec(false);
                scheck!(token == BT_HCI_1_0 && data.is_some());
                post(BT_CB_ACL, data.unwrap_or_default());
            }
            BT_CLOSE => {
                scheck!(token == BT_HCI_1_0);
                println!("  fake HAL: close");
                if reply {
                    w.i32(0);
                    sreply(&w);
                }
                sfree(tr.buffer);
                common::exit(if SFAIL.load(std::sync::atomic::Ordering::Relaxed) != 0 { 1 } else { 0 });
            }
            _ => scheck!(false), // an unknown HAL code
        }
        scheck!(!r.bad);
        w.i32(0);
    }
    sfree(tr.buffer);
    if reply {
        sreply(&w);
    }
}

fn fake_hal(device: &str, mode: Mode, ready: i32) {
    let _ = SMODE.set(mode);
    let path = std::ffi::CString::new(device).unwrap();
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    let _ = SFD.set(fd);
    // The previous scenario's HAL was killed: the driver frees its context manager in deferred work after the
    // process is gone, so a new one can see EBUSY for a moment (seen on Linux 6.18 in a VM). Retry for up to 2 s.
    let mut rc = -1;
    // SAFETY: a mapping of our binder fd, kept for the process's life.
    let mapped = fd >= 0
        && unsafe { libc::mmap(std::ptr::null_mut(), 1 << 20, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) }
            != libc::MAP_FAILED;
    if mapped {
        for _ in 0..200 {
            // SAFETY: BINDER_SET_CONTEXT_MGR on our fd.
            rc = unsafe { libc::ioctl(fd, BINDER_SET_CONTEXT_MGR as _, 0) };
            if rc == 0 || xh_core::sys::errno() != libc::EBUSY {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if rc < 0 {
        println!("fake hal: {}", std::io::Error::last_os_error());
        common::exit(91);
    }
    std::thread::spawn(reader);
    swrite(&BC_ENTER_LOOPER.to_ne_bytes());
    // SAFETY: one byte to the parent's pipe, then our end closed.
    unsafe {
        libc::write(ready, b"r".as_ptr().cast(), 1);
        libc::close(ready);
    }
    let mut buf = [0u8; 512];
    loop {
        let n = sread(&mut buf);
        let mut at = 0;
        while at + 4 <= n {
            let cmd = ne::u32(&buf, at);
            at += 4;
            if cmd == BR_TRANSACTION {
                serve(&TransactionData::parse(&buf[at..at + 64]));
            } else if cmd == BR_INCREFS || cmd == BR_ACQUIRE {
                let done = if cmd == BR_INCREFS { BC_INCREFS_DONE } else { BC_ACQUIRE_DONE };
                swrite(Commands::new().word(done).quad(ne::u64(&buf, at)).quad(ne::u64(&buf, at + 8)).as_bytes());
            }
            at += ioc_size(cmd);
        }
    }
}

fn start_hal(device: &str, mode: Mode) -> libc::pid_t {
    let mut fds = [0; 2];
    // SAFETY: pipe writes two fds.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return -1;
    }
    let device = device.to_owned();
    let pid = common::fork(move || {
        // SAFETY: the read end is the parent's.
        unsafe { libc::close(fds[0]) };
        fake_hal(&device, mode, fds[1])
    });
    let mut c = 0u8;
    // SAFETY: our pipe's ends.
    let got = unsafe {
        libc::close(fds[1]);
        let got = libc::read(fds[0], (&mut c as *mut u8).cast(), 1);
        libc::close(fds[0]);
        got
    };
    if got != 1 {
        -1
    } else {
        pid
    }
}

// ================================ the client (parent) ================================
#[derive(Default)]
struct Got {
    state: Option<HciState>,
    states: u32,
    packets: Vec<(u8, Vec<u8>)>,
}
#[derive(Default)]
struct Client {
    got: Mutex<Got>,
    cond: Condvar,
}

impl Client {
    fn open(self: &Arc<Self>) -> Option<Arc<Hci>> {
        let (a, b) = (self.clone(), self.clone());
        hci_hidl::open_hidl(
            Box::new(move |kind, p| {
                a.got.lock().unwrap().packets.push((kind, p.to_vec()));
                a.cond.notify_all();
            }),
            Box::new(move |s| {
                let mut g = b.got.lock().unwrap();
                g.state = Some(s);
                g.states += 1;
                b.cond.notify_all();
            }),
        )
    }
    fn wait(&self, ms: u64, done: impl Fn(&Got) -> bool) -> bool {
        let g = self.got.lock().unwrap();
        let (g, _) = self.cond.wait_timeout_while(g, Duration::from_millis(ms), |g| !done(g)).unwrap();
        done(&g)
    }
    fn state(&self) -> Option<HciState> {
        self.got.lock().unwrap().state
    }
}

const RESET: [u8; 3] = [0x03, 0x0c, 0];
const READ_ADDRESS: [u8; 3] = [0x09, 0x10, 0];
const ACL: [u8; 8] = [0x01, 0x00, 4, 0, 0xde, 0xad, 0xbe, 0xef];

fn full_session(device: &str, mode: Mode) {
    println!("{} HAL", if mode == Mode::Ok { "1.1" } else { "1.0-only" });
    let hal = start_hal(device, mode);
    check!(hal > 0);
    check!(hci_hidl::probe() == if mode == Mode::Ok { 11 } else { 10 });
    let c = Arc::new(Client::default());
    let Some(h) = c.open() else {
        check!(false);
        return;
    };
    check!(h.send(1, &RESET) == Err(-libc::ENOTCONN)); // not open yet
    check!(h.open().is_ok());
    check!(c.wait(3000, |g| g.state == Some(HciState::On))); // initializationComplete(SUCCESS)
    check!(h.send(1, &RESET).is_ok());
    check!(h.send(1, &READ_ADDRESS).is_ok());
    check!(h.send(2, &ACL).is_ok());
    check!(c.wait(3000, |g| g.packets.len() == 3));
    {
        let g = c.got.lock().unwrap();
        let p = &g.packets;
        check!(p.len() == 3);
        if p.len() == 3 {
            check!(p[0].0 == 4 && p[0].1 == b"\x0e\x04\x01\x03\x0c\x00");
            check!(p[1].0 == 4 && p[1].1.len() == 12 && p[1].1[6..12] == *b"\x66\x55\x44\x33\x22\x11");
            check!(p[2].0 == 2 && p[2].1 == ACL);
        }
    }
    h.close();
    check!(c.state() == Some(HciState::Off));
    check!(common::child_exit(hal, 3000) == 0); // the fake HAL saw close and no bad parcel
    drop(h);
}

fn main() {
    let device = match binderfs::enter("ONEIOS_HWBINDER", "hwbinder") {
        Entered::Device(path) => path,
        Entered::Skipped(why) => {
            println!("test_hidl_binder: skipped ({why})");
            return;
        }
        Entered::Ran(code) => std::process::exit(code),
    };
    common::log_to_stdout();
    // SAFETY: a signal disposition: a write to a dead pipe is an error, not the end.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

    full_session(&device, Mode::Ok);
    full_session(&device, Mode::V10);

    println!("no HAL registered");
    let hal = start_hal(&device, Mode::Absent);
    check!(hci_hidl::probe() == -libc::ENOENT);
    let c = Arc::new(Client::default());
    let h = c.open();
    check!(h.as_ref().is_some_and(|h| h.open().is_ok()) && c.wait(3000, |g| g.state == Some(HciState::Failed))); // failed: the guest hears it
    if let Some(h) = &h {
        h.close();
    }
    drop(h);
    common::kill(hal);

    println!("a HAL that never completes initialisation");
    std::env::set_var("ONEIOS_BT_INIT_TIMEOUT_MS", "300");
    let hal = start_hal(&device, Mode::Silent);
    let s = Arc::new(Client::default());
    let h = s.open();
    check!(h.as_ref().is_some_and(|h| h.open().is_ok()));
    std::thread::sleep(Duration::from_millis(150));
    check!(s.state() != Some(HciState::Failed)); // still waiting
    check!(s.wait(2000, |g| g.state == Some(HciState::Failed)));
    if let Some(h) = &h {
        h.close();
    }
    drop(h);
    common::kill(hal);
    std::env::remove_var("ONEIOS_BT_INIT_TIMEOUT_MS");

    println!("the HAL dies");
    let hal = start_hal(&device, Mode::Ok);
    let d = Arc::new(Client::default());
    let h = d.open();
    check!(h.as_ref().is_some_and(|h| h.open().is_ok()) && d.wait(3000, |g| g.state == Some(HciState::On)));
    common::kill(hal);
    check!(d.wait(2000, |g| g.state == Some(HciState::Failed))); // the death notification
    check!(h.as_ref().is_some_and(|h| h.send(1, &RESET) == Err(-libc::ENOTCONN)));

    std::process::exit(common::finish("test_hidl_binder"));
}
