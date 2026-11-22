// SPDX-License-Identifier: MIT
//! The Bluetooth HAL's HIDL parcels (android.hardware.bluetooth@1.0/1.1): the client's requests and what its
//! callback object answers. The wire format is [`crate::hal::hidl`]. C: the Bluetooth half of bt/hidl_parcel.{c,h}.

use crate::hal::hidl::{self, HidlParcel, HidlReader};
use std::ffi::CStr;

pub const BT_HCI_1_0: &CStr = c"android.hardware.bluetooth@1.0::IBluetoothHci";
pub const BT_HCI_1_1: &CStr = c"android.hardware.bluetooth@1.1::IBluetoothHci";
pub const BT_CALLBACKS_1_0: &CStr = c"android.hardware.bluetooth@1.0::IBluetoothHciCallbacks";
pub const BT_CALLBACKS_1_1: &CStr = c"android.hardware.bluetooth@1.1::IBluetoothHciCallbacks";
pub const HIDL_BASE: &CStr = c"android.hidl.base@1.0::IBase";
pub const HIDL_MANAGER: &CStr = c"android.hidl.manager@1.0::IServiceManager";

// IBluetoothHci
pub const BT_INITIALIZE: u32 = 1;
pub const BT_SEND_COMMAND: u32 = 2;
pub const BT_SEND_ACL: u32 = 3;
pub const BT_SEND_SCO: u32 = 4;
pub const BT_CLOSE: u32 = 5;
pub const BT_INITIALIZE_1_1: u32 = 6;
pub const BT_SEND_ISO: u32 = 7;
// IBluetoothHciCallbacks
pub const BT_CB_INIT_COMPLETE: u32 = 1;
pub const BT_CB_EVENT: u32 = 2;
pub const BT_CB_ACL: u32 = 3;
pub const BT_CB_SCO: u32 = 4;
pub const BT_CB_ISO: u32 = 5;

/// IServiceManager::get(fqName, instance).
pub fn get_service<'a>(p: &mut HidlParcel<'a>, fq_name: &'a CStr, instance: &'a CStr) {
    p.clear();
    p.put_token(HIDL_MANAGER);
    p.put_string(fq_name);
    p.put_string(instance);
}

/// initialize(callbacks) or initialize_1_1: our callback object.
pub fn initialize(p: &mut HidlParcel<'_>, v1_1: bool, callback_pointer: u64) {
    p.clear();
    p.put_token(if v1_1 { BT_HCI_1_1 } else { BT_HCI_1_0 });
    p.put_binder(callback_pointer, callback_pointer);
}

/// sendHciCommand, sendAclData, sendScoData, sendIsoData (1.1).
pub fn send<'a>(p: &mut HidlParcel<'a>, code: u32, packet: &'a [u8]) {
    p.clear();
    p.put_token(if code == BT_SEND_ISO { BT_HCI_1_1 } else { BT_HCI_1_0 });
    p.put_bytes(packet);
}

/// What a callback carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callback<'a> {
    /// An HCI packet for the guest: its H4 indicator and bytes.
    Packet(u8, &'a [u8]),
    /// initializationComplete: the HAL's status (0 = success).
    InitComplete(i32),
    /// One of IBase's methods, answered in the reply.
    Base,
}

