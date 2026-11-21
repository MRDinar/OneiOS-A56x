//! `image aea-decrypt IN OUT`: an Apple Encrypted Archive of the IPSW (the OS image and the SystemOS cryptex) to
//! its plain file, here and on the phone, with no outside program (the `ipsw` CLI did this on the PC until
//! 2026-11-21). tools/oneios-tool/README.md "image aea-decrypt" has the commands; docs/image-format.md the pins.
//!
//! The format, as blacktop/ipsw's `pkg/aea` (MIT, v3.1.743) reads it and Apple's `aea` program writes it; all
//! numbers little-endian. The IPSW's images are profile 1 (a symmetric key, no signature):
//!
//! ```text
//! "AEA1" | u32 profile (low 24 bits) and scrypt strength (high 8) | u32 auth size | auth data (u32 length, KEY\0VALUE)*
//! main salt (32)
//! root header MAC (32) | root header, encrypted (48) | the first cluster's MAC (32)
//! cluster 0: segment headers, encrypted (N x (u32 plain, u32 packed, checksum)) | next cluster's MAC (32) | N segment MACs (32 each)
//!            segment 0 .. N-1, encrypted (packed bytes each)
//! cluster 1 ...
//! padding (to the archive's size)
//! ```
//!
//! Keys, all HKDF-SHA256 (no salt unless named): main = HKDF(key, salt = main salt, "AEA_AMK" | u32 profile word),
//! 32 bytes; root header = HKDF(main, "AEA_RHEK"), cluster c = HKDF(main, "AEA_CK" | u32 c) and its header key
//! HKDF(cluster, "AEA_CHEK"), segment i = HKDF-Expand(HKDF-Extract(cluster), "AEA_SK" | u32 i), padding =
//! HKDF(main, "AEA_PAK"). The 80-byte ones are a MAC key (32), an AES-256 key (32) and a CTR IV (16). MACs are
//! HMAC-SHA256 of SALT | DATA | u64 length of SALT: the root header's salt is the first cluster's MAC and the auth
//! data, a cluster header's the MACs that follow its segment headers, a segment's nothing. So every byte is
//! authenticated before it is used: the root header's MAC (a wrong key, or a changed header, auth data or salt,
//! fails here), each cluster's MAC chained from the one before, each segment's MAC over its encrypted bytes, and the
//! padding's MAC (the last cluster's "next" MAC). Then AES-256-CTR, the segment's compression (LZFSE in 24A446;
//! LZVN, LZBITMAP, zlib and stored too) and its checksum (SHA-256 in 24A446; Murmur64A and none too).
//!
//! The archive key is not in the file: the auth data names Apple's key service. `com.apple.wkms.fcs-key-url` serves a
//! P-256 private key (PKCS#8 PEM) and `com.apple.wkms.fcs-response` holds `enc-request` (an HPKE encapsulated key)
//! and `wrapped-key` (the archive key sealed to it): HPKE base mode, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256,
//! AES-256-GCM, no info, no AAD (RFC 9180). The key is fetched through the tool's own HTTP client (`transfer`: its
//! tries, waits and give-up time) and may be kept in a cache folder the caller names, never anywhere else.
//!
//! The work: one reader (the calling thread) walks the cluster headers and hands out segments, `jobs` threads each
//! read, check, decrypt, decompress and check one segment at a time, and one writer puts them in order, hashes the
//! plain stream (SHA-256) and writes `OUT.part`. Buffers go round a pool of a fixed count, so memory is bounded
//! (about 70 MiB with 1 MiB segments and 8 threads), whatever the archive's size. Every loop asks `job::check()` and
//! the writer counts the bytes (the step's progress). `OUT.part` is renamed to `OUT` last. With `resume`, the part and a
//! state file (`OUT.part.state`: which archive, how many plain bytes are in the part, written after an fdatasync)
//! stay when the work ends early, and the next run hashes the part's bytes again and goes on from there; without it
//! they are removed.

use crate::{crash, err, Fail};
use aes::cipher::{KeyIvInit, StreamCipher};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use oneios_common::job;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;
type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

const MAGIC: &[u8; 4] = b"AEA1";
/// Symmetric encryption, no signature: the profile of the IPSW's images.
pub const PROFILE_SYMMETRIC: u32 = 1;
const HEADER: usize = 12;
/// The root header block after the main salt: its MAC, the header, the first cluster's MAC.
const ROOT_BLOCK: usize = 32 + 48 + 32;
const AUTH_MAX: usize = 16 << 20;
/// Apple's limits (AEAContext's setters).
const SEGMENT_MAX: u32 = 256 << 20;
const SEGMENTS_PER_CLUSTER_MAX: u32 = 256 << 10;
/// What the buffers in flight may take together, at most (at least two segments are always in flight).
const MEMORY: u64 = 64 << 20;
/// With `resume`, the state is written after this many plain bytes (and an fdatasync of the part).
const SAVE_EVERY: u64 = 512 << 20;
/// The key service's answer is a PEM of about 240 bytes.
const KEY_MAX: u64 = 64 << 10;

/// Where the archive key comes from.
#[derive(Clone, Debug)]
pub enum AeaKey {
    /// The archive's own fields: Apple's key service (`com.apple.wkms.fcs-key-url`, `fcs-response`), or an
    /// `encryption_key` field. `cache`: a folder where the service's key is kept for the next archive (both images
    /// of 24A446 name the same key) and looked for first.
    Service { cache: Option<PathBuf> },
    /// The key service's P-256 key, a PEM file (what the key URL serves).
    Pem(PathBuf),
    /// The archive key itself (32 bytes).
    Raw([u8; 32]),
}

/// What `aea_decrypt` is asked to do.
pub struct AeaOptions<'a> {
    pub key: AeaKey,
    /// Worker threads (0: the request's, `job::jobs()`).
    pub jobs: usize,
    /// Keep the part and its state when the work ends early, and go on from them.
    pub resume: bool,
    /// The plain file's SHA-256 (lower-case hex) it must have: else the part is removed and the call fails.
    pub plain_sha256: Option<String>,
    /// Lines for the user (the key's origin, a resume): `println!` for the command line.
    pub log: &'a (dyn Fn(String) + Sync),
    /// A step of a larger request (`make-image`): no steps of its own are said (only the progress of the caller's
    /// `decrypt` step, in plain bytes, a resume's hashing in it too) and no room is asked for (the caller asked once
    /// for all of its work).
    pub embedded: bool,
}

