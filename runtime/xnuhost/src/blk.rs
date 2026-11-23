// SPDX-License-Identifier: MIT
//! Port of blk.c: disks backed by the image set's files. The role of UniXNU's RamBlock and VirtioMmioBlock back ends
//! (https://github.com/UniXNU/RamBlock 60a54ce, https://github.com/UniXNU/VirtioMmioBlock 4e1013e): there the guest
//! keeps a seed in RAM and changed pages in a store window; here Linux's page cache and a sparse file do both, so the
//! store is simply a raw image the size of the Data disk.
//!   read-only: os.img, systemos.img, app.img (raw APFS containers), opened O_RDONLY.
//!   writable:  <data>/data.img, created sparse on the first start. A seed that is itself a disk image (not a tar) is
//!              copied in, holes kept; a tar seed is the userland's to unpack.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use xh_core::logi;
use xh_core::time::monotonic_ns;
use xh_core::virtio::Viov;

use crate::config::DiskConfig;
use crate::imageset::Verify;

/// sparse: costs what the guest writes
const DATA_DEFAULT_BYTES: u64 = 32 << 30;
/// the disks the reports list at most
const MAX_BLK: usize = 8;

/// A read, write or flush that failed or was refused (the guest gets an I/O error): the chunk checks' error too.
pub use crate::imageset::IoFailed;

/// Why a disk did not open: what (the C's *error) and the system's error.
#[derive(Debug)]
pub struct OpenError {
    pub what: &'static str,
    pub error: io::Error,
}

impl OpenError {
    fn new(what: &'static str, error: io::Error) -> Self {
        OpenError { what, error }
    }
}

/// A disk (xh_blk). The counters say what the guest asked of it, and its flushes with their time. A writable disk's
/// flush (VIRTIO_BLK_T_FLUSH: APFS's barriers and every fsync, SCPreferences' plist commits among them) is an
/// fdatasync of the whole image, which runs inside the guest's notify while the guest's disk driver holds its one gate
/// (kernel/patches/oneios-hosted/storage): every other request of the guest to that disk waits behind it. On the
/// phone (8644ae9, 2026-11-15) configd held NetworkInterfaces.plist's lock for 6.4 s around one commit; these say how
/// much of that, and of the boot, is flush time.
pub struct Blk {
    file: File,
    bytes: u64,
    writable: bool,
    verify: OnceLock<Verify>,
    label: String,
    reads: AtomicU64,
    writes: AtomicU64,
    read_bytes: AtomicU64,
    write_bytes: AtomicU64,
    flushes: AtomicU64,
    flush_ns: AtomicU64,
    flush_max_ns: AtomicU64,
    slow: AtomicU64,
    /// the counters at the last report
    shown: Mutex<[u64; 6]>,
}

/// The open disks, for the reports.
static DISKS: Mutex<Vec<Weak<Blk>>> = Mutex::new(Vec::new());

fn disks() -> Vec<Arc<Blk>> {
    let mut all = DISKS.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|d| d.strong_count() > 0);
    all.iter().filter_map(Weak::upgrade).collect()
}

fn is_tar(file: &File) -> bool {
    let mut magic = [0u8; 5];
    file.read_exact_at(&mut magic, 257).is_ok() && &magic == b"ustar"
}

/// Copies a seed image into a new store, skipping blocks of zeros so the store stays sparse.
fn copy_seed(from: &File, to: &File, bytes: u64) -> io::Result<()> {
    const CHUNK: usize = 1 << 20;
    let mut buffer = vec![0u8; CHUNK];
    let mut at = 0;
    while at < bytes {
        let n = from.read_at(&mut buffer, at)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        for (b, block) in buffer[..n].chunks(4096).enumerate() {
            if block.iter().any(|&x| x != 0) {
                to.write_all_at(block, at + (b * 4096) as u64)?;
            }
        }
        at += n as u64;
    }
    Ok(())
}

