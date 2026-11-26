//! "What's new": the changelog sections newer than the last version the player has seen.
//! `native/CHANGELOG.md` is embedded in the program (so it shows after any kind of update, a manual download included) in PlayDate's own format:
//! `## v2026.11.1 - date`, then `### New` / `### Improvements` / `### Fixes` with `- ` bullets. The app remembers the version it last showed in `state.json` (`whats_new_seen`);
//! a first-ever start records the running version and shows nothing.

#[derive(Debug, Clone, PartialEq)]
pub struct Section { pub version: String, pub date: String, pub groups: Vec<(String, Vec<String>)> }

/// `v2026.11.0-beta.2` / `2026.11.0` -> `[2026, 11, 0]`: the numbers of the base version (a beta shares its base with the release, as in PlayDate).
pub fn version_tuple(v: &str) -> Vec<u64> {
    let base = v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next().unwrap_or("");
    base.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

/// Every `## vX.Y.Z` section of a changelog, in file order (newest first), with its `###` groups of bullets. `**bold**` and backticks are removed from the text.
pub fn parse(text: &str) -> Vec<Section> {
    let clean = |s: &str| s.replace("**", "").replace('`', "").trim().to_string();
    let mut out: Vec<Section> = vec![];
    for line in text.lines() {
        let t = line.trim_end();
        if let Some(h) = t.strip_prefix("## ") {
            let h = h.trim();
            let Some(rest) = h.strip_prefix('v').or_else(|| h.strip_prefix('V')) else { continue };
            let ver_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
            if ver_end == 0 { continue; }
            let date = rest[ver_end..].trim().trim_start_matches(['\u{2014}', '-', ' ']).trim().to_string();
            out.push(Section { version: format!("v{}", &rest[..ver_end]), date, groups: vec![] });
        } else if let Some(g) = t.strip_prefix("### ") {
            if let Some(s) = out.last_mut() { s.groups.push((clean(g), vec![])); }
        } else if let Some(item) = t.strip_prefix("- ") {
            if let Some((_, items)) = out.last_mut().and_then(|s| s.groups.last_mut()) { let c = clean(item); if !c.is_empty() { items.push(c); } }
        }
    }
    out.into_iter().map(|mut s| { s.groups.retain(|(_, i)| !i.is_empty()); s }).filter(|s| !s.groups.is_empty()).collect()
}

/// What to show now: the sections newer than `seen` up to the running version (the running version's own section is always included). `None` for `seen` = a first start: nothing.
pub fn due(sections: &[Section], seen: Option<&str>, running: &str) -> Vec<Section> {
    let Some(seen) = seen else { return vec![] };
    if seen.trim() == running.trim() { return vec![]; }
    let (seen_t, run_t) = (version_tuple(seen), version_tuple(running));
    sections.iter().filter(|s| { let t = version_tuple(&s.version); t <= run_t && (t == run_t || t > seen_t) }).cloned().collect()
}

/// The changelog compiled into the program.
pub const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

#[cfg(test)]
mod tests {
    use super::*;
    const LOG: &str = "# Changelog\n\n## v2026.11.2 - 2026-11-20\n### New\n- **Faster** Library\n- `Zest` opens\n\n### Fixes\n- one fix\n\n## v2026.11.1\n### Fixes\n- older fix\n\n## v2026.10.9\n### New\n- very old\n\n## v2026.11.3\n### New\n- from the future\n\n## not a version\n- stray\n";
    #[test] fn sections_groups_and_bullets_are_read() {
        let s = parse(LOG);
        assert_eq!(s.iter().map(|x| x.version.as_str()).collect::<Vec<_>>(), vec!["v2026.11.2", "v2026.11.1", "v2026.10.9", "v2026.11.3"]);
        assert_eq!(s[0].date, "2026-11-20");
        assert_eq!(s[0].groups, vec![("New".to_string(), vec!["Faster Library".to_string(), "Zest opens".to_string()]), ("Fixes".to_string(), vec!["one fix".to_string()])]);
        assert_eq!(s[1].date, "");
    }
    #[test] fn only_what_is_newer_than_the_last_seen_version_up_to_the_running_one_shows() {
        let s = parse(LOG);
        let v = |d: Vec<Section>| d.into_iter().map(|x| x.version).collect::<Vec<_>>();
        assert!(due(&s, None, "2026.11.2").is_empty(), "a first start shows nothing");
        assert!(due(&s, Some("2026.11.2"), "2026.11.2").is_empty(), "already seen");
        assert_eq!(v(due(&s, Some("2026.10.9"), "2026.11.2")), vec!["v2026.11.2", "v2026.11.1"], "newer than seen, not newer than running");
        assert_eq!(v(due(&s, Some("2026.11.1"), "2026.11.2-beta.1")), vec!["v2026.11.2"], "a beta shares its base version with the release");
        assert_eq!(v(due(&s, Some("2026.11.2-beta.1"), "2026.11.2")), vec!["v2026.11.2"], "the running version's own section is shown once even if an earlier beta of it was seen");
    }
    #[test] fn versions_compare_by_their_numbers() {
        assert_eq!(version_tuple("v2026.11.0-beta.2"), vec![2026, 11, 0]);
        assert!(version_tuple("2026.11.10") > version_tuple("2026.11.9"));
    }
    #[test] fn the_embedded_changelog_has_a_section_for_the_running_version() {
        let s = parse(CHANGELOG);
        assert!(!s.is_empty(), "native/CHANGELOG.md must hold at least one section");
        let running = version_tuple(crate::updater::VERSION);
        assert!(s.iter().any(|x| version_tuple(&x.version) == running), "add a `## v{}` section to native/CHANGELOG.md before releasing", crate::updater::VERSION);
    }
}
