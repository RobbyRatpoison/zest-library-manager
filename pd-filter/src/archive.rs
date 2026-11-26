//! Unpacking the archives stores hand out (`.zip`, `.tar.gz` / `.tgz`, `.tar`) without a dependency beyond the inflate code already in the build. Paths are made safe (no absolute paths,
//! no `..`, Windows backslashes read as folders, as PlayDate's unzip did) and Unix permissions are kept, which is what makes a Linux build executable again. Encrypted
//! archives and other compressions are refused with a plain message; ZIP64 (more than 65,535 files, or over 4 GB) is read.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

/// A path inside an archive made safe to join onto `dest`: backslashes are folders, empty and `.` parts dropped; None when it climbs out (`..`) or is absolute.
pub fn safe_rel(name: &str) -> Option<PathBuf> {
    let n = name.replace('\\', "/");
    if n.starts_with('/') || (n.len() >= 2 && n.as_bytes()[1] == b':') { return None; }
    let mut out = PathBuf::new();
    for part in n.split('/') {
        match part { "" | "." => {}, ".." => return None, p => { if Path::new(p).components().any(|c| !matches!(c, Component::Normal(_))) { return None; } out.push(p); } }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn le16(b: &[u8], o: usize) -> u64 { u16::from_le_bytes([b[o], b[o + 1]]) as u64 }
fn le64(b: &[u8], o: usize) -> u64 { u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) }
fn le32(b: &[u8], o: usize) -> u64 { u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64 }

#[cfg(unix)] fn set_mode(p: &Path, mode: u32) { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode & 0o7777)); }
#[cfg(not(unix))] fn set_mode(_: &Path, _: u32) {}

struct Entry { name: String, method: u64, flags: u64, comp: u64, size: u64, offset: u64, mode: Option<u32>, is_dir: bool, is_link: bool }

fn zip_entries<R: Read + Seek>(f: &mut R) -> Result<Vec<Entry>, String> {
    let len = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let tail_len = len.min(65_557) as usize;
    f.seek(SeekFrom::Start(len - tail_len as u64)).map_err(|e| e.to_string())?;
    let mut tail = vec![0u8; tail_len]; f.read_exact(&mut tail).map_err(|e| e.to_string())?;
    let eocd = (0..tail_len.saturating_sub(21)).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or("That file is not a zip archive")?;
    let (mut n, mut cd_size, mut cd_off) = (le16(&tail, eocd + 10), le32(&tail, eocd + 12), le32(&tail, eocd + 16));
    let zip64 = n == 0xffff || cd_off == 0xffff_ffff || cd_size == 0xffff_ffff;
    if zip64 {
        // ZIP64: the real counts and offsets are in a record that the 20-byte locator just before the end record points at (a backup of a big library has more than 65,535 files)
        if eocd < 20 || tail[eocd - 20..eocd - 16] != [0x50, 0x4b, 0x06, 0x07] { return Err("The zip archive's ZIP64 directory is damaged".into()); }
        let rec_at = le64(&tail, eocd - 12);
        f.seek(SeekFrom::Start(rec_at)).map_err(|e| e.to_string())?;
        let mut rec = [0u8; 56]; f.read_exact(&mut rec).map_err(|e| e.to_string())?;
        if rec[..4] != [0x50, 0x4b, 0x06, 0x06] { return Err("The zip archive's ZIP64 directory is damaged".into()); }
        n = le64(&rec, 32); cd_size = le64(&rec, 40); cd_off = le64(&rec, 48);
    }
    // a zip appended to another file (a self-extracting installer): the offsets count from the zip's own start, which is where the directory ends minus its size
    // (not for ZIP64, whose end records sit between the directory and the end of the file)
    let eocd_at = len - tail_len as u64 + eocd as u64;
    let delta = if zip64 { 0 } else { eocd_at.checked_sub(cd_off + cd_size).ok_or("The zip archive's directory is damaged")? };
    let cd_off = cd_off + delta;
    f.seek(SeekFrom::Start(cd_off)).map_err(|e| e.to_string())?;
    let mut cd = vec![0u8; cd_size as usize]; f.read_exact(&mut cd).map_err(|e| e.to_string())?;
    let (mut out, mut p) = (vec![], 0usize);
    for _ in 0..n {
        if p + 46 > cd.len() || cd[p..p + 4] != [0x50, 0x4b, 0x01, 0x02] { return Err("The zip archive's directory is damaged".into()); }
        let (made_by, flags, method, mut comp, mut size, nl, el, cl, ext, mut offset) = (le16(&cd, p + 4), le16(&cd, p + 8), le16(&cd, p + 10), le32(&cd, p + 20), le32(&cd, p + 24), le16(&cd, p + 28) as usize, le16(&cd, p + 30) as usize, le16(&cd, p + 32) as usize, le32(&cd, p + 38), le32(&cd, p + 42));
        if p + 46 + nl + el + cl > cd.len() { return Err("The zip archive's directory is damaged".into()); }
        // a size or offset too big for 32 bits is 0xffffffff here and the real value is in the ZIP64 extra field, in this order: size, compressed size, offset
        if size == 0xffff_ffff || comp == 0xffff_ffff || offset == 0xffff_ffff {
            let ex = &cd[p + 46 + nl..p + 46 + nl + el];
            let mut q = 0usize;
            while q + 4 <= ex.len() {
                let (id, l) = (le16(ex, q), le16(ex, q + 2) as usize);
                if id == 1 && q + 4 + l <= ex.len() {
                    let mut o = q + 4;
                    for v in [&mut size, &mut comp, &mut offset] { if *v == 0xffff_ffff && o + 8 <= q + 4 + l { *v = le64(ex, o); o += 8; } }
                    break;
                }
                q += 4 + l;
            }
        }
        let raw = &cd[p + 46..p + 46 + nl];
        // bit 11 = UTF-8 names; otherwise the old code page, of which only ASCII is certain: anything else is read leniently
        let name = String::from_utf8_lossy(raw).to_string();
        let unix = (made_by >> 8) == 3;
        let mode = unix.then(|| (ext >> 16) as u32).filter(|m| *m != 0);
        let kind = mode.map(|m| m & 0o170000);
        out.push(Entry { is_dir: name.ends_with('/') || name.ends_with('\\') || kind == Some(0o040000), is_link: kind == Some(0o120000), name, method, flags, comp, size, offset: offset + delta, mode });
        p += 46 + nl + el + cl;
    }
    Ok(out)
}

/// The names of the entries in a zip file (directories included as they are written).
pub fn zip_names(archive: &Path) -> Result<Vec<String>, String> {
    let mut f = std::io::BufReader::new(std::fs::File::open(archive).map_err(|e| format!("Could not open the file: {e}"))?);
    Ok(zip_entries(&mut f)?.into_iter().map(|e| e.name).collect())
}

/// A zip opened once for many `extract` calls: the directory is read a single time (a restore of thousands of covers used to re-read it for every file).
pub struct ZipReader { f: std::io::BufReader<std::fs::File>, by_name: std::collections::HashMap<String, Entry> }
impl ZipReader {
    pub fn open(archive: &Path) -> Result<ZipReader, String> {
        let mut f = std::io::BufReader::with_capacity(256 * 1024, std::fs::File::open(archive).map_err(|e| format!("Could not open the file: {e}"))?);
        let by_name = zip_entries(&mut f)?.into_iter().map(|e| (e.name.clone(), e)).collect();
        Ok(ZipReader { f, by_name })
    }
    /// Writes one entry to `dest` (its folder is made); the caller decides the destination, so it must have checked the entry's name. Returns the bytes written.
    pub fn extract(&mut self, name: &str, dest: &Path) -> Result<u64, String> {
        let e = self.by_name.get(name).ok_or_else(|| format!("{name} is not in the archive"))?;
        if e.flags & 1 != 0 { return Err("This zip archive is password protected".into()); }
        let (offset, comp, method) = (e.offset, e.comp, e.method);
        let f = &mut self.f;
        f.seek(SeekFrom::Start(offset)).map_err(|x| x.to_string())?;
        let mut lh = [0u8; 30]; f.read_exact(&mut lh).map_err(|x| x.to_string())?;
        if lh[..4] != [0x50, 0x4b, 0x03, 0x04] { return Err("The zip archive is damaged".into()); }
        f.seek(SeekFrom::Current((le16(&lh, 26) + le16(&lh, 28)) as i64)).map_err(|x| x.to_string())?;
        let data = (&mut *f).take(comp);
        let mut body: Box<dyn Read> = match method { 0 => Box::new(data), 8 => Box::new(flate2::read::DeflateDecoder::new(data)), m => return Err(format!("This zip archive uses a compression Zest cannot read (method {m})")) };
        if let Some(d) = dest.parent() { std::fs::create_dir_all(d).map_err(|x| x.to_string())?; }
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, std::fs::File::create(dest).map_err(|x| format!("Could not write {}: {x}", dest.display()))?);
        let n = std::io::copy(&mut body, &mut out).map_err(|x| format!("Could not unpack {name}: {x}"))?;
        std::io::Write::flush(&mut out).map_err(|x| format!("Could not unpack {name}: {x}"))?;
        Ok(n)
    }
}

