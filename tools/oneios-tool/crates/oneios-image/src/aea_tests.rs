//! aea_decrypt's tests: archives made here by a small encoder of the same format (no Apple data), decrypted with
//! the archive key, with a key service's P-256 key from a file and from a local HTTP server (`rangeserve`), and
//! every way a wrong key or a changed byte must be refused.

use super::testkit::{encrypt, service_key, Spec};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn quiet(_: String) {}


/// Plain bytes that compress some, not all.
fn sample(n: usize) -> Vec<u8> {
    let mut x = 0x2545F491u32;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if (i / 3000) % 2 == 0 { (i % 251) as u8 } else { x as u8 }
        })
        .collect()
}

const KEY: [u8; 32] = [0x11; 32];

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("oneios-aea-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    fn write(&self, name: &str, data: &[u8]) -> String {
        let path = self.path(name);
        std::fs::write(&path, data).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn opts(key: AeaKey) -> AeaOptions<'static> {
    AeaOptions { key, jobs: 3, resume: false, plain_sha256: None, log: &quiet, embedded: false }
}

fn hex_sha(data: &[u8]) -> String {
    oneios_common::hex(&Sha256::digest(data))
}

/// Decrypt `archive` and return the result and the plain file.
fn run(dir: &Dir, archive: &[u8], o: &AeaOptions) -> Result<(AeaDone, Vec<u8>), Fail> {
    let input = dir.write("in.aea", archive);
    let out = dir.path("out.img");
    let _ = std::fs::remove_file(&out);
    let done = aea_decrypt(&input, &out, o)?;
    Ok((done, std::fs::read(&out).unwrap()))
}

fn leftovers(dir: &Dir) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&dir.0).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != "in.aea").collect();
    names.sort();
    names
}

#[test]
fn murmur64_is_aeas() {
    // the value Apple's `aea -checksum murmur64` writes (blacktop/ipsw's TestMurmur64)
    assert_eq!(murmur64(&b"synthetic archive data\n".repeat(4097)), 0xd512fdb49149a3af);
}

#[test]
fn round_trips() {
    let cases: Vec<(&str, usize, Spec)> = vec![
        ("stored, sha256, 3 clusters and a short segment", 4096 * 9 + 100, Spec::default()),
        ("lzfse, murmur", 4096 * 13 + 1, Spec { compression: b'e', checksum: 1, ..Spec::default() }),
        ("lzfse, no checksum, no padding", 4096 * 8, Spec { compression: b'e', checksum: 0, padding: 0, ..Spec::default() }),
        ("one cluster, one segment", 10, Spec { per_cluster: 1, ..Spec::default() }),
        ("empty", 0, Spec::default()),
        ("large segments", 300_000, Spec { segment: 65536, per_cluster: 3, compression: b'e', ..Spec::default() }),
    ];
    for (what, n, spec) in cases {
        let plain = sample(n);
        let archive = encrypt(&plain, &KEY, &spec);
        for jobs in [1, 4] {
            let dir = Dir::new("rt");
            let (done, got) = run(&dir, &archive, &AeaOptions { jobs, ..opts(AeaKey::Raw(KEY)) }).unwrap_or_else(|e| panic!("{what}, {jobs} jobs: {e}"));
            assert!(got == plain, "{what}, {jobs} jobs: the plain file differs");
            assert_eq!((done.bytes, done.sha256.as_str(), done.key_from, done.resumed_from), (n as u64, hex_sha(&plain).as_str(), "given", 0), "{what}");
            assert_eq!(done.encrypted, archive.len() as u64);
            assert_eq!(leftovers(&dir), vec!["out.img".to_string()], "{what}: only the output is left");
        }
    }
}

