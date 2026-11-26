//! GOG's content system v2 download (port of the install half of PlayDate's `plugins/gog/gog.py`): builds list -> secure-link CDN templates -> meta manifest -> one manifest per
//! depot -> every file's chunks, each chunk zlib-compressed and named by its compressed MD5. The network is a closure (`Fetch`: url -> bytes) so the logic is tested without GOG.
//! Differences from PlayDate: a manifest path that would leave the install folder (`..`, absolute) is refused instead of written; the `installDirectory` name from the manifest
//! is reduced to a plain folder name for the same reason; a chunk whose size does not match the manifest is reported rather than trusted.
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const CS_BASE: &str = "https://content-system.gog.com";
pub const DEPOT_BASE: &str = "https://gog-cdn-fastly.gog.com/content-system/v2/meta";

/// url -> body bytes (an Err carries the reason).
pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

/// `_zlib_decomp`: zlib, raw deflate, then gzip.
pub fn zlib_decomp(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).is_ok() { return Ok(out); }
    out.clear(); if flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).is_ok() { return Ok(out); }
    out.clear(); if flate2::read::GzDecoder::new(data).read_to_end(&mut out).is_ok() { return Ok(out); }
    Err("Could not decompress data (tried zlib/deflate/gzip)".into())
}

/// A manifest: plain JSON when the CDN sent it that way, else compressed JSON.
pub fn parse_manifest(bytes: &[u8]) -> Result<Value, String> {
    if let Ok(v) = serde_json::from_slice::<Value>(bytes) { return Ok(v); }
    serde_json::from_slice(&zlib_decomp(bytes)?).map_err(|e| format!("The manifest is not valid JSON: {e}"))
}

/// `_build_cdn_url`: the first secure-link template with `{placeholders}` filled and `rel` appended to its `path`.
pub fn build_cdn_url(cdn_urls: &Value, rel: &str) -> Result<String, String> {
    let src = cdn_urls.get(0).ok_or("GOG sent no download address")?;
    let mut url = src.get("url_format").and_then(|u| u.as_str()).ok_or("GOG's download address has no format")?.to_string();
    let params = src.get("parameters").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    let path = format!("{}/{}", params.get("path").and_then(|p| p.as_str()).unwrap_or("").trim_end_matches('/'), rel);
    for (k, v) in &params {
        let val = if k == "path" { path.clone() } else { match v { Value::String(s) => s.clone(), other => other.to_string() } };
        url = url.replace(&format!("{{{k}}}"), &val);
    }
    if !params.contains_key("path") { url = url.replace("{path}", &path); }
    Ok(url)
}
pub fn build_chunk_url(cdn_urls: &Value, compressed_md5: &str) -> Result<String, String> {
    if compressed_md5.len() < 4 || !compressed_md5.chars().all(|c| c.is_ascii_alphanumeric()) { return Err("A chunk has an invalid name".into()); }
    build_cdn_url(cdn_urls, &format!("{}/{}/{}", &compressed_md5[..2], &compressed_md5[2..4], compressed_md5))
}

/// Which OS's builds to look at, best first.
pub fn os_order(pref: &str, host_is_windows: bool) -> Vec<&'static str> {
    match pref { "linux" => vec!["linux"], "windows" => vec!["windows"], _ => if host_is_windows { vec!["windows", "linux"] } else { vec!["linux", "windows"] } }
}

/// A manifest-supplied relative path made safe: `\` becomes `/`, a leading `/` goes, and anything with `..` or an empty part is refused.
pub fn safe_rel(path: &str) -> Option<String> {
    let p = path.replace('\\', "/"); let p = p.trim_start_matches('/');
    let parts: Vec<&str> = p.split('/').collect();
    if p.is_empty() || parts.iter().any(|s| s.is_empty() || *s == ".." || *s == "." || s.contains(':')) { return None; }
    Some(p.to_string())
}

/// The folder name for a game: the manifest's `installDirectory`, else the game's name with punctuation dropped; always one plain component.
pub fn install_dir_name(meta: &Value, game_name: &str) -> String {
    let raw = meta.get("installDirectory").and_then(|d| d.as_str()).filter(|d| !d.is_empty()).map(String::from)
        .unwrap_or_else(|| game_name.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '_' || *c == '-').collect::<String>().trim().to_string());
    let last = raw.replace('\\', "/").rsplit('/').find(|s| !s.is_empty() && *s != "..").unwrap_or("").trim().to_string();
    if last.is_empty() || last == "." { "game".into() } else { last }
}

#[derive(Debug, PartialEq)]
pub struct FileJob { pub path: String, pub chunks: Vec<Value>, pub executable: bool }

