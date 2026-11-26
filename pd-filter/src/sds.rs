//! Amazon's "SDS" game manifest (protobuf package `tv.twitch.fuel.sds`), port of `plugins/amazon_games/sds_manifest.py`: a 4-byte big-endian header
//! length, a header message (compression, signature), then the usually LZMA-compressed manifest body, signed with RSA-PKCS1v1.5/SHA-256 under Amazon's
//! public key. The wire format is tiny (flat messages), so a generic varint / length-delimited reader covers it.
use std::collections::HashMap;

/// Amazon's manifest-signing key (RSA-2048, e = 65537), the same constant as the Python port (modulus, big-endian hex).
const AMZ_RSA_N_HEX: &str = "e9f4913148b75694edbfd3f8f8abcc01c0083f84986d341f0759eceefc948ec8fc9eb17694636d413b462e7acd98cd84959d91ed5990b4d891b4f3314e821645a8e29f41f43b2cc6ac703c3fac3de8c8f8ab525e3910b325e54580b3810a508232607ee5b2415a725cabfb04330bbd806ca709e89834bb7b64c5692f518eb0d8e5fb22e4ece30c1a9323d495825457e0604f4bb9dd506499651405a1441a13c1372d0146538a439b154a019be284882b0a21c6b2e163b26ef270c53341525d862fc73e8a1ad3da7b4b3e96fd688244fe05d4dc8865e685deff43ff40dd0246eff26d758b886cc862a288a24e277a991da3c577816479e4af34d693ae724d016b";

#[derive(Debug)]
pub enum SdsError { Verification(String), Malformed(String) }
impl std::fmt::Display for SdsError { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { match self { SdsError::Verification(s) | SdsError::Malformed(s) => f.write_str(s) } } }

