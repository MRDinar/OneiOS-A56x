// SPDX-License-Identifier: MIT
//! xnuloader-inspect: what the loader sees in a kernel collection or an Apple device tree.
//!   xnuloader-inspect kernel FILE     segments, entry, fileset members
//!   xnuloader-inspect adt FILE        the tree (iOSPlus10's external/slop/tools/adt.py dump)
//!   xnuloader-inspect board NCPU      the tree xnuhost builds for its board, flat, on stdout
use std::io::{self, Write};
use std::process::ExitCode;
use xh_core::parse::atoi;
use xnuloader::machine::{DeviceKind, Machine};
use xnuloader::{Kernel, Node};

fn slurp(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| {
        eprintln!("{path}: {}", xh_core::sys::error_text(&e));
        ExitCode::from(1)
    })
}

fn kernel(path: &str, out: &mut dyn Write) -> Result<(), ExitCode> {
    let data = slurp(path)?;
    let k = Kernel::parse(&data).map_err(|e| {
        eprintln!("{path}: {e}");
        ExitCode::from(1)
    })?;
    let print = |out: &mut dyn Write| -> io::Result<()> {
        writeln!(
            out,
            "{} {} base {:#x} end {:#x} entry {:#x}",
            if k.fileset { "MH_FILESET" } else { "MH_EXECUTE" },
            if k.cpusubtype == 2 { "arm64e" } else { "arm64" },
            k.base,
            k.end,
            k.entry
        )?;
        for s in &k.segments {
            writeln!(
                out,
                "  {:<16} va {:#x} size {:#x} file {:#x}+{:#x} prot {}",
                s.name, s.vmaddr, s.vmsize, s.fileoff, s.filesize, s.prot
            )?;
        }
        for e in &k.entries {
            writeln!(out, "  member {:<48} va {:#x} file {:#x}", e.name, e.vmaddr, e.fileoff)?;
        }
        Ok(())
    };
    print(out).map_err(|_| ExitCode::from(1))
}

fn adt(path: &str, out: &mut dyn Write) -> Result<(), ExitCode> {
    let data = slurp(path)?;
    let root = Node::parse(&data).map_err(|e| {
        eprintln!("{path}: {e}");
        ExitCode::from(1)
    })?;
    root.dump(out, 0).map_err(|_| ExitCode::from(1))
}

fn board(ncpu: &str, out: &mut dyn Write) -> Result<(), ExitCode> {
    use DeviceKind::*;
    let mut m = Machine::default();
    m.board(atoi(ncpu) as u32, &[Block, Block, Block, DataStore, Input, Gpu]);
    m.timebase_hz = 19_200_000;
    m.dram_base = 0x8_0000_0000;
    m.dram_size = 0x1_0000_0000;
    let root = m.tree().map_err(|e| {
        eprintln!("board: {e}");
        ExitCode::from(1)
    })?;
    out.write_all(&root.flatten()).map_err(|_| ExitCode::from(1))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [_, "kernel", file] => kernel(file, &mut out),
        [_, "adt", file] => adt(file, &mut out),
        [_, "board", ncpu] => board(ncpu, &mut out),
        _ => {
            eprintln!("usage: xnuloader-inspect kernel FILE | adt FILE | board NCPU");
            return ExitCode::from(2);
        }
    };
    let flushed = out.flush();
    match result {
        Err(code) => code,
        Ok(()) if flushed.is_err() => ExitCode::from(1),
        Ok(()) => ExitCode::SUCCESS,
    }
}