/// First start: a new store, written under a temporary name so a crash leaves no half store.
fn create_store(config: &DiskConfig) -> Result<File, OpenError> {
    let temporary = format!("{}.new", config.path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| OpenError::new("cannot create the Data store", e))?;
    let fill = || -> io::Result<()> {
        let seed = if config.seed.is_empty() { None } else { File::open(&config.seed).ok() };
        let image = seed.and_then(|s| {
            let len = s.metadata().ok()?.len();
            (!is_tar(&s) && len >= 4096).then_some((s, len))
        });
        let bytes = image.as_ref().map_or(DATA_DEFAULT_BYTES, |(_, len)| (*len).max(DATA_DEFAULT_BYTES));
        file.set_len(bytes)?;
        if let Some((seed, len)) = &image {
            copy_seed(seed, &file, *len)?;
        }
        file.sync_all()?;
        fs::rename(&temporary, &config.path)
    };
    if let Err(e) = fill() {
        let _ = fs::remove_file(&temporary);
        return Err(OpenError::new("cannot fill the new Data store", e));
    }
    Ok(file)
}

impl Blk {
    /// xh_blk_open: the disk of `config` (a writable one is created on the first start).
    pub fn open(config: &DiskConfig) -> Result<Arc<Blk>, OpenError> {
        let file = if !config.writable {
            File::open(&config.path).map_err(|e| OpenError::new("cannot open the image", e))?
        } else {
            match OpenOptions::new().read(true).write(true).open(&config.path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => create_store(config)?,
                Err(e) => return Err(OpenError::new("cannot open the Data store", e)),
            }
        };
        let what = if config.writable { "cannot open the Data store" } else { "cannot open the image" };
        let bytes = file.metadata().map_err(|e| OpenError::new(what, e))?.len() & !511;
        if bytes == 0 {
            return Err(OpenError::new("the image is empty", io::Error::from_raw_os_error(libc::EINVAL)));
        }
        let base = config.path.rsplit('/').next().unwrap_or(&config.path);
        let mut label = if config.data_store { "data" } else { base }.to_owned();
        xh_core::text::truncate_string(&mut label, 23); // C's char label[24]
        let b = Arc::new(Blk {
            file,
            bytes,
            writable: config.writable,
            verify: OnceLock::new(),
            label,
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            write_bytes: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            flush_ns: AtomicU64::new(0),
            flush_max_ns: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            shown: Mutex::new([0; 6]),
        });
        let mut all = DISKS.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|d| d.strong_count() > 0);
        if all.len() < MAX_BLK {
            all.push(Arc::downgrade(&b));
        }
        Ok(b)
    }

    /// The disk's size, a multiple of 512 bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    /// The name of its lines: the file's name, "data" for the Data store.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// xh_blk_set_verify: a read-only image whose chunks are checked against the image set's hash list before the
    /// guest gets them (imageset.rs). A chunk that differs is a read error. Once; a writable disk takes none.
    pub fn set_verify(&self, verify: Verify) {
        if !self.writable {
            let _ = self.verify.set(verify);
        }
    }

    /// The chunk checks, when the disk has them.
    pub fn verify(&self) -> Option<&Verify> {
        self.verify.get()
    }

    fn in_range(&self, offset: u64, length: u64) -> bool {
        offset <= self.bytes && length <= self.bytes - offset
    }

    /// xh_blk_read into a buffer.
    pub fn read(&self, offset: u64, buffer: &mut [u8]) -> Result<(), IoFailed> {
        let length = buffer.len() as u64;
        self.checked(offset, length)?;
        self.file.read_exact_at(buffer, offset).map_err(|_| IoFailed)?;
        self.count_read(length);
        Ok(())
    }

    /// A read into a part of a guest's chain (it must be writable by the device).
    pub fn read_iov(&self, offset: u64, iov: &Viov) -> Result<(), IoFailed> {
        let length = iov.len() as u64;
        if !iov.writable() {
            return Err(IoFailed);
        }
        self.checked(offset, length)?;
        // SAFETY: the part is mapped guest memory of `length` bytes the device may write (Viov's contract); pread writes
        // only inside it.
        unsafe { pio(self.file.as_raw_fd(), iov.as_ptr(), length, offset, false) }?;
        self.count_read(length);
        Ok(())
    }

    fn checked(&self, offset: u64, length: u64) -> Result<(), IoFailed> {
        if !self.in_range(offset, length) {
            return Err(IoFailed);
        }
        match self.verify.get() {
            Some(v) => v.range(&self.file, offset, length),
            None => Ok(()),
        }
    }

    fn count_read(&self, length: u64) {
        self.reads.fetch_add(1, Relaxed);
        self.read_bytes.fetch_add(length, Relaxed);
    }

    fn count_write(&self, length: u64) {
        self.writes.fetch_add(1, Relaxed);
        self.write_bytes.fetch_add(length, Relaxed);
    }

    /// xh_blk_write from a buffer.
    pub fn write(&self, offset: u64, buffer: &[u8]) -> Result<(), IoFailed> {
        let length = buffer.len() as u64;
        if !self.writable || !self.in_range(offset, length) {
            return Err(IoFailed);
        }
        self.file.write_all_at(buffer, offset).map_err(|_| IoFailed)?;
        self.count_write(length);
        Ok(())
    }

    /// A write from a part of a guest's chain.
    pub fn write_iov(&self, offset: u64, iov: &Viov) -> Result<(), IoFailed> {
        let length = iov.len() as u64;
        if !self.writable || !self.in_range(offset, length) {
            return Err(IoFailed);
        }
        // SAFETY: the part is mapped guest memory of `length` bytes (Viov's contract); pwrite only reads it.
        unsafe { pio(self.file.as_raw_fd(), iov.as_ptr(), length, offset, true) }?;
        self.count_write(length);
        Ok(())
    }

    /// xh_blk_flush: fdatasync of a writable disk, counted and timed.
    pub fn flush(&self) -> Result<(), IoFailed> {
        if !self.writable {
            return Ok(());
        }
        let t0 = monotonic_ns();
        let rc = self.file.sync_data();
        let ns = monotonic_ns() - t0;
        self.flushes.fetch_add(1, Relaxed);
        self.flush_ns.fetch_add(ns, Relaxed);
        self.flush_max_ns.fetch_max(ns, Relaxed);
        // The first slow ones as they happen, with the time: every other request of the guest to this disk waited.
        if ns >= 500_000_000 && self.slow.fetch_add(1, Relaxed) < 16 {
            logi!(
                "disk {}: a flush took {} ms (fdatasync; the guest's other requests to this disk waited)",
                self.label,
                ns / 1_000_000
            );
        }
        rc.map_err(|_| IoFailed)
    }

    /// xh_blk_flushes: flushes served (the tests).
    pub fn flushes(&self) -> u64 {
        self.flushes.load(Relaxed)
    }

    /// One line when the disk had requests since the last report (and at the stop, since the start):
    ///   disk data: 30 s: reads 1200 (45 MiB), writes 300 (20 MiB), flushes 40 in 1234 ms (max 456 ms)
    fn report(&self, since_start: bool, seconds: f64) -> Option<String> {
        let v = [&self.reads, &self.read_bytes, &self.writes, &self.write_bytes, &self.flushes, &self.flush_ns]
            .map(|c| c.load(Relaxed));
        let mut d = v;
        if !since_start {
            let mut shown = self.shown.lock().unwrap_or_else(|e| e.into_inner());
            for (k, dk) in d.iter_mut().enumerate() {
                *dk -= shown[k];
            }
            *shown = v;
        }
        if d[0] == 0 && d[2] == 0 && d[4] == 0 {
            return None;
        }
        let (when, longest) = if since_start {
            ("since the start".to_owned(), String::new())
        } else {
            let max = self.flush_max_ns.swap(0, Relaxed);
            (format!("{seconds:.0} s"), format!(" (max {} ms)", max / 1_000_000))
        };
        Some(format!(
            "disk {}: {when}: reads {} ({} MiB), writes {} ({} MiB), flushes {} in {} ms{longest}",
            self.label,
            d[0],
            d[1] >> 20,
            d[2],
            d[3] >> 20,
            d[4],
            d[5] / 1_000_000
        ))
    }
}

