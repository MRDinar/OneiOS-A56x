// SPDX-License-Identifier: MIT
//! The Rust mirrors of media_proto.h (the droidvm,virtio-media wire layout) and encoder_link.h (the helper's socket)
//! against the C headers: every constant, every struct's size and every field's offset and size, and every name of a
//! header mirrored.
#[path = "../../camera/tests/cprobe/mod.rs"]
mod cprobe;

use cprobe::{check_consts, check_layout, Probe};
use oneencoder::link::*;
use oneencoder::proto::*;
use std::path::{Path, PathBuf};

fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn media_proto() {
    let header = here().join("media_proto.h");
    let consts: &[(&str, u64)] = &[
        ("MEDIA_VIRTIO_DEVICE_ID", MEDIA_VIRTIO_DEVICE_ID.into()),
        ("MEDIA_QUEUE_COMMAND", MEDIA_QUEUE_COMMAND.into()),
        ("MEDIA_QUEUE_EVENT", MEDIA_QUEUE_EVENT.into()),
        ("MEDIA_NQUEUES", MEDIA_NQUEUES.into()),
        ("MEDIA_CMD_OPEN", MEDIA_CMD_OPEN.into()),
        ("MEDIA_CMD_CLOSE", MEDIA_CMD_CLOSE.into()),
        ("MEDIA_CMD_IOCTL", MEDIA_CMD_IOCTL.into()),
        ("MEDIA_CMD_MMAP", MEDIA_CMD_MMAP.into()),
        ("MEDIA_CMD_MUNMAP", MEDIA_CMD_MUNMAP.into()),
        ("MEDIA_EVT_ERROR", MEDIA_EVT_ERROR.into()),
        ("MEDIA_EVT_DQBUF", MEDIA_EVT_DQBUF.into()),
        ("MEDIA_EVT_EVENT", MEDIA_EVT_EVENT.into()),
        ("MEDIA_EVENT_BYTES", MEDIA_EVENT_BYTES.into()),
        ("MEDIA_VIDIOC_ENUM_FMT", MEDIA_VIDIOC_ENUM_FMT.into()),
        ("MEDIA_VIDIOC_G_FMT", MEDIA_VIDIOC_G_FMT.into()),
        ("MEDIA_VIDIOC_S_FMT", MEDIA_VIDIOC_S_FMT.into()),
        ("MEDIA_VIDIOC_REQBUFS", MEDIA_VIDIOC_REQBUFS.into()),
        ("MEDIA_VIDIOC_QUERYBUF", MEDIA_VIDIOC_QUERYBUF.into()),
        ("MEDIA_VIDIOC_QBUF", MEDIA_VIDIOC_QBUF.into()),
        ("MEDIA_VIDIOC_STREAMON", MEDIA_VIDIOC_STREAMON.into()),
        ("MEDIA_VIDIOC_STREAMOFF", MEDIA_VIDIOC_STREAMOFF.into()),
        ("MEDIA_VIDIOC_G_CTRL", MEDIA_VIDIOC_G_CTRL.into()),
        ("MEDIA_VIDIOC_ENUM_FRAMESIZES", MEDIA_VIDIOC_ENUM_FRAMESIZES.into()),
        ("MEDIA_VIDIOC_ENCODER_CMD", MEDIA_VIDIOC_ENCODER_CMD.into()),
        ("MEDIA_VIDIOC_SUBSCRIBE_EVENT", MEDIA_VIDIOC_SUBSCRIBE_EVENT.into()),
        ("MEDIA_VIDIOC_DECODER_CMD", MEDIA_VIDIOC_DECODER_CMD.into()),
        ("MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE", MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE.into()),
        ("MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE", MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE.into()),
        ("MEDIA_MEMORY_MMAP", MEDIA_MEMORY_MMAP.into()),
        ("MEDIA_CAP_VIDEO_M2M_MPLANE", MEDIA_CAP_VIDEO_M2M_MPLANE.into()),
        ("MEDIA_CAP_STREAMING", MEDIA_CAP_STREAMING.into()),
        ("MEDIA_BUF_FLAG_KEYFRAME", MEDIA_BUF_FLAG_KEYFRAME.into()),
        ("MEDIA_BUF_FLAG_ERROR", MEDIA_BUF_FLAG_ERROR.into()),
        ("MEDIA_BUF_FLAG_LAST", MEDIA_BUF_FLAG_LAST.into()),
        ("MEDIA_EVENT_EOS", MEDIA_EVENT_EOS.into()),
        ("MEDIA_EVENT_SOURCE_CHANGE", MEDIA_EVENT_SOURCE_CHANGE.into()),
        ("MEDIA_ENC_CMD_START", MEDIA_ENC_CMD_START.into()),
        ("MEDIA_ENC_CMD_STOP", MEDIA_ENC_CMD_STOP.into()),
        ("MEDIA_PIXFMT_NV12", MEDIA_PIXFMT_NV12.into()),
        ("MEDIA_PIXFMT_H264", MEDIA_PIXFMT_H264.into()),
        ("MEDIA_PIXFMT_HEVC", MEDIA_PIXFMT_HEVC.into()),
    ];
    let mut probe = Probe::new(&header, &[]);
    for (name, _) in consts {
        probe.ask(name, name);
    }
    probe.ask("fourcc", "MEDIA_FOURCC('a', 'b', 'c', 'd')");
    let buffer = [
        "index",
        "type",
        "bytesused",
        "flags",
        "field",
        "pad0",
        "sec",
        "usec",
        "timecode",
        "sequence",
        "memory",
        "m",
        "length",
        "reserved2",
        "request_fd",
        "pad1",
        "plane",
        "plane.bytesused",
        "plane.length",
        "plane.m",
        "plane.data_offset",
        "plane.reserved",
    ];
    let format = [
        "type",
        "pad",
        "width",
        "height",
        "pixelformat",
        "field",
        "colorspace",
        "planes",
        "planes[0].sizeimage",
        "planes[0].bytesperline",
        "planes[0].reserved",
        "num_planes",
        "flags",
        "ycbcr",
        "quantization",
        "xfer",
        "reserved",
        "rest",
    ];
    probe.layout("struct media_v4l2_buffer", &buffer).layout("struct media_v4l2_format", &format);
    let Some(values) = probe.run() else { return };
    check_consts(&header, &values, consts, &[]);
    assert_eq!(values["fourcc"], u64::from(fourcc(b'a', b'b', b'c', b'd')));
    type B = MediaV4l2Buffer;
    let mut fields = vec![
        field!(B, index),
        field!(B, type_),
        field!(B, bytesused),
        field!(B, flags),
        field!(B, field),
        field!(B, pad0),
        field!(B, sec),
        field!(B, usec),
        field!(B, timecode),
        field!(B, sequence),
        field!(B, memory),
        field!(B, m),
        field!(B, length),
        field!(B, reserved2),
        field!(B, request_fd),
        field!(B, pad1),
        field!(B, plane),
        field!(B, plane.bytesused),
        field!(B, plane.length),
        field!(B, plane.m),
        field!(B, plane.data_offset),
        field!(B, plane.reserved),
    ];
    for (f, c) in fields.iter_mut().zip(buffer) {
        f.0 = c; // `type_` is C's `type`
    }
    check_layout(&values, "struct media_v4l2_buffer", size_of::<B>(), &fields);
    type F = MediaV4l2Format;
    type P = MediaPlaneFormat;
    let plane0 = |(_, o, s): (&str, usize, usize)| (o + std::mem::offset_of!(F, planes), s);
    let mut fields = vec![
        field!(F, type_),
        field!(F, pad),
        field!(F, width),
        field!(F, height),
        field!(F, pixelformat),
        field!(F, field),
        field!(F, colorspace),
        field!(F, planes),
    ];
    for f in [field!(P, sizeimage), field!(P, bytesperline), field!(P, reserved)] {
        let (o, s) = plane0(f);
        fields.push(("", o, s));
    }
    fields.extend([
        field!(F, num_planes),
        field!(F, flags),
        field!(F, ycbcr),
        field!(F, quantization),
        field!(F, xfer),
        field!(F, reserved),
        field!(F, rest),
    ]);
    for (f, c) in fields.iter_mut().zip(format) {
        f.0 = c;
    }
    check_layout(&values, "struct media_v4l2_format", size_of::<F>(), &fields);
}

