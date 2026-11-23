// SPDX-License-Identifier: MIT
//! bin/oneios-camera: the camera helper process ([`onecamera::helper`]).
fn main() {
    std::process::exit(onecamera::helper::main());
}
