// SPDX-License-Identifier: MIT
//! camctl: tries the camera bridge on the phone without xnuhost (root; One UI may be running).
//!
//! ```text
//!   camctl [options] list                     the cameras of the back end, with the role each one gets
//!   camctl [options] capture                  opens a camera, takes frames out of the ring, writes one as a PNG
//!   camctl [options] helper PATH              the same through the helper process (PATH = oneios-camera), the
//!                                             way xnuhost uses it: socket, ring descriptor, frame notes
//! options:
//!   --backend auto|ndk|hal|test   auto: ndk while system_server runs, else hal
//!   --camera WHICH                wide (default), front, ultrawide, tele3, tele5, id:<camera id>, or a list index
//!   --size WxH                    default 1280x720 (at most 1920x1080)
//!   --fps N                       default 30
//!   --frames N                    default 60
//!   --out FILE.png                default /data/local/tmp/oneios-camera-<camera>.png
//!   --nv12 FILE                   also the same frame's raw NV12
//!   --zoom RATIO                  ndk only: a zoom ratio in the request (3 or 5 on the logical rear camera picks a telephoto)
//!   -v                            the steps, the session's and device's callbacks, the first captures and every 30th
//!   -vv                           every capture callback and every picture (the diagnostic run)
//! ```
//! Exit: 0 frames arrived and the picture was written; 1 not; 2 usage.
use crate::convert::{nv12_to_rgb, png_write};
use crate::core::Core;
use crate::link::{Camera, Event};
use crate::meta::CamInfo;
use crate::proto::{OcFrame, Role, OC_FACING_BACK, OC_FACING_FRONT, OC_FRAME_BYTES_MAX, OC_MAX_CAMERAS};
use crate::proto::{OC_MAX_HEIGHT, OC_MAX_WIDTH};
use crate::ring::RingReader;
use crate::util::{atof, atoi, now, parse_size, set_verbose, strerror, truncate, verbose};
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

/// The frame notes and the stream's end, from the core's or the bridge's thread to the reading loop.
#[derive(Default)]
struct Notes {
    state: Mutex<(u64, i32)>, // notes, ended
    wake: Condvar,
}

impl Notes {
    fn note(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 += 1;
        self.wake.notify_one();
    }

    fn end(&self, error: i32) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = error;
        self.wake.notify_one();
    }

    fn ended(&self) -> i32 {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1
    }

    /// Waits for a note newer than `*seen`, at most `ms`. False on a time-out or when the stream ended.
    fn wait(&self, seen: &mut u64, ms: u64) -> bool {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, Duration::from_millis(ms), |(notes, ended)| *notes == *seen && *ended == 0)
            .unwrap_or_else(PoisonError::into_inner);
        let fresh = guard.0 != *seen;
        *seen = guard.0;
        fresh
    }
}

fn facing_name(facing: i32) -> &'static str {
    match facing {
        OC_FACING_FRONT => "front",
        OC_FACING_BACK => "back ",
        _ => "other",
    }
}

fn print_camera(index: usize, c: &CamInfo) {
    let flags = format!(
        "{}{}{}{}",
        if c.logical { "logical " } else { "" },
        if c.hal_buffers { "hal-buffers " } else { "" },
        if c.parent.is_empty() { "" } else { "physical camera of id " },
        c.parent
    );
    println!(
        "{index:2}  id {:<4} {:<9} {}  {:5.2} mm (35 mm equivalent {:3.0})  sensor {}x{}  turned {:3}  {flags}",
        c.id,
        c.role.name(),
        facing_name(c.facing),
        f64::from(c.focal_mm),
        f64::from(c.equiv35),
        c.pixel_w,
        c.pixel_h,
        c.orientation
    );
    let mut line = String::from("      YUV sizes:");
    let fitting: Vec<_> =
        c.sizes.iter().filter(|(w, h)| *w <= OC_MAX_WIDTH as i32 && *h <= OC_MAX_HEIGHT as i32).collect();
    for (w, h) in fitting.iter().take(12) {
        line += &format!(" {w}x{h}");
    }
    line += &format!("{} ({} in all)\n      fps ranges:", if fitting.len() > 12 { " ..." } else { "" }, c.sizes.len());
    for (lo, hi) in &c.fps {
        line += &format!(" {lo}-{hi}");
    }
    println!("{line}");
}