/// How a decryption went.
#[derive(Clone, Debug, PartialEq)]
pub struct AeaDone {
    pub out: String,
    /// The plain file's size and SHA-256.
    pub bytes: u64,
    pub sha256: String,
    /// The archive's size.
    pub encrypted: u64,
    /// Plain bytes that were in the part from an earlier run.
    pub resumed_from: u64,
    /// Where the key came from: `service`, `cache`, `archive` (its `encryption_key` field), `pem`, `given`; `none`
    /// when `OUT` was there already with the SHA-256 asked for.
    pub key_from: &'static str,
    /// The compression's name (`lzfse`, `lzvn`, `lzbitmap`, `zlib`, `none`) and the checksum's (`sha256`, `murmur`,
    /// `none`).
    pub compression: &'static str,
    pub checksum: &'static str,
    pub segment_size: u32,
    pub segments_per_cluster: u32,
    pub clusters: u64,
    /// `OUT` was there with the SHA-256 asked for: nothing was done.
    pub already: bool,
}

// ---------------------------------------------------------------- the prologue

/// The archive's header, auth data, main salt and root header block, as they are in the file.
pub struct Prologue {
    /// The profile (low 24 bits) and the scrypt strength.
    pub word: u32,
    pub auth: Vec<u8>,
    salt: [u8; 32],
    root_mac: [u8; 32],
    root_enc: [u8; 48],
    first_mac: [u8; 32],
    /// The SHA-256 of all of it (what ipsw calls the archive's ID): which archive a resume's part is of.
    pub id: String,
}

impl Prologue {
    pub fn profile(&self) -> u32 {
        self.word & 0xff_ffff
    }

    /// The end of the prologue: where the first cluster begins.
    fn len(&self) -> u64 {
        (HEADER + self.auth.len() + 32 + ROOT_BLOCK) as u64
    }

    /// The auth data's fields (KEY, VALUE), in order.
    pub fn fields(&self) -> Vec<(String, Vec<u8>)> {
        let mut head = Vec::with_capacity(HEADER + self.auth.len());
        head.extend_from_slice(MAGIC);
        head.extend_from_slice(&self.word.to_le_bytes());
        head.extend_from_slice(&(self.auth.len() as u32).to_le_bytes());
        head.extend_from_slice(&self.auth);
        crate::aea::aea_info(&head).map(|(_, fields)| fields).unwrap_or_default()
    }

    fn field(&self, key: &str) -> Option<Vec<u8>> {
        self.fields().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

fn read_at(file: &File, buf: &mut [u8], at: u64, path: &str) -> Result<(), Fail> {
    file.read_exact_at(buf, at).map_err(|e| crash!("{path}: reading {} bytes at {at}: {e}", buf.len()))
}

/// Read and check the prologue of an AEA1 archive of `size` bytes.
pub fn read_prologue(file: &File, size: u64, path: &str) -> Result<Prologue, Fail> {
    let mut head = [0u8; HEADER];
    if size < HEADER as u64 {
        return Err(err!("{path}: not an AEA archive (it has {size} bytes)"));
    }
    read_at(file, &mut head, 0, path)?;
    if &head[..4] != MAGIC {
        return Err(err!("{path}: not an AEA archive (no AEA1 magic)"));
    }
    let (word, auth_len) = (le32(&head[4..]), le32(&head[8..]) as usize);
    if auth_len > AUTH_MAX {
        return Err(err!("{path}: the auth data claims {auth_len} bytes, more than the {AUTH_MAX} this reads"));
    }
    let end = (HEADER + auth_len + 32 + ROOT_BLOCK) as u64;
    if size < end {
        return Err(err!("{path}: the archive is cut short ({size} bytes, its header alone needs {end})"));
    }
    let mut rest = vec![0u8; auth_len + 32 + ROOT_BLOCK];
    read_at(file, &mut rest, HEADER as u64, path)?;
    let mut id = Sha256::new();
    id.update(head);
    id.update(&rest);
    let (auth, more) = rest.split_at(auth_len);
    let take = |at: usize, n: usize| more[at..at + n].to_vec();
    Ok(Prologue {
        word,
        auth: auth.to_vec(),
        salt: take(0, 32).try_into().unwrap(),
        root_mac: take(32, 32).try_into().unwrap(),
        root_enc: take(64, 48).try_into().unwrap(),
        first_mac: take(112, 32).try_into().unwrap(),
        id: oneios_common::hex(&id.finalize()),
    })
}

// ---------------------------------------------------------------- the primitives

/// A MAC key, an AES-256 key and a CTR IV.
struct Keys {
    mac: [u8; 32],
    key: [u8; 32],
    iv: [u8; 16],
}

impl Keys {
    fn from(okm: &[u8; 80]) -> Keys {
        Keys { mac: okm[..32].try_into().unwrap(), key: okm[32..64].try_into().unwrap(), iv: okm[64..].try_into().unwrap() }
    }

    fn crypt(&self, data: &mut [u8]) {
        Aes256Ctr::new(&self.key.into(), &self.iv.into()).apply_keystream(data);
    }
}

fn hkdf<const N: usize>(ikm: &[u8], salt: Option<&[u8]>, info: &[&[u8]]) -> [u8; N] {
    let mut out = [0u8; N];
    Hkdf::<Sha256>::new(salt, ikm).expand_multi_info(info, &mut out).expect("an HKDF length of at most 255 blocks");
    out
}

fn expand80(prk: &Hkdf<Sha256>, info: &[&[u8]]) -> Keys {
    let mut out = [0u8; 80];
    prk.expand_multi_info(info, &mut out).expect("80 bytes");
    Keys::from(&out)
}

/// HMAC-SHA256 of SALT | DATA | u64 length of SALT (`salt` and `data` in parts).
fn mac_of(key: &[u8; 32], salt: &[&[u8]], data: &[&[u8]]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("any key length");
    let mut salt_len = 0u64;
    for part in salt {
        mac.update(part);
        salt_len += part.len() as u64;
    }
    for part in data {
        mac.update(part);
    }
    mac.update(&salt_len.to_le_bytes());
    mac
}

/// The archive's keys below the archive key.
struct Archive {
    main: [u8; 32],
}

impl Archive {
    fn new(key: &[u8; 32], p: &Prologue) -> Archive {
        Archive { main: hkdf::<32>(key, Some(&p.salt), &[b"AEA_AMK", &p.word.to_le_bytes()]) }
    }

    fn keys(&self, info: &[u8]) -> Keys {
        Keys::from(&hkdf::<80>(&self.main, None, &[info]))
    }

    /// Cluster `c`'s key (as HKDF-Extract's result, for the segments' keys) and its header's keys.
    fn cluster(&self, c: u32) -> ([u8; 32], Keys) {
        let key = hkdf::<32>(&self.main, None, &[b"AEA_CK", &c.to_le_bytes()]);
        let header = Keys::from(&hkdf::<80>(&key, None, &[b"AEA_CHEK"]));
        let (prk, _) = Hkdf::<Sha256>::extract(None, &key);
        (prk.into(), header)
    }
}

// ---------------------------------------------------------------- the root header

#[derive(Clone, Copy, Debug)]
struct Root {
    plain: u64,
    encrypted: u64,
    segment: u32,
    per_cluster: u32,
    compression: u8,
    checksum: u8,
}

fn compression_name(c: u8) -> Option<&'static str> {
    Some(match c {
        b'-' => "none",
        b'e' => "lzfse",
        b'f' => "lzvn",
        b'b' => "lzbitmap",
        b'z' => "zlib",
        b'4' => "lz4",
        b'x' => "lzma",
        _ => return None,
    })
}

fn checksum_size(c: u8) -> Option<usize> {
    match c {
        0 => Some(0),
        1 => Some(8),
        2 => Some(32),
        _ => None,
    }
}

fn checksum_name(c: u8) -> &'static str {
    ["none", "murmur", "sha256"].get(c as usize).copied().unwrap_or("?")
}

