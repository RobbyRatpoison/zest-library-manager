//! Windows' own DPAPI (`CryptUnprotectData` / `CryptProtectData` in crypt32): what a program running as the signed-in Windows user can open without any secret of its own, because Windows
//! keeps the key tied to the user. Ubisoft Connect keeps the key of its game-list cache this way (`store/<key id>.bin`); under Wine the scheme is Wine's own (see `wine_dpapi`).
//! Everything here is Windows only; elsewhere both calls answer None.

/// The plain bytes inside a DPAPI blob written for this Windows user (`entropy` when the writer used one); None when it is not ours, damaged, or not Windows.
pub fn unprotect(blob: &[u8], entropy: Option<&[u8]>) -> Option<Vec<u8>> { imp::call(blob, entropy, false) }
/// A DPAPI blob for `data` that only this Windows user can open (used by the tests and by anything that needs to keep a secret on this machine).
pub fn protect(data: &[u8], entropy: Option<&[u8]>) -> Option<Vec<u8>> { imp::call(data, entropy, true) }

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    #[repr(C)] struct Blob { len: u32, data: *mut u8 }
    #[link(name = "crypt32")] extern "system" {
        fn CryptUnprotectData(i: *const Blob, descr: *mut *mut u16, entropy: *const Blob, reserved: *const c_void, prompt: *const c_void, flags: u32, out: *mut Blob) -> i32;
        fn CryptProtectData(i: *const Blob, descr: *const u16, entropy: *const Blob, reserved: *const c_void, prompt: *const c_void, flags: u32, out: *mut Blob) -> i32;
    }
    #[link(name = "kernel32")] extern "system" { fn LocalFree(h: *mut c_void) -> *mut c_void; }
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 1;
    pub fn call(input: &[u8], entropy: Option<&[u8]>, protect: bool) -> Option<Vec<u8>> {
        if input.len() > u32::MAX as usize / 2 { return None; }
        let i = Blob { len: input.len() as u32, data: input.as_ptr() as *mut u8 };
        let e = entropy.map(|b| Blob { len: b.len() as u32, data: b.as_ptr() as *mut u8 });
        let ep = e.as_ref().map_or(std::ptr::null(), |b| b as *const Blob);
        let mut out = Blob { len: 0, data: std::ptr::null_mut() };
        // SAFETY: both blobs point at live slices for the whole call; the output buffer is Windows' own and is freed with LocalFree after it is copied
        let ok = unsafe {
            if protect { CryptProtectData(&i, std::ptr::null(), ep, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) }
            else { CryptUnprotectData(&i, std::ptr::null_mut(), ep, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) }
        };
        if ok == 0 || out.data.is_null() { return None; }
        let v = unsafe { std::slice::from_raw_parts(out.data, out.len as usize).to_vec() };
        unsafe { LocalFree(out.data as *mut c_void); }
        Some(v)
    }
}
#[cfg(not(windows))]
mod imp { pub fn call(_: &[u8], _: Option<&[u8]>, _: bool) -> Option<Vec<u8>> { None } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn off_windows_there_is_no_dpapi() { if !cfg!(windows) { assert!(unprotect(b"x", None).is_none() && protect(b"x", None).is_none()); } }
    #[cfg(windows)]
    #[test] fn a_blob_opens_for_the_same_user_and_only_with_the_same_entropy() {
        let secret = [7u8; 32];
        let b = protect(&secret, None).expect("protect"); assert_ne!(&b[..], &secret[..]);
        assert_eq!(unprotect(&b, None).unwrap(), secret.to_vec());
        let e = protect(&secret, Some(b"salt")).unwrap();
        assert!(unprotect(&e, None).is_none() && unprotect(&e, Some(b"other")).is_none());
        assert_eq!(unprotect(&e, Some(b"salt")).unwrap(), secret.to_vec());
        assert!(unprotect(b"not a blob", None).is_none());
    }
}