/// Writes one entry of a zip file to `dest`. For many entries use `ZipReader`, which reads the directory only once.
pub fn extract_entry(archive: &Path, name: &str, dest: &Path) -> Result<u64, String> { ZipReader::open(archive)?.extract(name, dest) }

/// Is this file a zip, or a program with a zip appended (MojoSetup and similar self-extracting installers)? Judged by its directory, not its name.
pub fn is_zip_payload(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    zip_entries(&mut std::io::BufReader::new(f)).map_or(false, |e| !e.is_empty())
}

/// Extracts a zip archive into `dest`. `progress(done, total)` counts unpacked bytes; `cancel` is polled between files. Returns how many files were written.
pub fn extract_zip(archive: &Path, dest: &Path, progress: &mut dyn FnMut(u64, u64), cancel: &dyn Fn() -> bool) -> Result<usize, String> {
    let mut f = std::io::BufReader::new(std::fs::File::open(archive).map_err(|e| format!("Could not open the archive: {e}"))?);
    let entries = zip_entries(&mut f)?;
    let total: u64 = entries.iter().filter(|e| !e.is_dir && !e.is_link && safe_rel(&e.name).is_some()).map(|e| e.size).sum();
    let (mut done, mut files) = (0u64, 0usize);
    std::fs::create_dir_all(dest).map_err(|e| format!("Could not make {}: {e}", dest.display()))?;
    for e in &entries {
        if cancel() { return Err("Cancelled".into()); }
        let Some(rel) = safe_rel(&e.name) else { continue };   // an entry outside the folder is skipped, not followed
        let target = dest.join(&rel);
        if e.is_dir { std::fs::create_dir_all(&target).map_err(|x| x.to_string())?; continue; }
        if e.flags & 1 != 0 { return Err("This zip archive is password protected".into()); }
        if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).map_err(|x| x.to_string())?; }
        f.seek(SeekFrom::Start(e.offset)).map_err(|x| x.to_string())?;
        let mut lh = [0u8; 30]; f.read_exact(&mut lh).map_err(|x| x.to_string())?;
        if lh[..4] != [0x50, 0x4b, 0x03, 0x04] { return Err("The zip archive is damaged".into()); }
        f.seek(SeekFrom::Current((le16(&lh, 26) + le16(&lh, 28)) as i64)).map_err(|x| x.to_string())?;
        let data = (&mut f).take(e.comp);
        let mut body: Box<dyn Read> = match e.method { 0 => Box::new(data), 8 => Box::new(flate2::read::DeflateDecoder::new(data)), m => return Err(format!("This zip archive uses a compression Zest cannot unpack (method {m})")) };
        if e.is_link {
            let mut t = String::new(); body.read_to_string(&mut t).map_err(|x| x.to_string())?;
            #[cfg(unix)] if safe_rel(&t.replace("../", "")).is_some() && !t.starts_with('/') && !t.split('/').any(|p| p == "..") { let _ = std::fs::remove_file(&target); let _ = std::os::unix::fs::symlink(&t, &target); }
            continue;
        }
        let mut out = std::fs::File::create(&target).map_err(|x| format!("Could not write {}: {x}", target.display()))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = body.read(&mut buf).map_err(|x| format!("Could not unpack {}: {x}", e.name))?;
            if n == 0 { break; }
            out.write_all(&buf[..n]).map_err(|x| format!("Could not write {}: {x}", target.display()))?;
            done += n as u64;
            progress(done, total);
        }
        drop(out);
        if let Some(m) = e.mode { set_mode(&target, m); }
        files += 1;
    }
    Ok(files)
}