#[derive(Default)]
struct Stats {
    frames: u64,
    missed: u64,
    first_seq: u64,
    last_seq: u64,
    first_ns: u64,
    last_ns: u64,
    first_wall: f64,
    last_wall: f64,
}

impl Stats {
    fn count(&mut self, f: &OcFrame) {
        let wall = now();
        if self.frames == 0 {
            (self.first_seq, self.first_ns, self.first_wall) = (f.sequence, f.timestamp_ns, wall);
        } else if f.sequence > self.last_seq + 1 {
            self.missed += f.sequence - self.last_seq - 1;
        }
        (self.last_seq, self.last_ns, self.last_wall) = (f.sequence, f.timestamp_ns, wall);
        self.frames += 1;
    }
}

fn report(s: &Stats, f: &OcFrame, nv12: &[u8], out: &str, raw: Option<&str>, open_seconds: f64) -> i32 {
    if s.frames == 0 {
        println!("no frame arrived");
        return 1;
    }
    // The camera's rate: frames it made (read or missed) over the sensor's own time; ours: what was read.
    let made = s.last_seq - s.first_seq;
    let sensor = if s.last_ns > s.first_ns { (s.last_ns - s.first_ns) as f64 / 1e9 } else { 0.0 };
    let wall = s.last_wall - s.first_wall;
    println!(
        "frames: {} read, {} missed by the reader; first frame {:.0} ms after the start",
        s.frames,
        s.missed,
        open_seconds * 1000.0
    );
    let mut line = if made != 0 && sensor > 0.0 {
        format!("fps: {:.2} by the sensor's time", made as f64 / sensor)
    } else {
        "fps: no sensor time".into()
    };
    if s.frames > 1 && wall > 0.0 {
        line += &format!(", {:.2} read (wall clock)", (s.frames - 1) as f64 / wall);
    }
    println!("{line}");
    let (w, h, stride) = (f.width as usize, f.height as usize, f.stride as usize);
    let sum: u64 = (0..h).map(|row| nv12[row * stride..][..w].iter().map(|&v| u64::from(v)).sum::<u64>()).sum();
    println!(
        "picture: {}x{} NV12, sequence {}, mean luma {:.1}{}",
        f.width,
        f.height,
        f.sequence,
        sum as f64 / (w as f64 * h as f64),
        if sum < (w * h * 3) as u64 { " (black: a covered lens or no light?)" } else { "" }
    );
    let mut rgb = vec![0u8; w * h * 3];
    nv12_to_rgb(nv12, w, h, stride, &mut rgb);
    if let Err(e) = png_write(out, &rgb, w, h) {
        println!("{out}: {}", xh_core::sys::error_text(&e));
        return 1;
    }
    println!("wrote {out}");
    if let Some(raw) = raw {
        let written = std::fs::File::create(raw).and_then(|mut file| file.write_all(&nv12[..f.bytes as usize]));
        if let Err(e) = written {
            println!("{raw}: {}", xh_core::sys::error_text(&e));
            return 1;
        }
        println!("wrote {raw} ({} bytes, NV12 {}x{})", f.bytes, f.width, f.height);
    }
    0
}

const USAGE: &str = "usage: camctl [--backend auto|ndk|hal|test] [--camera wide|front|ultrawide|tele3|tele5|id:ID|INDEX] [--size WxH] [--fps N]\n              [--frames N] [--out FILE.png] [--nv12 FILE] [--zoom RATIO] [-v | -vv] list | capture | helper PATH-TO-oneios-camera";

struct Options {
    backend: String,
    which: String,
    out: Option<String>,
    raw: Option<String>,
    command: String,
    helper: Option<String>,
    width: i32,
    height: i32,
    fps: i32,
    frames: i32,
    zoom: f64,
}

