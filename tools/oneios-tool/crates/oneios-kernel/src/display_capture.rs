//! `oneios-tool kernel display-capture --session DIR [--out PREFIX] [--seconds N] [--every S]`: watch what xnuhost
//! shows on a PC boot, a client of DIR/display.sock in place of oneios-display (Linux only). It was
//! kernel/patches/oneios-hosted/display/display-capture.py until 2026-11-21 (Python is retired from this
//! repository); the lines it prints are the same, and it also reads the half-float frames (display-format=rgba16f),
//! which the script refused.
//!
//! Connects to DIR/display.sock (waiting for it to appear), takes the hello and the frame's memfd as oneios-display
//! does (docs/booter.md 2.8: hello {"XHD1", width, height, stride, format} with the memfd, then flush {"XHDF", x, y,
//! w, h} per changed rectangle), and prints one line when the picture changed since the last look: the time, the
//! flushes so far, the last rectangle, how many distinct colours the frame has (sampled every 5th pixel of every
//! 7th row) and a hash of it. With --out it also writes the frame as PREFIX-NNN.png each time it changed (at most
//! one every --every seconds, default 5). It sends no panel message: xnuhost keeps the copy path.
//!
//! It replaces a helper that is connected (xnuhost serves one client), so do not run it beside oneios-display.
//! A boot's console is a few colours of text on black; the system's frames are not: the colour count tells them
//! apart without opening the picture.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

#[derive(clap::Args)]
pub struct Args {
    /// The session folder xnuhost serves display.sock in
    #[arg(long)]
    pub session: PathBuf,
    /// Write each changed frame as PREFIX-NNN.png
    #[arg(long)]
    pub out: Option<String>,
    /// Stop after this long (default: when xnuhost ends)
    #[arg(long, default_value_t = 0.0)]
    pub seconds: f64,
    /// Look at the frame at most this often
    #[arg(long, default_value_t = 5.0)]
    pub every: f64,
}

/// B,G,R,X 8-bit sRGB (include/xnuhost/pixels.h XH_PIXELS_BGRX8).
pub const BGRX8: u32 = 1;
/// R,G,B,A IEEE 754 half floats, linear light, sRGB primaries, extended range (XH_PIXELS_RGBA16F).
pub const RGBA16F: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hello {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
}