impl Drop for Blk {
    /// xh_blk_close: a writable disk is synced.
    fn drop(&mut self) {
        if self.writable {
            let _ = self.file.sync_all();
        }
    }
}

/// pread/pwrite of `length` bytes at `ptr`, the whole of it (EINTR retried).
///
/// # Safety
/// `ptr` must be valid for `length` bytes: writable for a read, readable for a write.
unsafe fn pio(fd: i32, ptr: *mut u8, length: u64, offset: u64, write: bool) -> Result<(), IoFailed> {
    let mut done = 0u64;
    while done < length {
        let (p, n, at) = (ptr.wrapping_add(done as usize), (length - done) as usize, (offset + done) as libc::off_t);
        // SAFETY: [p, p + n) lies inside the caller's buffer.
        let r = unsafe {
            if write {
                libc::pwrite(fd, p as *const libc::c_void, n, at)
            } else {
                libc::pread(fd, p as *mut libc::c_void, n, at)
            }
        };
        if r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if r <= 0 {
            return Err(IoFailed);
        }
        done += r as u64;
    }
    Ok(())
}

fn report_all_lines(since_start: bool, seconds: f64) {
    for d in disks() {
        if let Some(line) = d.report(since_start, seconds) {
            logi!("{}", line);
        }
    }
}

