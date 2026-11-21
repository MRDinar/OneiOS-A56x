//! userland/config: the device, the daemon policy and the preference seeds (roottweak/config.py).
//!
//! The three files are built into the tool (the phone has no checkout); `--config DIR` reads another folder, and
//! the image build on a PC gives the checkout's own (scripts/image/build.sh), so an edit there is used at once.

use crate::plist::P;
use crate::Refused;
use anyhow::{bail, Result};
use oneios_common::pyjson::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

const DEVICE: &str = include_str!("../../../../../userland/config/device-iphone18,2.toml");
const DAEMONS: &str = include_str!("../../../../../userland/config/daemons.toml");
const PREFS: &str = include_str!("../../../../../userland/config/prefs.toml");
pub const DEVICE_NAME: &str = "device-iphone18,2.toml";
pub const SCALES: [i64; 2] = [2, 3];

fn text(name: &str, dir: Option<&Path>, built_in: &str) -> Result<String> {
    match dir {
        Some(d) => {
            let path = d.join(name);
            std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
        }
        None => Ok(built_in.to_string()),
    }
}

fn table(name: &str, dir: Option<&Path>, built_in: &str) -> Result<toml::Table> {
    let t = text(name, dir, built_in)?;
    t.parse::<toml::Table>().map_err(|e| anyhow::anyhow!("{name}: {}", e.message()))
}

fn to_value(v: &toml::Value) -> Result<Value> {
    Ok(match v {
        toml::Value::String(s) => Value::Str(s.clone()),
        toml::Value::Integer(i) => Value::Int(*i as i128),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Array(a) => Value::List(a.iter().map(to_value).collect::<Result<_>>()?),
        toml::Value::Table(t) => Value::Dict(t.iter().map(|(k, v)| Ok((k.clone(), to_value(v)?))).collect::<Result<_>>()?),
        toml::Value::Datetime(d) => bail!(Refused(format!("a TOML date ({d}) where the tweaks take none"))),
    })
}

pub fn to_plist(v: &toml::Value) -> Result<P> {
    Ok(match v {
        toml::Value::String(s) => P::Str(s.clone()),
        toml::Value::Integer(i) => P::Int(*i as i128),
        toml::Value::Float(f) => P::Real(*f),
        toml::Value::Boolean(b) => P::Bool(*b),
        toml::Value::Array(a) => P::Array(a.iter().map(to_plist).collect::<Result<_>>()?),
        toml::Value::Table(t) => P::Dict(t.iter().map(|(k, v)| Ok((k.clone(), to_plist(v)?))).collect::<Result<_>>()?),
        toml::Value::Datetime(d) => bail!(Refused(format!("prefs.toml: a date ({d}) in a preference: not written by the tweaks"))),
    })
}

/// The device profile. `scale` (2 or 3) replaces the profile's UI scale in every place that states it.
pub fn device(dir: Option<&Path>, scale: Option<i64>) -> Result<Value> {
    let name = DEVICE_NAME;
    let mut d = to_value(&toml::Value::Table(table(name, dir, DEVICE)?))?;
    for t in ["device", "display", "adt", "mobilegestalt"] {
        if d.get(t).is_none() {
            bail!(Refused(format!("{name}: no [{t}]")));
        }
    }
    let class = d.get("device").and_then(|v| v.get("device_class")).cloned().unwrap_or(Value::Null);
    if class.as_str() != Some("iPhone") {
        bail!(Refused(format!("{name}: OneiOS targets iOS on an iPhone, not {}", oneios_common::pyrepr::str_value(&class))));
    }
    if let Some(s) = scale {
        if !SCALES.contains(&s) {
            bail!(Refused(format!("the UI scale is 2 or 3, not {s}")));
        }
        d.get_mut("display").unwrap().set("scale", s);
    }
    let display = d.get("display").unwrap();
    let int = |k: &str| display.get(k).and_then(Value::as_int).ok_or_else(|| Refused(format!("{name}: [display] {k} is not an integer")));
    let (width, height, scale) = (int("width")?, int("height")?, int("scale")?);
    if scale == 0 {
        bail!(Refused(format!("{name}: [display] scale is 0")));
    }
    // One value everywhere: the points, /chosen display-scale (MobileGestalt's main-screen-scale), the artwork scale.
    d.get_mut("display").unwrap().set("points", vec![Value::Int(width.div_euclid(scale)), Value::Int(height.div_euclid(scale))]);
    let adt = d.get_mut("adt").unwrap();
    match adt.get_mut("chosen") {
        Some(c @ Value::Dict(_)) => c.set("display-scale", scale),
        _ => bail!(Refused(format!("{name}: no [adt.chosen]"))),
    }
    if adt.get("product").is_none() {
        adt.set("product", Value::dict());
    }
    adt.get_mut("product").unwrap().set("artwork-scale-factor", scale);
    let mg = d.get_mut("mobilegestalt").unwrap();
    if mg.get("expect").is_none() {
        mg.set("expect", Value::dict());
    }
    let expect = mg.get_mut("expect").unwrap();
    expect.set("main-screen-scale", scale.to_string());
    expect.set("main-screen-width", width.to_string());
    expect.set("main-screen-height", height.to_string());
    Ok(d)
}

pub struct Group {
    pub labels: Vec<String>,
    pub source: String,
    pub default: bool,
}

