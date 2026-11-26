//! Dev check, run on Windows: Windows' real DPAPI round trip through `pd_filter::windpapi`.
fn main() {
    let secret = [7u8; 32];
    let b = pd_filter::windpapi::protect(&secret, None);
    println!("protect: {}", b.as_ref().map_or("failed".to_string(), |b| format!("{} bytes, differs from the secret: {}", b.len(), b[..] != secret[..])));
    let Some(b) = b else { std::process::exit(1) };
    println!("unprotect same user, no entropy: {}", pd_filter::windpapi::unprotect(&b, None).map_or(false, |v| v == secret));
    let e = pd_filter::windpapi::protect(&secret, Some(b"salt")).unwrap();
    println!("entropy blob opens without entropy: {}", pd_filter::windpapi::unprotect(&e, None).is_some());
    println!("entropy blob opens with wrong entropy: {}", pd_filter::windpapi::unprotect(&e, Some(b"other")).is_some());
    println!("entropy blob opens with its entropy: {}", pd_filter::windpapi::unprotect(&e, Some(b"salt")).map_or(false, |v| v == secret));
    println!("garbage opens: {}", pd_filter::windpapi::unprotect(b"not a blob", None).is_some());
}
