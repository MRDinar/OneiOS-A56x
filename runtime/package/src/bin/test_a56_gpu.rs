// SPDX-License-Identifier: MIT
//! The Galaxy A56's GPU path, alone (port of tools/test_a56_gpu.c, docs/a56.md): opens the DRM render node (argv[1],
//! default /dev/dri/renderD128), asks the driver's version and PRIME support, makes a 1 MiB udmabuf from a sealed memfd
//! and imports it into the node with PRIME_FD_TO_HANDLE. Read only beside One UI: nothing it makes outlives it.
use std::ffi::{c_char, c_int};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use xh_core::ioctl::{iow, iowr};
use xh_core::sys::error_text;
use xh_core::text::nul_text;

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

const DRM_IOCTL_VERSION: u32 = iowr(b'd', 0x00, size_of::<DrmVersion>());
const DRM_IOCTL_GET_CAP: u32 = iowr(b'd', 0x0c, size_of::<DrmGetCap>());
const DRM_IOCTL_PRIME_FD_TO_HANDLE: u32 = iowr(b'd', 0x2e, size_of::<DrmPrimeHandle>());
const UDMABUF_CREATE: u32 = iow(b'u', 0x42, size_of::<UdmabufCreate>());
const DRM_CAP_PRIME: u64 = 0x5;
const BUFFER_SIZE: u64 = 1024 * 1024;

/// ioctl(fd, request, arg): the call's result (a new fd for UDMABUF_CREATE) or the error.
fn ioctl<T>(fd: &impl AsRawFd, request: u32, arg: &mut T) -> io::Result<c_int> {
    // SAFETY: `arg` is a live, exclusively borrowed value of the type `request` encodes the size of.
    unsafe { xh_core::ioctl::ioctl(fd.as_raw_fd(), request, arg) }
}

/// The errno, for the C's `(errno N)` lines.
fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
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
        Err(e) => println!("[-] DRM_IOCTL_VERSION failed: {}", error_text(&e)),
        Ok(_) => {
            println!(
                "[+] DRM Driver: '{}' (version {}.{}.{}, date: '{}')",
                nul_text(&name),
                version.version_major,
                version.version_minor,
                version.version_patchlevel,
                nul_text(&date)
            );
            println!("[+] Description: '{}'", nul_text(&desc));
        }
    }
}

fn query_prime(node: &File) {
    let mut cap = DrmGetCap { capability: DRM_CAP_PRIME, ..Default::default() };
    match ioctl(node, DRM_IOCTL_GET_CAP, &mut cap) {
        Err(e) => println!("[-] DRM_CAP_PRIME query failed: {}", error_text(&e)),
        Ok(_) => {
            let yes = |bit: u64| if cap.value & bit != 0 { "YES" } else { "NO" };
            println!("[+] DRM_CAP_PRIME support: 0x{:x} (import={}, export={})", cap.value, yes(1), yes(2));
        }
    }
}

/// A sealable memfd of BUFFER_SIZE bytes, its shrinking sealed (udmabuf needs F_SEAL_SHRINK; a seal refused is
/// UDMABUF_CREATE's to say).
fn sealed_memfd() -> Result<OwnedFd, String> {
    let memfd = xh_core::fd::memfd(c"test_gpu_buf", libc::MFD_ALLOW_SEALING)
        .map_err(|e| format!("[-] memfd_create failed: {}", error_text(&e)))?;
    let file = File::from(memfd);
    file.set_len(BUFFER_SIZE).map_err(|e| format!("[-] ftruncate failed: {}", error_text(&e)))?;
    let _ = xh_core::fd::seal_shrink(file.as_fd());
    Ok(file.into())
}

fn import_udmabuf(node: &File, node_path: &str) {
    println!("\n[2] Testing memory buffer creation via /dev/udmabuf...");
    let udmabuf = match open_rw("/dev/udmabuf") {
        Err(e) => return println!("[-] Failed to open /dev/udmabuf: {}", error_text(&e)),
        Ok(f) => f,
    };
    println!("[+] Opened /dev/udmabuf (fd={})", udmabuf.as_raw_fd());
    let memfd = match sealed_memfd() {
        Err(line) => return println!("{line}"),
        Ok(fd) => fd,
    };
    let mut create = UdmabufCreate { memfd: memfd.as_raw_fd() as u32, size: BUFFER_SIZE, ..Default::default() };
    let dmabuf = match ioctl(&udmabuf, UDMABUF_CREATE, &mut create) {
        Err(e) => return println!("[-] UDMABUF_CREATE failed: {}", error_text(&e)),
        // SAFETY: UDMABUF_CREATE returned a new dma-buf descriptor that we now own.
        Ok(fd) => unsafe { OwnedFd::from_raw_fd(fd) },
    };
    println!("[+] Successfully allocated 1MB udmabuf dma-buf (fd={})", dmabuf.as_raw_fd());
    println!("[3] Importing dma-buf into {node_path} via PRIME_FD_TO_HANDLE...");
    let mut prime = DrmPrimeHandle { fd: dmabuf.as_raw_fd(), ..Default::default() };
    match ioctl(node, DRM_IOCTL_PRIME_FD_TO_HANDLE, &mut prime) {
        Err(e) => println!("[-] PRIME_FD_TO_HANDLE failed: {} (errno {})", error_text(&e), errno_of(&e)),
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
            println!("[-] Failed to open {node_path}: {} (errno {})", error_text(&e), errno_of(&e));
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