/// What the callback object answers to transaction `code` with request `r`: Ok with the reply parcel made, or the
/// status code to reply with (UNKNOWN_TRANSACTION).
pub fn callback_transact<'a>(
    code: u32,
    r: &mut HidlReader<'a>,
    reply: &mut HidlParcel<'static>,
) -> Result<Callback<'a>, i32> {
    const CHAIN: [&CStr; 3] = [BT_CALLBACKS_1_1, BT_CALLBACKS_1_0, HIDL_BASE];
    reply.clear();
    let token = r.token().ok_or(hidl::UNKNOWN_TRANSACTION)?;
    reply.put_i32(0); // Status::ok()
    match code {
        BT_CB_INIT_COMPLETE => {
            if token != BT_CALLBACKS_1_0 {
                return Err(hidl::UNKNOWN_TRANSACTION);
            }
            let status = r.i32();
            if r.is_bad() {
                return Err(hidl::UNKNOWN_TRANSACTION);
            }
            Ok(Callback::InitComplete(status))
        }
        BT_CB_EVENT | BT_CB_ACL | BT_CB_SCO | BT_CB_ISO => {
            if token != if code == BT_CB_ISO { BT_CALLBACKS_1_1 } else { BT_CALLBACKS_1_0 } {
                return Err(hidl::UNKNOWN_TRANSACTION);
            }
            let packet = r.bytes().ok_or(hidl::UNKNOWN_TRANSACTION)?;
            let h4 = match code {
                BT_CB_EVENT => 4,
                BT_CB_ACL => 2,
                BT_CB_SCO => 3,
                _ => 5,
            };
            Ok(Callback::Packet(h4, packet))
        }
        hidl::INTERFACE_CHAIN => {
            reply.put_string_vec(&CHAIN);
            Ok(Callback::Base)
        }
        hidl::INTERFACE_DESCRIPTOR => {
            reply.put_string(BT_CALLBACKS_1_1);
            Ok(Callback::Base)
        }
        hidl::GET_HASH_CHAIN => {
            // hidl_vec<uint8_t[32]>: empty (nothing compares the hashes of a client's callback).
            reply.put_empty_vec();
            Ok(Callback::Base)
        }
        hidl::GET_DEBUG_INFO => {
            // DebugInfo { int32 pid; uint64 ptr; int32 arch }: pid -1 and ptr 0 are "not told", arch 1 = 64 bit.
            reply.put_struct(&[u64::from(u32::MAX), 0, 1]);
            Ok(Callback::Base)
        }
        hidl::PING | hidl::SET_HAL_INSTRUMENTATION | hidl::NOTIFY_SYSPROPS_CHANGED | hidl::DEBUG => Ok(Callback::Base),
        _ => Err(hidl::UNKNOWN_TRANSACTION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hal::hidl::{BUFFER_HAS_PARENT, HIDL_REF, TYPE_BINDER, TYPE_PTR};

    /// The hidl_ref a buffer object points at: { pointer, size }.
    fn hidl_ref(at: u64) -> (u64, u32) {
        // SAFETY: a scratch buffer of the parcel under test, alive for the call.
        let b = unsafe { std::slice::from_raw_parts(at as *const u8, HIDL_REF) };
        (u64::from_ne_bytes(b[..8].try_into().unwrap()), u32::from_ne_bytes(b[8..12].try_into().unwrap()))
    }
    fn c_text(at: u64) -> &'static CStr {
        // SAFETY: a NUL-terminated constant the parcel points at.
        unsafe { CStr::from_ptr(at as *const libc::c_char) }
    }

    // tests/test_bridges.c test_bt: the parcels of the HAL client.
    #[test]
    fn get_service_parcel() {
        let mut p = HidlParcel::new();
        get_service(&mut p, BT_HCI_1_1, c"default");
        assert!(!p.is_overflow() && p.object_offsets().len() == 4);
        assert_eq!(CStr::from_bytes_until_nul(p.bytes()).unwrap(), HIDL_MANAGER);
        let token = (HIDL_MANAGER.to_bytes().len() + 1 + 3) & !3;
        assert!(
            p.object_offsets()[0] == token as u64
                && p.object_offsets()[1] == token as u64 + 40
                && p.size() == token + 160
        );
        let (o0, o1, o3) = (p.buffer_object(0), p.buffer_object(1), p.buffer_object(3));
        assert!(o0.kind == TYPE_PTR && o0.length == 16 && o0.flags == 0);
        assert!(o1.flags == BUFFER_HAS_PARENT && o1.parent == 0 && o1.parent_offset == 0);
        assert_eq!(o1.length as usize, BT_HCI_1_1.to_bytes().len() + 1);
        assert!(c_text(o1.buffer) == BT_HCI_1_1 && o3.parent == 2 && o3.length == 8);
        assert_eq!(p.buffers(), 16 + ((BT_HCI_1_1.to_bytes().len() as u64 + 1 + 7) & !7) + 16 + 8);
        let (pointer, size) = hidl_ref(o0.buffer);
        assert!(size as usize == BT_HCI_1_1.to_bytes().len() && pointer == o1.buffer);
    }

    #[test]
    fn initialize_and_send_parcels() {
        let mut p = HidlParcel::new();
        initialize(&mut p, true, 0x1234);
        assert_eq!(p.object_offsets().len(), 1);
        assert_eq!(CStr::from_bytes_until_nul(p.bytes()).unwrap(), BT_HCI_1_1);
        let at = p.object_offsets()[0] as usize;
        let flat = crate::hal::aidl::FlatObject::parse(&p.bytes()[at..at + 24]);
        assert!(flat.kind == TYPE_BINDER && flat.binder_or_handle == 0x1234 && p.buffers() == 0);
        // A send parcel is what the HAL's callback gets back in the other direction: read our own.
        let reset = [0x03u8, 0x0c, 0x00];
        send(&mut p, BT_SEND_COMMAND, &reset);
        let mut r = p.reader();
        assert_eq!(r.token(), Some(BT_HCI_1_0));
        assert_eq!(r.bytes(), Some(&reset[..]));
        assert!(!r.is_bad());
    }

    #[test]
    fn callbacks() {
        let complete = [0x0eu8, 0x04, 0x01, 0x03, 0x0c, 0x00];
        let mut reply = HidlParcel::new();
        // hciEventReceived as the kernel delivers it (the same layout, the callbacks' token).
        let mut p = HidlParcel::new();
        p.put_token(BT_CALLBACKS_1_0);
        p.put_bytes(&complete);
        assert_eq!(callback_transact(BT_CB_EVENT, &mut p.reader(), &mut reply), Ok(Callback::Packet(4, &complete[..])));
        assert!(reply.size() == 4 && reply.object_offsets().is_empty()); // Status::ok()
        assert_eq!(callback_transact(BT_CB_ISO, &mut p.reader(), &mut reply), Err(hidl::UNKNOWN_TRANSACTION)); // 1.1's method, 1.0's token
        let mut p = HidlParcel::new();
        p.put_token(BT_CALLBACKS_1_0);
        p.put_i32(0);
        assert_eq!(callback_transact(BT_CB_INIT_COMPLETE, &mut p.reader(), &mut reply), Ok(Callback::InitComplete(0)));
        // A truncated event parcel is refused, not read.
        let mut p = HidlParcel::new();
        p.put_token(BT_CALLBACKS_1_0);
        p.put_bytes(&complete);
        let size = p.size();
        assert_eq!(
            callback_transact(BT_CB_EVENT, &mut p.reader_prefix(size - 40), &mut reply),
            Err(hidl::UNKNOWN_TRANSACTION)
        );
        // interfaceChain: a vector of three strings, each embedded in the array, in rising offsets.
        let mut p = HidlParcel::new();
        p.put_token(HIDL_BASE);
        assert_eq!(callback_transact(hidl::INTERFACE_CHAIN, &mut p.reader(), &mut reply), Ok(Callback::Base));
        assert!(reply.object_offsets().len() == 5 && !reply.is_overflow());
        let (c1, c4) = (reply.buffer_object(1), reply.buffer_object(4));
        assert!(
            c1.length == 48
                && c1.parent == 0
                && c4.parent == 1
                && c4.parent_offset == 32
                && c_text(c4.buffer) == HIDL_BASE
        );
        assert_eq!(callback_transact(0x7777, &mut p.reader(), &mut reply), Err(hidl::UNKNOWN_TRANSACTION));
        // A parcel too large for its buffer says so instead of writing past it.
        let big = [0u8; 10];
        let mut p = HidlParcel::new();
        for _ in 0..20 {
            p.put_bytes(&big);
        }
        assert!(p.is_overflow());
    }
}
