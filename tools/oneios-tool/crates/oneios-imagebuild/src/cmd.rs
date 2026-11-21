//! The image tool's commands, run in this process: `oneios_image::run` with the text they print given back.
//!
//! build.sh starts `oneios-tool image profile|seed-pack|manifest|record-plain` and reads what they print. Here the
//! same commands run in the process (nothing is started): for the call, the process's standard output and error
//! go to a temporary file, which is read afterwards; what the command said on standard error is said again, so a
//! failure's reason is where it was before (`serve`: a `line` event). The standard output is locked for the call,
//! so no other thread of the process writes into the file (the commands' own threads print nothing).

use oneios_common::job;
use std::io::{Read, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// What a command said: its exit status, its standard output, its standard error.
pub struct Said {
    pub status: i32,
    pub out: String,
    pub err: String,
}

/// `oneios-tool image ARGS...` in this process.
pub fn image(args: &[&str]) -> std::io::Result<Said> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let path = std::env::temp_dir().join(format!("oneios-imagebuild-{}-{}.out", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let guard = job::temp(&path);
    let (mut out_file, mut err_file) = (std::fs::File::options().read(true).write(true).create_new(true).open(&path)?, tempfile_beside(&path)?);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.flush();
    let _ = std::io::stderr().flush();
    // Safety: dup, dup2 and close of this process's own descriptors 1 and 2 and of two files it holds open.
    let status = unsafe {
        let (saved_out, saved_err) = (libc::dup(1), libc::dup(2));
        if saved_out < 0 || saved_err < 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::dup2(out_file.as_raw_fd(), 1);
        libc::dup2(err_file.0.as_raw_fd(), 2);
        let status = std::panic::catch_unwind(|| oneios_image::run(&args)).unwrap_or(1);
        let _ = lock.flush();
        let _ = std::io::stderr().flush();
        libc::dup2(saved_out, 1);
        libc::dup2(saved_err, 2);
        libc::close(saved_out);
        libc::close(saved_err);
        status
    };
    drop(lock);
    let read = |f: &mut std::fs::File| -> std::io::Result<String> {
        let mut text = String::new();
        f.seek(std::io::SeekFrom::Start(0))?;
        f.read_to_string(&mut text)?;
        Ok(text)
    };
    let said = Said { status, out: read(&mut out_file)?, err: read(&mut err_file.0)? };
    drop(guard);
    if !said.err.is_empty() {
        eprint!("{}", said.err);
    }
    Ok(said)
}

/// A second temporary file, removed with its guard.
struct Beside(std::fs::File, #[allow(dead_code)] job::Temp);

fn tempfile_beside(path: &std::path::Path) -> std::io::Result<Beside> {
    let err = path.with_extension("err");
    let guard = job::temp(&err);
    Ok(Beside(std::fs::File::options().read(true).write(true).create_new(true).open(&err)?, guard))
}
