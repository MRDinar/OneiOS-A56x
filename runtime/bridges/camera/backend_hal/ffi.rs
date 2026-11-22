// SPDX-License-Identifier: MIT
//! The provider HAL over libbinder_ndk (Android only): the binder calls, the callback object, the buffers.
use crate::cam_log;
use crate::core::{Backend, FrameSink};
use crate::meta::CamInfo;
use std::sync::Arc;

/// The provider-HAL back end.
#[derive(Default)]
pub struct HalBackend {}

impl Backend for HalBackend {
    fn name(&self) -> &'static str {
        "hal"
    }

    fn list(&mut self, _max: usize) -> Result<Vec<CamInfo>, i32> {
        cam_log!("hal: the provider HAL back end is not in this build yet");
        Err(libc::ENOSYS)
    }

    fn start(&mut self, _: &CamInfo, _: i32, _: i32, _: i32, _: i32, _: Arc<dyn FrameSink>) -> Result<(), i32> {
        Err(libc::ENOSYS)
    }

    fn stop(&mut self) {}
}
