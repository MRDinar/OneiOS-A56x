// SPDX-License-Identifier: MIT
//! mkkernel ELF KERNEL.KC DISK: wraps the test kernel (a static AArch64 ELF, runtime/xnuhost/testkernel) as an
//! MH_EXECUTE Mach-O the loader accepts, and writes the test disk. Nothing of Apple's is involved.
use std::process::ExitCode;
use xh_core::bytes::le;
use xh_core::sys::error_text;
use xnuloader::fixture;

const PT_LOAD: u32 = 1;
const EM_AARCH64: u16 = 183;

/// What mkkernel reads of an ELF64 program header.
struct Load {
    vaddr: u64,
    offset: u64,
    filesz: u64,
    memsz: u64,
}

/// The entry and the PT_LOAD segments of an AArch64 ELF64; None if it is not one.
fn read_elf(elf: &[u8]) -> Option<(u64, Vec<Load>)> {
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" || le::try_u16(elf, 18)? != EM_AARCH64 {
        return None;
    }
    let (entry, phoff, phnum) = (le::try_u64(elf, 24)?, le::try_u64(elf, 32)? as usize, le::try_u16(elf, 56)? as usize);
    let mut loads = Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * 56;
        if le::try_u32(elf, ph)? != PT_LOAD {
            continue;
        }
        loads.push(Load {
            offset: le::try_u64(elf, ph + 8)?,
            vaddr: le::try_u64(elf, ph + 16)?,
            filesz: le::try_u64(elf, ph + 32)?,
            memsz: le::try_u64(elf, ph + 40)?,
        });
    }
    Some((entry, loads))
}

/// The disk: 2048 sectors; sector n starts with "ONEIOSTD", its number, then (n ^ 0x5a) bytes.
fn test_disk() -> Vec<u8> {
    let mut disk = Vec::with_capacity(2048 * 512);
    for n in 0..2048u32 {
        let mut sector = [(n ^ 0x5a) as u8; 512];
        sector[..8].copy_from_slice(b"ONEIOSTD");
        sector[8..12].copy_from_slice(&n.to_le_bytes());
        disk.extend_from_slice(&sector);
    }
    disk
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, elf_path, kernel_path, disk_path] = &args[..] else {
        eprintln!("usage: mkkernel ELF KERNEL DISK");
        return ExitCode::from(2);
    };
    let elf = match std::fs::read(elf_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{elf_path}: {}", error_text(&e));
            return ExitCode::from(1);
        }
    };
    let Some((entry, loads)) = read_elf(&elf) else {
        eprintln!("not an AArch64 ELF");
        return ExitCode::from(1);
    };
    // _start is the first instruction of the text, one page (the Mach-O header's) above the base.
    let base = entry.wrapping_sub(0x1000);
    let loads: Vec<&Load> = loads.iter().filter(|l| l.memsz != 0).collect();
    let file_end = loads.iter().filter(|l| l.filesz != 0).map(|l| l.vaddr + l.filesz).max().unwrap_or(0);
    let mem_end = loads.iter().map(|l| l.vaddr + l.memsz).max().unwrap_or(0);
    let text_size = (file_end.wrapping_sub(base) + 0x3fff) & !0x3fff;
    let bss_size = if mem_end > base + text_size { (mem_end - base - text_size + 0x3fff) & !0x3fff } else { 0x4000 };
    let mut out = vec![0u8; text_size as usize];
    for l in loads.iter().filter(|l| l.filesz != 0) {
        // A segment that starts below the text carries only the ELF's own headers there.
        let skip = (base + 0x1000).saturating_sub(l.vaddr);
        if skip < l.filesz {
            let (from, n) = ((l.offset + skip) as usize, (l.filesz - skip) as usize);
            let at = (l.vaddr + skip - base) as usize;
            let Some(bytes) = elf.get(from..from + n) else {
                eprintln!("not an AArch64 ELF");
                return ExitCode::from(1);
            };
            out[at..at + n].copy_from_slice(bytes);
        }
    }
    let mut c = 32;
    c += fixture::segment(&mut out[c..], "__TEXT", base, text_size, 0, text_size, 7); // code and initialised data together
    c += fixture::segment(&mut out[c..], "__DATA", base + text_size, bss_size, 0, 0, 3);
    c += fixture::thread(&mut out[c..], entry);
    fixture::header(&mut out, 0, 2, 3, c - 32);
    for (path, bytes) in [(kernel_path, &out), (disk_path, &test_disk())] {
        if let Err(e) = std::fs::write(path, bytes) {
            eprintln!("{path}: {}", error_text(&e));
            return ExitCode::from(1);
        }
    }
    println!("mkkernel: base {base:#x}, text {text_size:#x}, bss {bss_size:#x}, entry {entry:#x}");
    ExitCode::SUCCESS
}