#[test]
fn encoder_link() {
    let header = here().join("encoder_link.h");
    let consts: &[(&str, u64)] = &[
        ("ENC_LINK_VERSION", ENC_LINK_VERSION.into()),
        ("ENC_MSG_HELLO", ENC_MSG_HELLO.into()),
        ("ENC_MSG_OPEN", ENC_MSG_OPEN.into()),
        ("ENC_MSG_OPEN_REPLY", ENC_MSG_OPEN_REPLY.into()),
        ("ENC_MSG_FEED", ENC_MSG_FEED.into()),
        ("ENC_MSG_DRAIN", ENC_MSG_DRAIN.into()),
        ("ENC_MSG_CLOSE", ENC_MSG_CLOSE.into()),
        ("ENC_MSG_CODED", ENC_MSG_CODED.into()),
        ("ENC_MSG_EOS", ENC_MSG_EOS.into()),
        ("ENC_MSG_ERROR", ENC_MSG_ERROR.into()),
    ];
    let mut probe = Probe::new(&header, &[]);
    for (name, _) in consts {
        probe.ask(name, name);
    }
    let names = ["type", "session", "pts", "a", "b", "c", "len"];
    probe.layout("struct enc_msg", &names);
    let Some(values) = probe.run() else { return };
    check_consts(&header, &values, consts, &[]);
    type M = EncMsg;
    let mut fields = vec![
        field!(M, type_),
        field!(M, session),
        field!(M, pts),
        field!(M, a),
        field!(M, b),
        field!(M, c),
        field!(M, len),
    ];
    for (f, c) in fields.iter_mut().zip(names) {
        f.0 = c;
    }
    check_layout(&values, "struct enc_msg", size_of::<M>(), &fields);
}
