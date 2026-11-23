//! oneios-tool image: the image tool, a port of scripts/image/imagetool.py (docs/image-format.md; docs/building.md
//! "Rust tools"). Same subcommands, options, output text, exit status and output files, byte for byte.
//!
//! In Rust: `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`, `manifest`, `im4p` (LZFSE by
//! the lzfse_rust crate: no `lzfse` program) and `verify` of a set folder. The others (`check-ipsw`, `fetch`,
//! `record-plain`, `aea-info`, `pack`, and `verify` of a `.tar.zst`) are run by the Python tool: this one starts
//! `python3 -I scripts/image/imagetool.py ...` for them. Only here: `apfs-ls`, `apfs-copy` (apfs/files.rs) and
//! `apfs-checkpoints` (apfs/check.rs: the checkpoints of a store copied after use, and the checks before a read) and
//! `aea-decrypt` (aea_decrypt.rs: the IPSW's AEA images, the key from Apple's key service).
//!
//! What differs from Python, on purpose:
//! - where Python ends with a traceback (a file that is not there, a damaged image), this prints one line
//!   `imagetool: ...` and exits 1;
//! - a message that quotes Python's TOML or JSON parser (a profile or a manifest that is not valid TOML or JSON)
//!   quotes this tool's parser instead;
//! - names that are not UTF-8 (file names, xattr names, names in an image's records) are refused with a message;
//!   Python carries them as escapes;
//! - the usage text names `oneios-tool image`.

pub mod apfs;
pub mod apfs_tools;
#[cfg(test)]
mod fixtures;
mod hashes;
pub mod im4p;
mod manifest;
mod profile;
mod py;
mod seed;
mod aea;
pub mod aea_decrypt;
mod archive;
mod ipsw;
pub mod rangeserve;
pub mod remotezip;
pub mod transfer;

use oneios_common::json::{self, Value};
use std::io::Write;

/// How a command ends when it does not end well.
#[derive(Debug)]
pub enum Fail {
    /// imagetool.py's `Error`: `imagetool: MESSAGE` on stderr, exit 1.
    Error(String),
    /// An IndexError or TypeError in Python's `main` (a missing argument, mostly): the usage on stderr, exit 1.
    Usage,
    /// Where Python ends with a traceback: `imagetool: MESSAGE` on stderr, exit 1.
    Crash(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Error(m) | Fail::Crash(m) => f.write_str(m),
            Fail::Usage => f.write_str("wrong arguments"),
        }
    }
}

macro_rules! err {
    ($($arg:tt)*) => { $crate::Fail::Error(format!($($arg)*)) };
}
macro_rules! crash {
    ($($arg:tt)*) => { $crate::Fail::Crash(format!($($arg)*)) };
}
pub(crate) use {crash, err};

