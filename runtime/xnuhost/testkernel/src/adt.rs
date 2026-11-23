// SPDX-License-Identifier: MIT
//! The device tree, as XNU reads it: a node is { u32 nprops, u32 nchildren, props..., children... }, a property
//! { char name[32], u32 length (bit 31: placeholder), value padded to 4 bytes }.

fn u32_at(p: *const u8) -> u32 {
    // SAFETY: inside the flat tree the loader placed in the window.
    unsafe { (p as *const u32).read_unaligned() }
}

pub(crate) fn u64_at(p: *const u8) -> u64 {
    // SAFETY: a property's value inside the flat tree.
    unsafe { (p as *const u64).read_unaligned() }
}

fn add(p: *const u8, n: usize) -> *const u8 {
    p.wrapping_add(n)
}

/// The NUL-terminated name at `p` is `name`.
fn named(p: *const u8, name: &str) -> bool {
    // SAFETY: inside the tree: a property name field (32 bytes) or a NUL-terminated value.
    let byte = |k: usize| unsafe { *p.add(k) };
    name.bytes().enumerate().all(|(k, b)| byte(k) == b) && byte(name.len()) == 0
}

/// A property's value length.
fn length(p: *const u8) -> u32 {
    u32_at(add(p, 32)) & 0x7fff_ffff
}

/// The next property after the one at `p`: its header and its padded value.
fn next_prop(p: *const u8) -> *const u8 {
    add(p, 36 + ((length(p) as usize + 3) & !3))
}

/// The node's first child: after its properties.
fn first_child(node: *const u8) -> *const u8 {
    (0..u32_at(node)).fold(add(node, 8), |p, _| next_prop(p))
}

/// The end of the node: after its properties and all its children.
fn end(node: *const u8) -> *const u8 {
    (0..u32_at(add(node, 4))).fold(first_child(node), |p, _| end(p))
}

/// A property's value and length.
pub(crate) fn prop(node: *const u8, name: &str) -> Option<(*const u8, u32)> {
    let mut p = add(node, 8);
    for _ in 0..u32_at(node) {
        if named(p, name) {
            return Some((add(p, 36), length(p)));
        }
        p = next_prop(p);
    }
    None
}

/// A direct child by name.
pub(crate) fn child(node: *const u8, name: &str) -> Option<*const u8> {
    let mut p = first_child(node);
    for _ in 0..u32_at(add(node, 4)) {
        if prop(p, "name").is_some_and(|(n, _)| named(n, name)) {
            return Some(p);
        }
        p = end(p);
    }
    None
}
