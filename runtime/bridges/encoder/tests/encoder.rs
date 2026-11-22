// SPDX-License-Identifier: MIT
//! The video encoder bridge's device (test_encoder.c): xnuhost's "droidvm,virtio-media" device over xh-core's real
//! transport, driven by a guest of plain memory ([`oneencoder::encctl::Guest`]) replaying exactly the V4L2 encoder
//! sequence UniXNU-droidvm's DroidVMVTHost.cpp issues: the message layout, the pool MMAP, and that the NV12 test
//! pattern reaches the stand-in encoder and comes back as an HEVC Annex-B access unit (VPS, SPS, PPS, IDR) that holds
//! the pattern's own samples. Twice: through the oneios-encoder helper process (the phone's arrangement, with the
//! stand-in in MediaCodec's place), and with the stand-in inside the device's process (xnuhost's encoder=sim and its
//! PC boots). No phone, no Android, no Apple files.
mod standin_reader;

use oneencoder::backend::Backend;
use oneencoder::backend_helper::HelperBackend;
use oneencoder::backend_stub::StubBackend;
use oneencoder::encctl::{querybuf, Guest};
use oneencoder::pod::Pod;
use oneencoder::proto::*;
use std::sync::Arc;
use std::time::Duration;

const W: u32 = 256;
const H: u32 = 256;