const USAGE: &str = "\
The OneiOS image set: profile, inputs, hash lists, Data seed, manifest, archive.

    oneios-tool image profile PROFILE [--shell | --members]      check a profile; print it as JSON, shell or a table
    oneios-tool image apfs-info IMAGE                            container and volume UUIDs of a raw APFS image
    oneios-tool image hashes IMAGE OUT [--chunk BYTES]           the chunk hash list of an image
    oneios-tool image tree-copy SRC DST META [--owners-from IMAGE --owners-path DIR]
                                                                 copy a tree, recording owners, modes, times, xattrs
                                                                 (owners from IMAGE's APFS records: macOS without root)
    oneios-tool image apfs-owners IMAGE [DIR]                    the owners, modes and xattr names in IMAGE's records
    oneios-tool image seed-pack TREE META OUT --epoch N [--owners FILE]   pack the Data seed (deterministic tar)
    oneios-tool image manifest PROFILE SETDIR [--inputs FILE] [--seed-info FILE] [--tweaks FILE]
    oneios-tool image verify SET [--quick]                       SET: a set folder or its .tar.zst
    oneios-tool image im4p IN OUT                                the payload of an IM4P (LZFSE decoded here: no `lzfse` program)

Only in this tool (imagetool.py has no such command): read a volume without mounting it
    oneios-tool image apfs-ls IMAGE [PATH] [--volume N|NAME] [--newest]      a folder's entries: mode, owner, size, name
    oneios-tool image apfs-copy IMAGE SRC DST [--volume N|NAME] [--newest]   copy a file or a folder out of the image
                                                                 (--newest: the newest checkpoint that passes the
                                                                 checks, for a store copied after use; --checkpoint XID:
                                                                 that one; files kept compressed are decoded)
    oneios-tool image apfs-checkpoints IMAGE                     every checkpoint of a store, and whether the container
                                                                 and each volume pass the checks at it

The IPSW, the AEA header and the archive (ported on 2026-11-13; imagetool.py has the same commands):
    oneios-tool image check-ipsw PROFILE [--source URL|FILE]     compare the profile with the IPSW's directory and BuildManifest
    oneios-tool image fetch PROFILE INDIR [-j N] [--only ROLE,..] [--source URL|FILE]
    oneios-tool image aea-info FILE                              the key-service fields of an AEA header
    oneios-tool image pack SETDIR OUT.tar.zst                    the archive the OneOS fork imports, and OUT.tar.zst.sha256
    oneios-tool image verify SET.tar.zst                         the archive: its .sha256, the tar rules, every file
    oneios-tool image record-plain INDIR ROLE PLAIN              after an AEA was decrypted: the plain image in inputs.json

The root tweaks and the userland hook (the port of userland/roottweak and userland/image-hook, 2026-11-21):
    oneios-tool image tweaks apply --root OS --data DATA --out OUT [...]   the tweaks; --help lists the options
    oneios-tool image tweaks hook [--repo DIR]                   the image build's hook (API 1, ONEIOS_* from build.sh)
    oneios-tool image tweaks daemons|cdhash|boot-check ...       the daemon policy, CDHashes, a boot's launchd check

Only in this tool: the IPSW's AEA images decrypted here (no `ipsw` program; the key from Apple's key service)
    oneios-tool image aea-decrypt IN OUT [--key-cache DIR] [--key-file PEM | --key base64:..|hex:..]
                                 [--sha256 HEX | --profile PROFILE] [--resume] [-j N]
                                                                 authenticated: a wrong key or a changed byte fails;
                                                                 OUT.part is renamed last; --resume goes on from it

Formats: docs/image-format.md. The Rust port of scripts/image/imagetool.py: same output, byte for byte.
";

/// Arguments after the ones a command reads. imagetool.py ignores them (it takes its arguments by position and
/// looks for its options by name), so they are taken and ignored here too.
#[derive(clap::Args, Debug)]
pub struct More {
    #[arg(hide = true, allow_hyphen_values = true)]
    extra: Vec<String>,
}

/// The arguments of a command that reads them as imagetool.py does: by position, its options by name.
#[derive(clap::Args, Debug)]
pub struct Raw {
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    args: Vec<String>,
}