/// The root header, authenticated and decrypted; `None`: its MAC is not right (a wrong key, or a changed byte of
/// the prologue).
fn open_root(a: &Archive, p: &Prologue) -> Option<Root> {
    let keys = a.keys(b"AEA_RHEK");
    mac_of(&keys.mac, &[&p.first_mac, &p.auth], &[&p.root_enc]).verify_slice(&p.root_mac).ok()?;
    let mut r = p.root_enc;
    keys.crypt(&mut r);
    Some(Root { plain: le64(&r), encrypted: le64(&r[8..]), segment: le32(&r[16..]), per_cluster: le32(&r[20..]), compression: r[24], checksum: r[25] })
}

fn check_root(r: &Root, size: u64, path: &str) -> Result<(), Fail> {
    if r.encrypted != size {
        return Err(err!("{path}: the archive has {size} bytes, its root header says {}", r.encrypted));
    }
    if r.segment == 0 || r.segment > SEGMENT_MAX || r.per_cluster == 0 || r.per_cluster > SEGMENTS_PER_CLUSTER_MAX || r.plain > i64::MAX as u64 {
        return Err(err!("{path}: the root header's sizes are out of range (segment {}, {} per cluster)", r.segment, r.per_cluster));
    }
    if (r.plain.max(1) - 1) / (r.segment as u64 * r.per_cluster as u64) >= 1 << 32 {
        return Err(err!("{path}: too many clusters"));
    }
    match compression_name(r.compression) {
        None => return Err(err!("{path}: compression {:#04x} is not known", r.compression)),
        Some(name @ ("lz4" | "lzma")) => return Err(err!("{path}: {name} segments are not read by this tool (the IPSW's images are LZFSE)")),
        Some(_) => {}
    }
    if checksum_size(r.checksum).is_none() {
        return Err(err!("{path}: checksum kind {} is not known", r.checksum));
    }
    Ok(())
}

// ---------------------------------------------------------------- the archive key

fn ok_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

/// The cache file of a key URL: the URL's last path part (the key's id), with only safe characters.
fn cache_name(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or("key");
    let name: String = last.chars().map(|c| if c.is_ascii_alphanumeric() || "-_=.".contains(c) { c } else { '_' }).collect();
    format!("{}.pem", name.trim_start_matches('.'))
}

/// The archive key, sealed by HPKE in the archive's `fcs-response`, opened with the key service's P-256 key.
pub fn unwrap_key(pem: &str, fcs_response: &[u8]) -> Result<[u8; 32], String> {
    use base64::Engine;
    use hpke::{aead::AesGcm256, kdf::HkdfSha256, kem::DhP256HkdfSha256, Deserializable, Kem, OpModeR};
    use p256::pkcs8::DecodePrivateKey;
    let answer: serde_json::Value = serde_json::from_slice(fcs_response).map_err(|e| format!("the fcs-response field is not JSON: {e}"))?;
    let field = |name: &str| -> Result<Vec<u8>, String> {
        let text = answer.get(name).and_then(|v| v.as_str()).ok_or_else(|| format!("the fcs-response field has no {name}"))?;
        base64::engine::general_purpose::STANDARD.decode(text.trim()).map_err(|e| format!("the fcs-response's {name} is not base64: {e}"))
    };
    let (enc, wrapped) = (field("enc-request")?, field("wrapped-key")?);
    let secret = p256::SecretKey::from_pkcs8_pem(pem.trim())
        .or_else(|_| p256::SecretKey::from_sec1_pem(pem.trim()))
        .map_err(|e| format!("the key service's answer is not a P-256 private key in PEM: {e}"))?;
    let sk = <DhP256HkdfSha256 as Kem>::PrivateKey::from_bytes(&secret.to_bytes()).map_err(|e| format!("the P-256 key: {e:?}"))?;
    let encapped = <DhP256HkdfSha256 as Kem>::EncappedKey::from_bytes(&enc).map_err(|e| format!("the enc-request is not a P-256 point: {e:?}"))?;
    let key = hpke::single_shot_open::<AesGcm256, HkdfSha256, DhP256HkdfSha256>(&OpModeR::Base, &sk, &encapped, b"", &wrapped, b"")
        .map_err(|_| "the key service's key does not open this archive's wrapped key (HPKE): it is not this archive's key".to_string())?;
    key.as_slice().try_into().map_err(|_| format!("the unwrapped archive key has {} bytes, not 32", key.len()))
}

/// GET a small file through the tool's HTTP client (its tries, waits and give-up time).
fn fetch_small(url: &str) -> Result<Vec<u8>, String> {
    let source = crate::transfer::Source::open(url)?;
    if source.size == 0 || source.size > KEY_MAX {
        return Err(format!("{url}: {} bytes, not a key", source.size));
    }
    source.read(0, source.size - 1)
}

