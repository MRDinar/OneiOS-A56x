//! `oneios-tool debug holes FILE [--map]` (new, 2026-11-21; an inline Python block of
//! tests/phone/sparse-compare.sh before): what the file system holds for a file.
//!
//! One line, `LENGTH ALLOCATED REGIONS DATA` (bytes): the length, the blocks the file system counts for it (after an
//! fsync: APFS and btrfs count a file's blocks when it is written out), and its data regions as SEEK_DATA and
//! SEEK_HOLE report them, how many and their bytes (REGIONS is `-` where the file system does not report them).
//! With --map, then one line per region: `START..END (BYTES)`.

use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;

use anyhow::{Context, Result};

/// (length, allocated bytes, the data regions; None where the file system does not report them)
pub fn holes(path: &str) -> Result<(u64, u64, Option<Vec<(u64, u64)>>)> {
    let f = std::fs::File::open(path).with_context(|| path.to_string())?;
    f.sync_all().with_context(|| format!("fsync {path}"))?;
    let st = f.metadata().with_context(|| path.to_string())?;
    let fd = f.as_raw_fd();
    let (mut regions, mut at) = (Vec::new(), 0u64);
    while at < st.len() {
        // SAFETY: lseek on a descriptor this function owns
        let data = unsafe { libc::lseek(fd, at as libc::off_t, libc::SEEK_DATA) };
        if data < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                break; // only a hole from here to the end
            }
            return Ok((st.len(), st.blocks() * 512, None));
        }
        let end = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        if end < 0 {
            return Ok((st.len(), st.blocks() * 512, None));
        }
        regions.push((data as u64, end as u64));
        at = end as u64;
    }
    Ok((st.len(), st.blocks() * 512, Some(regions)))
}

pub fn run(path: &str, map: bool) -> Result<i32> {
    let (len, allocated, regions) = holes(path)?;
    match &regions {
        Some(r) => println!("{len} {allocated} {} {}", r.len(), r.iter().map(|(a, b)| b - a).sum::<u64>()),
        None => println!("{len} {allocated} - 0"),
    }
    if map {
        for (a, b) in regions.unwrap_or_default() {
            println!("          {a:#x}..{b:#x} ({})", b - a);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_with_a_hole_in_the_middle() {
        use std::io::{Seek, SeekFrom, Write};
        let path = std::env::temp_dir().join(format!("oneios-holes-{}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[1u8; 8192]).unwrap();
        f.seek(SeekFrom::Start(8 << 20)).unwrap();
        f.write_all(&[2u8; 8192]).unwrap();
        drop(f);
        let (len, _, regions) = super::holes(path.to_str().unwrap()).unwrap();
        assert_eq!(len, (8 << 20) + 8192);
        if let Some(r) = regions {
            // a file system that keeps holes reports two regions; one that does not, one region of every byte
            let data: u64 = r.iter().map(|(a, b)| b - a).sum();
            assert!((r.len() == 2 && data < 1 << 20) || (r.len() == 1 && data == len), "{r:?}");
        }
        std::fs::remove_file(&path).unwrap();
    }
}