/// `oneios-tool image`'s subcommands (clap): imagetool.py's commands with its arguments and options. An option
/// given twice counts once, the first time, as in imagetool.py.
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Check a profile; print it as JSON, shell or a table
    Profile {
        profile: String,
        #[arg(long)]
        shell: bool,
        #[arg(long)]
        members: bool,
        #[command(flatten)]
        more: More,
    },
    /// Container and volume UUIDs of a raw APFS image
    ApfsInfo {
        image: String,
        #[command(flatten)]
        more: More,
    },
    /// The owners, modes and xattr names in an image's records
    ApfsOwners {
        image: String,
        dir: Option<String>,
        #[command(flatten)]
        more: More,
    },
    /// The chunk hash list of an image
    Hashes {
        image: String,
        out: String,
        #[arg(long, value_name = "BYTES")]
        chunk: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Copy a tree, recording owners, modes, times, xattrs
    TreeCopy {
        src: String,
        dst: String,
        meta: String,
        #[arg(long = "owners-from", value_name = "IMAGE")]
        owners_from: Vec<String>,
        #[arg(long = "owners-path", value_name = "DIR")]
        owners_path: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Pack the Data seed (deterministic tar)
    SeedPack {
        tree: String,
        meta: String,
        out: String,
        #[arg(long, value_name = "N")]
        epoch: Vec<String>,
        #[arg(long, value_name = "FILE")]
        owners: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Write manifest.json and the hash lists of a set folder
    Manifest {
        profile: String,
        setdir: String,
        #[arg(long, value_name = "FILE")]
        inputs: Vec<String>,
        #[arg(long = "seed-info", value_name = "FILE")]
        seed_info: Vec<String>,
        #[arg(long, value_name = "FILE")]
        tweaks: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Check a set folder (here) or its .tar.zst (the Python tool)
    Verify {
        set: String,
        #[arg(long)]
        quick: bool,
        #[command(flatten)]
        more: More,
    },
    /// The payload of an IM4P (LZFSE decoded here: no `lzfse` program)
    Im4p {
        input: String,
        out: String,
        #[command(flatten)]
        more: More,
    },
    /// Only in this tool: a folder's entries (mode, owner, size, name), read without a mount
    ApfsLs {
        image: String,
        path: Option<String>,
        #[arg(long, value_name = "N|NAME")]
        volume: Option<String>,
        /// The newest checkpoint that passes the checks, for a store copied after use
        #[arg(long)]
        newest: bool,
        /// The checkpoint of this transaction (see `apfs-checkpoints`)
        #[arg(long, value_name = "XID", conflicts_with = "newest")]
        checkpoint: Option<u64>,
        /// The reader: ours, apfs-fuse (Linux), or ours and apfs-fuse when ours refuses
        #[arg(long, value_enum, default_value = "auto")]
        apfs: apfs::fuse::Backend,
    },
    /// Only in this tool: copy a file or a folder out of an image, without a mount
    ApfsCopy {
        image: String,
        src: String,
        dst: String,
        #[arg(long, value_name = "N|NAME")]
        volume: Option<String>,
        /// The newest checkpoint that passes the checks, for a store copied after use
        #[arg(long)]
        newest: bool,
        /// The checkpoint of this transaction (see `apfs-checkpoints`)
        #[arg(long, value_name = "XID", conflicts_with = "newest")]
        checkpoint: Option<u64>,
        /// The reader: ours, apfs-fuse (Linux), or ours and apfs-fuse when ours refuses
        #[arg(long, value_enum, default_value = "auto")]
        apfs: apfs::fuse::Backend,
    },
    /// Only in this tool: every checkpoint of a store copied after use, and whether the container and each
    /// volume pass the checks at it
    ApfsCheckpoints { image: String },
    /// Compare the profile with the IPSW's directory and BuildManifest
    CheckIpsw(Raw),
    /// Fetch the IPSW's members a profile names
    Fetch(Raw),
    /// Record a decrypted image in inputs.json
    RecordPlain(Raw),
    /// The key-service fields of an AEA header
    AeaInfo(Raw),
    /// The archive the OneOS fork imports
    Pack(Raw),
    /// Only in this tool: decrypt an AEA image of the IPSW (the key from Apple's key service)
    AeaDecrypt {
        input: String,
        out: String,
        /// A folder where the key service's key is kept for the next archive (the work folder)
        #[arg(long = "key-cache", value_name = "DIR")]
        key_cache: Option<String>,
        /// The key service's P-256 key, a PEM file, instead of fetching it
        #[arg(long = "key-file", value_name = "PEM")]
        key_file: Option<String>,
        /// The archive key itself (base64:... or hex:...)
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        /// The plain file's SHA-256 it must have
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// Check the plain file against this profile's plain_sha256 of the member named like IN
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// Keep OUT.part and its state when the work ends early, and go on from them
        #[arg(long)]
        resume: bool,
        /// Worker threads (default: the online CPUs, at most 8)
        #[arg(short = 'j', value_name = "N")]
        jobs: Option<usize>,
    },
}

#[derive(clap::Parser)]
#[command(name = "oneios-tool image", args_override_self = true)]
struct Standalone {
    #[command(subcommand)]
    command: Command,
}

/// The usage, as a wrong command line prints it (imagetool.py prints its own there).
pub fn usage() -> &'static str {
    USAGE
}

/// Run `oneios-tool image ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    match <Standalone as clap::Parser>::try_parse_from(std::iter::once("image".to_string()).chain(args.iter().cloned())) {
        Ok(parsed) => main(parsed.command),
        Err(e) if !e.use_stderr() => {
            let _ = e.print();
            0
        }
        Err(_) => {
            eprintln!("{USAGE}");
            1
        }
    }
}