/// The archive key, and where it came from.
fn archive_key(p: &Prologue, how: &AeaKey, path: &str, log: &(dyn Fn(String) + Sync)) -> Result<([u8; 32], &'static str), Fail> {
    let response = || p.field("com.apple.wkms.fcs-response").ok_or_else(|| err!("{path}: no com.apple.wkms.fcs-response field: the archive's key is not Apple's key service's"));
    match how {
        AeaKey::Raw(key) => Ok((*key, "given")),
        AeaKey::Pem(pem_path) => {
            let pem = std::fs::read_to_string(pem_path).map_err(|e| crash!("{}: {e}", pem_path.display()))?;
            Ok((unwrap_key(&pem, &response()?).map_err(|e| err!("{path}: {e}"))?, "pem"))
        }
        AeaKey::Service { cache } => {
            if let Some(hex) = p.field("encryption_key") {
                let text = String::from_utf8_lossy(&hex).trim().to_string();
                let bytes = (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap_or("x"), 16)).collect::<Result<Vec<u8>, _>>();
                let key: [u8; 32] = bytes.ok().and_then(|b| b.try_into().ok()).ok_or_else(|| err!("{path}: its encryption_key field is not 32 bytes of hex"))?;
                return Ok((key, "archive"));
            }
            let url = p.field("com.apple.wkms.fcs-key-url").ok_or_else(|| err!("{path}: no com.apple.wkms.fcs-key-url field: this archive's key cannot be fetched from Apple"))?;
            let url = String::from_utf8(url).map_err(|_| err!("{path}: the key URL is not text"))?;
            if !ok_url(&url) {
                return Err(err!("{path}: the key URL is not https: {url}"));
            }
            let response = response()?;
            let cached = cache.as_ref().map(|dir| dir.join(cache_name(&url)));
            if let Some(file) = cached.as_ref().filter(|f| f.is_file()) {
                match std::fs::read_to_string(file).map_err(|e| e.to_string()).and_then(|pem| unwrap_key(&pem, &response)) {
                    Ok(key) => {
                        log(format!("key: the key service's key from {}", file.display()));
                        return Ok((key, "cache"));
                    }
                    Err(e) => log(format!("key: {} does not open this archive ({e}): fetching it again", file.display())),
                }
            }
            log(format!("key: fetching {url}"));
            let pem = fetch_small(&url).map_err(|e| err!("{path}: the key service: {e}"))?;
            let pem = String::from_utf8(pem).map_err(|_| err!("{path}: the key service's answer is not text"))?;
            let key = unwrap_key(&pem, &response).map_err(|e| err!("{path}: {e}"))?;
            if let Some(file) = cached {
                // (the cache is the caller's work folder; only the owner may read it)
                let keep = || -> std::io::Result<()> {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::create_dir_all(file.parent().unwrap())?;
                    let tmp = file.with_extension("pem.tmp");
                    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?.write_all(pem.as_bytes())?;
                    std::fs::rename(&tmp, &file)
                };
                match keep() {
                    Ok(()) => log(format!("key: kept in {}", file.display())),
                    Err(e) => log(format!("key: not kept in {}: {e}", file.display())),
                }
            }
            Ok((key, "service"))
        }
    }
}

// ---------------------------------------------------------------- the segments

/// One segment for a worker.
struct Seg {
    /// Its place in the plain file, in segments.
    seq: u64,
    cluster: u32,
    index: u32,
    at: u64,
    packed: u32,
    plain: u32,
    sum: [u8; 32],
    mac: [u8; 32],
    prk: [u8; 32],
    buf: Vec<u8>,
}

/// AEA's Murmur64A (MurmurHash2, 64 bits), seed 0xE2236FDC26A5F6D2.
pub fn murmur64(data: &[u8]) -> u64 {
    const M: u64 = 0xc6a4a7935bd1e995;
    let mut h = 0xE2236FDC26A5F6D2u64 ^ (data.len() as u64).wrapping_mul(M);
    let mut chunks = data.chunks_exact(8);
    for c in &mut chunks {
        let mut k = u64::from_le_bytes(c.try_into().unwrap()).wrapping_mul(M);
        k ^= k >> 47;
        h = (h ^ k.wrapping_mul(M)).wrapping_mul(M);
    }
    let rest = chunks.remainder();
    for (i, b) in rest.iter().enumerate() {
        h ^= (*b as u64) << (8 * i);
    }
    if !rest.is_empty() {
        h = h.wrapping_mul(M);
    }
    h ^= h >> 47;
    h = h.wrapping_mul(M);
    h ^ (h >> 47)
}

/// Decode `src` (a segment's plain bytes, compressed) into `out`, which must come out `want` bytes long.
fn decode(kind: u8, src: &[u8], want: usize, out: &mut Vec<u8>, scratch: &mut Vec<u8>) -> Result<(), String> {
    out.clear();
    match kind {
        b'e' => lzfse_rust::decode_bytes(src, out).map(|_| ()).map_err(|e| format!("LZFSE: {e}"))?,
        b'f' => {
            // a bare LZVN payload, as one `bvxn` block of an LZFSE stream (as apfs/decmpfs.rs does)
            scratch.clear();
            scratch.extend_from_slice(b"bvxn");
            scratch.extend_from_slice(&(want as u32).to_le_bytes());
            scratch.extend_from_slice(&(src.len() as u32).to_le_bytes());
            scratch.extend_from_slice(src);
            scratch.extend_from_slice(b"bvx$");
            lzfse_rust::decode_bytes(scratch, out).map(|_| ()).map_err(|e| format!("LZVN: {e}"))?
        }
        b'b' => zbitmap::decompress(src, out, want).map(|_| ()).map_err(|e| format!("LZBITMAP: {e}"))?,
        b'z' => {
            flate2::read::ZlibDecoder::new(src).take(want as u64 + 1).read_to_end(out).map_err(|e| format!("zlib: {e}"))?;
        }
        _ => return Err(format!("compression {kind:#04x} is not read here")),
    }
    if out.len() != want {
        return Err(format!("decoded to {} bytes, {want} were expected", out.len()));
    }
    Ok(())
}

/// A worker's job: read, authenticate, decrypt, decompress and check one segment. The plain bytes come back in
/// `seg.buf` (the other buffer is the worker's `spare`).
fn open_segment(file: &File, path: &str, root: &Root, seg: &mut Seg, spare: &mut Vec<u8>, scratch: &mut Vec<u8>) -> Result<(), Fail> {
    let n = seg.packed as usize;
    seg.buf.resize(n, 0);
    read_at(file, &mut seg.buf[..n], seg.at, path)?;
    let prk = Hkdf::<Sha256>::from_prk(&seg.prk).expect("32 bytes");
    let keys = expand80(&prk, &[b"AEA_SK", &seg.index.to_le_bytes()]);
    if mac_of(&keys.mac, &[], &[&seg.buf]).verify_slice(&seg.mac).is_err() {
        return Err(err!("{path}: segment {} of cluster {} (at byte {}) does not authenticate: the file was changed", seg.index, seg.cluster, seg.at));
    }
    keys.crypt(&mut seg.buf);
    if seg.packed != seg.plain {
        decode(root.compression, &seg.buf, seg.plain as usize, spare, scratch).map_err(|e| err!("{path}: segment {} of cluster {}: {e}", seg.index, seg.cluster))?;
        std::mem::swap(&mut seg.buf, spare);
    }
    let good = match root.checksum {
        1 => murmur64(&seg.buf).to_le_bytes() == seg.sum[..8],
        2 => Sha256::digest(&seg.buf)[..] == seg.sum[..],
        _ => true,
    };
    if !good {
        return Err(err!("{path}: segment {} of cluster {}: its {} checksum does not match", seg.index, seg.cluster, checksum_name(root.checksum)));
    }
    Ok(())
}

