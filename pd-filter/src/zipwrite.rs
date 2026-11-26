//! Writing a zip archive (the backup file): a stored or deflated entry per file, UTF-8 names, and ZIP64 records when the archive has more than 65,534 entries or runs past 4 GB, so a
//! backup of a big library (tens of thousands of covers) opens in Python's `zipfile` and in `archive::extract_zip`. One entry is under 4 GB.
use std::io::{self, Read, Write};
use std::path::Path;

struct Ent { name: String, crc: u32, size: u64, comp: u64, method: u16, offset: u64 }

pub struct ZipOut<W: Write> { w: W, pos: u64, ents: Vec<Ent> }

fn crc_of(path: &Path) -> io::Result<(u32, u64)> {
    let mut f = std::fs::File::open(path)?; let mut c = flate2::Crc::new(); let mut buf = vec![0u8; 256 * 1024]; let mut n = 0u64;
    loop { let k = f.read(&mut buf)?; if k == 0 { break; } c.update(&buf[..k]); n += k as u64; }
    Ok((c.sum(), n))
}

impl<W: Write> ZipOut<W> {
    pub fn new(w: W) -> Self { ZipOut { w, pos: 0, ents: vec![] } }
    fn put(&mut self, b: &[u8]) -> io::Result<()> { self.w.write_all(b)?; self.pos += b.len() as u64; Ok(()) }
    fn header(&mut self, name: &str, method: u16, crc: u32, comp: u64, size: u64) -> io::Result<()> {
        if comp >= 0xffff_ffff || size >= 0xffff_ffff { return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is too big for a backup entry (4 GB)"))); }
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend([0x50, 0x4b, 0x03, 0x04]); h.extend(20u16.to_le_bytes()); h.extend(0x0800u16.to_le_bytes()); h.extend(method.to_le_bytes());
        h.extend(0u16.to_le_bytes()); h.extend(0x0021u16.to_le_bytes());   // 1980-01-01 00:00
        h.extend(crc.to_le_bytes()); h.extend((comp as u32).to_le_bytes()); h.extend((size as u32).to_le_bytes());
        h.extend((name.len() as u16).to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(name.as_bytes());
        self.put(&h)
    }
    pub fn add_bytes(&mut self, name: &str, data: &[u8], deflate: bool) -> io::Result<()> {
        let mut c = flate2::Crc::new(); c.update(data);
        let packed: Vec<u8>; let (method, body): (u16, &[u8]) = if deflate { let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6)); e.write_all(data)?; packed = e.finish()?; (8, &packed) } else { (0, data) };
        let offset = self.pos;
        self.header(name, method, c.sum(), body.len() as u64, data.len() as u64)?;
        self.put(body)?;
        self.ents.push(Ent { name: name.into(), crc: c.sum(), size: data.len() as u64, comp: body.len() as u64, method, offset });
        Ok(())
    }
    /// A file as an entry: stored entries stream from disk (two reads: the checksum comes first), deflated ones are read whole.
    pub fn add_file(&mut self, name: &str, path: &Path, deflate: bool) -> io::Result<()> {
        if deflate { return self.add_bytes(name, &std::fs::read(path)?, true); }
        let (crc, size) = crc_of(path)?;
        let offset = self.pos;
        self.header(name, 0, crc, size, size)?;
        let mut f = std::fs::File::open(path)?; let mut buf = vec![0u8; 256 * 1024]; let mut n = 0u64;
        loop { let k = f.read(&mut buf)?; if k == 0 { break; } self.put(&buf[..k])?; n += k as u64; }
        if n != size { return Err(io::Error::new(io::ErrorKind::Other, format!("{name} changed while it was being backed up"))); }
        self.ents.push(Ent { name: name.into(), crc, size, comp: size, method: 0, offset });
        Ok(())
    }
    pub fn count(&self) -> usize { self.ents.len() }
    pub fn finish(mut self) -> io::Result<W> {
        let cd_start = self.pos;
        let ents = std::mem::take(&mut self.ents);
        for e in &ents {
            let big = e.offset >= 0xffff_ffff;
            let mut h = Vec::with_capacity(46 + e.name.len() + 12);
            h.extend([0x50, 0x4b, 0x01, 0x02]); h.extend(((3u16 << 8) | 20).to_le_bytes()); h.extend((if big { 45u16 } else { 20u16 }).to_le_bytes());
            h.extend(0x0800u16.to_le_bytes()); h.extend(e.method.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(0x0021u16.to_le_bytes());
            h.extend(e.crc.to_le_bytes()); h.extend((e.comp as u32).to_le_bytes()); h.extend((e.size as u32).to_le_bytes());
            h.extend((e.name.len() as u16).to_le_bytes()); h.extend((if big { 12u16 } else { 0u16 }).to_le_bytes()); h.extend(0u16.to_le_bytes());
            h.extend(0u16.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend((0o100644u32 << 16).to_le_bytes());
            h.extend((if big { 0xffff_ffffu32 } else { e.offset as u32 }).to_le_bytes()); h.extend(e.name.as_bytes());
            if big { h.extend(1u16.to_le_bytes()); h.extend(8u16.to_le_bytes()); h.extend(e.offset.to_le_bytes()); }
            self.put(&h)?;
        }
        let (cd_size, n) = (self.pos - cd_start, ents.len() as u64);
        if n >= 0xffff || cd_start >= 0xffff_ffff || cd_size >= 0xffff_ffff {
            let rec_at = self.pos;
            let mut z = Vec::with_capacity(56 + 20);
            z.extend([0x50, 0x4b, 0x06, 0x06]); z.extend(44u64.to_le_bytes()); z.extend(((3u16 << 8) | 45).to_le_bytes()); z.extend(45u16.to_le_bytes());
            z.extend(0u32.to_le_bytes()); z.extend(0u32.to_le_bytes()); z.extend(n.to_le_bytes()); z.extend(n.to_le_bytes()); z.extend(cd_size.to_le_bytes()); z.extend(cd_start.to_le_bytes());
            z.extend([0x50, 0x4b, 0x06, 0x07]); z.extend(0u32.to_le_bytes()); z.extend(rec_at.to_le_bytes()); z.extend(1u32.to_le_bytes());
            self.put(&z)?;
        }
        let mut e = Vec::with_capacity(22);
        e.extend([0x50, 0x4b, 0x05, 0x06]); e.extend(0u16.to_le_bytes()); e.extend(0u16.to_le_bytes());
        let n16 = if n >= 0xffff { 0xffffu16 } else { n as u16 };
        e.extend(n16.to_le_bytes()); e.extend(n16.to_le_bytes());
        e.extend((if cd_size >= 0xffff_ffff { 0xffff_ffffu32 } else { cd_size as u32 }).to_le_bytes()); e.extend((if cd_start >= 0xffff_ffff { 0xffff_ffffu32 } else { cd_start as u32 }).to_le_bytes());
        e.extend(0u16.to_le_bytes());
        self.put(&e)?;
        self.w.flush()?;
        Ok(self.w)
    }
}