/// `image hashes IMAGE OUT`: the chunk hash list of a file, for the other crates (`prelink build`, `serve`).
/// Returns (the file's size, its SHA-256).
pub fn hashes_file(image: &str, out: &str, chunk: u64) -> Result<(u64, String), Fail> {
    hashes::write_hashes(image, out, chunk)
}

/// `image fetch PROFILE INDIR` for `serve`: `keep_parts` false removes the `.part` files of a fetch that ends early.
pub fn fetch(profile: &str, indir: &str, jobs: usize, only: Option<&str>, source: Option<&str>, keep_parts: bool) -> Result<(), Fail> {
    ipsw::fetch(profile, indir, jobs, only, source, keep_parts)
}

/// The `[os]` table of an IPSW profile, as `image profile` reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileOs {
    pub name: String,
    pub version: String,
    pub build: String,
    pub device: String,
}

/// The `[os]` of the IPSW profile at `path`, or None when `image profile` does not read the file (a host profile
/// `profiles/host-*.toml`, another schema, not TOML). For `serve`'s `make-image`, which chooses among a runtime's
/// `lib/profiles/`.
pub fn profile_os(path: &str) -> Option<ProfileOs> {
    let p = profile::load_profile(path).ok()?;
    let text = |key: &str| p.text("os", key).to_string();
    Some(ProfileOs { name: text("name"), version: text("version"), build: text("build"), device: text("device") })
}

/// The SHA-256 of a file, read in blocks (`serve`'s `sums`: a stop of the request ends the pass).
pub fn sha256_of(path: &str) -> Result<String, Fail> {
    hashes::sha256_file(path)
}

/// `image verify SET` of a set folder, for `serve`: the line `image verify` prints.
pub fn verify_set(set: &str, quick: bool) -> Result<String, Fail> {
    Ok(manifest::ok_line(&manifest::verify_dir(set, quick)?))
}

/// Run a parsed command (oneios-tool's main parses it); the exit status.
pub fn main(command: Command) -> i32 {
    match dispatch(command) {
        Ok(()) => 0,
        Err(Fail::Error(m)) | Err(Fail::Crash(m)) => {
            eprintln!("imagetool: {m}");
            1
        }
        Err(Fail::Usage) => {
            eprintln!("{USAGE}");
            1
        }
    }
}

/// With ONEIOS_IMAGE_TIMING set: how long each part of a command took, on stderr (`timing: PART 1.234 s`).
pub(crate) struct Laps {
    on: bool,
    last: std::time::Instant,
}

impl Laps {
    pub(crate) fn start() -> Laps {
        Laps { on: std::env::var_os("ONEIOS_IMAGE_TIMING").is_some_and(|v| !v.is_empty()), last: std::time::Instant::now() }
    }
    pub(crate) fn lap(&mut self, part: &str) {
        if self.on {
            eprintln!("timing: {part} {:.3} s", self.last.elapsed().as_secs_f64());
        }
        self.last = std::time::Instant::now();
    }
}

/// `print(text)`.
fn say(text: &str) -> Result<(), Fail> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush()).map_err(|e| crash!("stdout: {e}"))
}

/// `_load_json(path)`.
fn load_json(path: Option<&str>) -> Result<Option<Value>, Fail> {
    match path.filter(|p| !p.is_empty()) {
        None => Ok(None),
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| crash!("{}", py::os_error(&e, path)))?;
            json::parse(&text).map(Some).map_err(|e| crash!("{path}: {e}"))
        }
    }
}

/// `_opt(args, name)`: the first value of an option.
fn first(values: &[String]) -> Option<&str> {
    values.first().map(String::as_str)
}

