//! Opens a blob that Wine's own `CryptProtectData` wrote (Wine's DPAPI is a plain scheme with no secret kept apart from the program: the key comes from the Wine user's name, a fixed string and
//! the blob's own salt). Ubisoft Connect running under Wine keeps the key of its game-list cache this way, in `store/<id>.bin`. This only reads what a program of that same Wine user could
//! read by asking Wine; it does not touch Windows' real DPAPI (there, the system hands the key to the user's own programs).
//! The layout is the one Wine's `protectdata.c` documents; the key is derived as CryptoAPI's `CryptDeriveKey` does for a hash shorter than the key (two padded hashes joined).
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use ring::digest::{digest, SHA1_FOR_LEGACY_USE_ONLY};

const SECRET: &[u8] = b"I'm hunting wabbits";
const CALG_3DES: u32 = 0x6603;
const CALG_SHA1: u32 = 0x8004;

struct Cursor<'a> { b: &'a [u8], p: usize }
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> { let s = self.b.get(self.p..self.p.checked_add(n)?)?; self.p += n; Some(s) }
    fn u32(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
}

fn derive_3des_key(username: &[u8], salt: &[u8]) -> [u8; 24] {
    let mut input = username.to_vec(); input.extend_from_slice(SECRET); input.extend_from_slice(salt);
    let h = digest(&SHA1_FOR_LEGACY_USE_ONLY, &input);
    let pad = |fill: u8| { let mut block = vec![fill; 64]; for (i, b) in h.as_ref().iter().enumerate() { block[i] ^= *b; } digest(&SHA1_FOR_LEGACY_USE_ONLY, &block) };
    let (a, b) = (pad(0x36), pad(0x5c));
    let mut key = [0u8; 24];
    key[..20].copy_from_slice(a.as_ref()); key[20..].copy_from_slice(&b.as_ref()[..4]);
    key
}

/// The data inside `blob`, when it was protected by a Wine user called `username` (Wine hashes the name with its end mark). None for another user's blob or anything unreadable.
pub fn unprotect(blob: &[u8], username: &str) -> Option<Vec<u8>> {
    let mut c = Cursor { b: blob, p: 0 };
    c.u32()?; c.take(16)?; c.u32()?; c.take(16)?; c.u32()?;           // counts and the two fixed 16-byte records, then an end mark
    let descr = c.u32()? as usize; c.take(descr)?;                    // the data description
    if c.u32()? != CALG_3DES { return None; }
    c.u32()?; let data = c.u32()? as usize; c.take(data)?; c.u32()?;
    if c.u32()? != CALG_SHA1 { return None; }
    c.u32()?; let salt_len = c.u32()? as usize; let salt = c.take(salt_len)?;
    let cipher_len = c.u32()? as usize; let cipher = c.take(cipher_len)?;
    let mut user = username.as_bytes().to_vec(); user.push(0);
    let key = derive_3des_key(&user, salt);
    let mut buf = cipher.to_vec();
    let plain = cbc::Decryptor::<des::TdesEde3>::new_from_slices(&key, &[0u8; 8]).ok()?.decrypt_padded_mut::<Pkcs7>(&mut buf).ok()?;
    Some(plain.to_vec())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn hex(s: &str) -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() }
    // made with OpenSSL's 3DES from the same recipe, for the key bytes 1..=32 and the user "steamuser"
    pub(crate) const BLOB: &str = "0100000057696e652043727970743332206f6b000100000057696e652043727970743332206f6b000000000002000000000003660000a800000010000000575757575757575757575757575757570000000004800000a0000000100000006465666768696a6b6c6d6e6f707172732800000010b007beed53e08bfcc999064e37481aedeb9c038bc1070f89502dcad6c0c9d195b06391377e5f5b1400000011f6ad8ec52a2984abaafd7c3b516503785c2072";
    #[test] fn a_blob_opens_for_the_wine_user_that_protected_it_and_for_nobody_else() {
        let b = hex(BLOB);
        assert_eq!(unprotect(&b, "steamuser"), Some((1u8..=32).collect()));
        assert_eq!(unprotect(&b, "rob"), None);
        assert_eq!(unprotect(&b[..40], "steamuser"), None, "a cut-off blob is nothing");
        assert_eq!(unprotect(b"not a blob at all, just text", "steamuser"), None);
    }
}