/// The files to download: every depot with a manifest and an English (or any-language) tag, its items that have chunks. A depot whose manifest cannot be fetched is skipped, as in PlayDate.
pub fn collect_files(meta: &Value, fetch: Fetch, log: &mut dyn FnMut(&str)) -> (Vec<FileJob>, u64) {
    let (mut files, mut total) = (vec![], 0u64);
    for depot in meta.get("depots").and_then(|d| d.as_array()).into_iter().flatten() {
        let Some(id) = depot.get("manifest").and_then(|m| m.as_str()).filter(|m| m.len() >= 4) else { continue };
        let langs: Vec<&str> = depot.get("languages").and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect()).filter(|l: &Vec<&str>| !l.is_empty()).unwrap_or_else(|| vec!["*"]);
        if !langs.iter().any(|l| *l == "*" || l.starts_with("en")) { log(&format!("GOG install: depot {id} language mismatch: {langs:?}")); continue; }
        let url = format!("{DEPOT_BASE}/{}/{}/{id}", &id[..2], &id[2..4]);
        let dm = match fetch(&url).and_then(|b| parse_manifest(&b)) { Ok(d) => d, Err(e) => { log(&format!("GOG install: depot {id} manifest failed: {e}")); continue } };
        for item in dm.get("depot").and_then(|d| d.get("items")).and_then(|i| i.as_array()).into_iter().flatten() {
            let Some(chunks) = item.get("chunks").and_then(|c| c.as_array()).filter(|c| !c.is_empty()) else { continue };
            let Some(path) = item.get("path").and_then(|p| p.as_str()) else { continue };
            total += chunks.iter().map(|c| c.get("size").and_then(|s| s.as_u64()).unwrap_or(0)).sum::<u64>();
            let executable = item.get("flags").and_then(|f| f.as_array()).map_or(false, |f| f.iter().any(|x| x.as_str() == Some("executable")));
            files.push(FileJob { path: path.to_string(), chunks: chunks.clone(), executable });
        }
    }
    (files, total)
}

