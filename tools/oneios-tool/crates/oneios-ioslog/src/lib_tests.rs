//! Tests of the command line and the time helpers (no log file needed).

use super::*;

#[test]
fn times() {
    let t = parse_time("2026-11-12T14:12:00Z").unwrap();
    assert_eq!(t, 1_794_492_720_000_000_000f64);
    assert_eq!(parse_time("2026-11-12 14:12:00").unwrap(), t);
    assert_eq!(parse_time("2026-11-12 14:12").unwrap(), t);
    assert_eq!(parse_time("2026-11-12T19:42:00+05:30").unwrap(), t);
    assert_eq!(parse_time("2026-11-12 14:12:00.5").unwrap(), t + 5e8);
    assert!(parse_time("yesterday").is_err());
    // the decoder's times are f64 nanoseconds: steps of 256 ns at this date, so only whole milliseconds are exact
    assert_eq!(time_text(t), "2026-11-12 14:12:00.000000Z");
    assert_eq!(time_text(t + 5e8), "2026-11-12 14:12:00.500000Z");
    assert_eq!(base_name("/usr/libexec/backboardd"), "backboardd");
    assert_eq!(uuid_text(Uuid::parse_str("9a6a3124-274a-44b2-9abf-2bc9e4599b3b").unwrap()), "9A6A3124274A44B29ABF2BC9E4599B3B");
}

#[test]
fn command_line() {
    let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("ioslog").chain(args.iter().copied()));
    assert!(parse(&["show", "data.img", "-f", "csv", "-o", "x.csv", "--boot", "last", "--proc", "backboardd", "--proc", "SpringBoard", "--type", "error,fault"]).is_ok());
    assert!(parse(&["save", "data.img", "x.logarchive", "--volume", "Data"]).is_ok());
    assert!(parse(&["boots", "data.img", "--block0"]).is_ok());
    assert!(parse(&["checks", "data.img", "--boot", "all", "--examples", "5"]).is_ok());
    assert!(parse(&["health", "data.img", "--boot", "all", "--top", "3", "--detail", "-o", "h.txt", "--checkpoint", "198"]).is_ok() && parse(&["health"]).is_ok());
    assert!(parse(&["show"]).is_err() && parse(&["show", "a", "-f", "xml"]).is_err() && parse(&[]).is_err());
}

/// The guest's clock against the library's own resolver: a 19.2 MHz boot (625/12) with three time records, an
/// Apple Silicon boot (125/3) and a 1/1 boot, whose times stay the library's.
#[test]
fn guest_clock() {
    use macos_unifiedlogs::timesync::{RawTimesyncRecord, TimestampResolver};
    let record = |kernel_time: u64, walltime: i64| RawTimesyncRecord { kernel_time, walltime, timezone: 0, daylight_savings: 0 };
    let boot = |uuid: Uuid, numerator: u32, denominator: u32, records: Vec<RawTimesyncRecord>| RawTimesyncBoot {
        boot_uuid: uuid,
        timebase_numerator: numerator,
        timebase_denominator: denominator,
        boot_time: 1_795_000_000_000_000_000,
        timezone_offset_mins: 0,
        daylight_savings: 0,
        records,
    };
    let (phone, mac, intel) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let start = 1_795_000_000_000_000_000i64;
    // the phone: ticks of 625/12 ns; records at 1 s, 100 s and 400 s of its clock
    let ns = |ticks: u64| ticks as f64 * 625.0 / 12.0;
    let tick_of = |seconds: u64| seconds * 19_200_000;
    let records = |at: &[u64]| at.iter().map(|s| record(tick_of(*s), start + ns(tick_of(*s)) as i64)).collect::<Vec<_>>();
    let mut data = HashMap::new();
    data.insert(phone, boot(phone, 625, 12, records(&[1, 100, 400])));
    data.insert(mac, boot(mac, 125, 3, vec![record(24_000_000, start + 1_000_000_000)]));
    data.insert(intel, boot(intel, 1, 1, vec![record(1_000_000_000, start + 1_000_000_000)]));
    let library = TimestampResolver::new(data.clone());
    let clock = Clock::new(&data);
    // entries at 1.5 s, 10 s, 99 s, 100 s, 250 s and 7 min: each comes back at its own second
    for seconds in [1.5f64, 10.0, 99.0, 100.0, 250.0, 420.0] {
        let ticks = (seconds * 19_200_000.0) as u64;
        let theirs = library.resolve(&phone, ticks, 1);
        let ours = clock.time(&phone, theirs);
        let want = start as f64 + ns(ticks);
        assert!((ours - want).abs() < 2_000.0, "{seconds} s: {ours} for {want} (the library: {theirs})");
        if seconds != 100.0 {
            // (on a record the library is right)
            assert!((theirs - want).abs() > 1e8, "the library's time of {seconds} s is not off: the fix is not needed");
        }
    }
    // before the first record (a chunk with a base time): placed from the first record, as the library does
    let early = library.resolve(&phone, tick_of(1) / 2, 1);
    assert!((clock.time(&phone, early) - (start as f64 + ns(tick_of(1) / 2))).abs() < 2_000.0);
    // 125/3 and 1/1: the library's times are right and stay as they are
    for (uuid, ticks) in [(mac, 48_000_000u64), (intel, 5_000_000_000u64)] {
        let theirs = library.resolve(&uuid, ticks, 1);
        assert_eq!(clock.time(&uuid, theirs), theirs);
    }
    // a boot without a time record and a default clock change nothing
    assert_eq!(clock.time(&Uuid::from_u128(9), 5.0), 5.0);
    assert_eq!(Clock::default().time(&phone, 5.0), 5.0);
    // show's switch for compare.sh
    let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("ioslog").chain(args.iter().copied()));
    assert!(parse(&["show", "data.img", "-f", "csv", "--library-times"]).is_ok());
}
