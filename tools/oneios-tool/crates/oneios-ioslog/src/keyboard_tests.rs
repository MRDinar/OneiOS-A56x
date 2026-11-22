use super::*;
use crate::health::{count_text, Counter};

/// Lines as `ioslog show` writes them, by hand (no Apple file): the shapes of the 2026-11-22 session's lines.
const LINES: &str = "\
2026-11-22 14:59:39.773199Z WebSheet[256] Default [com.apple.UIKit:KeyboardTouch] touch down
2026-11-22 14:59:39.773200Z WebSheet[256] Default [com.apple.UIKit:KeyboardTouch] touch up
2026-11-22 14:59:39.773201Z WebSheet[256] Default [com.apple.UIKit:KeyboardTouch] touch up
2026-11-22 14:59:39.800000Z Spotlight[192] Default [com.apple.UIKit:KeyboardTouch] touch down
2026-11-22 14:59:39.800001Z Spotlight[192] Default [com.apple.UIKit:KeyboardTouch] touch up
2026-11-22 14:59:40.000000Z WebSheet[256] Default [com.apple.hangtracer:] Hang detected: 1.59s (always-on hang reporting)
2026-11-22 14:59:35.006010Z kbd[155] Error [com.apple.MobileAsset:AutoSet] MA-auto-set[AUTO-SHORT-TERM][FRAMEWORK]{auto-set(_shortTermLockAtomicSync)[atomic-instance]:_shortTermOpenSharedLockFile} (SYMLINK) | lockReason:UAFAssetSet lock of com.apple.linguisticdata for SHORT_TERM_LOCKER | unable to obtain shared lock | errno:2
2026-11-22 14:59:35.550476Z cameracaptured[51] Error [com.apple.MobileAsset:AutoSet] MA-auto-set[AUTO-SHORT-TERM][FRAMEWORK]{auto-set(_shortTermLockAtomicSync)[atomic-instance]:_shortTermOpenSharedLockFile} (SYMLINK) | lockReason:UAFAssetSet lock of com.apple.AppleDepth.MLModel for SHORT_TERM_LOCKER | unable to obtain shared lock | errno:2
2026-11-22 14:59:35.006100Z kbd[155] Create UAF.retrieveAssetSetWithToken
2026-11-22 14:59:35.006200Z kbd[155] Default [com.apple.DataDeliveryServices:UAFAssetManager] assetsForQuery: <query: com.apple.MobileAsset.LinguisticData, locO: 1, iO: 0>
2026-11-22 14:59:36.000000Z kbd[155] Error updateAdditionalLexicons  LM model is not valid yet
2026-11-22 14:59:36.100000Z Spotlight[192] Error [com.apple.spotlight.QueryParser:QPUNParser] [QPNLU][qid=4] Unable to load <private> UN model with timeout:500ms embedding generation timeout: 500ms lastError:(null)
2026-11-22 14:59:36.200000Z Spotlight[192] Error [com.apple.UIKit:KeyboardUI] Keyboard screen not found for window (null)
2026-11-22 14:59:36.300000Z WebSheet[256] Default [com.apple.UIKit:UIPeripheralHost] endPlacementForInputViewSet, returning -> <UIInputViewSetPlacementOnScreenWithAccessory>
2026-11-22 14:59:36.400000Z WebSheet[256] Default [com.apple.UIKit:KeyboardScene] Notifying observers of geometry update -> app scene 1: {{0, 0}, {480, 1040}}, keyboard scene: 1: {{0, 0}, {480, 1040}}
2026-11-22 14:59:36.500000Z WebSheet[256] Default [com.apple.TextInputUI:InputSession_Proxy] -[TUIInputSessionProxy handleRequest:], invocation: <private>
2026-11-22 14:59:36.600000Z WebSheet[256] Default [com.apple.UIKit:EventDispatch] Sending UIEvent type: 0; subtype: 0; to windows: 1; dispatchLevel: 0
";

fn report() -> crate::health::Report {
    let mut counter = Counter::with(KEYBOARD, 2).unwrap();
    count_text(LINES.as_bytes(), &mut counter).unwrap();
    counter.finish("test".to_string(), "test".to_string())
}

fn kind<'a>(report: &'a crate::health::Report, name: &str) -> &'a crate::health::Count {
    report.kinds.iter().find(|k| k.kind.name == name).unwrap()
}

#[test]
fn every_text_compiles_and_names_are_unique() {
    Counter::with(KEYBOARD, 0).unwrap();
    let mut names: Vec<&str> = KEYBOARD.iter().map(|k| k.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), KEYBOARD.len());
}

#[test]
fn counts_taps_by_app_downs_and_ups_apart() {
    let r = report();
    let taps = kind(&r, "key-taps");
    assert_eq!(taps.all.count, 5);
    assert_eq!(taps.texts, vec![2, 3]); // 2 downs, 3 ups
    assert_eq!(taps.loggers[0].0, "WebSheet");
    assert_eq!(taps.loggers[0].1.count, 3);
    // per text, by app: WebSheet has 2 ups for 1 down
    assert_eq!(taps.text_loggers[1][0].0, "WebSheet");
    assert_eq!(taps.text_loggers[1][0].1.count, 2);
}

#[test]
fn counts_the_rest() {
    let r = report();
    let hangs = kind(&r, "hangs");
    assert_eq!(hangs.all.count, 1);
    assert!((hangs.all.seconds - 1.59).abs() < 1e-9);
    assert_eq!(hangs.all.over, 1);
    let lock = kind(&r, "asset-lock");
    assert_eq!(lock.all.count, 2);
    let sets: Vec<&str> = lock.names.iter().map(|(n, _)| n.as_str()).collect();
    assert!(sets.contains(&"com.apple.linguisticdata") && sets.contains(&"com.apple.AppleDepth.MLModel"));
    let query = kind(&r, "asset-query");
    assert_eq!(query.all.count, 2);
    assert_eq!(query.names[0].0, "com.apple.MobileAsset.LinguisticData");
    assert_eq!(kind(&r, "language-model").all.count, 2);
    assert_eq!(kind(&r, "kb-screen").all.count, 1);
    assert_eq!(kind(&r, "kb-screen").loggers[0].0, "Spotlight");
    assert_eq!(kind(&r, "kb-placement").texts, vec![1, 1]);
    assert_eq!(kind(&r, "text-input").all.count, 1);
    assert_eq!(r.entries, 17);
}

#[test]
fn the_report_has_one_line_per_kind() {
    let r = report();
    let text = r.text(TOP, false);
    assert_eq!(text.lines().count(), 1 + KEYBOARD.len());
    assert!(text.lines().nth(1).unwrap().starts_with("key-taps"));
}