fn options(args: &[String]) -> Option<Options> {
    let mut o = Options {
        backend: "auto".into(),
        which: "wide".into(),
        out: None,
        raw: None,
        command: String::new(),
        helper: None,
        width: 1280,
        height: 720,
        fps: 30,
        frames: 60,
        zoom: 0.0,
    };
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let v = args.get(i + 1).map(String::as_str);
        match (a, v) {
            ("-v", _) => set_verbose(if verbose() < 1 { 1 } else { 2 }),
            ("-vv", _) => set_verbose(2),
            ("--zoom", Some(v)) => {
                o.zoom = atof(v);
                i += 1;
            }
            ("--backend", Some(v)) => {
                o.backend = v.to_owned();
                i += 1;
            }
            ("--camera", Some(v)) => {
                o.which = v.to_owned();
                i += 1;
            }
            ("--size", Some(v)) if parse_size(v).is_some() => {
                (o.width, o.height) = parse_size(v).unwrap();
                i += 1;
            }
            ("--fps", Some(v)) => {
                o.fps = atoi(v);
                i += 1;
            }
            ("--frames", Some(v)) => {
                o.frames = atoi(v);
                i += 1;
            }
            ("--out", Some(v)) => {
                o.out = Some(v.to_owned());
                i += 1;
            }
            ("--nv12", Some(v)) => {
                o.raw = Some(v.to_owned());
                i += 1;
            }
            ("list" | "capture", _) if o.command.is_empty() => o.command = a.to_owned(),
            ("helper", Some(v)) if o.command.is_empty() => {
                o.command = a.to_owned();
                o.helper = Some(v.to_owned());
                i += 1;
            }
            _ => return None,
        }
        i += 1;
    }
    let bad = o.command.is_empty()
        || o.zoom < 0.0
        || o.zoom > 100.0
        || o.frames < 1
        || o.fps < 1
        || o.width < 2
        || o.height < 2
        || o.width > OC_MAX_WIDTH as i32
        || o.height > OC_MAX_HEIGHT as i32;
    (!bad).then_some(o)
}

/// Reads frames until `frames` came, the stream ended, a wait timed out or the time is up: (stats, the last frame,
/// seconds from the start to the first frame).
fn collect(
    o: &Options,
    notes: &Notes,
    started: f64,
    picture: &mut [u8],
    mut read: impl FnMut(&mut OcFrame, &mut [u8]) -> bool,
) -> (Stats, OcFrame, f64) {
    let (mut s, mut frame, mut kept, mut first, mut seen) =
        (Stats::default(), OcFrame::default(), OcFrame::default(), 0.0, 0);
    let limit = 20.0 + f64::from(o.frames) / f64::from(o.fps) * 3.0;
    while s.frames < o.frames as u64 && notes.ended() == 0 && now() - started < limit {
        if !notes.wait(&mut seen, if s.frames != 0 { 2000 } else { 15000 }) {
            break;
        }
        if !read(&mut frame, picture) {
            continue;
        }
        if s.frames == 0 {
            first = now() - started;
        }
        s.count(&frame);
        kept = frame;
    }
    (s, kept, first)
}

fn helper_command(o: &Options, out: &str, picture: &mut [u8]) -> i32 {
    // As xnuhost does it: the helper process, its socket, the ring it passes.
    if verbose() != 0 {
        // the helper prints what camctl would
        std::env::set_var("ONEIOS_CAMERA_VERBOSE", if verbose() > 1 { "2" } else { "1" });
    }
    let Some(camera) = Camera::helper(o.helper.as_deref().unwrap(), Some(&o.backend)) else { return 1 };
    let notes = Arc::new(Notes::default());
    let n = Arc::clone(&notes);
    camera.listen(Some(Arc::new(move |event| match event {
        Event::Frame => n.note(),
        Event::State(status) if status != 0 => n.end(status),
        Event::State(_) => {}
    })));
    let list = camera.list(OC_MAX_CAMERAS as usize, Duration::from_millis(15000));
    println!("helper: {} cameras", list.len());
    for c in &list {
        println!(
            "  {:<9} {}  up to {}x{} at {} fps, {} mm equivalent, turned {}",
            Role::name_of(c.role),
            if c.facing == OC_FACING_FRONT as u32 { "front" } else { "back " },
            c.max_width,
            c.max_height,
            c.max_fps,
            c.equiv35_mm,
            c.orientation
        );
    }
    let role = Role::from_name(&o.which);
    if role == Role::None {
        println!("helper: --camera takes a role here (wide, front, ultrawide, tele3, tele5)");
        return 2;
    }
    let started = now();
    if let Err(e) = camera.start(role as u32, o.width as u32, o.height as u32, o.fps as u32) {
        println!("helper: start: {}", strerror(e));
        return 1;
    }
    let (s, kept, first) = collect(o, &notes, started, picture, |f, p| camera.frame(f, p).is_ok());
    if notes.ended() != 0 {
        println!("helper: the stream ended: {}", strerror(notes.ended()));
    }
    let _ = camera.stop();
    report(&s, &kept, picture, out, o.raw.as_deref(), first)
}