impl Hello {
    fn bpp(&self) -> u32 {
        if self.format == RGBA16F { 8 } else { 4 }
    }
    pub fn len(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

fn words(message: &[u8]) -> [u32; 4] {
    std::array::from_fn(|i| u32::from_le_bytes(message[4 + 4 * i..8 + 4 * i].try_into().unwrap()))
}

/// The hello message, checked as oneios-display checks it.
pub fn hello(message: &[u8], fds: usize) -> Result<Hello> {
    if message.len() != 20 || &message[..4] != b"XHD1" || fds == 0 {
        bail!("display-capture: no hello with a memfd");
    }
    let [width, height, stride, format] = words(message);
    let h = Hello { width, height, stride, format };
    if !(format == BGRX8 || format == RGBA16F) || !(1..=16384).contains(&width) || !(1..=16384).contains(&height) || (stride as u64) < width as u64 * h.bpp() as u64 {
        bail!("display-capture: unusable hello {width}x{height} stride {stride} format {format}");
    }
    Ok(h)
}

/// A flush message's rectangle (x, y, w, h); None for anything else.
pub fn flush(message: &[u8]) -> Option<[u32; 4]> {
    (message.len() == 20 && &message[..4] == b"XHDF").then(|| words(message))
}

fn half(h: u16) -> f32 {
    let (sign, exp, mant) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f32);
    let v = match exp {
        0 => mant * (2f32).powi(-24),
        31 => if mant == 0.0 { f32::INFINITY } else { f32::NAN },
        e => (1.0 + mant / 1024.0) * (2f32).powi(e - 15),
    };
    if sign == 1 { -v } else { v }
}

/// Linear light -> an 8-bit sRGB code (clamped to 0..1: the extended range is cut).
fn srgb8(linear: f32) -> u8 {
    let v = if linear.is_nan() { 0.0 } else { linear.clamp(0.0, 1.0) };
    let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

/// The pixel at (x, y) as 8-bit R, G, B.
fn rgb(h: &Hello, frame: &[u8], x: usize, y: usize) -> [u8; 3] {
    let at = y * h.stride as usize + x * h.bpp() as usize;
    if h.format == RGBA16F {
        std::array::from_fn(|c| srgb8(half(u16::from_le_bytes([frame[at + 2 * c], frame[at + 2 * c + 1]]))))
    } else {
        [frame[at + 2], frame[at + 1], frame[at]]
    }
}

/// How many distinct colours the frame has, every 5th pixel of every 7th row (the script's sample: the colour
/// bytes as they are in the frame).
pub fn colours(h: &Hello, frame: &[u8]) -> usize {
    let (bpp, colour) = (h.bpp() as usize, if h.format == RGBA16F { 6 } else { 3 });
    let mut seen = HashSet::new();
    for y in (0..h.height as usize).step_by(7) {
        for x in (0..h.width as usize).step_by(5) {
            let at = y * h.stride as usize + x * bpp;
            seen.insert(&frame[at..at + colour]);
        }
    }
    seen.len()
}

/// The frame as an RGB PNG (8 bits a channel, no filter, zlib level 3, as the script wrote it).
pub fn png(h: &Hello, frame: &[u8]) -> Vec<u8> {
    let (w, ht) = (h.width as usize, h.height as usize);
    let mut rows = Vec::with_capacity(ht * (1 + 3 * w));
    for y in 0..ht {
        rows.push(0);
        for x in 0..w {
            rows.extend_from_slice(&rgb(h, frame, x, y));
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(3));
    z.write_all(&rows).unwrap();
    let idat = z.finish().unwrap();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&h.width.to_be_bytes());
    ihdr.extend_from_slice(&h.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [(b"IHDR", &ihdr[..]), (b"IDAT", &idat[..]), (b"IEND", &[][..])] {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    out
}

#[cfg(target_os = "linux")]
mod socket {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub struct Socket(OwnedFd);

    impl Socket {
        pub fn connect(path: &Path) -> std::io::Result<Socket> {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            // SAFETY: plain socket calls on a zeroed sockaddr_un whose path fits (checked).
            unsafe {
                let mut addr: libc::sockaddr_un = std::mem::zeroed();
                if bytes.len() >= addr.sun_path.len() {
                    return Err(std::io::Error::other("the socket's path is too long"));
                }
                addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
                for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
                    *d = *s as libc::c_char;
                }
                let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let sock = Socket(OwnedFd::from_raw_fd(fd));
                if libc::connect(fd, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(sock)
            }
        }

        /// One message of at most 64 bytes and the descriptors that came with it.
        pub fn recv_with_fds(&self) -> std::io::Result<(Vec<u8>, Vec<OwnedFd>)> {
            let mut data = [0u8; 64];
            // room for 4 descriptors, aligned as cmsghdr wants
            let mut control = [0u64; 8];
            // SAFETY: msghdr points at the two local buffers above for the call's length.
            unsafe {
                let mut iov = libc::iovec { iov_base: data.as_mut_ptr().cast(), iov_len: data.len() };
                let mut msg: libc::msghdr = std::mem::zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = control.as_mut_ptr().cast();
                msg.msg_controllen = libc::CMSG_SPACE(4 * 4) as _;
                let n = libc::recvmsg(self.0.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
                if n < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut fds = Vec::new();
                let mut c = libc::CMSG_FIRSTHDR(&msg);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                        let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / 4;
                        let at = libc::CMSG_DATA(c) as *const libc::c_int;
                        for i in 0..count {
                            fds.push(OwnedFd::from_raw_fd(at.add(i).read_unaligned()));
                        }
                    }
                    c = libc::CMSG_NXTHDR(&msg, c);
                }
                Ok((data[..n as usize].to_vec(), fds))
            }
        }

        /// Whether a message is waiting, within TIMEOUT.
        pub fn ready(&self, timeout: Duration) -> bool {
            let mut p = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one pollfd.
            unsafe { libc::poll(&mut p, 1, timeout.as_millis() as libc::c_int) > 0 }
        }

        pub fn recv(&self) -> Vec<u8> {
            let mut data = [0u8; 64];
            // SAFETY: a recv into the local buffer.
            let n = unsafe { libc::recv(self.0.as_raw_fd(), data.as_mut_ptr().cast(), data.len(), 0) };
            if n <= 0 { Vec::new() } else { data[..n as usize].to_vec() }
        }
    }
}

/// The client's loop, its lines written to OUT.
#[cfg(target_os = "linux")]
pub fn run(args: &Args, out: &mut dyn Write) -> Result<()> {
    let path = args.session.join("display.sock");
    let began = Instant::now();
    let t = || began.elapsed().as_secs_f64();
    let sock = loop {
        match socket::Socket::connect(&path) {
            Ok(sock) => break sock,
            Err(_) => {
                if args.seconds > 0.0 && t() > args.seconds {
                    bail!("display-capture: no {}", path.display());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    };
    let (message, fds) = sock.recv_with_fds().context("display-capture: the hello")?;
    let h = hello(&message, fds.len())?;
    let memfd = std::fs::File::from(fds.into_iter().next().unwrap());
    // SAFETY: a read-only shared map of xnuhost's frame; xnuhost writes into it while we read (we copy it first).
    let frame = unsafe { memmap2::MmapOptions::new().len(h.len()).map(&memfd) }.context("display-capture: the frame's memfd")?;
    writeln!(out, "{:8.1}s hello {}x{} stride {}{}", t(), h.width, h.height, h.stride, if h.format == RGBA16F { " (half floats)" } else { "" })?;
    let (mut flushes, mut last, mut seen, mut shots, mut looked) = (0u64, None::<[u32; 4]>, String::new(), 0u32, None::<Instant>);
    while args.seconds <= 0.0 || t() < args.seconds {
        if sock.ready(Duration::from_secs(1)) {
            let message = sock.recv();
            if message.is_empty() {
                writeln!(out, "{:8.1}s the socket closed after {flushes} flushes", t())?;
                break;
            }
            if let Some(rect) = flush(&message) {
                flushes += 1;
                last = Some(rect);
            }
        }
        if flushes > 0 && looked.is_none_or(|l| l.elapsed().as_secs_f64() >= args.every) {
            looked = Some(Instant::now());
            let picture = frame.to_vec();
            let digest: String = Sha256::digest(&picture).iter().take(6).map(|b| format!("{b:02x}")).collect();
            if digest != seen {
                seen = digest.clone();
                let mut note = String::new();
                if let Some(prefix) = &args.out {
                    shots += 1;
                    let file = format!("{prefix}-{shots:03}.png");
                    std::fs::write(&file, png(&h, &picture)).with_context(|| file.clone())?;
                    note = format!(" -> {file}");
                }
                let last = last.map_or("None".to_string(), |[x, y, w, ht]| format!("({x}, {y}, {w}, {ht})"));
                writeln!(out, "{:8.1}s {flushes} flushes, last {last}, {} colours sampled, frame {digest}{note}", t(), colours(&h, &picture))?;
            }
        }
        out.flush()?;
    }
    writeln!(out, "{:8.1}s done: {flushes} flushes, {shots} pictures", t())?;
    Ok(())
}

pub fn main(args: Args) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let stdout = std::io::stdout();
        let mut out = LineOut(stdout.lock());
        run(&args, &mut out)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        bail!("display-capture: Linux only (xnuhost's display.sock)")
    }
}

/// stdout, flushed at each line (the script printed with flush=True).
struct LineOut<W: Write>(W);

impl<W: Write> Write for LineOut<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(buf)?;
        if buf[..n].contains(&b'\n') {
            self.0.flush()?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(magic: &[u8; 4], w: [u32; 4]) -> Vec<u8> {
        let mut m = magic.to_vec();
        w.iter().for_each(|v| m.extend_from_slice(&v.to_le_bytes()));
        m
    }

    #[test]
    fn hello_and_flush() {
        assert_eq!(hello(&message(b"XHD1", [8, 2, 40, 1]), 1).unwrap(), Hello { width: 8, height: 2, stride: 40, format: 1 });
        assert_eq!(hello(&message(b"XHD1", [8, 2, 64, 2]), 1).unwrap().format, RGBA16F);
        assert_eq!(hello(&message(b"XHD1", [8, 2, 40, 1]), 0).unwrap_err().to_string(), "display-capture: no hello with a memfd");
        assert!(hello(&message(b"XHDF", [8, 2, 40, 1]), 1).is_err());
        assert_eq!(hello(&message(b"XHD1", [8, 2, 31, 1]), 1).unwrap_err().to_string(), "display-capture: unusable hello 8x2 stride 31 format 1");
        assert!(hello(&message(b"XHD1", [8, 2, 40, 2]), 1).is_err(), "a half-float row is 8 bytes a pixel");
        assert!(hello(&message(b"XHD1", [16385, 2, 65540, 1]), 1).is_err());
        assert!(hello(&message(b"XHD1", [8, 2, 40, 3]), 1).is_err());
        assert_eq!(flush(&message(b"XHDF", [1, 2, 3, 4])), Some([1, 2, 3, 4]));
        assert_eq!(flush(&message(b"XHDP", [1, 2, 3, 4])), None);
        assert_eq!(flush(b"XHDF"), None);
    }

    /// Reads the PNG back: the signature, each chunk's CRC, IHDR and the rows.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let (mut at, mut idat, mut size, mut kinds) = (8, Vec::new(), (0, 0), Vec::new());
        while at < png.len() {
            let n = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let (kind, data) = (&png[at + 4..at + 8], &png[at + 8..at + 8 + n]);
            let mut crc = flate2::Crc::new();
            crc.update(&png[at + 4..at + 8 + n]);
            assert_eq!(crc.sum().to_be_bytes(), png[at + 8 + n..at + 12 + n]);
            kinds.push(String::from_utf8_lossy(kind).into_owned());
            match kind {
                b"IHDR" => {
                    assert_eq!(&data[8..], [8, 2, 0, 0, 0]);
                    size = (u32::from_be_bytes(data[..4].try_into().unwrap()), u32::from_be_bytes(data[4..8].try_into().unwrap()));
                }
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            at += 12 + n;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        let mut rows = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&idat[..]), &mut rows).unwrap();
        (size.0, size.1, rows)
    }

    #[test]
    fn png_of_a_bgrx_frame() {
        // 3x2, stride 16 (4 bytes of padding a row)
        let h = Hello { width: 3, height: 2, stride: 16, format: BGRX8 };
        let mut frame = vec![0xeeu8; 32];
        for (i, px) in [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12], [13, 14, 15], [16, 17, 18]].iter().enumerate() {
            let at = (i / 3) * 16 + (i % 3) * 4;
            frame[at..at + 3].copy_from_slice(px); // B, G, R
        }
        let (w, ht, rows) = decode(&png(&h, &frame));
        assert_eq!((w, ht), (3, 2));
        assert_eq!(rows, [0, 3, 2, 1, 6, 5, 4, 9, 8, 7, 0, 12, 11, 10, 15, 14, 13, 18, 17, 16]);
    }

    #[test]
    fn half_float_frames() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0x3800), 0.5);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0x0001), (2f32).powi(-24));
        assert_eq!((srgb8(0.0), srgb8(1.0), srgb8(4.0), srgb8(-1.0)), (0, 255, 255, 0));
        let h = Hello { width: 1, height: 1, stride: 8, format: RGBA16F };
        let frame: Vec<u8> = [0x3c00u16, 0x0000, 0x3800, 0x3c00].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(decode(&png(&h, &frame)).2, [0, 255, 0, 188]);
    }

    #[test]
    fn colours_are_sampled() {
        // 10x8: the sample is x 0 and 5 of rows 0 and 7
        let h = Hello { width: 10, height: 8, stride: 40, format: BGRX8 };
        let mut frame = vec![0u8; 320];
        frame[5 * 4] = 9; // (5, 0)
        frame[7 * 40 + 1] = 9; // (0, 7)
        frame[7 * 40 + 5 * 4 + 3] = 9; // (5, 7): only X differs, the same colour as black
        frame[3 * 40] = 7; // row 3: not sampled
        assert_eq!(colours(&h, &frame), 3);
    }

    /// A stand-in xnuhost: display.sock, the hello with a memfd, two flushes, then the end.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_session_against_a_stand_in_xnuhost() {
        let dir = std::env::temp_dir().join(format!("oneios-display-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("display.sock");
        let h = Hello { width: 4, height: 3, stride: 16, format: BGRX8 };
        // SAFETY: test-only socket calls on local buffers.
        let (listen, memfd) = unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0);
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as _;
            for (d, s) in addr.sun_path.iter_mut().zip(std::os::unix::ffi::OsStrExt::as_bytes(sock_path.as_os_str())) {
                *d = *s as _;
            }
            assert_eq!(libc::bind(fd, &addr as *const _ as *const _, std::mem::size_of::<libc::sockaddr_un>() as _), 0);
            assert_eq!(libc::listen(fd, 1), 0);
            let m = libc::memfd_create(c"frame".as_ptr(), 0);
            assert_eq!(libc::ftruncate(m, h.len() as _), 0);
            (fd, m)
        };
        let server = std::thread::spawn(move || unsafe {
            let c = libc::accept(listen, std::ptr::null_mut(), std::ptr::null_mut());
            assert!(c >= 0);
            let pixel = [0x10u8, 0x20, 0x30, 0];
            for i in 0..12 {
                libc::pwrite(memfd, pixel.as_ptr().cast(), 4, (i * 4) as _);
            }
            let mut hello = message(b"XHD1", [4, 3, 16, 1]);
            let mut iov = libc::iovec { iov_base: hello.as_mut_ptr().cast(), iov_len: hello.len() };
            let mut control = [0u64; 4];
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(4) as _;
            let cm = libc::CMSG_FIRSTHDR(&msg);
            (*cm).cmsg_level = libc::SOL_SOCKET;
            (*cm).cmsg_type = libc::SCM_RIGHTS;
            (*cm).cmsg_len = libc::CMSG_LEN(4) as _;
            (libc::CMSG_DATA(cm) as *mut libc::c_int).write_unaligned(memfd);
            assert_eq!(libc::sendmsg(c, &msg, 0), 20);
            for rect in [[0, 0, 4, 3], [1, 1, 2, 1]] {
                let f = message(b"XHDF", rect);
                assert_eq!(libc::send(c, f.as_ptr().cast(), f.len(), 0), 20);
            }
            std::thread::sleep(Duration::from_millis(200));
            libc::close(c);
            libc::close(listen);
            libc::close(memfd);
        });
        let prefix = dir.join("shot").display().to_string();
        let args = Args { session: dir.clone(), out: Some(prefix.clone()), seconds: 20.0, every: 0.0 };
        let mut out = Vec::new();
        run(&args, &mut out).unwrap();
        server.join().unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().map(|l| l.trim_start().splitn(2, "s ").nth(1).unwrap()).collect();
        assert_eq!(lines[0], "hello 4x3 stride 16");
        assert!(lines[1].starts_with("1 flushes, last (0, 0, 4, 3), 1 colours sampled, frame "), "{text}");
        assert!(lines[1].ends_with(&format!(" -> {prefix}-001.png")), "{text}");
        // the second flush: the same picture, no line, no second file
        assert_eq!(&lines[2..], ["the socket closed after 2 flushes", "done: 2 flushes, 1 pictures"], "{text}");
        let (w, ht, rows) = decode(&std::fs::read(format!("{prefix}-001.png")).unwrap());
        assert_eq!((w, ht, &rows[..4]), (4, 3, &[0u8, 0x30, 0x20, 0x10][..]));
        assert!(!Path::new(&format!("{prefix}-002.png")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
