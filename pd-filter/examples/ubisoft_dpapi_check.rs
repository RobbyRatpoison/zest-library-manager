//! Dev check, run on Windows: builds a stand-in current-client cache (an encrypted configurations.vdat plus its key file protected with this Windows user's real DPAPI) under a temporary
//! LOCALAPPDATA, then asks Zest's Ubisoft source for the game list. Proves the whole Windows path except what the real client writes.
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
fn varint(mut v: u64) -> Vec<u8> { let mut o = vec![]; loop { let b = (v & 0x7f) as u8; v >>= 7; if v == 0 { o.push(b); return o; } o.push(b | 0x80); } }
fn field(n: u64, data: &[u8]) -> Vec<u8> { let mut o = varint(n << 3 | 2); o.extend(varint(data.len() as u64)); o.extend_from_slice(data); o }
fn main() {
    let plain = include_bytes!("../tests/data/ubisoft_configurations").to_vec();
    let key: Vec<u8> = (1u8..=32).collect(); let nonce = [7u8; 12]; let key_id = "51bd3a919cf546c1b3c40b2013a4c83f";
    let mut sealed = plain.clone();
    let tag = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).unwrap()).seal_in_place_separate_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut sealed).unwrap();
    let mut enc = varint(1 << 3); enc.extend(varint(1)); enc.extend(field(2, key_id.as_bytes())); enc.extend(field(3, &nonce)); enc.extend(field(4, tag.as_ref()));
    let mut header = field(2, &enc); header.splice(0..0, field(1, &[]));
    let mut envelope = field(1, &header); envelope.extend(field(2, &sealed));
    let vdat = field(1, &envelope);
    let root = std::env::temp_dir().join(format!("zest_ubi_check_{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
    let base = root.join("Ubisoft Game Launcher");
    std::fs::create_dir_all(base.join("cache/configuration")).unwrap(); std::fs::create_dir_all(base.join("store")).unwrap();
    std::fs::write(base.join("cache/configuration/configurations.vdat"), &vdat).unwrap();
    let blob = pd_filter::windpapi::protect(&key, None).expect("DPAPI protect");
    std::fs::write(base.join(format!("store/{key_id}.bin")), &blob).unwrap();
    std::env::set_var("LOCALAPPDATA", &root);
    let cfg = root.join("cfg"); std::fs::create_dir_all(&cfg).unwrap();
    let s = pd_filter::sources::ubisoft::UbisoftSource::new(cfg.clone(), cfg.join("games_1.db"));
    let got = s.cache_bytes();
    let want = pd_filter::sources::ubisoft::parse_configurations(&plain);
    let have = got.as_ref().map(|g| pd_filter::sources::ubisoft::parse_configurations(g)).unwrap_or_default();
    println!("cache opened: {}; games in it: {} (expected {}); same names: {}", got.is_some(), have.len(), want.len(), have.len() == want.len() && have.iter().zip(&want).all(|(a, b)| a.name == b.name));
    std::fs::write(base.join(format!("store/{key_id}.bin")), b"not the key").unwrap();
    println!("with a wrong key file: opened = {}", s.cache_bytes().is_some());
    let _ = std::fs::remove_dir_all(&root);
}
