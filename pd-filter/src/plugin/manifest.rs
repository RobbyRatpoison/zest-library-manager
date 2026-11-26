//! `plugin.json`: the usual id / name / version / platform plus a `zest` block (api version, how to run it, what it can do). PlayDate's Python plugins carry no
//! `zest` block and are not Zest plugins; PlayDate ignores the block in a plugin that has one.
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The protocol version this build speaks.
pub const CORE_API: u32 = 1;
pub const CAPABILITIES: &[&str] = &["auth", "sync", "rescrape", "description", "art", "store_page_metadata", "launch", "uninstall", "installed_scan", "junk_scan"];

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String, pub name: String, pub version: String, pub platform: String, pub api: u32,
    pub run: Vec<String>, pub capabilities: Vec<String>, pub store_url: Option<String>, pub store_label: Option<String>, pub source: Option<String>,
    /// The plugin's folder (the working directory it runs in).
    pub dir: PathBuf,
}

fn ident(s: &str) -> bool { !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') }

impl Manifest {
    pub fn parse(text: &str, dir: &Path) -> Result<Manifest, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("plugin.json is not valid JSON: {e}"))?;
        let st = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_str()).map(String::from);
        let id = st(&v, "id").ok_or("plugin.json has no id")?;
        let platform = st(&v, "platform").unwrap_or_else(|| id.clone());
        if !ident(&id) || !ident(&platform) { return Err("id and platform may only use letters, digits and underscores".into()); }
        let z = v.get("zest").ok_or("not a Zest plugin: plugin.json has no \"zest\" block")?;
        let api = z.get("api").and_then(|x| x.as_u64()).ok_or("zest.api is missing")? as u32;
        if api == 0 || api > CORE_API { return Err(format!("the plugin needs plugin protocol {api}, this Zest speaks {CORE_API}")); }
        let run: Vec<String> = z.get("run").and_then(|r| r.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if run.is_empty() || run.iter().any(|a| a.is_empty()) { return Err("zest.run must be a command, e.g. [\"python3\", \"plugin.py\"]".into()); }
        let caps: Vec<String> = z.get("capabilities").and_then(|r| r.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).filter(|c| CAPABILITIES.contains(c)).map(String::from).collect()).unwrap_or_default();
        Ok(Manifest { name: st(&v, "name").unwrap_or_else(|| id.clone()), version: st(&v, "version").unwrap_or_else(|| "0".into()), id, platform, api, run, capabilities: caps,
            store_url: st(z, "store_url").filter(|u| u.starts_with("https://")), store_label: st(z, "store_label"), source: st(&v, "source"), dir: dir.to_path_buf() })
    }
    pub fn load(dir: &Path) -> Result<Manifest, String> { Self::parse(&std::fs::read_to_string(dir.join("plugin.json")).map_err(|e| format!("no plugin.json: {e}"))?, dir) }
    pub fn can(&self, cap: &str) -> bool { self.capabilities.iter().any(|c| c == cap) }
}

/// Every Zest plugin under `dir` (one folder each), with the reason for each one that could not be read.
pub fn discover(dir: &Path) -> (Vec<Manifest>, Vec<(String, String)>) {
    let (mut ok, mut bad) = (vec![], vec![]);
    let Ok(rd) = std::fs::read_dir(dir) else { return (ok, bad) };
    let mut names: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("plugin.json").is_file()).collect();
    names.sort();
    for p in names {
        match Manifest::load(&p) {
            Ok(m) => ok.push(m),
            // a PlayDate-only plugin is simply not for Zest: not an error worth showing
            Err(e) if e.starts_with("not a Zest plugin") => {}
            Err(e) => bad.push((p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), e)),
        }
    }
    (ok, bad)
}

/// A PlayDate-only plugin (no `zest` block): Zest cannot run it, but the Plugins dialog lists it and says what will happen to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Legacy { pub id: String, pub name: String, pub version: String, pub platform: String }

pub fn discover_legacy(dir: &Path) -> Vec<Legacy> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<Legacy> = rd.flatten().map(|e| e.path()).filter_map(|p| {
        let j: Value = serde_json::from_str(&std::fs::read_to_string(p.join("plugin.json")).ok()?).ok()?;
        if j.get("zest").is_some() { return None; }
        let s = |k: &str| j.get(k).and_then(|x| x.as_str()).map(String::from);
        let id = s("id")?;
        Some(Legacy { name: s("name").unwrap_or_else(|| id.clone()), version: s("version").unwrap_or_default(), platform: s("platform").unwrap_or_else(|| id.clone()), id })
    }).collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    const OK: &str = r#"{"id":"toy","name":"Toy","version":"1","platform":"toy","zest":{"api":1,"run":["python3","plugin.py"],"capabilities":["sync","bogus","launch"],"store_url":"https://t/{slug}"}}"#;
    #[test] fn parses_and_filters_capabilities() {
        let m = Manifest::parse(OK, Path::new("/p/toy")).unwrap();
        assert_eq!((m.id.as_str(), m.api, m.run.len()), ("toy", 1, 2)); assert!(m.can("sync") && m.can("launch") && !m.can("bogus") && !m.can("auth"));
        assert_eq!(m.store_url.as_deref(), Some("https://t/{slug}"));
    }
    #[test] fn playdate_only_plugins_are_listed_not_loaded() {
        let d = std::env::temp_dir().join(format!("zest_legacy_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        for (n, j) in [("gog", r#"{"id":"gog","name":"GOG","version":"1","platform":"gog"}"#), ("toy", r#"{"id":"toy","name":"Toy","platform":"toy","zest":{"api":1,"run":["x"]}}"#)] {
            std::fs::create_dir_all(d.join(n)).unwrap(); std::fs::write(d.join(n).join("plugin.json"), j).unwrap();
        }
        let l = discover_legacy(&d); assert_eq!(l.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["gog"]);
        let (zest, bad) = discover(&d); assert_eq!((zest.len(), bad.len()), (1, 0), "the Zest plugin loads, the PlayDate one is not an error");
    }
    #[test] fn refuses_what_it_should() {
        let p = Path::new("/p");
        assert!(Manifest::parse(r#"{"id":"a","platform":"a"}"#, p).unwrap_err().starts_with("not a Zest plugin"));
        assert!(Manifest::parse(r#"{"id":"a b","zest":{"api":1,"run":["x"]}}"#, p).unwrap_err().contains("letters"));
        assert!(Manifest::parse(r#"{"id":"a","zest":{"api":2,"run":["x"]}}"#, p).unwrap_err().contains("protocol 2"));
        assert!(Manifest::parse(r#"{"id":"a","zest":{"api":1}}"#, p).unwrap_err().contains("zest.run"));
        assert!(Manifest::parse("{", p).is_err());
        assert_eq!(Manifest::parse(r#"{"id":"a","zest":{"api":1,"run":["x"],"store_url":"http://insecure/{slug}"}}"#, p).unwrap().store_url, None, "only https links");
    }
}