/// Walk the clusters from the first, authenticating each header; `give` gets every segment whose plain bytes
/// begin at or after `from` (a multiple of the segment size). Then the padding's MAC. Returns the clusters.
fn walk(file: &File, path: &str, p: &Prologue, a: &Archive, root: &Root, from: u64, mut give: impl FnMut(Seg) -> Result<(), Fail>) -> Result<u64, Fail> {
    let spc = root.per_cluster as usize;
    let entry = 8 + checksum_size(root.checksum).unwrap();
    let mut header = vec![0u8; spc * entry + 32 + 32 * spc];
    let (mut chain, mut at, mut plain, mut cluster) = (p.first_mac, p.len(), 0u64, 0u32);
    while plain < root.plain {
        job::check().map_err(|s| err!("{s}"))?;
        if at + header.len() as u64 > root.encrypted {
            return Err(err!("{path}: cluster {cluster}'s header is beyond the end of the file"));
        }
        read_at(file, &mut header, at, path)?;
        let (heads, macs) = header.split_at_mut(spc * entry);
        let (prk, keys) = a.cluster(cluster);
        if mac_of(&keys.mac, &[macs], &[heads]).verify_slice(&chain).is_err() {
            return Err(err!("{path}: cluster {cluster}'s header (at byte {at}) does not authenticate: the file was changed"));
        }
        chain.copy_from_slice(&macs[..32]);
        keys.crypt(heads);
        at += header.len() as u64;
        for index in 0..spc {
            let e = &header[index * entry..(index + 1) * entry];
            let (size, packed) = (le32(e), le32(&e[4..]));
            let want = (root.segment as u64).min(root.plain - plain);
            if size as u64 != want {
                return Err(err!("{path}: segment {index} of cluster {cluster} has {size} plain bytes, {want} were expected"));
            }
            if want == 0 {
                if packed != 0 {
                    return Err(err!("{path}: segment {index} of cluster {cluster} is past the end and not empty"));
                }
                continue;
            }
            if packed == 0 || packed > root.segment || at + packed as u64 > root.encrypted {
                return Err(err!("{path}: segment {index} of cluster {cluster} claims {packed} bytes"));
            }
            if packed > size && root.compression == b'-' {
                return Err(err!("{path}: segment {index} of cluster {cluster} is stored and has {packed} bytes for {size}"));
            }
            if plain >= from {
                let mut seg = Seg { seq: plain / root.segment as u64, cluster, index: index as u32, at, packed, plain: size, sum: [0; 32], mac: [0; 32], prk, buf: Vec::new() };
                seg.sum[..entry - 8].copy_from_slice(&e[8..]);
                seg.mac.copy_from_slice(&header[spc * entry + 32 + 32 * index..][..32]);
                give(seg)?;
            }
            at += packed as u64;
            plain += want;
        }
        cluster += 1;
    }
    // the padding: its MAC is the last cluster's "next" MAC (as ipsw checks it; none when there is no padding)
    let pad = root.encrypted - at;
    if pad > 0 {
        let keys = a.keys(b"AEA_PAK");
        let mut mac = HmacSha256::new_from_slice(&keys.mac).unwrap();
        let mut buf = vec![0u8; pad.min(1 << 20) as usize];
        let mut left = pad;
        while left > 0 {
            job::check().map_err(|s| err!("{s}"))?;
            let n = left.min(buf.len() as u64) as usize;
            read_at(file, &mut buf[..n], root.encrypted - left, path)?;
            mac.update(&buf[..n]);
            left -= n as u64;
        }
        if mac.verify_slice(&chain).is_err() {
            return Err(err!("{path}: the padding after the last cluster ({pad} bytes) does not authenticate: the file was changed"));
        }
    }
    Ok(cluster as u64)
}

// ---------------------------------------------------------------- resume

fn state_line(p: &Prologue, size: u64, root: &Root, done: u64) -> String {
    serde_json::json!({"aea": p.id, "encrypted": size, "plain": root.plain, "done": done}).to_string() + "\n"
}

/// The plain bytes an earlier run left in the part, when its state is of this archive.
fn resumable(part: &str, state: &str, p: &Prologue, size: u64, root: &Root) -> Option<u64> {
    let text = std::fs::read_to_string(state).ok()?;
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let done = v.get("done")?.as_u64()?;
    let same = v.get("aea")?.as_str()? == p.id && v.get("encrypted")?.as_u64()? == size && v.get("plain")?.as_u64()? == root.plain;
    let have = std::fs::metadata(part).ok()?.len();
    (same && done <= have && done <= root.plain && (done % root.segment as u64 == 0 || done == root.plain)).then_some(done)
}

fn save_state(state: &str, line: &str) -> std::io::Result<()> {
    let tmp = format!("{state}.tmp");
    std::fs::write(&tmp, line)?;
    std::fs::rename(&tmp, state)
}

/// Free bytes of the volume that holds `path` (its nearest folder that exists).
fn free_bytes(path: &Path) -> Option<u64> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    let c = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).ok()?;
    // Safety: statvfs fills the zeroed block it is given; the path is a C string.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