/// Downloads every file into `root`; `progress(done, total)` after each chunk. Err("Cancelled") on cancel. Nothing is cleaned up here: the caller removes `root` on any Err.
pub fn download_files(root: &Path, files: &[FileJob], total: u64, cdn_urls: &Value, fetch: Fetch, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write};
    let mut done = 0u64;
    for f in files {
        if cancelled() { return Err("Cancelled".into()); }
        let rel = safe_rel(&f.path).ok_or_else(|| format!("GOG listed a file outside the game folder ({:?}); skipped the install", f.path))?;
        let dest: PathBuf = root.join(&rel);
        if let Some(d) = dest.parent() { std::fs::create_dir_all(d).map_err(|e| format!("Could not make {}: {e}", d.display()))?; }
        let mut fh = std::fs::File::create(&dest).map_err(|e| format!("Could not write {rel}: {e}"))?;
        let mut pos = 0u64;
        for c in &f.chunks {
            if cancelled() { return Err("Cancelled".into()); }
            let md5 = c.get("compressedMd5").and_then(|m| m.as_str()).ok_or_else(|| format!("Download failed for {rel}: a chunk has no name"))?;
            let url = build_chunk_url(cdn_urls, md5)?;
            let raw = fetch(&url).and_then(|b| zlib_decomp(&b)).map_err(|e| format!("Download failed for {rel}: {e}"))?;
            if let Some(want) = c.get("size").and_then(|s| s.as_u64()) { if want != raw.len() as u64 { return Err(format!("Download failed for {rel}: a chunk came back {} bytes, expected {want}", raw.len())); } }
            // the chunk's offset when the manifest gives one, else straight after the last
            let off = c.get("offset").and_then(|o| o.as_u64()).unwrap_or(pos);
            fh.seek(SeekFrom::Start(off)).and_then(|_| fh.write_all(&raw)).map_err(|e| format!("Could not write {rel}: {e}"))?;
            pos = off + raw.len() as u64;
            done += raw.len() as u64; progress(done, total);
        }
        drop(fh);
        #[cfg(unix)] if f.executable { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    fn zlib(b: &[u8]) -> Vec<u8> { let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default()); e.write_all(b).unwrap(); e.finish().unwrap() }
    fn deflate(b: &[u8]) -> Vec<u8> { let mut e = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::default()); e.write_all(b).unwrap(); e.finish().unwrap() }
    fn gz(b: &[u8]) -> Vec<u8> { let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default()); e.write_all(b).unwrap(); e.finish().unwrap() }
    fn cdn() -> Value { json!([{"url_format": "https://cdn{path}?t={token}&e={expires_at}", "parameters": {"path": "/store/123/", "token": "abc", "expires_at": 99}}]) }

    #[test] fn compressed_data_is_read_in_all_three_formats_and_manifests_may_be_plain_json() {
        for c in [zlib(b"hello"), deflate(b"hello"), gz(b"hello")] { assert_eq!(zlib_decomp(&c).unwrap(), b"hello"); }
        assert!(zlib_decomp(b"not compressed at all").is_err());
        assert_eq!(parse_manifest(br#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(parse_manifest(&zlib(br#"{"a":2}"#)).unwrap(), json!({"a": 2}));
        assert!(parse_manifest(&zlib(b"plain text")).is_err());
    }
    #[test] fn cdn_addresses_fill_the_template_and_append_to_the_store_path() {
        assert_eq!(build_cdn_url(&cdn(), "meta/ab/cd/abcd").unwrap(), "https://cdn/store/123/meta/ab/cd/abcd?t=abc&e=99");
        assert_eq!(build_chunk_url(&cdn(), "ab12cd34").unwrap(), "https://cdn/store/123/ab/12/ab12cd34?t=abc&e=99");
        assert!(build_chunk_url(&cdn(), "../x").is_err() && build_cdn_url(&json!([]), "x").is_err());
    }
    #[test] fn paths_and_folder_names_from_a_manifest_cannot_leave_the_install_folder() {
        assert_eq!(safe_rel("\\bin\\Game.exe").as_deref(), Some("bin/Game.exe"));
        for bad in ["../x", "a/../../x", "", "/", "C:/x", "a//b"] { assert_eq!(safe_rel(bad), None, "{bad}"); }
        assert_eq!(install_dir_name(&json!({"installDirectory": "Cool Game"}), "n"), "Cool Game");
        assert_eq!(install_dir_name(&json!({"installDirectory": "../../etc"}), "n"), "etc");
        assert_eq!(install_dir_name(&json!({}), "Cool: Game!"), "Cool Game");
        assert_eq!(install_dir_name(&json!({"installDirectory": ".."}), "!!!"), "game");
        assert_eq!(os_order("auto", false), vec!["linux", "windows"]); assert_eq!(os_order("auto", true), vec!["windows", "linux"]); assert_eq!(os_order("linux", true), vec!["linux"]);
    }
    #[test] fn a_game_is_assembled_from_depots_by_language_and_chunks_are_written_at_their_offsets() {
        let (a, b) = ("aa11bb22", "cc33dd44");
        let depot = json!({"depot": {"items": [
            {"path": "bin\\run.sh", "flags": ["executable"], "chunks": [{"compressedMd5": a, "size": 5, "offset": 0}, {"compressedMd5": b, "size": 3, "offset": 5}]},
            {"path": "dir/empty", "chunks": []}, {"path": "data/x.dat", "chunks": [{"compressedMd5": b, "size": 3}]}]}});
        let meta = json!({"installDirectory": "G", "depots": [{"manifest": "11223344", "languages": ["*"]}, {"manifest": "55667788", "languages": ["de-DE"]}, {"languages": ["*"]}, {"manifest": "99aabbcc", "languages": ["en-US"]}]});
        let depot_bytes = zlib(depot.to_string().as_bytes());
        let urls = std::cell::RefCell::new(vec![]);
        let fetch = |u: &str| -> Result<Vec<u8>, String> {
            urls.borrow_mut().push(u.to_string());
            if u.ends_with("/11/22/11223344") { Ok(depot_bytes.clone()) }
            else if u.contains("/aa/11/") { Ok(zlib(b"hello")) }
            else if u.contains("/cc/33/") { Ok(zlib(b"abc")) }
            else { Err("404".into()) }   // the third depot (en-US) fails: skipped
        };
        let mut logs = vec![];
        let (files, total) = collect_files(&meta, &fetch, &mut |m| logs.push(m.to_string()));
        assert_eq!(files.len(), 2); assert_eq!(total, 5 + 3 + 3); assert!(files[0].executable && !files[1].executable);
        assert!(logs.iter().any(|l| l.contains("language mismatch")) && logs.iter().any(|l| l.contains("99aabbcc")));
        let root = std::env::temp_dir().join(format!("zest_goginst_{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        let mut seen = vec![];
        download_files(&root, &files, total, &cdn(), &fetch, &|| false, &mut |d, t| seen.push((d, t))).unwrap();
        assert_eq!(std::fs::read(root.join("bin/run.sh")).unwrap(), b"helloabc");
        assert_eq!(std::fs::read(root.join("data/x.dat")).unwrap(), b"abc");
        assert_eq!(seen.last(), Some(&(11, 11)));
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(root.join("bin/run.sh")).unwrap().permissions().mode() & 0o777, 0o755); }
        // cancel, a bad path, a wrong-sized chunk and a failed chunk each stop with a clear reason
        assert_eq!(download_files(&root, &files, total, &cdn(), &fetch, &|| true, &mut |_, _| {}).unwrap_err(), "Cancelled");
        let evil = vec![FileJob { path: "../evil".into(), chunks: vec![json!({"compressedMd5": a, "size": 5})], executable: false }];
        assert!(download_files(&root, &evil, 5, &cdn(), &fetch, &|| false, &mut |_, _| {}).unwrap_err().contains("outside the game folder") && !root.parent().unwrap().join("evil").exists());
        let short = vec![FileJob { path: "s".into(), chunks: vec![json!({"compressedMd5": a, "size": 9})], executable: false }];
        assert!(download_files(&root, &short, 9, &cdn(), &fetch, &|| false, &mut |_, _| {}).unwrap_err().contains("expected 9"));
        let gone = vec![FileJob { path: "g".into(), chunks: vec![json!({"compressedMd5": "ffee0011", "size": 1})], executable: false }];
        assert!(download_files(&root, &gone, 1, &cdn(), &fetch, &|| false, &mut |_, _| {}).unwrap_err().starts_with("Download failed for g: 404"));
    }
}