/// xh_blk_tick (the main loop): a "disk NAME: 30 s: ..." line per busy disk every 30 s.
pub fn tick() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = monotonic_ns();
    let last = LAST.load(Relaxed);
    if last == 0 {
        LAST.store(now, Relaxed);
        return;
    }
    if now - last < 30_000_000_000 {
        return;
    }
    report_all_lines(false, (now - last) as f64 / 1e9);
    LAST.store(now, Relaxed);
}

/// xh_blk_report_all (the stop): the same since the start.
pub fn report_all() {
    report_all_lines(true, 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lines() {
        let dir = std::env::temp_dir().join(format!("xnuhost-blk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("os.img");
        fs::write(&path, vec![1u8; 8192]).unwrap();
        let b = Blk::open(&DiskConfig { path: path.to_str().unwrap().into(), ..Default::default() }).unwrap();
        assert!(b.report(false, 30.0).is_none()); // nothing asked: no line
        let mut buf = [0u8; 1024];
        assert!(b.read(0, &mut buf).is_ok() && b.read(8192 - 512, &mut buf[..512]).is_ok());
        assert!(b.read(8192, &mut buf[..1]).is_err() && b.write(0, &buf).is_err()); // past the end; read-only
        assert_eq!(
            b.report(false, 30.0).as_deref(),
            Some("disk os.img: 30 s: reads 2 (0 MiB), writes 0 (0 MiB), flushes 0 in 0 ms (max 0 ms)")
        );
        assert!(b.report(false, 30.0).is_none());
        assert_eq!(
            b.report(true, 0.0).as_deref(),
            Some("disk os.img: since the start: reads 2 (0 MiB), writes 0 (0 MiB), flushes 0 in 0 ms")
        );
        assert!(b.flush().is_ok() && b.flushes() == 0); // a read-only disk is not flushed
        let empty = dir.join("empty.img");
        fs::write(&empty, b"").unwrap();
        let e = Blk::open(&DiskConfig { path: empty.to_str().unwrap().into(), ..Default::default() }).err().unwrap();
        assert_eq!(e.what, "the image is empty");
        let e = Blk::open(&DiskConfig { path: dir.join("none").to_str().unwrap().into(), ..Default::default() });
        assert_eq!(e.err().unwrap().what, "cannot open the image");
        let _ = fs::remove_dir_all(&dir);
    }
}
