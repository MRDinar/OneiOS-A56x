// SPDX-License-Identifier: MIT
//! audio/: sound. The virtio-snd device's backends ([`backend`]: the AAudio helper process and a null one), the
//! helper's protocol ([`proto`]) and the helper itself ([`aaudio_helper`], bin oneios-audio).

pub mod backend; // audio/backend.c
pub mod proto; // audio/audio_proto.h

pub use backend::{AudioBackend, AudioHelper, NullAudio};
pub mod aaudio_helper; // audio/aaudio_helper.c: the helper's logic, AAudio behind a trait (the bin: oneios_audio.rs)

#[cfg(test)]
mod test_audio_helper; // tests/test_audio_helper.c and its fake AAudio
