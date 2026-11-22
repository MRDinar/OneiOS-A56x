// SPDX-License-Identifier: MIT
//! fake-hal: a stand-in servicemanager with stand-in HALs on a binder device (onebridges::hal::fake), for the
//! stand-in phone (tools/oneios-tool/tests/phone/hw-backend.sh).
//!   ONEIOS_BINDER=DEVICE fake-hal DIR   the context manager of DEVICE until killed; "ready" on stdout
//! C: tests/fake_hal.c (its main).

use onebridges::hal::{binderfs, fake};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: ONEIOS_BINDER=DEVICE fake-hal DIR");
        std::process::exit(2);
    }
    // A binderfs of a kernel with no default devices has only binder-control: ask it for the device by name.
    if let Ok(device) = std::env::var("ONEIOS_BINDER") {
        let _ = binderfs::add_device(&device);
    }
    std::process::exit(fake::serve(std::path::Path::new(&args[1]), &mut std::io::stdout()));
}