struct Counting<R> { inner: R, read: std::sync::Arc<std::sync::atomic::AtomicU64> }
impl<R: Read> Read for Counting<R> { fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> { let n = self.inner.read(b)?; self.read.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed); Ok(n) } }

fn octal(b: &[u8]) -> u64 { b.iter().take_while(|c| **c != 0 && **c != b' ').fold(0, |a, c| a * 8 + (*c as char).to_digit(8).unwrap_or(0) as u64) }
fn cstr(b: &[u8]) -> String { String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).to_string() }

/// Extracts a tar archive (optionally gzip-compressed) into `dest`. Progress counts bytes read from the file against its size.
pub fn extract_tar(archive: &Path, dest: &Path, gzip: bool, progress: &mut dyn FnMut(u64, u64), cancel: &dyn Fn() -> bool) -> Result<usize, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("Could not open the archive: {e}"))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = Counting { inner: std::io::BufReader::new(file), read: read.clone() };
    let mut src: Box<dyn Read> = if gzip { Box::new(flate2::read::MultiGzDecoder::new(counted)) } else { Box::new(counted) };
    std::fs::create_dir_all(dest).map_err(|e| format!("Could not make {}: {e}", dest.display()))?;
    let (mut files, mut long_name): (usize, Option<String>) = (0, None);
    let mut hdr = [0u8; 512];
    loop {
        if cancel() { return Err("Cancelled".into()); }
        if src.read_exact(&mut hdr).is_err() { break; }
        if hdr.iter().all(|b| *b == 0) { break; }
        let (kind, size, mode) = (hdr[156], octal(&hdr[124..136]), octal(&hdr[100..108]) as u32);
        let mut name = cstr(&hdr[..100]);
        if &hdr[257..262] == b"ustar" { let prefix = cstr(&hdr[345..500]); if !prefix.is_empty() { name = format!("{prefix}/{name}"); } }
        let padded = size.div_ceil(512) * 512;
        let mut data = Vec::new();
        let keep = matches!(kind, b'L' | b'x' | b'2' | b'1') ;
        if keep { (&mut src).take(size).read_to_end(&mut data).map_err(|e| e.to_string())?; std::io::copy(&mut (&mut src).take(padded - size), &mut std::io::sink()).map_err(|e| e.to_string())?; }
        match kind {
            b'L' => { long_name = Some(cstr(&data)); continue; }
            b'x' => {   // pax: "len key=value\n" records; only the path matters
                let t = String::from_utf8_lossy(&data).to_string();
                for rec in t.split('\n') { if let Some((_, kv)) = rec.split_once(' ') { if let Some(p) = kv.strip_prefix("path=") { long_name = Some(p.to_string()); } } }
                continue;
            }
            _ => {}
        }
        let name = long_name.take().unwrap_or(name);
        let target_rel = safe_rel(&name);
        match (kind, target_rel) {
            (b'5', Some(rel)) => { std::fs::create_dir_all(dest.join(&rel)).map_err(|e| e.to_string())?; }
            (b'0' | 0, Some(rel)) => {
                let target = dest.join(&rel);
                if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                let mut out = std::fs::File::create(&target).map_err(|e| format!("Could not write {}: {e}", target.display()))?;
                std::io::copy(&mut (&mut src).take(size), &mut out).map_err(|e| format!("Could not unpack {name}: {e}"))?;
                drop(out);
                std::io::copy(&mut (&mut src).take(padded - size), &mut std::io::sink()).map_err(|e| e.to_string())?;
                set_mode(&target, mode);
                files += 1;
            }
            (b'2', Some(rel)) => {
                let link = cstr(&hdr[157..257]);
                #[cfg(unix)] if !link.starts_with('/') && !link.split('/').any(|p| p == "..") { let t = dest.join(&rel); if let Some(parent) = t.parent() { let _ = std::fs::create_dir_all(parent); } let _ = std::fs::remove_file(&t); let _ = std::os::unix::fs::symlink(&link, &t); }
                let _ = link;
            }
            _ => { if !keep && size > 0 { std::io::copy(&mut (&mut src).take(padded), &mut std::io::sink()).map_err(|e| e.to_string())?; } }
        }
        progress(read.load(std::sync::atomic::Ordering::Relaxed).min(total), total);   // the file's bytes read so far, against its size
    }
    Ok(files)
}

