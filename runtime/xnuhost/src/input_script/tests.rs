// SPDX-License-Identifier: MIT
//! test_xnuhost.c's test_input_script: the file's lines as steps (the pen's lines are pen.rs's test's).
use std::sync::Arc;

use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam};

use super::*;

fn event(s: &Step) -> (u16, u16, i32) {
    match *s {
        Step::Event { kind, code, value, .. } => (kind, code, value),
        _ => (99, 0, 0),
    }
}

#[test]
fn steps() {
    let s = parse("# a panel\nabs 0x35 0 1439 10\n\nabs 0x36 0 3119\nready\nwait 250\ntap 700 1500 40  # a tap\nkey 116\nrepeat 2\n").unwrap();
    // 2 axes, ready, wait, tap (6 events, wait, 3 events), key (2 events, wait, 2 events), repeat
    assert_eq!(s.len(), 2 + 1 + 1 + 10 + 5 + 1);
    assert_eq!(s[0], Step::Abs { pen: false, code: 0x35, min: 0, max: 1439, res: 10 });
    assert!(matches!(s[1], Step::Abs { res: 0, .. }));
    assert!(s[2] == Step::Ready && s[3] == Step::Wait(250));
    assert!(event(&s[4]) == (3, 0x2f, 0) && event(&s[5]) == (3, 0x39, 1) && event(&s[6]) == (3, 0x35, 700));
    assert!(event(&s[7]) == (3, 0x36, 1500) && event(&s[8]) == (1, 0x14a, 1) && event(&s[9]).0 == 0);
    assert!(s[10] == Step::Wait(40) && event(&s[11]) == (3, 0x39, -1) && event(&s[12]).2 == 0 && event(&s[13]).0 == 0);
    assert!(event(&s[14]) == (1, 116, 1) && s[16] == Step::Wait(100) && event(&s[17]).2 == 0);
    assert_eq!(s[19], Step::Repeat(Some(2)));
    let s = parse("swipe 100 3000 100 1000 160\nrepeat\n").unwrap();
    assert!(s.len() == 6 + 10 * 4 + 3 + 1 && s[s.len() - 1] == Step::Repeat(None));
    assert!(s[6] == Step::Wait(16) && event(&s[8]) == (3, 0x36, 2800) && event(&s[44]).2 == 1000);
    assert_eq!(parse("wait 5\n\nev 3 0x35\n"), Err(3));
    assert_eq!(parse("abs 0x40 0 10\n"), Err(1));
    assert!(parse("tap 1 2 x\n").is_err());
    testing::install();
    let ram = GuestRam::new(1 << 20);
    let dev = crate::virtio_input::create(0, 44).unwrap();
    assert!(!dev.ready());
    assert!(Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, 8, ram.pa(0x10000)).is_some() && dev.ready());
}
