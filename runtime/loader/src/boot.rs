// SPDX-License-Identifier: BSD-2-Clause-Patent
//! boot_args, the boot sidecars and the placement of everything in the kernel's memory window (boot.c).
//!
//! Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957: BootArgs.h, PackedRamdisk.h, and
//! Boot/ValidTrustCaches of DarwinBoot.c), Copyright (c) 2026 DarwinBoot contributors, BSD-2-Clause-Patent. The
//! 608-byte command line variant is the layout iOSPlus10's shim loader (external/slop/shim/loader.c) writes for
//! Darwin 22 and older. Changes: no UEFI; the window is a caller-supplied buffer; the device tree is edited as a tree
//! ([`crate::adt`]) instead of patched in its flat form; virt_base is a parameter, so a kernel can be handed the
//! address at which the window is already mapped (xnuhost: XNU as a process, no MMU switch).

use crate::adt::Node;
use crate::macho::Kernel;
use crate::Error;

const SIZE_4K: u64 = 0x1000;
const SIZE_16K: u64 = 0x4000;
const SIZE_2M: u64 = 0x20_0000;

const fn align(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

/// The command line of pexpert/pexpert/arm64/boot.h, Revision 2 Version 2: Darwin 23 and later (xnu-10063 ..
/// xnu-12377).
pub const BOOT_LINE_LENGTH: usize = 1024;
/// Darwin 22 and older.
pub const BOOT_LINE_LENGTH_LEGACY: usize = 608;

/// The fields of boot_args (all little-endian) at their offsets: `revision`, `version` u16 at 0 and 2, then u64s:
/// virt_base 0x8, phys_base 0x10, mem_size 0x18, top_of_kernel_data 0x20, video[6] 0x28 (baseAddr, display,
/// rowBytes, width, height, depth), machine_type u32 0x58, device_tree 0x60 (a kernel virtual address),
/// device_tree_length u32 0x68, command_line 0x6c; boot_flags and mem_size_actual after the command line, 8-byte
/// aligned.
pub mod args {
    pub const VIRT_BASE: usize = 0x08;
    pub const PHYS_BASE: usize = 0x10;
    pub const MEM_SIZE: usize = 0x18;
    pub const TOP_OF_KERNEL_DATA: usize = 0x20;
    pub const VIDEO: usize = 0x28;
    pub const MACHINE_TYPE: usize = 0x58;
    pub const DEVICE_TREE: usize = 0x60;
    pub const DEVICE_TREE_LENGTH: usize = 0x68;
    pub const COMMAND_LINE: usize = 0x6c;

    /// boot_flags of the layout with a `line`-byte command line (0x470 for 1024 bytes, 720 for 608);
    /// mem_size_actual follows it.
    pub const fn boot_flags(line: usize) -> usize {
        (COMMAND_LINE + line + 7) & !7
    }

    /// sizeof the boot_args of that layout (0x480, 736).
    pub const fn size(line: usize) -> usize {
        boot_flags(line) + 16
    }
}

const _: () = assert!(args::boot_flags(BOOT_LINE_LENGTH) + 8 == 0x478, "memory ABI");
const _: () = assert!(args::size(BOOT_LINE_LENGTH) == 0x480, "boot_args ABI");
const _: () = assert!(args::size(BOOT_LINE_LENGTH_LEGACY) == 736, "legacy boot_args ABI");

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn put_u64(out: &mut [u8], at: usize, v: u64) {
    out[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// True for a well-formed trust cache sidecar: u32 count, u32 offsets[count], then version 2 modules (version,
/// uuid[16], entry count, sorted 24-byte entries).
pub fn trustcaches_valid(data: &[u8]) -> bool {
    let size = data.len();
    if size < 8 {
        return false;
    }
    let count = u32_at(data, 0) as usize;
    if count == 0 || count > 128 || count > (size - 4) / 4 {
        return false;
    }
    (0..count).all(|i| {
        let start = u32_at(data, 4 + i * 4) as usize;
        let end = if i + 1 < count { u32_at(data, 8 + i * 4) as usize } else { size };
        if start < 4 + count * 4 || start & 3 != 0 || end > size || end < start || end - start < 24 {
            return false;
        }
        if u32_at(data, start) != 2 {
            return false;
        }
        let entries = u32_at(data, start + 20) as usize;
        if entries > (end - start - 24) / 24 || end - start != 24 + entries * 24 {
            return false;
        }
        let hashes: Vec<&[u8]> = data[start + 24..end].chunks_exact(24).collect();
        hashes.iter().all(|h| (1..=4).contains(&h[20])) && hashes.windows(2).all(|w| w[0][..20] < w[1][..20])
    })
}

const RAMDISK_MAX_LOGICAL: u64 = 1 << 40;

/// What a RAM disk expands to: its logical size, and whether it is a "DBRDISK1" packed one (only its nonzero pages
/// kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamdiskLayout {
    pub expanded: u64,
    pub packed: bool,
}

/// The layout of a RAM disk (a plain one: its own size); None for a malformed packed one.
pub fn ramdisk_layout(data: &[u8]) -> Option<RamdiskLayout> {
    let size = data.len() as u64;
    let plain = RamdiskLayout { expanded: size, packed: false };
    if size < 8 || &data[..8] != b"DBRDISK1" {
        return Some(plain);
    }
    if size < 32 || u32_at(data, 8) != 1 || u64::from(u32_at(data, 12)) != SIZE_4K {
        return None;
    }
    let (logical, count) = (u64_at(data, 16), u64_at(data, 24));
    if logical == 0
        || logical > RAMDISK_MAX_LOGICAL
        || logical & (SIZE_4K - 1) != 0
        || count > 65536
        || count > (size - 32) / 16
    {
        return None;
    }
    let (mut cursor, mut end) = (32 + count * 16, 0);
    for i in 0..count as usize {
        let (offset, length) = (u64_at(data, 32 + i * 16), u64_at(data, 40 + i * 16));
        if length == 0
            || (offset | length) & (SIZE_4K - 1) != 0
            || offset < end
            || offset > logical
            || length > logical - offset
            || length > size - cursor
        {
            return None;
        }
        end = offset + length;
        cursor += length;
    }
    (cursor == size).then_some(RamdiskLayout { expanded: logical, packed: true })
}

/// Expands a RAM disk into `destination`, which must be zeroed and hold its expanded size; the input must have passed
/// [`ramdisk_layout`].
pub fn ramdisk_copy(destination: &mut [u8], data: &[u8], packed: bool) {
    if !packed {
        destination[..data.len()].copy_from_slice(data);
        return;
    }
    let count = u64_at(data, 24) as usize;
    let mut cursor = 32 + count * 16;
    for i in 0..count {
        let (offset, length) = (u64_at(data, 32 + i * 16) as usize, u64_at(data, 40 + i * 16) as usize);
        destination[offset..offset + length].copy_from_slice(&data[cursor..cursor + length]);
        cursor += length;
    }
}

/// What to place in the kernel's window.
#[derive(Debug, Clone, Copy)]
pub struct BootRequest<'a> {
    /// The kernel image (whole file) and its parsed form.
    pub image: &'a [u8],
    pub kernel: &'a Kernel,
    /// Where the kernel sees the window: physical address phys_base and virtual address virt_base. A firmware loader
    /// passes virt_base = kernel.base (the kernel maps itself there). xnuhost passes the host address of the window,
    /// which must differ from kernel.base by a multiple of 2 MiB (the slide the kernel finds when it rebases itself).
    pub phys_base: u64,
    pub virt_base: u64,
    pub command_line: &'a str,
    pub video: [u64; 6],
    pub boot_flags: u64,
    /// boot_args memSizeActual: the device's whole DRAM, which XNU keeps as mem_actual and max_mem_actual
    /// (hw.memsize_physical on iOS; hw.memsize is max_mem, the managed RAM: arm_vm_init.c, kern_mib.c). 0 or less
    /// than the window: the window. xnuhost's ram-view= (docs/booter.md 2.2, "The memory view").
    pub mem_size_actual: u64,
    /// 608-byte command line
    pub legacy_boot_args: bool,
    /// Optional sidecars. A present sidecar needs its /chosen/memory-map property in the tree (RAMDisk or
    /// PackedRAMDisk, TrustCache, GpuAddon, RootPolicies), 16 zero bytes; a tree that names one whose sidecar is
    /// absent is refused, as DarwinBoot refuses it.
    pub ramdisk: Option<&'a [u8]>,
    pub trustcache: Option<&'a [u8]>,
    pub gpu_addon: Option<&'a [u8]>,
    pub root_policies: Option<&'a [u8]>,
}

/// Where everything went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootLayout {
    /// virt_base - kernel.base
    pub slide: u64,
    pub entry_va: u64,
    pub entry_pa: u64,
    pub args_pa: u64,
    pub args_va: u64,
    pub tree_pa: u64,
    pub tree_size: u64,
    pub ramdisk_pa: u64,
    pub ramdisk_size: u64,
    pub trustcache_pa: u64,
    pub gpu_addon_pa: u64,
    pub root_policies_pa: u64,
    /// bytes of the window in use: top_of_kernel_data - phys_base
    pub used: u64,
}