/// A program on the system that can unpack the other formats (`bsdtar` reads rar, 7z, xz, bz2 and zstd; `7z` and `unrar` are the fallbacks).
fn find_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).chain(std::env::split_paths(&path).map(|d| d.join(format!("{name}.exe")))).find(|p| p.is_file())
}
fn external_formats(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    [".rar", ".7z", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".tbz", ".tar.zst", ".tzst"].iter().find(|e| n.ends_with(*e)).map(|_| "external")
}
fn tool_for(name: &str) -> Option<(&'static str, PathBuf)> {
    let n = name.to_lowercase();
    let mut order: Vec<&'static str> = vec!["bsdtar"];
    if n.ends_with(".rar") { order.extend(["unrar", "7z", "7za"]); } else if n.ends_with(".7z") { order.extend(["7z", "7za"]); }
    order.into_iter().find_map(|t| find_tool(t).map(|p| (t, p)))
}

/// What an archive file name says it is: a zip, a (gzipped) tar, an `"external"` one a program on this system can unpack, or none.
pub fn kind_of(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.ends_with(".zip") { Some("zip") } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") { Some("tar.gz") } else if n.ends_with(".tar") { Some("tar") }
    else if external_formats(name).is_some() && tool_for(name).is_some() { Some("external") } else { None }
}