fn space(out: &str, need: u64) -> Result<(), Fail> {
    let dir = Path::new(out).parent().unwrap_or(Path::new("."));
    if let Some(check) = crate::apfs_tools::SPACE_CHECK.get() {
        return check(dir, need);
    }
    match free_bytes(dir) {
        Some(free) if free < need => Err(err!("not enough free space in {}: {need} bytes are needed, {free} are free", dir.display())),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- the whole

/// Decrypt the AEA archive `input` into `out` (see the module comment). The steps it says (`job::step`): `key`,
/// `check` (a resume: the part's bytes hashed again), `decrypt` (progress in plain bytes), `place`.
pub fn aea_decrypt(input: &str, out: &str, opts: &AeaOptions) -> Result<AeaDone, Fail> {
    let file = File::open(input).map_err(|e| crash!("{input}: {e}"))?;
    let size = file.metadata().map_err(|e| crash!("{input}: {e}"))?.len();
    let p = read_prologue(&file, size, input)?;
    if p.profile() != PROFILE_SYMMETRIC {
        return Err(err!("{input}: AEA profile {}: only profile 1 (a symmetric key, no signature: the IPSW's images) is read here", p.profile()));
    }
    let blank = |key_from| AeaDone {
        out: out.to_string(),
        bytes: 0,
        sha256: String::new(),
        encrypted: size,
        resumed_from: 0,
        key_from,
        compression: "",
        checksum: "",
        segment_size: 0,
        segments_per_cluster: 0,
        clusters: 0,
        already: false,
    };
    if std::fs::symlink_metadata(out).is_ok() {
        if let Some(want) = &opts.plain_sha256 {
            if Path::new(out).is_file() && &crate::hashes::sha256_file(out)? == want {
                let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
                return Ok(AeaDone { bytes, sha256: want.clone(), already: true, ..blank("none") });
            }
        }
        return Err(err!("{out} is there already"));
    }
    if !opts.embedded {
        job::step("key");
    }
    let (key, key_from) = archive_key(&p, &opts.key, input, opts.log)?;
    let a = Archive::new(&key, &p);
    let root = open_root(&a, &p).ok_or_else(|| err!("{input}: the root header does not authenticate: a wrong key, or the archive's first {} bytes were changed", p.len()))?;
    check_root(&root, size, input)?;
    let mut done = AeaDone {
        compression: compression_name(root.compression).unwrap(),
        checksum: checksum_name(root.checksum),
        segment_size: root.segment,
        segments_per_cluster: root.per_cluster,
        ..blank(key_from)
    };

    if let Some(parent) = Path::new(out).parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| crash!("{}: {e}", parent.display()))?;
    }
    let (part, state) = (format!("{out}.part"), format!("{out}.part.state"));
    let names = [part.clone(), state.clone(), format!("{state}.tmp")];
    let from = if opts.resume { resumable(&part, &state, &p, size, &root).unwrap_or(0) } else { 0 };
    if from == 0 {
        names.iter().for_each(|n| {
            let _ = std::fs::remove_file(n);
        });
    }
    // Without resume the part is a temporary file (a stop, an error or a panic removes it); with it, it stays.
    let guards: Vec<job::Temp> = if opts.resume { Vec::new() } else { names.iter().map(job::temp).collect() };
    if !opts.embedded {
        space(out, root.plain - from)?;
    }
    let begin = |name: &str, total: u64| if opts.embedded { job::Progress::new("decrypt", "bytes", total) } else { job::begin(name, "bytes", total) };
    let mut writer = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part).map_err(|e| crash!("{part}: {e}"))?;
    writer.set_len(from).map_err(|e| crash!("{part}: {e}"))?;
    let mut hash = Sha256::new();
    if from > 0 {
        (opts.log)(format!("resume: {from} of {} plain bytes are in {part}: hashing them again", root.plain));
        let progress = begin("check", from);
        let mut buf = vec![0u8; 4 << 20];
        let mut at = 0u64;
        while at < from {
            job::check().map_err(|s| err!("{s}"))?;
            let n = (from - at).min(buf.len() as u64) as usize;
            read_at(&writer, &mut buf[..n], at, &part)?;
            hash.update(&buf[..n]);
            at += n as u64;
            progress.set(at);
        }
        progress.finish();
        if !opts.embedded {
            job::end();
        }
    }
    use std::io::Seek;
    writer.seek(std::io::SeekFrom::Start(from)).map_err(|e| crash!("{part}: {e}"))?;
    done.resumed_from = from;

    let progress = begin("decrypt", root.plain);
    progress.set(from);
    let seg = root.segment as u64;
    let jobs = if opts.jobs == 0 { job::jobs() } else { opts.jobs }.max(1);
    let workers = jobs.min((MEMORY / seg / 2).max(1) as usize).min(((root.plain.saturating_sub(from) + seg - 1) / seg).max(1) as usize);
    let in_flight = (workers * 4).min((MEMORY / seg) as usize).max(2);
    let (pool_tx, pool_rx) = mpsc::sync_channel::<Vec<u8>>(in_flight);
    for _ in 0..in_flight {
        pool_tx.send(Vec::with_capacity(root.segment as usize)).unwrap();
    }
    // As many places as buffers: a job holds a buffer, so the reader never waits here, only for a buffer from the
    // pool, and the pool ends when the writer and the hash thread end (a failure, a stop): the reader cannot be left
    // waiting on workers that are gone.
    let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Seg>(in_flight);
    let jobs_rx = Mutex::new(jobs_rx);
    let (res_tx, res_rx) = mpsc::sync_channel::<Result<Seg, Fail>>(in_flight);
    let state_of = |n| state_line(&p, size, &root, n);
    let (part_r, state_r) = (&part, &state);
    let counter = progress.clone();
    let (walked, wrote) = std::thread::scope(|scope| {
        for w in 0..workers {
            let (res_tx, jobs_rx, file, root) = (res_tx.clone(), &jobs_rx, &file, &root);
            std::thread::Builder::new()
                .name(format!("aea-{w}"))
                .spawn_scoped(scope, move || {
                    let (mut spare, mut scratch) = (Vec::with_capacity(root.segment as usize), Vec::new());
                    loop {
                        let next = jobs_rx.lock().unwrap().recv();
                        let Ok(mut s) = next else { return };
                        let r = job::check().map_err(|stop| err!("{stop}")).and_then(|_| open_segment(file, input, root, &mut s, &mut spare, &mut scratch));
                        let failed = r.is_err();
                        if res_tx.send(r.map(|_| s)).is_err() || failed {
                            return;
                        }
                    }
                })
                .expect("a worker thread");
        }
        drop(res_tx);
        // The hash of the plain stream has its own thread: one SHA-256 stream and the writes, each about as fast as
        // the other on the PC (to tmpfs), overlap.
        let (hash_tx, hash_rx) = mpsc::sync_channel::<Vec<u8>>(in_flight);
        let hasher = std::thread::Builder::new()
            .name("aea-hash".into())
            .spawn_scoped(scope, move || {
                for buf in hash_rx {
                    hash.update(&buf);
                    let _ = pool_tx.send(buf);
                }
                hash
            })
            .expect("the hash thread");
        let writer_thread = std::thread::Builder::new()
            .name("aea-write".into())
            .spawn_scoped(scope, move || -> Result<u64, Fail> {
                let (mut pending, mut next, mut written, mut saved) = (BTreeMap::new(), from / seg, from, from);
                while written < root.plain {
                    let s = match res_rx.recv() {
                        Ok(r) => r?,
                        Err(_) => return Err(err!("{input}: the decryption ended before the last segment")),
                    };
                    pending.insert(s.seq, s.buf);
                    while let Some(buf) = pending.remove(&next) {
                        job::check().map_err(|stop| err!("{stop}"))?;
                        writer.write_all(&buf).map_err(|e| crash!("{part_r}: {e}"))?;
                        written += buf.len() as u64;
                        counter.add(buf.len() as u64);
                        next += 1;
                        hash_tx.send(buf).map_err(|_| crash!("the hash thread ended"))?;
                        if opts.resume && written - saved >= SAVE_EVERY && written < root.plain {
                            writer.sync_data().map_err(|e| crash!("{part_r}: {e}"))?;
                            save_state(state_r, &state_of(written)).map_err(|e| crash!("{state_r}: {e}"))?;
                            saved = written;
                        }
                    }
                }
                writer.sync_data().map_err(|e| crash!("{part_r}: {e}"))?;
                Ok(written)
            })
            .expect("the writer thread");
        let walked = walk(&file, input, &p, &a, &root, from, |mut s| {
            // (no buffer: the writer ended, and its error is the one to say)
            s.buf = pool_rx.recv().map_err(|_| err!("{input}: the writer ended"))?;
            jobs_tx.send(s).map_err(|_| err!("{input}: the workers ended"))
        });
        drop(jobs_tx);
        let wrote = writer_thread.join().unwrap_or_else(|_| Err(crash!("the writer thread failed")));
        let hash = hasher.join().expect("the hash thread");
        (walked, wrote.map(|w| (w, hash)))
    });
    progress.finish();
    if !opts.embedded {
        job::end();
    }
    let (written, hash) = match (walked, wrote) {
        (Ok(clusters), Ok(w)) => {
            done.clusters = clusters;
            w
        }
        (Err(walk_err), Err(write_err)) => return Err(if write_err.to_string().contains("ended before the last segment") { walk_err } else { write_err }),
        (Err(e), Ok(_)) | (Ok(_), Err(e)) => return Err(e),
    };
    done.bytes = written;
    done.sha256 = oneios_common::hex(&hash.finalize());
    if let Some(want) = &opts.plain_sha256 {
        if &done.sha256 != want {
            drop(guards);
            names.iter().for_each(|n| {
                let _ = std::fs::remove_file(n);
            });
            return Err(err!("{input}: decrypted to SHA-256 {}, {want} was expected: nothing was kept", done.sha256));
        }
    }
    if !opts.embedded {
        job::step("place");
    }
    job::check().map_err(|s| err!("{s}"))?;
    std::fs::rename(&part, out).map_err(|e| crash!("{out}: {e}"))?;
    let _ = std::fs::remove_file(&state);
    guards.into_iter().for_each(job::Temp::keep);
    Ok(done)
}

