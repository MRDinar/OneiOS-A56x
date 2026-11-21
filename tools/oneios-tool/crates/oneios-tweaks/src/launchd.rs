//! launchd's job set: which jobs the guest runs, and how a job that must not run is kept from running
//! (roottweak/launchd.py; its header has the whole story: the compiled cache on the OS volume, the
//! `disabled.plist` override on the Data volume, iOS 27's `DisableAllowed = false` in the jetsam properties).

use crate::plist::{py_eq, P};
use std::collections::BTreeSet;

pub const CACHE: &str = "System/Library/xpc/launchd.plist";
pub const DAEMONS_DIR: &str = "System/Library/LaunchDaemons";
/// in the Data tree (/private/var)
pub const OVERRIDES_DIR: &str = "db/com.apple.xpc.launchd";

/// (label, group, key) of every job in a compiled cache dictionary, the first of a label kept, in the cache's order.
pub fn jobs(cache: &P) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (group, entries) in cache.items() {
        let P::Dict(entries) = entries else { continue };
        for (key, job) in entries {
            if let (P::Dict(_), Some(P::Str(label))) = (job, job.get("Label")) {
                if seen.insert(label.clone()) {
                    out.push((label.clone(), group.clone(), key.clone()));
                }
            }
        }
    }
    out
}

pub fn labels(cache: &P) -> BTreeSet<String> {
    jobs(cache).into_iter().map(|(l, _, _)| l).collect()
}

/// The job of a (group, key).
pub fn job_mut<'a>(cache: &'a mut P, group: &str, key: &str) -> Option<&'a mut P> {
    cache.get_mut(group)?.get_mut(key)
}

/// Takes the jobs with these labels out of the cache. The labels that were there.
pub fn remove_jobs(cache: &mut P, labels: &[String]) -> Vec<String> {
    let have = jobs(cache);
    let mut gone = Vec::new();
    for label in labels {
        if let Some((_, group, key)) = have.iter().find(|(l, _, _)| l == label) {
            if let Some(g) = cache.get_mut(group) {
                g.remove(key);
            }
            gone.push(label.clone());
        }
    }
    gone
}

/// Adds or replaces a LaunchDaemon as vphone's `inject_daemon` does. True when the cache changed.
pub fn add_daemon(cache: &mut P, name: &str, job: &P) -> bool {
    if cache.get("LaunchDaemons").is_none() {
        cache.set("LaunchDaemons", P::dict());
    }
    let key = format!("/{DAEMONS_DIR}/{name}.plist");
    let daemons = cache.get_mut("LaunchDaemons").unwrap();
    if daemons.get(&key).is_some_and(|have| py_eq(have, job)) {
        return false;
    }
    daemons.set(&key, job.clone());
    true
}

/// The disabled.plist content: `existing` with `disable` set to true.
pub fn overrides(existing: &P, disable: &[String]) -> P {
    let mut out = existing.clone();
    for label in disable {
        out.set(label, P::Bool(true));
    }
    out
}

/// The `Override` tables of a jetsam properties plist: (version, kind) of each.
fn override_tables(jetsam: &P) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for (version, body) in jetsam.items() {
        if !body.is_dict() {
            continue;
        }
        for kind in ["Daemon", "XPCService"] {
            if let Some(k @ P::Dict(_)) = body.get(kind) {
                if let Some(P::Dict(_)) = k.get("Override") {
                    out.push((version.clone(), kind));
                }
            }
        }
    }
    out
}

fn table_mut<'a>(jetsam: &'a mut P, version: &str, kind: &str) -> Option<&'a mut P> {
    jetsam.get_mut(version)?.get_mut(kind)?.get_mut("Override")
}

/// Labels a jetsam properties plist marks `DisableAllowed = false`.
pub fn forced(jetsam: &P) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (version, kind) in override_tables(jetsam) {
        let table = jetsam.get(&version).and_then(|b| b.get(kind)).and_then(|k| k.get("Override")).unwrap();
        for (label, entry) in table.items() {
            if entry.is_dict() && matches!(entry.get("DisableAllowed"), Some(P::Bool(false))) {
                out.insert(label.clone());
            }
        }
    }
    out
}

/// Removes every `DisableAllowed = false`. The labels it was removed from, sorted.
pub fn allow_disable(jetsam: &mut P) -> Vec<String> {
    let mut out = BTreeSet::new();
    for (version, kind) in override_tables(jetsam) {
        let table = table_mut(jetsam, &version, kind).unwrap();
        for (label, entry) in table.items_mut().unwrap() {
            if entry.is_dict() && matches!(entry.get("DisableAllowed"), Some(P::Bool(false))) {
                entry.remove("DisableAllowed");
                out.insert(label.clone());
            }
        }
    }
    out.into_iter().collect()
}

/// Removes `RecoverOnCrash` from these labels' entries. The labels changed, sorted.
pub fn drop_recover_on_crash(jetsam: &mut P, labels: &[&str]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for (version, kind) in override_tables(jetsam) {
        let table = table_mut(jetsam, &version, kind).unwrap();
        for label in labels {
            if let Some(entry @ P::Dict(_)) = table.get_mut(label) {
                if entry.remove("RecoverOnCrash").is_some() {
                    out.insert(label.to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}
