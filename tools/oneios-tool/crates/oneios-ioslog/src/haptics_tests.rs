use super::*;
use crate::health::{count_text, Counter};

/// Lines as `ioslog show` writes them, by hand (no Apple file): the shapes of the phone's boots of 2026-11-22 (a
/// feedback played by Core Haptics, the keyboard's mode) and 2026-11-23 (a prewarm and nothing asked).
const LINES: &str = "\
2026-11-22 21:59:52.754584Z SpringBoard[30] Default [com.apple.UIKit:Feedback] prewarm engine <_UIFeedbackCoreHapticsHapticsOnlyEngine: 0x119aa6780>, prewarmCount: 0 -> 1
2026-11-22 21:59:52.772130Z SpringBoard[30] Default [com.apple.corehaptics:hapi]         CHHapticEngine.mm:1543  -[CHHapticEngine initWithAudioSession:sessionIsShared:options:error:]: Creating engine 11985A920 with unshared audio session 0
2026-11-22 21:59:52.928050Z SpringBoard[30] Default [com.apple.corehaptics:hapi]         CHHapticEngine.mm:1235  -[CHHapticEngine prewarmWithCompletionHandler:]: Prewarming the underlying Haptic Player
2026-11-22 21:59:52.929000Z SpringBoard[30] Default [com.apple.corehaptics:hapi]         CHHapticEngine.mm:1262  -[CHHapticEngine doStartWithCompletionHandler:]: Starting underlying Haptic Player
2026-11-22 21:59:52.929100Z SpringBoard[30] Default [com.apple.UIKit:Feedback] generator <UINotificationFeedbackGenerator: 0x1376a2640> playing feedback <_UICustomDiscreteFeedback: 0x13768fe80>
2026-11-22 21:59:52.929200Z SpringBoard[30] Default [com.apple.corehaptics:hcln]         AVHapticPlayer.mm:103   -[AVHapticPlayerChannel sendEvents:withImmediateParameters:atTime:error:]: sending event array: clientID: 0x300001E atTime: 12215.240
2026-11-22 21:59:52.929300Z SpringBoard[30] Default [com.apple.UIKit:Feedback] played feedback <_UICustomDiscreteFeedback: 0x16ac9d180> with engine <_UIFeedbackCoreHapticsIgnoreCaptureHapticsOnlyEngine: 0x1607cf980> at time 12215.239723593751
2026-11-22 22:00:03.592730Z SpringBoard[30] Default [com.apple.UIKit:Feedback] generator <UINotificationFeedbackGenerator: 0x1376a2640> playing feedback <_UICustomDiscreteFeedback: 0x13768fe80>
2026-11-22 22:00:04.109944Z SpringBoard[30] Default [com.apple.UIKit:Feedback] played feedback <_UICustomDiscreteFeedback: 0x13768fe80> with engine <_UIFeedbackSystemSoundEngine: 0x1377d8a00> at time 29994.550389062504
2026-11-22 21:59:59.439130Z SpringBoard[30] Default [com.apple.UIKit:UIKBFeedbackGenerator] <_UIKBFeedbackGenerator: 0x13f929bc0>: Updating mode. Haptics: supported. Haptics: disabled. Ringer: on. Sound: enabled. Mode: audio only
2026-11-22 22:00:36.242757Z Spotlight[247] Default [com.apple.UIKit:UIKBFeedbackGenerator] <_UIKBFeedbackGenerator: 0x105152940>: Updating mode. Haptics: supported. Haptics: enabled. Ringer: on. Sound: enabled. Mode: audio and haptics
2026-11-22 22:00:36.300000Z Notes[250] Default [com.apple.UIKit:UIKBFeedbackGenerator] <_UIKBFeedbackGenerator: 0x105152941>: Updating mode. Haptics: unsupported. Ringer: on. Sound: enabled. Mode: audio only
2026-11-22 22:00:37.000000Z Preferences[180] Error [com.apple.corehaptics:hcln]         AVHapticClient.mm:1138  -[AVHapticClient init]: Initial XPC call to server timed out. Invalidating connection to prevent hang
2026-11-22 21:59:45.360504Z audiomxd[101] Default [com.apple.coreaudio:default]    HapticServerManager.mm:41    operator()_block_invoke: === Initializing Haptic 2.0 Engine ===
2026-11-22 21:59:52.929250Z audiomxd[101] Default [com.apple.coreaudio:hsrv]         AVHapticServer.mm:2524  -[AVHapticServer doPrewarm:]: Starting prewarm on cold client ID: 0x100001E
2026-11-22 21:59:52.930000Z audiomxd[101] Default [com.apple.coreaudio:hsrv]            ClientEntry.mm:760   start: client ID: 0x100001E audio: 0 haptic: 1
2026-11-22 21:59:52.930100Z audiomxd[101] Default [com.apple.coreaudio:hsrv]            ClientEntry.mm:760   start: client ID: 0x100001F audio: 1 haptic: 0
2026-11-22 21:59:52.930200Z audiomxd[101] Default [com.apple.coreaudio:hmut]          ServerManager.mm:1374  setClientPlayerBehavior: Muting haptics on client ID 0x20000B9 due to player behaviors: MuteHaptics 0, AudioOnly 1
2026-11-22 21:59:45.020477Z audiomxd[101] Default [com.apple.coreaudio:va_stream]       StreamUtilities.cpp:402   Created virtual stream for [vzzz] (cbkg/imdf) port pact with DSP chain 'd42_actuator_general', HAL DSP override 'no', input metering 'no'
2026-11-22 21:59:45.020400Z audiomxd[101] Default [com.apple.coreaudio:va_dsp   ] RoutingSettings_Aspen.cpp:700  Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general
2026-11-22 21:59:45.021450Z audiomxd[101] Default [com.apple.coreaudio:va_aggdev] AggregateDevice_CommonBase.cpp:302   failed to fetch software volume command on AggregateDevice [ id: 76; uid: \"VAD [vzzz] AggDev 2\" ] with port [ type: pact; uid: Actuator; conn: 1 ]
2026-11-22 17:01:42.174143Z audiomxd[112] Default [com.apple.coreaudio:aid] IssueReporting.cpp:490   RTAID [ use_case=Generic report_type=RMS Generic Chain clientID=HAL node=Actuator-Output issue_detected_sample_time=469084 ] -- [ rms:[-120.000000], peaks:[-240.000000] ]
2026-11-22 17:01:42.274143Z audiomxd[112] Default [com.apple.coreaudio:aid] IssueReporting.cpp:490   RTAID [ use_case=Generic report_type=RMS Generic Chain clientID=VA node=PreDSP - { [ type: pact; uid: Actuator; conn: 1; rout: 1; quies: 0 ] } issue_detected_sample_time=0 ] -- [ rms:[-34.225143], peaks:[-17.548647] ]
2026-11-22 17:01:42.476457Z audiomxd[112] Default [com.apple.coreaudio:aid] IssueReporting.cpp:490   RTAID [ use_case=Generic report_type=RMS Generic Chain clientID=HAL node=Speaker-Output issue_detected_sample_time=300240 ] -- [ rms:[-44.404453], peaks:[-17.548647] ]
2026-11-22 21:59:44.000000Z kernel.development[0] Default ONEIOS HAPTIC: Actuator: user client 1 opened by audiomxd
2026-11-22 21:59:52.940000Z kernel.development[0] Default ONEIOS HAPTIC: Actuator: I/O started by audiomxd (start 1)
2026-11-22 22:00:06.390000Z kernel.development[0] Default ONEIOS HAPTIC: Actuator: I/O stopped: 2689 periods, 0 loud, 0 requests to the motor so far
2026-11-22 22:00:16.390000Z kernel.development[0] Default ONEIOS HAPTIC: Actuator: I/O stopped: 3000 periods, 12 loud, 3 requests to the motor so far
2026-11-22 22:00:03.628117Z audiomxd[101] Default [com.apple.coreaudio:sss]        SSSActionLists.cpp:131   mSSID 1106, mShouldPlayAudio 1, mShouldVibe 0, mAudioVolume 1, mVibeIntensity 1, mNeedsFinishCall 1
2026-11-22 22:00:04.628117Z audiomxd[101] Default [com.apple.coreaudio:sss]        SSSActionLists.cpp:131   mSSID 1107, mShouldPlayAudio 0, mShouldVibe 1, mAudioVolume 1, mVibeIntensity 1, mNeedsFinishCall 1
2026-11-22 22:00:05.000000Z SpringBoard[30] Default [com.apple.UIKit:EventDispatch] Sending UIEvent type: 0; subtype: 0; to windows: 1; dispatchLevel: 0
";

fn report() -> crate::health::Report {
    let mut counter = Counter::with(HAPTICS, 2).unwrap();
    count_text(LINES.as_bytes(), &mut counter).unwrap();
    counter.finish("test".to_string(), "test".to_string())
}

fn kind<'a>(report: &'a crate::health::Report, name: &str) -> &'a crate::health::Count {
    report.kinds.iter().find(|k| k.kind.name == name).unwrap()
}