/// `image aea-decrypt IN OUT [--key-cache DIR] [--key-file PEM] [--key base64:..|hex:..] [--sha256 HEX |
/// --profile PROFILE] [--resume] [-j N]`: the lines it prints (stderr: the key, a resume; stdout: the result).
#[allow(clippy::too_many_arguments)]
pub fn cmd(input: &str, out: &str, key_cache: Option<&str>, key_file: Option<&str>, key: Option<&str>, sha256: Option<&str>, profile: Option<&str>, resume: bool, jobs: Option<usize>) -> Result<String, Fail> {
    let how = match (key, key_file) {
        (Some(_), Some(_)) => return Err(err!("--key and --key-file: one of them")),
        (Some(text), None) => AeaKey::Raw(parse_key(text)?),
        (None, Some(pem)) => AeaKey::Pem(pem.into()),
        (None, None) => AeaKey::Service { cache: key_cache.map(PathBuf::from) },
    };
    let want = match (sha256, profile) {
        (Some(_), Some(_)) => return Err(err!("--sha256 and --profile: one of them")),
        (Some(hex), None) if crate::py::is_hex64(hex) => Some(hex.to_ascii_lowercase()),
        (Some(hex), None) => return Err(err!("--sha256 {hex}: not 64 hex digits")),
        (None, Some(profile)) => profile_pin(profile, input)?,
        (None, None) => None,
    };
    let log = |line: String| eprintln!("aea-decrypt: {line}");
    let opts = AeaOptions { key: how, jobs: jobs.unwrap_or(0), resume, plain_sha256: want.clone(), log: &log, embedded: false };
    let started = std::time::Instant::now();
    let d = aea_decrypt(input, out, &opts)?;
    if d.already {
        return Ok(format!("{}  {}  {}  (there already)", d.sha256, d.bytes, d.out));
    }
    let checked = if want.is_some() { ", SHA-256 as pinned" } else { "" };
    eprintln!(
        "aea-decrypt: {} clusters of {} {}-byte segments, {}, {} checksums, key from {}{}; {:.1} s{checked}",
        d.clusters,
        d.segments_per_cluster,
        d.segment_size,
        d.compression,
        d.checksum,
        d.key_from,
        if d.resumed_from > 0 { format!(", resumed at {}", d.resumed_from) } else { String::new() },
        started.elapsed().as_secs_f64()
    );
    Ok(format!("{}  {}  {}", d.sha256, d.bytes, d.out))
}

/// `--key base64:...` or `hex:...` (or bare hex): 32 bytes.
fn parse_key(text: &str) -> Result<[u8; 32], Fail> {
    use base64::Engine;
    let bytes = if let Some(b64) = text.strip_prefix("base64:") {
        base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()
    } else {
        let hex = text.strip_prefix("hex:").unwrap_or(text).trim();
        (hex.len() % 2 == 0).then(|| (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16)).collect::<Result<Vec<u8>, _>>().ok()).flatten()
    };
    bytes.and_then(|b| b.try_into().ok()).ok_or_else(|| err!("--key: not 32 bytes as base64:... or hex:..."))
}

/// The profile's `plain_sha256` of the member whose file name is `input`'s (none: not pinned; said on stderr).
pub fn profile_pin(profile: &str, input: &str) -> Result<Option<String>, Fail> {
    let p = crate::profile::load_profile(profile)?;
    let name = crate::py::basename(input);
    let m = p.members().into_iter().find(|m| m.get("path").and_then(|v| v.as_str()).map(crate::py::basename) == Some(name)).ok_or_else(|| err!("{profile}: no member {name}"))?;
    let pin = m.get("plain_sha256").and_then(|v| v.as_str()).map(str::to_string);
    if pin.is_none() {
        eprintln!("aea-decrypt: {profile} pins no plain_sha256 for {name}: the plain file is not checked against a pin");
    }
    Ok(pin)
}

/// A test helper, for this crate's tests and `serve`'s (tests/serve.rs): an archive of the same format made
/// here (AEA1 profile 1, stored or LZFSE segments), with no Apple data. Not used by any command.
#[doc(hidden)]
pub mod testkit {
    use super::*;