fn core_command(o: &Options, out: &str, picture: &mut [u8]) -> i32 {
    let notes = Arc::new(Notes::default());
    let (n, e) = (Arc::clone(&notes), Arc::clone(&notes));
    let mut core =
        match Core::new(Some(&o.backend), Some(Box::new(move |_| n.note())), Some(Box::new(move |err| e.end(err)))) {
            Ok(core) => core,
            Err(err) => {
                println!("{}: no camera list: {}", o.backend, strerror(err));
                return 1;
            }
        };
    if let Err(err) = core.list() {
        println!("{}: no camera list: {}", core.backend_name(), strerror(err));
        return 1;
    }
    println!("back end: {}, {} cameras", core.backend_name(), core.cameras().len());
    if o.command == "list" {
        for (i, c) in core.cameras().iter().enumerate() {
            print_camera(i, c);
        }
        return if core.cameras().is_empty() { 1 } else { 0 };
    }
    let Some(index) = core.find(&o.which) else {
        println!("no camera '{}' (camctl list shows them)", o.which);
        return 1;
    };
    // The reader's own mapping of the ring, as xnuhost has it.
    let reader = match core.ring_fd().and_then(RingReader::attach) {
        Ok(reader) => reader,
        Err(err) => {
            println!("ring: {}", xh_core::sys::error_text(&err));
            return 1;
        }
    };
    let mut last = 0;
    let started = now();
    if let Err(err) = core.start(index, o.width, o.height, o.fps) {
        println!("{}: camera {} did not start: {}", core.backend_name(), core.cameras()[index].id, strerror(err));
        return 1;
    }
    let (s, kept, first) = collect(o, &notes, started, picture, |f, p| reader.read(&mut last, f, p).is_ok());
    if notes.ended() != 0 {
        println!("{}: the stream ended: {}", core.backend_name(), strerror(notes.ended()));
    }
    core.stop();
    let c = &core.cameras()[index];
    println!(
        "camera: id {} ({}), asked {}x{} at {} fps, got {}x{} at {} fps",
        c.id,
        c.role.name(),
        o.width,
        o.height,
        o.fps,
        core.width(),
        core.height(),
        core.fps()
    );
    report(&s, &kept, picture, out, o.raw.as_deref(), first)
}

/// camctl's main: its exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let Some(o) = options(&args) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let name = o.which.strip_prefix("id:").unwrap_or(&o.which);
    let default_out = truncate(&format!("/data/local/tmp/oneios-camera-{name}.png"), 127).to_owned();
    let out = o.out.clone().unwrap_or(default_out);
    println!(
        "system_server: {}",
        if crate::core::system_server_running() {
            "running (One UI's framework is up)"
        } else {
            "not running (a session, or the framework is stopped)"
        }
    );
    let mut picture = vec![0u8; OC_FRAME_BYTES_MAX as usize];
    #[cfg(target_os = "android")]
    crate::backend_ndk::set_zoom(o.zoom as f32);
    if o.command == "helper" {
        helper_command(&o, &out, &mut picture)
    } else {
        core_command(&o, &out, &mut picture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        std::iter::once("camctl").chain(line.split_whitespace()).map(String::from).collect()
    }

    #[test]
    fn command_lines() {
        let o = options(&args("--camera front --size 640x480 --fps 60 --frames 5 capture")).unwrap();
        assert!(o.which == "front" && o.width == 640 && o.height == 480 && o.fps == 60 && o.frames == 5);
        assert_eq!(o.command, "capture");
        let o = options(&args("helper /bin/oneios-camera --zoom 3")).unwrap();
        assert!(o.helper.as_deref() == Some("/bin/oneios-camera") && o.zoom == 3.0);
        assert!(options(&args("")).is_none());
        assert!(options(&args("--size 4000x3000 capture")).is_none());
        assert!(options(&args("--size big capture")).is_none());
        assert!(options(&args("list capture")).is_none());
        assert!(options(&args("--frames 0 list")).is_none());
    }
}