fn dispatch(command: Command) -> Result<(), Fail> {
    match command {
        Command::Profile { profile, shell, members, .. } => {
            let mut args = vec![profile];
            if shell {
                args.push("--shell".into());
            }
            if members {
                args.push("--members".into());
            }
            let text = profile::cmd_profile(&args)?;
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes()).and_then(|_| out.flush()).map_err(|e| crash!("stdout: {e}"))
        }
        Command::ApfsInfo { image, .. } => say(&json::dumps(&apfs::apfs_info(&image)?.to_value(), Some(2), false)),
        Command::ApfsOwners { image, dir, .. } => {
            let tree = apfs::ApfsTree::open(&image)?;
            let walked = tree.walk(dir.as_deref().unwrap_or(""))?;
            say(&json::dumps(&apfs::owners_value(walked), None, true))
        }
        Command::Hashes { image, out, chunk, .. } => {
            let chunk = match first(&chunk) {
                None => hashes::DEFAULT_CHUNK,
                Some(text) => py::int(text, 10)
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| crash!("--chunk {}: not a count of bytes", py::repr_str(text)))?,
            };
            let (size, digest) = hashes::write_hashes(&image, &out, chunk)?;
            say(&format!("{digest}  {size}  {image}"))
        }
        Command::TreeCopy { src, dst, meta, owners_from, owners_path, .. } => {
            let owners_from = first(&owners_from).filter(|s| !s.is_empty());
            let owners_path = first(&owners_path).unwrap_or("");
            let (count, skipped) = seed::tree_copy(&src, &dst, &meta, owners_from, owners_path)?;
            let more = if skipped.is_empty() { String::new() } else { format!(", skipped {} special files", skipped.len()) };
            say(&format!("copied {count} entries{more}"))
        }
        Command::SeedPack { tree, meta, out, epoch, owners, .. } => {
            // Python: int(None) is a TypeError, int("x") a ValueError
            let epoch = first(&epoch).ok_or(Fail::Usage)?;
            let epoch = py::int(epoch, 10).ok_or_else(|| crash!("--epoch {}: not a whole number", py::repr_str(epoch)))?;
            let (count, added, changed) = seed::seed_pack(&tree, &meta, &out, epoch, first(&owners))?;
            let mut info = Value::dict();
            info.set("format", "tar");
            info.set("root", "/private/var");
            info.set("entries", count);
            info.set("added", added);
            info.set("changed", changed);
            say(&json::dumps(&info, None, false))
        }
        Command::Manifest { profile, setdir, inputs, seed_info, tweaks, .. } => {
            let profile = profile::load_profile(&profile)?;
            let inputs = load_json(first(&inputs))?;
            let seed_info = load_json(first(&seed_info))?;
            let tweaks = load_json(first(&tweaks))?;
            let (files, bytes) = manifest::make_manifest(&profile, &setdir, inputs, seed_info, tweaks)?;
            say(&format!("manifest.json: {files} files, {bytes} bytes"))
        }
        Command::Verify { set, quick, more } => {
            if std::fs::metadata(&set).map(|m| m.is_dir()).unwrap_or(false) {
                let m = manifest::verify_dir(&set, quick)?;
                return say(&manifest::ok_line(&m));
            }
            let _ = more;
            say(&manifest::ok_line(&archive::verify_archive(&set)?))
        }
        Command::Im4p { input, out, .. } => say(&im4p::cmd(&input, &out)?),
        Command::ApfsLs { image, path, volume, newest, checkpoint, apfs } => apfs_tools::ls(&image, path.as_deref().unwrap_or(""), volume.as_deref(), apfs_tools::pick(newest, checkpoint), apfs),
        Command::ApfsCopy { image, src, dst, volume, newest, checkpoint, apfs } => apfs_tools::copy(&image, &src, &dst, volume.as_deref(), apfs_tools::pick(newest, checkpoint), apfs),
        Command::ApfsCheckpoints { image } => say(apfs::Container::open(&image, apfs::Pick::Newest)?.report().trim_end()),
        Command::CheckIpsw(Raw { args }) => ipsw::cmd_check_ipsw(arg(&args, 0)?, opt(&args, "--source")?),
        Command::Fetch(Raw { args }) => {
            let jobs = match opt(&args, "-j")? {
                Some(n) => py::int(n, 10).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| crash!("invalid literal for int() with base 10: {}", py::repr_str(n)))?,
                None if oneios_common::job::jobs_given() => oneios_common::job::jobs(),
                None => 8,
            };
            ipsw::cmd_fetch(arg(&args, 0)?, arg(&args, 1)?, jobs, opt(&args, "--only")?, opt(&args, "--source")?)
        }
        Command::RecordPlain(Raw { args }) => ipsw::record_plain(arg(&args, 0)?, arg(&args, 1)?, arg(&args, 2)?),
        Command::AeaInfo(Raw { args }) => aea::cmd_aea_info(arg(&args, 0)?).map(|_| ()),
        Command::Pack(Raw { args }) => say(&format!("{}  {}", archive::pack(arg(&args, 0)?, arg(&args, 1)?)?, arg(&args, 1)?)),
        Command::AeaDecrypt { input, out, key_cache, key_file, key, sha256, profile, resume, jobs } => say(&aea_decrypt::cmd(&input, &out, key_cache.as_deref(), key_file.as_deref(), key.as_deref(), sha256.as_deref(), profile.as_deref(), resume, jobs)?),
    }
}


