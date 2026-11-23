// SPDX-License-Identifier: MIT
//! MediaCodec's numbers and AMediaCodecBufferInfo's layout, as backend_ndk.rs uses them, against the NDK's headers (a
//! probe built with the NDK's clang and run under qemu-aarch64; skipped with a line where either is missing). The
//! encoder's own wire layouts (proto.rs: droidvm,virtio-media; link.rs: the helper's socket) have no C side any more:
//! xnuhost and the helper are both this crate, so those layouts are checked by tests/encoder.rs's round trips.
// The camera's probe module, shared: this test uses its layout and NDK parts only.
#[allow(dead_code)]
#[path = "../../camera/tests/cprobe/mod.rs"]
mod cprobe;

use cprobe::{check_layout, Probe};

/// MediaCodec's numbers and AMediaCodecBufferInfo's layout (backend_ndk.rs) against the NDK's headers.
#[test]
fn ndk_numbers() {
    use oneencoder::backend_ndk::*;
    let consts: &[(&str, i64)] = &[
        ("AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG", BUFFER_FLAG_CODEC_CONFIG.into()),
        ("AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM", BUFFER_FLAG_END_OF_STREAM.into()),
        ("AMEDIACODEC_BUFFER_FLAG_KEY_FRAME", BUFFER_FLAG_KEY_FRAME.into()),
        ("AMEDIACODEC_INFO_TRY_AGAIN_LATER", INFO_TRY_AGAIN_LATER as i64),
        ("AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED", INFO_OUTPUT_FORMAT_CHANGED as i64),
        ("AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED", INFO_OUTPUT_BUFFERS_CHANGED as i64),
        ("AMEDIACODEC_CONFIGURE_FLAG_ENCODE", 1),
    ];
    let mut probe = Probe::ndk(&["media/NdkMediaCodec.h"]);
    for (name, _) in consts {
        probe.ask(name, &format!("(long long){name}"));
    }
    let names = ["offset", "size", "presentationTimeUs", "flags"];
    probe.layout("AMediaCodecBufferInfo", &names);
    let Some(values) = probe.run() else { return };
    for (name, v) in consts {
        assert_eq!(values[*name] as i64, *v, "{name}");
    }
    type I = BufferInfo;
    let mut fields = vec![field!(I, offset), field!(I, size), field!(I, presentation_time_us), field!(I, flags)];
    for (f, c) in fields.iter_mut().zip(names) {
        f.0 = c;
    }
    check_layout(&values, "AMediaCodecBufferInfo", size_of::<I>(), &fields);
}