#[derive(Clone, Debug, PartialEq)]
pub struct Hash { pub algorithm: &'static str, pub value: String }
#[derive(Clone, Debug, PartialEq)]
pub struct Dir { pub path: String, pub mode: u64 }
#[derive(Clone, Debug, PartialEq)]
pub struct File { pub path: String, pub mode: u64, pub size: u64, pub hash: Hash, pub hidden: bool }
#[derive(Clone, Debug, PartialEq)]
pub struct Package { pub name: String, pub files: Vec<File>, pub dirs: Vec<Dir> }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Manifest { pub packages: Vec<Package> }

enum V { I(u64), B(Vec<u8>) }
type Fields = HashMap<u64, Vec<V>>;

fn varint(buf: &[u8], pos: &mut usize) -> Result<u64, SdsError> {
    let (mut result, mut shift) = (0u64, 0u32);
    loop {
        let b = *buf.get(*pos).ok_or_else(|| SdsError::Malformed("Manifest is truncated".into()))?;
        *pos += 1;
        if shift < 64 { result |= ((b & 0x7f) as u64) << shift; }
        if b & 0x80 == 0 { return Ok(result); }
        shift += 7;
        if shift > 70 { return Err(SdsError::Malformed("Bad varint in manifest".into())); }
    }
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], SdsError> {
    let end = pos.checked_add(n).filter(|e| *e <= buf.len()).ok_or_else(|| SdsError::Malformed("Manifest is truncated".into()))?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

fn decode(buf: &[u8]) -> Result<Fields, SdsError> {
    let (mut f, mut pos): (Fields, usize) = (HashMap::new(), 0);
    while pos < buf.len() {
        let tag = varint(buf, &mut pos)?;
        let v = match tag & 7 {
            0 => V::I(varint(buf, &mut pos)?),
            1 => V::B(take(buf, &mut pos, 8)?.to_vec()),
            2 => { let n = varint(buf, &mut pos)? as usize; V::B(take(buf, &mut pos, n)?.to_vec()) }
            5 => V::B(take(buf, &mut pos, 4)?.to_vec()),
            w => return Err(SdsError::Malformed(format!("Unsupported protobuf wire type {w}"))),
        };
        f.entry(tag >> 3).or_default().push(v);
    }
    Ok(f)
}

fn f_bytes(f: &Fields, n: u64) -> Vec<u8> { match f.get(&n).and_then(|v| v.first()) { Some(V::B(b)) => b.clone(), _ => Vec::new() } }
fn f_str(f: &Fields, n: u64) -> String { String::from_utf8_lossy(&f_bytes(f, n)).to_string() }
fn f_int(f: &Fields, n: u64) -> u64 { match f.get(&n).and_then(|v| v.first()) { Some(V::I(i)) => *i, _ => 0 } }
fn f_msg(f: &Fields, n: u64) -> Result<Fields, SdsError> { decode(&f_bytes(f, n)) }
fn f_msgs(f: &Fields, n: u64) -> Result<Vec<Fields>, SdsError> { f.get(&n).map(|v| v.iter().filter_map(|x| if let V::B(b) = x { Some(decode(b)) } else { None }).collect()).unwrap_or_else(|| Ok(Vec::new())) }

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn parse_body(raw: &[u8]) -> Result<Manifest, SdsError> {
    let top = decode(raw)?;
    let mut m = Manifest::default();
    for p in f_msgs(&top, 1)? {
        let files = f_msgs(&p, 2)?.iter().map(|f| {
            let h = f_msg(f, 5)?;
            Ok(File { path: f_str(f, 1), mode: f_int(f, 2), size: f_int(f, 3), hidden: f_int(f, 6) != 0,
                hash: Hash { algorithm: if f_int(&h, 1) == 1 { "shake128" } else { "sha256" }, value: hex(&f_bytes(&h, 2)) } })
        }).collect::<Result<Vec<_>, SdsError>>()?;
        let dirs = f_msgs(&p, 3)?.iter().map(|d| Dir { path: f_str(d, 1), mode: f_int(d, 2) }).collect();
        m.packages.push(Package { name: f_str(&p, 1), files, dirs });
    }
    Ok(m)
}

/// DER `RSAPublicKey` (what ring verifies against) for a big-endian modulus and exponent.
pub fn rsa_public_key_der(n: &[u8], e: &[u8]) -> Vec<u8> {
    fn len(l: usize) -> Vec<u8> { if l < 128 { vec![l as u8] } else if l < 256 { vec![0x81, l as u8] } else { vec![0x82, (l >> 8) as u8, l as u8] } }
    fn int(v: &[u8]) -> Vec<u8> {
        let v = { let s = v.iter().position(|b| *b != 0).unwrap_or(v.len().saturating_sub(1)); &v[s..] };
        let mut body = if v.first().map_or(false, |b| b & 0x80 != 0) { vec![0] } else { vec![] };
        body.extend_from_slice(v);
        let mut out = vec![0x02]; out.extend(len(body.len())); out.extend(body); out
    }
    let mut inner = int(n); inner.extend(int(e));
    let mut out = vec![0x30]; out.extend(len(inner.len())); out.extend(inner); out
}

pub fn amazon_key() -> Vec<u8> {
    let n: Vec<u8> = (0..AMZ_RSA_N_HEX.len() / 2).map(|i| u8::from_str_radix(&AMZ_RSA_N_HEX[2 * i..2 * i + 2], 16).unwrap()).collect();
    rsa_public_key_der(&n, &[0x01, 0x00, 0x01])
}

fn decompress(raw: &[u8]) -> Result<Vec<u8>, SdsError> {
    let mut out = Vec::new();
    let r = if raw.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) { lzma_rs::xz_decompress(&mut &raw[..], &mut out).map_err(|e| e.to_string()) } else { lzma_rs::lzma_decompress(&mut &raw[..], &mut out).map_err(|e| e.to_string()) };
    r.map_err(|e| SdsError::Malformed(format!("Could not unpack the manifest: {e}")))?;
    Ok(out)
}

/// Parse a v3 manifest response. With `verify` the signature is checked against Amazon's key.
pub fn parse_manifest(content: &[u8], verify: bool) -> Result<Manifest, SdsError> { parse_manifest_with(content, if verify { Some(amazon_key()) } else { None }) }

/// As `parse_manifest`, checking the signature against the given DER `RSAPublicKey` (tests use their own key; None = no check).
pub fn parse_manifest_with(content: &[u8], key: Option<Vec<u8>>) -> Result<Manifest, SdsError> {
    if content.len() < 4 { return Err(SdsError::Malformed("Manifest is truncated".into())); }
    let hsize = u32::from_be_bytes([content[0], content[1], content[2], content[3]]) as usize;
    let hdr = content.get(4..4usize.saturating_add(hsize)).ok_or_else(|| SdsError::Malformed("Manifest is truncated".into()))?;
    let header = decode(hdr)?;
    let compression = f_int(&f_msg(&header, 1)?, 1);
    let sigf = f_msg(&header, 3)?;
    let (sig_alg, sig) = (f_int(&sigf, 1), f_bytes(&sigf, 2));
    let mut raw = content[4 + hsize..].to_vec();
    match compression { 1 => raw = decompress(&raw)?, 0 => {}, c => return Err(SdsError::Verification(format!("Unknown compression algorithm {c}"))) }
    if let Some(der) = key {
        if sig_alg != 0 { return Err(SdsError::Verification(format!("Unknown signature algorithm {sig_alg}"))); }
        let pk = ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, der);
        pk.verify(&raw, &sig).map_err(|_| SdsError::Verification("Manifest signature verification failed".into()))?;
    }
    parse_body(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    fn unhex(s: &str) -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() }
    fn golden() -> Value { serde_json::from_str(include_str!("../tests/golden/sds.json")).unwrap() }
    fn same(m: &Manifest, want: &Value) {
        let want = want.as_array().unwrap();
        assert_eq!(m.packages.len(), want.len());
        for (p, w) in m.packages.iter().zip(want) {
            assert_eq!(p.name, w["name"].as_str().unwrap());
            assert_eq!(p.dirs.iter().map(|d| (d.path.clone(), d.mode)).collect::<Vec<_>>(), w["dirs"].as_array().unwrap().iter().map(|d| (d[0].as_str().unwrap().to_string(), d[1].as_u64().unwrap())).collect::<Vec<_>>());
            assert_eq!(p.files.len(), w["files"].as_array().unwrap().len());
            for (f, wf) in p.files.iter().zip(w["files"].as_array().unwrap()) {
                assert_eq!((f.path.as_str(), f.mode, f.size, f.hash.algorithm, f.hash.value.as_str(), f.hidden),
                    (wf["path"].as_str().unwrap(), wf["mode"].as_u64().unwrap(), wf["size"].as_u64().unwrap(), wf["hash_algo"].as_str().unwrap(), wf["hash"].as_str().unwrap(), wf["hidden"].as_bool().unwrap()));
            }
        }
    }
    #[test] fn plain_xz_and_lzma_alone_manifests_match_the_python_parser() {
        for c in golden()["cases"].as_array().unwrap() { same(&parse_manifest(&unhex(c["hex"].as_str().unwrap()), false).unwrap(), &c["expected"]); }
    }
    #[test] fn a_signature_made_with_the_matching_key_verifies_and_a_flipped_one_does_not() {
        let g = golden(); let s = &g["signed"];
        let key = unhex(s["pubkey_der_hex"].as_str().unwrap());
        same(&parse_manifest_with(&unhex(s["hex"].as_str().unwrap()), Some(key.clone())).unwrap(), &s["expected"]);
        assert!(matches!(parse_manifest_with(&unhex(s["bad_hex"].as_str().unwrap()), Some(key)), Err(SdsError::Verification(_))));
    }
    #[test] fn the_amazon_key_is_rejected_for_a_manifest_it_did_not_sign_and_the_der_is_well_formed() {
        let g = golden(); let s = &g["signed"];
        assert!(parse_manifest(&unhex(s["hex"].as_str().unwrap()), true).is_err());
        // our DER builder reproduces openssl's for the throwaway key
        let der = unhex(s["pubkey_der_hex"].as_str().unwrap());
        let n = &der[der.len() - 5 - 256..der.len() - 5];
        assert_eq!(rsa_public_key_der(n, &[1, 0, 1]), der);
        assert_eq!(g["amazon_e"].as_u64().unwrap(), 65537);
        assert_eq!(g["amazon_n_hex"].as_str().unwrap(), AMZ_RSA_N_HEX);
    }
    #[test] fn truncated_input_is_an_error_not_a_panic() {
        let g = golden(); let h = unhex(g["cases"][0]["hex"].as_str().unwrap());
        for cut in [0, 3, 5, h.len() / 2, h.len() - 1] { let _ = parse_manifest(&h[..cut], false); }
        assert!(parse_manifest(&h[..3], false).is_err());
    }
}