#[test]
fn every_text_compiles_and_names_are_unique() {
    Counter::with(HAPTICS, 0).unwrap();
    let mut names: Vec<&str> = HAPTICS.iter().map(|k| k.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), HAPTICS.len());
}

#[test]
fn counts_each_step_of_the_path() {
    let r = report();
    // asked 2, played by Core Haptics 1, by the system sound engine 1; the prewarm line is none of them
    assert_eq!(kind(&r, "feedback").texts, vec![2, 1, 1]);
    assert_eq!(kind(&r, "feedback").loggers[0].0, "SpringBoard");
    // on (Spotlight), off in Settings (SpringBoard), unsupported (Notes)
    assert_eq!(kind(&r, "keyboard-mode").texts, vec![1, 1, 1]);
    // made, prewarm, start, events, timed out
    assert_eq!(kind(&r, "engine").texts, vec![1, 1, 1, 1, 1]);
    // up, prewarm, start (the client with haptic: 1 only), haptics muted, preset missing, gave up
    assert_eq!(kind(&r, "server").texts, vec![1, 1, 1, 1, 0, 0]);
    assert_eq!(kind(&r, "chain").texts, vec![1, 1, 1]);
    // the Actuator's output silent, its PreDSP node with a level; the Speaker's node is not counted
    assert_eq!(kind(&r, "level").texts, vec![1, 1]);
    // opened, I/O started, stopped with requests, stopped silent
    assert_eq!(kind(&r, "device").texts, vec![1, 1, 1, 1]);
    assert_eq!(kind(&r, "device").loggers[0].0, "kernel.development");
    assert_eq!(kind(&r, "system-sound").texts, vec![1, 1]);
    assert_eq!(r.entries, 31);
}

#[test]
fn the_report_has_one_line_per_kind_with_each_step() {
    let r = report();
    let text = r.text(TOP, false);
    assert_eq!(text.lines().count(), 1 + HAPTICS.len());
    let feedback = text.lines().nth(1).unwrap();
    assert!(feedback.starts_with("feedback"), "{feedback}");
    assert!(feedback.contains("asked 2") && feedback.contains("core haptics 1") && feedback.contains("system sound 1"), "{feedback}");
    let server = text.lines().find(|l| l.starts_with("server")).unwrap();
    assert!(server.contains("gave up 0"), "{server}");
}