/// One session of one frame on a device over `backend`, as DroidVMVTHost.cpp runs it.
fn run(backend: Arc<dyn Backend>, base: u64) {
    let mut g = Guest::up(backend, base).expect("the device and its guest");
    assert_eq!(g.dev.mmio_read(0x08, 4), u64::from(MEDIA_VIRTIO_DEVICE_ID));
    assert_eq!(g.dev.mmio_read(0x100, 4), u64::from(MEDIA_CAP_VIDEO_M2M_MPLANE | MEDIA_CAP_STREAMING));

    let session = g.open().expect("OPEN");
    assert_ne!(session, 0);

    // ENUM_FMT, as the guest's transport lists each queue once at start (DroidVMVirtioMedia.c check()): OUTPUT takes
    // NV12, CAPTURE gives HEVC and H.264, and the list ends with an error.
    for (kind, index, format) in [
        (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 0, MEDIA_PIXFMT_NV12),
        (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 1, 0),
        (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 0, MEDIA_PIXFMT_HEVC),
        (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 1, MEDIA_PIXFMT_H264),
        (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 2, 0),
    ] {
        let mut fd = [0u32; 16]; // index, type, flags, description[32], pixelformat, mbus_code, reserved[3]
        (fd[0], fd[1]) = (index, kind);
        let status = g.ioctl(session, MEDIA_VIDIOC_ENUM_FMT, fd.as_bytes_mut());
        if format != 0 {
            assert!(status == 0 && fd[11] == format, "{kind} {index}");
        } else {
            assert_ne!(status, 0, "{kind} {index}");
        }
    }

    // S_FMT CAPTURE (HEVC) then OUTPUT (NV12), as startEncoder does.
    let mut cap = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
        width: W,
        height: H,
        pixelformat: MEDIA_PIXFMT_HEVC,
        num_planes: 1,
        ..Default::default()
    };
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_S_FMT, cap.as_bytes_mut()), 0);
    let mut out = MediaV4l2Format {
        type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
        width: W,
        height: H,
        pixelformat: MEDIA_PIXFMT_NV12,
        num_planes: 1,
        ..Default::default()
    };
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_S_FMT, out.as_bytes_mut()), 0);
    assert_eq!(out.planes[0].sizeimage, W * H * 3 / 2);

    // REQBUFS + QUERYBUF + MMAP for OUTPUT.
    let mut req = [2, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    assert!(g.ioctl(session, MEDIA_VIDIOC_REQBUFS, req.as_bytes_mut()) == 0 && req[0] == 2);
    let qb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 0);
    let (out_buf, out_size) = g.mmap(session, qb.plane.m).expect("MMAP of the OUTPUT buffer");
    assert!(out_size != 0);

    // REQBUFS + QUERYBUF + MMAP + QBUF for CAPTURE.
    let mut req = [4, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, MEDIA_MEMORY_MMAP, 0, 0];
    assert!(g.ioctl(session, MEDIA_VIDIOC_REQBUFS, req.as_bytes_mut()) == 0 && req[0] == 4);
    for i in 0..4 {
        let mut cb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, i);
        assert!(g.mmap(session, cb.plane.m).is_ok_and(|(_, size)| size != 0));
        assert_eq!(g.ioctl(session, MEDIA_VIDIOC_QBUF, cb.as_bytes_mut()), 0);
    }

    // STREAMON OUTPUT then CAPTURE: the backend opens.
    assert_eq!(g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE), 0);
    assert_eq!(g.stream_on(session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE), 0);

    // A NV12 test pattern into the OUTPUT buffer, then QBUF it.
    let frame_len = (W * H * 3 / 2) as usize;
    let pattern: Vec<u8> = (0..frame_len).map(|i| (i * 7 + 13) as u8).collect();
    g.write(out_buf, &pattern);
    let mut qbo = MediaV4l2Buffer { type_: MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, index: 0, sec: 1, ..Default::default() };
    qbo.plane.bytesused = frame_len as u32;
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_QBUF, qbo.as_bytes_mut()), 0);

    // The events: a DQBUF of the returned OUTPUT buffer and a DQBUF of the coded CAPTURE buffer.
    let (mut saw_output, mut coded) = (false, None);
    for _ in 0..4 {
        if saw_output && coded.is_some() {
            break;
        }
        let (kind, sess, ev) = g.wait_event(Duration::from_secs(3)).expect("an event");
        assert!(sess == session && kind == MEDIA_EVT_DQBUF);
        let b = MediaV4l2Buffer::read_from(&ev[8..]);
        if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            saw_output = true;
        } else if b.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
            assert!(b.flags & MEDIA_BUF_FLAG_KEYFRAME != 0);
            // MMAP the CAPTURE buffer again to read the bitstream (the same pool handle).
            let cb = querybuf(&mut g, session, MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, b.index);
            let (pa, _) = g.mmap(session, cb.plane.m).expect("MMAP of the CAPTURE buffer");
            coded = Some(g.read(pa, b.plane.bytesused as usize));
        }
    }
    let coded = coded.expect("a coded access unit");
    assert!(saw_output && !coded.is_empty());

    // The coded access unit: VPS (32), SPS (33), PPS (34), IDR (19), Annex-B, no larger than the coded buffer the
    // guest asks for; the slice's samples are the pattern's (at the PCM depth the stand-in wrote).
    assert!(coded.len() <= (W * H / 2 + 65536) as usize);
    let nals = standin_reader::split(&coded, true, 8);
    assert_eq!(nals.iter().map(|n| n.kind).collect::<Vec<_>>(), [32, 33, 34, 19]);
    let rbsp = standin_reader::rbsp(&nals[3], true);
    let mut picture = vec![0u8; frame_len];
    let (luma, chroma) = standin_reader::hevc_picture(&rbsp, W, H, &mut picture).expect("the stand-in's slice");
    assert!(standin_reader::same_picture(&picture, &pattern, W, H, luma, chroma));

    // ENCODER_CMD STOP -> drain -> EOS.
    let mut enc = [0u32; 10];
    enc[0] = MEDIA_ENC_CMD_STOP;
    assert_eq!(g.ioctl(session, MEDIA_VIDIOC_ENCODER_CMD, enc.as_bytes_mut()), 0);
    let mut saw_eos = false;
    for _ in 0..6 {
        let Some((kind, _, ev)) = g.wait_event(Duration::from_secs(3)) else { break };
        if kind == MEDIA_EVT_EVENT && u32::from_le_bytes(ev[8..12].try_into().unwrap()) == MEDIA_EVENT_EOS {
            saw_eos = true;
            break;
        }
    }
    assert!(saw_eos);
    let st = g.stats();
    assert!(st.sessions == 1 && st.frames_in == 1 && st.frames_out == 1 && st.errors == 0, "{st:?}");

    // CLOSE, then more sessions than the device has slots, one after another: each close gives its slot back (the
    // guest's transport opens and closes one at start, VideoToolbox one for every picture).
    assert_eq!(g.close(session), Some(0));
    for _ in 0..5 {
        let again = g.open().expect("OPEN again");
        assert!(again != 0 && again != session);
        assert_eq!(g.close(again), Some(0));
    }
}

#[test]
fn through_the_helper_process() {
    run(HelperBackend::new(env!("CARGO_BIN_EXE_oneios-encoder")), 0x0a00_0000);
}

#[test]
fn with_the_standin_in_process() {
    run(Arc::new(StubBackend), 0x0a00_1000);
}