    /// PKCS#8 of a P-256 private key with its public key (RFC 5915 inside RFC 5208), as PEM: the layout of the key
    /// service's answer.
    fn pem_of(secret: &[u8; 32], public: &[u8]) -> String {
        use base64::Engine;
        assert_eq!(public.len(), 65);
        let mut der = hex_bytes("308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b0201010420");
        der.extend_from_slice(secret);
        der.extend_from_slice(&hex_bytes("a144034200"));
        der.extend_from_slice(public);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|l| std::str::from_utf8(l).unwrap()).collect();
        format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"))
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    /// A counter as the RNG of the test's HPKE seal (only the test's sender needs one).
    struct Rng(u64);

    impl hpke::rand_core::RngCore for Rng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.iter_mut().for_each(|b| *b = (self.next_u64() >> 33) as u8);
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), hpke::rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl hpke::rand_core::CryptoRng for Rng {}

    /// The key service's key (PEM) and the archive's `fcs-response` field that seals `KEY` to it.
    pub fn service_key(seed: u8, key: &[u8; 32]) -> (String, Vec<u8>) {
        use base64::Engine;
        use hpke::{aead::AesGcm256, kdf::HkdfSha256, kem::DhP256HkdfSha256, Kem, OpModeS, Serializable};
        let (sk, pk) = DhP256HkdfSha256::derive_keypair(&[seed; 32]);
        let secret: [u8; 32] = sk.to_bytes().as_slice().try_into().unwrap();
        let (enc, wrapped) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, DhP256HkdfSha256, _>(&OpModeS::Base, &pk, b"", key, b"", &mut Rng(seed as u64)).unwrap();
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let response = format!("{{\"enc-request\": \"{}\", \"wrapped-key\": \"{}\"}}", b64(&enc.to_bytes()), b64(&wrapped));
        (pem_of(&secret, &pk.to_bytes()), response.into_bytes())
    }

    /// What the test encoder makes.
    #[derive(Clone)]
    pub struct Spec {
        pub segment: u32,
        pub per_cluster: u32,
        pub compression: u8,
        pub checksum: u8,
        pub padding: usize,
        pub fields: Vec<(String, Vec<u8>)>,
    }

    impl Default for Spec {
        fn default() -> Spec {
            Spec { segment: 4096, per_cluster: 4, compression: b'-', checksum: 2, padding: 700, fields: vec![("test".into(), b"oneios".to_vec())] }
        }
    }

    /// An AEA1 profile 1 archive of `plain` (the layout of the module comment, written in the order a reader checks it).
    pub fn encrypt(plain: &[u8], key: &[u8; 32], s: &Spec) -> Vec<u8> {
        let word = PROFILE_SYMMETRIC;
        let mut auth = Vec::new();
        for (k, v) in &s.fields {
            auth.extend_from_slice(&((4 + k.len() + 1 + v.len()) as u32).to_le_bytes());
            auth.extend_from_slice(k.as_bytes());
            auth.push(0);
            auth.extend_from_slice(v);
        }
        let salt = [0x5au8; 32];
        let p = Prologue { word, auth: auth.clone(), salt, root_mac: [0; 32], root_enc: [0; 48], first_mac: [0; 32], id: String::new() };
        let a = Archive::new(key, &p);
        let (seg, spc) = (s.segment as usize, s.per_cluster as usize);
        let entry = 8 + checksum_size(s.checksum).unwrap();
        // each cluster: (its header keys, the segment headers encrypted, the segment MACs, the segments' bytes)
        let mut clusters = Vec::new();
        for (c, chunk) in plain.chunks(seg * spc).enumerate() {
            let (prk, keys) = a.cluster(c as u32);
            let (mut heads, mut macs, mut body) = (vec![0u8; spc * entry], vec![0u8; 32 * spc], Vec::new());
            for (i, piece) in chunk.chunks(seg).enumerate() {
                let mut data = match s.compression {
                    b'e' => {
                        let mut packed = Vec::new();
                        lzfse_rust::encode_bytes(piece, &mut packed).unwrap();
                        if packed.len() < piece.len() { packed } else { piece.to_vec() }
                    }
                    _ => piece.to_vec(),
                };
                let e = &mut heads[i * entry..(i + 1) * entry];
                e[..4].copy_from_slice(&(piece.len() as u32).to_le_bytes());
                e[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
                match s.checksum {
                    1 => e[8..16].copy_from_slice(&murmur64(piece).to_le_bytes()),
                    2 => e[8..40].copy_from_slice(&Sha256::digest(piece)),
                    _ => {}
                }
                let k = expand80(&Hkdf::<Sha256>::from_prk(&prk).unwrap(), &[b"AEA_SK", &(i as u32).to_le_bytes()]);
                k.crypt(&mut data);
                macs[32 * i..32 * (i + 1)].copy_from_slice(&mac_of(&k.mac, &[], &[&data]).finalize().into_bytes());
                body.extend_from_slice(&data);
            }
            keys.crypt(&mut heads);
            clusters.push((keys, heads, macs, body));
        }
        let padding: Vec<u8> = (0..s.padding).map(|i| (i * 7) as u8).collect();
        let pak = a.keys(b"AEA_PAK");
        let mut next: [u8; 32] = HmacSha256::new_from_slice(&pak.mac).unwrap().chain_update(&padding).finalize().into_bytes().into();
        let mut regions = Vec::new();
        for (keys, heads, macs, body) in clusters.into_iter().rev() {
            let mut tail = next.to_vec();
            tail.extend_from_slice(&macs);
            next = mac_of(&keys.mac, &[&tail], &[&heads]).finalize().into_bytes().into();
            let mut region = heads;
            region.extend_from_slice(&tail);
            region.extend_from_slice(&body);
            regions.push(region);
        }
        regions.reverse();
        let size = (HEADER + auth.len() + 32 + ROOT_BLOCK + regions.iter().map(Vec::len).sum::<usize>() + padding.len()) as u64;
        let mut root = [0u8; 48];
        root[..8].copy_from_slice(&(plain.len() as u64).to_le_bytes());
        root[8..16].copy_from_slice(&size.to_le_bytes());
        root[16..20].copy_from_slice(&s.segment.to_le_bytes());
        root[20..24].copy_from_slice(&s.per_cluster.to_le_bytes());
        (root[24], root[25]) = (s.compression, s.checksum);
        let rhek = a.keys(b"AEA_RHEK");
        rhek.crypt(&mut root);
        let root_mac = mac_of(&rhek.mac, &[&next, &auth], &[&root]).finalize().into_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&word.to_le_bytes());
        out.extend_from_slice(&(auth.len() as u32).to_le_bytes());
        out.extend_from_slice(&auth);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&root_mac);
        out.extend_from_slice(&root);
        out.extend_from_slice(&next);
        regions.iter().for_each(|r| out.extend_from_slice(r));
        out.extend_from_slice(&padding);
        assert_eq!(out.len() as u64, size);
        out
    }
}

#[cfg(test)]
#[path = "aea_tests.rs"]
mod tests;
