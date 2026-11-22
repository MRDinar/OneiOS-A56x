// SPDX-License-Identifier: MIT
//! The Galaxy A56's GPU path, alone (port of tools/test_a56_gpu.c, docs/a56.md): opens the DRM render node (argv[1],
//! default /dev/dri/renderD128), asks the driver's version and PRIME support, makes a 1 MiB udmabuf from a sealed memfd
//! and imports it into the node with PRIME_FD_TO_HANDLE. Read only beside One UI: nothing it makes outlives it.
use std::ffi::{c_char, c_int, c_ulong, CString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[repr(C)]
struct DrmVersion {
    version_major: c_int,
    version_minor: c_int,
    version_patchlevel: c_int,
    name_len: usize,
    name: *mut c_char,
    date_len: usize,
    date: *mut c_char,
    desc_len: usize,
    desc: *mut c_char,
}

#[repr(C)]
#[derive(Default)]
struct DrmGetCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct DrmPrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// Linux's _IOC: direction, type, number, size.
const fn ioc(dir: c_ulong, ty: u8, nr: u8, size: usize) -> c_ulong {
    (dir << 30) | ((size as c_ulong) << 16) | ((ty as c_ulong) << 8) | nr as c_ulong
}
const IOC_WRITE: c_ulong = 1;
const IOC_READ_WRITE: c_ulong = 3;
const DRM_IOCTL_VERSION: c_ulong = ioc(IOC_READ_WRITE, b'd', 0x00, size_of::<DrmVersion>());
const DRM_IOCTL_GET_CAP: c_ulong = ioc(IOC_READ_WRITE, b'd', 0x0c, size_of::<DrmGetCap>());
const DRM_IOCTL_PRIME_FD_TO_HANDLE: c_ulong = ioc(IOC_READ_WRITE, b'd', 0x2e, size_of::<DrmPrimeHandle>());
const UDMABUF_CREATE: c_ulong = ioc(IOC_WRITE, b'u', 0x42, size_of::<UdmabufCreate>());
const DRM_CAP_PRIME: u64 = 0x5;
const BUFFER_SIZE: u64 = 1024 * 1024;

/// ioctl(fd, request, arg): the call's result (a new fd for UDMABUF_CREATE) or the error.
fn ioctl<T>(fd: &impl AsRawFd, request: c_ulong, arg: &mut T) -> io::Result<c_int> {
    // SAFETY: `arg` is a live, exclusively borrowed value of the type `request` encodes the size of.
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg as *mut T) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// "<strerror>" and, for the C's `(errno N)` lines, the number.
fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

/// The text of a C string buffer up to its NUL.
fn c_text(buffer: &[u8]) -> String {
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// strerror's text without Rust's " (os error N)".
fn reason(e: &io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

fn open_rw(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path) // std opens with O_CLOEXEC
}

fn query_version(node: &File) {
    let (mut name, mut date, mut desc) = ([0u8; 128], [0u8; 128], [0u8; 256]);
    let mut version = DrmVersion {
        name: name.as_mut_ptr().cast(),
        name_len: name.len() - 1,
        date: date.as_mut_ptr().cast(),
        date_len: date.len() - 1,
        desc: desc.as_mut_ptr().cast(),
        desc_len: desc.len() - 1,
        version_major: 0,
        version_minor: 0,
        version_patchlevel: 0,
    };
    match ioctl(node, DRM_IOCTL_VERSION, &mut version) {
        Err(e) => println!("[-] DRM_IOCTL_VERSION failed: {}", reason(&e)),
        Ok(_) => {
            println!(
                "[+] DRM Driver: '{}' (version {}.{}.{}, date: '{}')",
                c_text(&name),
                version.version_major,
                version.version_minor,
                version.version_patchlevel,
                c_text(&date)
            );
            println!("[+] Description: '{}'", c_text(&desc));
        }
    }
}

fn query_prime(node: &File) {
    let mut cap = DrmGetCap { capability: DRM_CAP_PRIME, ..Default::default() };
    match ioctl(node, DRM_IOCTL_GET_CAP, &mut cap) {
        Err(e) => println!("[-] DRM_CAP_PRIME query failed: {}", reason(&e)),
        Ok(_) => {
            let yes = |bit: u64| if cap.value & bit != 0 { "YES" } else { "NO" };
            println!("[+] DRM_CAP_PRIME support: 0x{:x} (import={}, export={})", cap.value, yes(1), yes(2));
        }
    }
}

/// A sealable memfd of BUFFER_SIZE bytes, its shrinking sealed (udmabuf needs F_SEAL_SHRINK).
fn sealed_memfd() -> Result<OwnedFd, String> {
    let name = CString::new("test_gpu_buf").expect("no NUL");
    // SAFETY: memfd_create with a valid C string; the fd it returns is owned by the OwnedFd below.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(format!("[-] memfd_create failed: {}", reason(&io::Error::last_os_error())));
    }
    // SAFETY: `fd` is a new descriptor nobody else owns.
    let memfd = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = File::from(memfd);
    file.set_len(BUFFER_SIZE).map_err(|e| format!("[-] ftruncate failed: {}", reason(&e)))?;
    // SAFETY: fcntl on a descriptor we own; F_ADD_SEALS takes an int.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) };
    Ok(file.into())
}

fn import_udmabuf(node: &File, node_path: &str) {
    println!("\n[2] Testing memory buffer creation via /dev/udmabuf...");
    let udmabuf = match open_rw("/dev/udmabuf") {
        Err(e) => return println!("[-] Failed to open /dev/udmabuf: {}", reason(&e)),
        Ok(f) => f,
    };
    println!("[+] Opened /dev/udmabuf (fd={})", udmabuf.as_raw_fd());
    let memfd = match sealed_memfd() {
        Err(line) => return println!("{line}"),
        Ok(fd) => fd,
    };
    let mut create = UdmabufCreate { memfd: memfd.as_raw_fd() as u32, size: BUFFER_SIZE, ..Default::default() };
    let dmabuf = match ioctl(&udmabuf, UDMABUF_CREATE, &mut create) {
        Err(e) => return println!("[-] UDMABUF_CREATE failed: {}", reason(&e)),
        // SAFETY: UDMABUF_CREATE returned a new dma-buf descriptor that we now own.
        Ok(fd) => unsafe { OwnedFd::from_raw_fd(fd) },
    };
    println!("[+] Successfully allocated 1MB udmabuf dma-buf (fd={})", dmabuf.as_raw_fd());
    println!("[3] Importing dma-buf into {node_path} via PRIME_FD_TO_HANDLE...");
    let mut prime = DrmPrimeHandle { fd: dmabuf.as_raw_fd(), ..Default::default() };
    match ioctl(node, DRM_IOCTL_PRIME_FD_TO_HANDLE, &mut prime) {
        Err(e) => println!("[-] PRIME_FD_TO_HANDLE failed: {} (errno {})", reason(&e), errno_of(&e)),
        Ok(_) => println!("[+] SUCCESS: dma-buf imported into SGPU! GEM Handle: {}", prime.handle),
    }
}

fn main() {
    println!("=== SAMSUNG GALAXY A56 5G (EXYNOS 1580 / SGPU) GPU TEST ===\n");
    // port-a56: argv[1] overrides the node; the default is the A56's render node
    let node_path = std::env::args().nth(1).unwrap_or_else(|| "/dev/dri/renderD128".to_string());
    println!("[1] Opening DRM render node: {node_path} ...");
    let node = match open_rw(&node_path) {
        Err(e) => {
            println!("[-] Failed to open {node_path}: {} (errno {})", reason(&e), errno_of(&e));
            std::process::exit(1);
        }
        Ok(f) => f,
    };
    println!("[+] Successfully opened {node_path} (fd={})", node.as_raw_fd());
    query_version(&node);
    query_prime(&node);
    import_udmabuf(&node, &node_path);
    drop(node);
    println!("\n=== TEST COMPLETED ===");
}