/// `args[index]`: a missing one is Python's IndexError, which printed the usage.
fn arg(args: &[String], index: usize) -> Result<&str, Fail> {
    args.get(index).map(String::as_str).ok_or(Fail::Usage)
}

/// `args[args.index(name) + 1] if name in args`: the value after an option's first place.
fn opt<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, Fail> {
    match args.iter().position(|a| a == name) {
        Some(at) => arg(args, at + 1).map(Some),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------- library entry points of `image build`
//
// `oneios-tool image build` and `serve`'s `make-image` (crates/oneios-imagebuild) run scripts/image/build.sh's steps
// in one process. These are the commands build.sh starts, called in the process: the same work, the text they print
// returned instead of printed. Added 2026-11-21 (image-build); nothing above changed for them.

/// `image profile PROFILE --shell` (form "shell") or `--members` (form "members"): the text the command prints.
pub fn profile_text(profile: &str, form: &str) -> Result<String, Fail> {
    profile::cmd_profile(&[profile.to_string(), format!("--{form}")])
}

/// `image seed-pack TREE META OUT --epoch N [--owners FILE]`: the JSON line the command prints (build.sh keeps it
/// as seed.json for `manifest --seed-info`).
pub fn seed_pack_line(tree: &str, meta: &str, out: &str, epoch: i128, owners: Option<&str>) -> Result<String, Fail> {
    let (count, added, changed) = seed::seed_pack(tree, meta, out, epoch, owners)?;
    let mut info = Value::dict();
    info.set("format", "tar");
    info.set("root", "/private/var");
    info.set("entries", count);
    info.set("added", added);
    info.set("changed", changed);
    Ok(json::dumps(&info, None, false))
}

/// `image manifest PROFILE SETDIR [--inputs FILE] [--seed-info FILE] [--tweaks FILE]`: the line it prints.
pub fn manifest_line(profile: &str, setdir: &str, inputs: Option<&str>, seed_info: Option<&str>, tweaks: Option<&str>) -> Result<String, Fail> {
    let profile = profile::load_profile(profile)?;
    let (files, bytes) = manifest::make_manifest(&profile, setdir, load_json(inputs)?, load_json(seed_info)?, load_json(tweaks)?)?;
    Ok(format!("manifest.json: {files} files, {bytes} bytes"))
}

/// `image record-plain INDIR ROLE PLAIN`: after an AEA was decrypted, the plain image's size and SHA-256 in
/// inputs.json.
pub fn record_plain_input(indir: &str, role: &str, plain: &str) -> Result<(), Fail> {
    ipsw::record_plain(indir, role, plain)
}