#[test]
fn a_wrong_key_is_refused_at_the_root_header() {
    let archive = encrypt(&sample(20000), &KEY, &Spec::default());
    let dir = Dir::new("key");
    let e = run(&dir, &archive, &opts(AeaKey::Raw([0x12; 32]))).unwrap_err().to_string();
    assert!(e.contains("root header does not authenticate"), "{e}");
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

#[test]
fn every_changed_byte_is_refused() {
    let plain = sample(4096 * 9 + 100);
    let spec = Spec { compression: b'e', ..Spec::default() };
    let archive = encrypt(&plain, &KEY, &spec);
    let auth_len = le32(&archive[8..]) as usize;
    let first = HEADER + auth_len + 32 + ROOT_BLOCK;
    let header = 4 * 40 + 32 + 4 * 32;
    let cases = [
        ("the scrypt byte of the profile word", 7, "root header does not authenticate"),
        ("the auth data", HEADER + 5, "root header does not authenticate"),
        ("the main salt", HEADER + auth_len + 3, "root header does not authenticate"),
        ("the root header", HEADER + auth_len + 32 + 40, "root header does not authenticate"),
        ("the first cluster's MAC", first - 1, "root header does not authenticate"),
        ("a segment header", first + 3, "cluster 0's header"),
        ("a segment MAC", first + header - 5, "cluster 0's header"),
        ("a segment's bytes", first + header + 10, "segment 0 of cluster 0"),
        ("the padding", archive.len() - 1, "padding"),
    ];
    for (what, at, says) in cases {
        let mut bad = archive.clone();
        bad[at] ^= 0x01;
        let dir = Dir::new("tamper");
        let e = run(&dir, &bad, &opts(AeaKey::Raw(KEY))).expect_err(what).to_string();
        assert!(e.contains(says), "{what}: {e}");
        assert!(leftovers(&dir).is_empty(), "{what}: {:?}", leftovers(&dir));
    }
    // a later cluster, found by walking the first ones' sizes
    let mut bad = archive.clone();
    let n = bad.len();
    bad[n - spec.padding - 3] ^= 0x80;
    let dir = Dir::new("tamper-last");
    let e = run(&dir, &bad, &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
    assert!(e.contains("segment") && e.contains("of cluster 2"), "{e}");
    // cut short, and one byte more
    for (what, bytes) in [("cut", archive[..archive.len() - 1].to_vec()), ("longer", [archive.clone(), vec![0]].concat())] {
        let dir = Dir::new("size");
        let e = run(&dir, &bytes, &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
        assert!(e.contains("its root header says"), "{what}: {e}");
    }
    let dir = Dir::new("magic");
    let e = run(&dir, b"AEA2xxxxxxxxxxxxxxxx", &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
    assert!(e.contains("no AEA1 magic"), "{e}");
}

#[test]
fn other_profiles_are_refused() {
    let mut archive = encrypt(&sample(100), &KEY, &Spec::default());
    archive[4] = 3;
    let dir = Dir::new("profile");
    let e = run(&dir, &archive, &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
    assert!(e.contains("AEA profile 3"), "{e}");
}

#[test]
fn the_plain_sha256_asked_for_is_checked() {
    let plain = sample(30000);
    let archive = encrypt(&plain, &KEY, &Spec::default());
    let dir = Dir::new("sha");
    let e = run(&dir, &archive, &AeaOptions { plain_sha256: Some("0".repeat(64)), ..opts(AeaKey::Raw(KEY)) }).unwrap_err().to_string();
    assert!(e.contains("was expected: nothing was kept"), "{e}");
    assert!(leftovers(&dir).is_empty());
    // right: done; and again with the output there: nothing is done, no key is needed
    let want = Some(hex_sha(&plain));
    let (done, _) = run(&dir, &archive, &AeaOptions { plain_sha256: want.clone(), ..opts(AeaKey::Raw(KEY)) }).unwrap();
    assert!(!done.already);
    let again = aea_decrypt(&dir.path("in.aea"), &dir.path("out.img"), &AeaOptions { plain_sha256: want, ..opts(AeaKey::Raw([0; 32])) }).unwrap();
    assert!(again.already && again.key_from == "none");
    let e = aea_decrypt(&dir.path("in.aea"), &dir.path("out.img"), &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
    assert!(e.contains("is there already"), "{e}");
}

#[test]
fn a_resume_goes_on_from_the_part() {
    let plain = sample(4096 * 10 + 77);
    let archive = encrypt(&plain, &KEY, &Spec { compression: b'e', ..Spec::default() });
    let dir = Dir::new("resume");
    let input = dir.write("in.aea", &archive);
    let out = dir.path("out.img");
    let f = File::open(&input).unwrap();
    let p = read_prologue(&f, archive.len() as u64, &input).unwrap();
    let root = open_root(&Archive::new(&KEY, &p), &p).unwrap();
    // what an earlier run left: 6 segments of the plain file and the state that says so
    let done = 6 * 4096u64;
    std::fs::write(format!("{out}.part"), &plain[..done as usize + 1000]).unwrap();
    std::fs::write(format!("{out}.part.state"), state_line(&p, archive.len() as u64, &root, done)).unwrap();
    let o = AeaOptions { resume: true, plain_sha256: Some(hex_sha(&plain)), ..opts(AeaKey::Raw(KEY)) };
    let d = aea_decrypt(&input, &out, &o).unwrap();
    assert_eq!(d.resumed_from, done);
    assert!(std::fs::read(&out).unwrap() == plain);
    assert_eq!(leftovers(&dir), vec!["out.img".to_string()]);
    // a state of another archive: started over
    std::fs::remove_file(&out).unwrap();
    std::fs::write(format!("{out}.part"), vec![0u8; 8192]).unwrap();
    std::fs::write(format!("{out}.part.state"), state_line(&p, archive.len() as u64 + 1, &root, 8192)).unwrap();
    let d = aea_decrypt(&input, &out, &o).unwrap();
    assert_eq!(d.resumed_from, 0);
    assert!(std::fs::read(&out).unwrap() == plain);
    // a part whose bytes were changed since: the SHA-256 asked for catches it, and nothing is kept
    std::fs::remove_file(&out).unwrap();
    let mut changed = plain[..done as usize].to_vec();
    changed[5] ^= 1;
    std::fs::write(format!("{out}.part"), &changed).unwrap();
    std::fs::write(format!("{out}.part.state"), state_line(&p, archive.len() as u64, &root, done)).unwrap();
    let e = aea_decrypt(&input, &out, &o).unwrap_err().to_string();
    assert!(e.contains("was expected"), "{e}");
    assert_eq!(leftovers(&dir), Vec::<String>::new());
}

#[test]
fn a_stop_removes_the_part_unless_resume() {
    // A failure in the middle (a changed segment of the last cluster) after earlier segments were written: without
    // resume nothing is left; with it the part and its state stay only when a state was written (not for a small
    // file), so the part is removed either way here, and the state never names bytes that were not written.
    let plain = sample(4096 * 12);
    let mut archive = encrypt(&plain, &KEY, &Spec::default());
    let n = archive.len();
    archive[n - 700 - 5] ^= 1;
    let dir = Dir::new("stop");
    let e = run(&dir, &archive, &opts(AeaKey::Raw(KEY))).unwrap_err().to_string();
    assert!(e.contains("does not authenticate"), "{e}");
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

// ---------------------------------------------------------------- the key service


#[test]
fn the_wrapped_key_opens_with_the_services_key_only() {
    let (pem, response) = service_key(7, &KEY);
    assert_eq!(unwrap_key(&pem, &response).unwrap(), KEY);
    let (other, _) = service_key(8, &KEY);
    assert!(unwrap_key(&other, &response).unwrap_err().contains("does not open"));
    assert!(unwrap_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n", &response).unwrap_err().contains("not a P-256 private key"));
    assert!(unwrap_key(&pem, b"{}").unwrap_err().contains("no enc-request"));
}

#[test]
fn a_pem_file_gives_the_key() {
    let (pem, response) = service_key(9, &KEY);
    let plain = sample(9000);
    let spec = Spec { fields: vec![("com.apple.wkms.fcs-response".into(), response)], ..Spec::default() };
    let archive = encrypt(&plain, &KEY, &spec);
    let dir = Dir::new("pem");
    let pem_path = dir.write("key.pem", pem.as_bytes());
    let (done, got) = run(&dir, &archive, &opts(AeaKey::Pem(pem_path.clone().into()))).unwrap();
    assert!(got == plain && done.key_from == "pem");
    // a key service field with another archive's key: the archive key does not open the root header
    let (_, other_response) = service_key(10, &KEY);
    let (other_pem, _) = service_key(10, &KEY);
    let spec = Spec { fields: vec![("com.apple.wkms.fcs-response".into(), other_response)], ..Spec::default() };
    let archive = encrypt(&plain, &KEY, &spec);
    std::fs::write(&pem_path, other_pem).unwrap();
    let dir2 = Dir::new("pem2");
    let pem2 = dir2.write("key.pem", service_key(11, &KEY).0.as_bytes());
    let e = run(&dir2, &archive, &opts(AeaKey::Pem(pem2.into()))).unwrap_err().to_string();
    assert!(e.contains("does not open"), "{e}");
}

#[test]
fn the_service_key_comes_over_http_and_is_cached() {
    let (pem, response) = service_key(12, &KEY);
    let server = Dir::new("server");
    std::fs::write(server.0.join("Tk3y_id="), &pem).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = server.0.clone();
    std::thread::spawn(move || crate::rangeserve::serve_on(listener, root, Default::default()));
    let url = format!("http://127.0.0.1:{port}/Tk3y_id=");
    let plain = sample(12000);
    let fields = vec![("com.apple.wkms.fcs-response".into(), response), ("com.apple.wkms.fcs-key-url".into(), url.into_bytes())];
    let archive = encrypt(&plain, &KEY, &Spec { fields, ..Spec::default() });
    let dir = Dir::new("svc");
    let cache = dir.0.join("keys");
    let (done, got) = run(&dir, &archive, &opts(AeaKey::Service { cache: Some(cache.clone()) })).unwrap();
    assert!(got == plain && done.key_from == "service");
    let kept = cache.join("Tk3y_id=.pem");
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), pem);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777, 0o600);
    // the second archive: from the cache, with no request
    std::fs::remove_file(server.0.join("Tk3y_id=")).unwrap();
    let (done, _) = run(&dir, &archive, &opts(AeaKey::Service { cache: Some(cache.clone()) })).unwrap();
    assert_eq!(done.key_from, "cache");
    // a cache that holds another key is fetched again (here: the server has none now, so it fails)
    std::fs::write(&kept, service_key(13, &KEY).0).unwrap();
    let e = run(&dir, &archive, &opts(AeaKey::Service { cache: Some(cache) })).unwrap_err().to_string();
    assert!(e.contains("the key service"), "{e}");
}

#[test]
fn an_archive_without_key_service_fields_says_so() {
    let archive = encrypt(&sample(100), &KEY, &Spec::default());
    let dir = Dir::new("nofields");
    let e = run(&dir, &archive, &opts(AeaKey::Service { cache: None })).unwrap_err().to_string();
    assert!(e.contains("no com.apple.wkms.fcs-key-url field"), "{e}");
    // an https URL only
    let fields = vec![("com.apple.wkms.fcs-response".into(), b"{}".to_vec()), ("com.apple.wkms.fcs-key-url".into(), b"http://example.com/k".to_vec())];
    let archive = encrypt(&sample(100), &KEY, &Spec { fields, ..Spec::default() });
    let e = run(&dir, &archive, &opts(AeaKey::Service { cache: None })).unwrap_err().to_string();
    assert!(e.contains("not https"), "{e}");
}

#[test]
fn keys_on_the_command_line() {
    use base64::Engine;
    assert_eq!(parse_key(&format!("base64:{}", base64::engine::general_purpose::STANDARD.encode(KEY))).unwrap(), KEY);
    assert_eq!(parse_key(&format!("hex:{}", "11".repeat(32))).unwrap(), KEY);
    assert!(parse_key("hex:11").is_err());
    assert_eq!(cache_name("https://wkms-public.apple.com/fcs-keys/QCq/../x+y="), "x_y=.pem");
}

#[test]
fn a_bad_first_segment_of_many_ends_the_work() {
    // The writer ends at the first segment's error while the reader still has many segments to hand out: the
    // reader must not wait for workers that are gone (a deadlock until 2026-11-21: the jobs' channel was full).
    let plain = sample(4096 * 4 * 40);
    let archive = encrypt(&plain, &KEY, &Spec::default());
    let first = HEADER + le32(&archive[8..]) as usize + 32 + ROOT_BLOCK + 4 * 40 + 32 + 4 * 32;
    for jobs in [1, 2, 8] {
        let mut bad = archive.clone();
        bad[first + 1] ^= 1;
        let dir = Dir::new("many");
        let e = run(&dir, &bad, &AeaOptions { jobs, ..opts(AeaKey::Raw(KEY)) }).unwrap_err().to_string();
        assert!(e.contains("segment 0 of cluster 0"), "{jobs} jobs: {e}");
        assert!(leftovers(&dir).is_empty());
    }
}