fn extract_external(archive: &Path, dest: &Path, cancel: &dyn Fn() -> bool) -> Result<usize, String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (tool, path) = tool_for(&name).ok_or_else(|| format!("Unpacking {name} needs bsdtar, 7z or unrar installed"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("Could not make {}: {e}", dest.display()))?;
    let mut cmd = std::process::Command::new(path);
    { use crate::proc::Hide; cmd.hidden(); }
    match tool {
        "bsdtar" => { cmd.arg("-xf").arg(archive).arg("-C").arg(dest); }
        "unrar" => { cmd.args(["x", "-o+", "-y"]).arg(archive).arg(format!("{}/", dest.display())); }
        _ => { cmd.args(["x", "-y"]).arg(archive).arg(format!("-o{}", dest.display())); }
    }
    let mut child = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("Could not start {tool}: {e}"))?;
    loop {
        if cancel() { let _ = child.kill(); let _ = child.wait(); return Err("Cancelled".into()); }
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(st) => {
                if st.success() { break; }
                let mut err = String::new(); if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut err); }
                return Err(format!("{tool} could not unpack {name}{}", if err.trim().is_empty() { String::new() } else { format!(": {}", err.trim().chars().take(200).collect::<String>()) }));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    fn count(p: &Path) -> usize { std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| if e.path().is_dir() { count(&e.path()) } else { 1 }).sum()).unwrap_or(0) }
    Ok(count(dest))
}

/// Unpacks `archive` into `dest` by its name; Err for a kind Zest cannot unpack.
pub fn extract(archive: &Path, dest: &Path, progress: &mut dyn FnMut(u64, u64), cancel: &dyn Fn() -> bool) -> Result<usize, String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match kind_of(&name) {
        Some("zip") => extract_zip(archive, dest, progress, cancel),
        Some("tar.gz") => extract_tar(archive, dest, true, progress, cancel),
        Some("tar") => extract_tar(archive, dest, false, progress, cancel),
        Some(_) => extract_external(archive, dest, cancel),
        None if is_zip_payload(archive) => extract_zip(archive, dest, progress, cancel),
        None if external_formats(&name).is_some() => Err(format!("Unpacking {name} needs bsdtar, 7z or unrar installed")),
        None => Err(format!("Zest cannot unpack {name} yet")),
    }
}
