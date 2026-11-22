// SPDX-License-Identifier: MIT
//! audio/: sound. The virtio-snd device's backends ([`backend`]: the AAudio helper process and a null one), the
//! helper's protocol ([`proto`]) and the helper itself ([`aaudio_helper`], bin oneios-audio).

pub mod backend; // audio/backend.c
pub mod proto; // audio/audio_proto.h

pub use backend::{AudioBackend, AudioHelper, NullAudio};