pub struct Daemons {
    pub keep: HashSet<String>,
    pub keep_list: Vec<String>,
    pub keep_reasons: BTreeMap<String, String>,
    pub idle: Vec<(String, String)>,
    pub replace: Vec<(String, String)>,
    pub replace_default: bool,
    /// in the file's order
    pub groups: Vec<(String, Group)>,
}

fn strs(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn pairs(v: Option<&toml::Value>) -> Vec<(String, String)> {
    v.and_then(|v| v.as_table())
        .map(|t| t.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| format!("{v:?}")))).collect())
        .unwrap_or_default()
}

impl Daemons {
    pub fn from_toml(raw: &toml::Table) -> Result<Daemons> {
        let keep = raw.get("keep").ok_or_else(|| Refused("daemons.toml: no [keep]".into()))?;
        let replace = raw.get("replace").ok_or_else(|| Refused("daemons.toml: no [replace]".into()))?;
        let mut groups = Vec::new();
        if let Some(t) = raw.get("group").and_then(|g| g.as_table()) {
            for (name, g) in t {
                groups.push((
                    name.clone(),
                    Group {
                        labels: strs(g.get("labels")),
                        source: g.get("source").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                        default: g.get("default").and_then(|d| d.as_bool()).unwrap_or(false),
                    },
                ));
            }
        }
        let keep_list = strs(keep.get("labels"));
        let d = Daemons {
            keep: keep_list.iter().cloned().collect(),
            keep_list,
            keep_reasons: pairs(keep.get("reasons")).into_iter().collect(),
            idle: pairs(raw.get("idle")),
            replace: pairs(replace.get("labels")),
            replace_default: replace.get("default").and_then(|d| d.as_bool()).unwrap_or(true),
            groups,
        };
        d.check()?;
        Ok(d)
    }

    /// The policy must not contradict itself: nothing kept or idled is also disabled.
    fn check(&self) -> Result<()> {
        let mut problems = Vec::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for (name, g) in &self.groups {
            if g.source.is_empty() {
                problems.push(format!("group {name} names no source"));
            }
            for label in &g.labels {
                if let Some(other) = seen.get(label) {
                    problems.push(format!("{label} is in groups {other} and {name}"));
                }
                seen.insert(label.clone(), name.clone());
            }
        }
        for (label, _) in &self.replace {
            if let Some(other) = seen.get(label) {
                problems.push(format!("{label} is replaced and in group {other}"));
            }
            seen.insert(label.clone(), "replace".into());
        }
        let idle: HashSet<&String> = self.idle.iter().map(|(l, _)| l).collect();
        for (label, place) in &seen {
            if self.keep.contains(label) {
                problems.push(format!("{label} is kept and in {place}"));
            }
            if idle.contains(label) {
                problems.push(format!("{label} is idled and in {place}"));
            }
        }
        if !problems.is_empty() {
            bail!(Refused(format!("daemons.toml: {}", problems.join("; "))));
        }
        Ok(())
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|(n, _)| n == name).map(|(_, g)| g)
    }

    /// [(label, why)] to disable, in Python's dict order: the default groups and `groups`, less `without` groups and
    /// `keep` labels.
    pub fn selected(&self, groups: &[String], without: &[String], keep: &[String]) -> Result<Vec<(String, String)>> {
        for name in groups.iter().chain(without) {
            if name != "replace" && self.group(name).is_none() {
                let mut names: Vec<&str> = self.groups.iter().map(|(n, _)| n.as_str()).collect();
                names.sort();
                bail!(Refused(format!("no daemon group {name}; there are: replace, {}", names.join(", "))));
            }
        }
        let mut out: Vec<(String, String)> = Vec::new();
        let put = |label: &str, why: &str, out: &mut Vec<(String, String)>| match out.iter_mut().find(|(l, _)| l == label) {
            Some(slot) => slot.1 = why.to_string(),
            None => out.push((label.to_string(), why.to_string())),
        };
        let has = |list: &[String], n: &str| list.iter().any(|x| x == n);
        if (self.replace_default || has(groups, "replace")) && !has(without, "replace") {
            for (label, _) in &self.replace {
                put(label, "replace", &mut out);
            }
        }
        for (name, g) in &self.groups {
            if (g.default || has(groups, name)) && !has(without, name) {
                for label in &g.labels {
                    put(label, name, &mut out);
                }
            }
        }
        out.retain(|(l, _)| !has(keep, l));
        Ok(out)
    }
}

pub fn daemons(dir: Option<&Path>) -> Result<Daemons> {
    Daemons::from_toml(&table("daemons.toml", dir, DAEMONS)?)
}

pub struct Pref {
    pub user: String,
    pub domain: String,
    pub group: String,
    pub source: String,
    /// the [pref.values] table as tomllib gives it
    pub values: toml::Value,
}

pub fn prefs(dir: Option<&Path>) -> Result<Vec<Pref>> {
    let t = table("prefs.toml", dir, PREFS)?;
    let mut out = Vec::new();
    for p in t.get("pref").and_then(|p| p.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
        let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let (user, domain, group, source) = (s("user"), s("domain"), s("group"), s("source"));
        if !["mobile", "root", "any"].contains(&user.as_str()) || domain.is_empty() || group.is_empty() || source.is_empty() {
            bail!(Refused(format!("prefs.toml: {domain}: needs user, domain, group and source")));
        }
        let values = p.get("values").cloned().unwrap_or(toml::Value::Table(toml::Table::new()));
        out.push(Pref { user, domain, group, source, values });
    }
    if t.get("pref").is_none() {
        bail!(Refused("prefs.toml: no [[pref]]".into()));
    }
    Ok(out)
}