/// Whether the 16-byte (address, size) property of /chosen/memory-map for a sidecar is there to fill: it must be
/// present and zero when the sidecar is, and absent when it is not.
fn claim(map: Option<&Node>, name: &str, present: bool) -> Result<bool, Error> {
    let p = map.and_then(|m| m.get(name));
    if !present {
        return match p {
            Some(_) => Err(Error("the device tree names a memory-map sidecar that was not supplied")),
            None => Ok(false),
        };
    }
    match p {
        Some(p) if p.value == [0; 16] => Ok(true),
        _ => Err(Error("a supplied sidecar has no empty memory-map property in the device tree")),
    }
}

fn len(blob: Option<&[u8]>) -> u64 {
    blob.map_or(0, |b| b.len() as u64)
}

impl BootRequest<'_> {
    /// Copies the kernel's segments, the device tree and the sidecars into the window `ram` (zeroed, writable; the
    /// kernel's window is all of it) and writes boot_args. The tree's memory map is filled in before it is flattened.
    /// On failure the window's content is unspecified.
    pub fn place(&self, ram: &mut [u8], tree: &mut Node) -> Result<BootLayout, Error> {
        let k = self.kernel;
        let window = ram.len() as u64;
        if window & (SIZE_2M - 1) != 0 || window < 16 * SIZE_2M {
            return Err(Error("the window must be a multiple of 2 MiB, at least 32 MiB"));
        }
        if self.virt_base.wrapping_sub(k.base) & (SIZE_2M - 1) != 0 {
            return Err(Error("the kernel slide must be a multiple of 2 MiB"));
        }
        if (self.phys_base ^ k.base) & (SIZE_2M - 1) != 0 {
            return Err(Error("the physical base must keep the kernel's offset inside its 2 MiB block"));
        }
        if k.end - k.base > window - SIZE_2M {
            return Err(Error("the kernel does not fit the window"));
        }

        let line = if self.legacy_boot_args { BOOT_LINE_LENGTH_LEGACY } else { BOOT_LINE_LENGTH };
        let command = self.command_line.split('\0').next().unwrap_or("").as_bytes();
        if command.len() >= line {
            return Err(Error("boot arguments too long"));
        }
        if !command.is_ascii() {
            return Err(Error("boot arguments are not ASCII"));
        }

        let ramdisk = match self.ramdisk {
            Some(data) => Some(ramdisk_layout(data).ok_or(Error("ramdisk layout rejected"))?),
            None => None,
        };
        let (expanded, packed) = ramdisk.map_or((0, false), |l| (l.expanded, l.packed));

        let map = tree.find("chosen/memory-map");
        let keep_packed = map.is_some_and(|m| m.get("PackedRAMDisk").is_some());
        if keep_packed && map.is_some_and(|m| m.get("RAMDisk").is_some()) {
            return Err(Error("the device tree names both RAMDisk and PackedRAMDisk"));
        }
        if keep_packed && !packed {
            return Err(Error("the device tree asks for a packed ramdisk and the ramdisk is not packed"));
        }
        let ramdisk_name = if keep_packed { "PackedRAMDisk" } else { "RAMDisk" };
        let ramdisk_slot = claim(map, ramdisk_name, self.ramdisk.is_some())?;
        // RAMDisk1 mirrors RAMDisk for iOS 16 userlands (port-a56, docs/a56.md); the tree only names it then.
        let ramdisk1_present = self.ramdisk.is_some() && map.is_some_and(|m| m.get("RAMDisk1").is_some());
        let ramdisk1_slot = claim(map, "RAMDisk1", ramdisk1_present)?;
        let trust_slot = claim(map, "TrustCache", self.trustcache.is_some())?;
        let addon_slot = claim(map, "GpuAddon", self.gpu_addon.is_some())?;
        let policy_slot = claim(map, "RootPolicies", self.root_policies.is_some())?;
        if self.trustcache.is_some_and(|t| !trustcaches_valid(t)) {
            return Err(Error("trust cache sidecar rejected"));
        }
        if let Some(a) = self.gpu_addon {
            if a.len() < 24 || &a[..8] != b"NGPUADD2" || u64_at(a, 16) != a.len() as u64 {
                return Err(Error("GPU addon sidecar rejected"));
            }
        }
        if let Some(p) = self.root_policies {
            if p.len() < 32
                || &p[..8] != b"VROOTB01"
                || u64_at(p, 8) != 1
                || u64_at(p, 16) == 0
                || u64_at(p, 16) > 16
                || u64_at(p, 24) != p.len() as u64
            {
                return Err(Error("root policy sidecar rejected"));
            }
        }

        // Offsets inside the window, in DarwinBoot's order: kernel, boot_args, tree, ramdisk, trust caches, GPU
        // addon, root policies.
        let tree_size = tree.flat_size() as u64;
        let args = align(k.end - k.base, SIZE_16K);
        let tree_at = args + SIZE_4K;
        let ramdisk_at = align(tree_at + tree_size, SIZE_4K);
        let ramdisk_bytes = align(if keep_packed { len(self.ramdisk) } else { expanded }, SIZE_4K);
        if ramdisk_bytes > window {
            return Err(Error("the ramdisk does not fit the window"));
        }
        let (trust_size, addon_size, policy_size) =
            (len(self.trustcache), len(self.gpu_addon), len(self.root_policies));
        let trust = ramdisk_at + ramdisk_bytes;
        let addon = align(trust + trust_size, SIZE_16K);
        let policy = align(addon + addon_size, SIZE_16K);
        let used = align(policy + policy_size, SIZE_16K);
        if trust_size > window || addon_size > window || policy_size > window || used > window - SIZE_2M {
            return Err(Error("the kernel and its sidecars do not fit the window"));
        }

        let (pa, va) = (self.phys_base, self.virt_base);
        let ramdisk_size = if keep_packed { len(self.ramdisk) } else { ramdisk_bytes };
        if let Some(map) = tree.find_mut("chosen/memory-map") {
            let mut fill = |slot: bool, name: &str, address: u64, size: u64| {
                if slot {
                    let mut value = [0u8; 16];
                    value[..8].copy_from_slice(&address.to_le_bytes());
                    value[8..].copy_from_slice(&size.to_le_bytes());
                    map.set(name, value).expect("a memory-map name fits");
                }
            };
            fill(ramdisk_slot, ramdisk_name, pa + ramdisk_at, ramdisk_size);
            fill(ramdisk1_slot, "RAMDisk1", pa + ramdisk_at, ramdisk_size);
            fill(trust_slot, "TrustCache", pa + trust, trust_size);
            fill(addon_slot, "GpuAddon", pa + addon, addon_size);
            fill(policy_slot, "RootPolicies", pa + policy, policy_size);
        }

        ram[..used as usize].fill(0);
        for s in &k.segments {
            let from = self
                .image
                .get(s.fileoff as usize..(s.fileoff + s.filesize) as usize)
                .ok_or(Error("the kernel image does not hold its segments"))?;
            let at = (s.vmaddr - k.base) as usize;
            ram[at..at + from.len()].copy_from_slice(from);
        }
        tree.flatten_into(&mut ram[tree_at as usize..]);
        let copy = |ram: &mut [u8], at: u64, blob: Option<&[u8]>| {
            if let Some(b) = blob {
                ram[at as usize..at as usize + b.len()].copy_from_slice(b);
            }
        };
        if let Some(data) = self.ramdisk {
            let at = ramdisk_at as usize;
            if keep_packed {
                copy(ram, ramdisk_at, Some(data));
            } else {
                ramdisk_copy(&mut ram[at..at + expanded as usize], data, packed);
            }
        }
        copy(ram, trust, self.trustcache);
        copy(ram, addon, self.gpu_addon);
        copy(ram, policy, self.root_policies);

        let a = &mut ram[args as usize..args as usize + args::size(line)];
        a.fill(0);
        a[0..2].copy_from_slice(&2u16.to_le_bytes()); // revision
        a[2..4].copy_from_slice(&2u16.to_le_bytes()); // version
        put_u64(a, args::VIRT_BASE, va);
        put_u64(a, args::PHYS_BASE, pa);
        put_u64(a, args::MEM_SIZE, window);
        put_u64(a, args::TOP_OF_KERNEL_DATA, pa + used);
        for (i, v) in self.video.iter().enumerate() {
            put_u64(a, args::VIDEO + i * 8, *v);
        }
        put_u64(a, args::DEVICE_TREE, va + tree_at);
        a[args::DEVICE_TREE_LENGTH..args::DEVICE_TREE_LENGTH + 4].copy_from_slice(&(tree_size as u32).to_le_bytes());
        a[args::COMMAND_LINE..args::COMMAND_LINE + command.len()].copy_from_slice(command);
        put_u64(a, args::boot_flags(line), self.boot_flags);
        put_u64(a, args::boot_flags(line) + 8, self.mem_size_actual.max(window));

        let present = |blob: Option<&[u8]>, at: u64| if blob.is_some() { pa + at } else { 0 };
        Ok(BootLayout {
            slide: va.wrapping_sub(k.base),
            entry_va: va + (k.entry - k.base),
            entry_pa: pa + (k.entry - k.base),
            args_pa: pa + args,
            args_va: va + args,
            tree_pa: pa + tree_at,
            tree_size,
            ramdisk_pa: present(self.ramdisk, ramdisk_at),
            ramdisk_size: if self.ramdisk.is_some() { ramdisk_size } else { 0 },
            trustcache_pa: present(self.trustcache, trust),
            gpu_addon_pa: present(self.gpu_addon, addon),
            root_policies_pa: present(self.root_policies, policy),
            used,
        })
    }
}
