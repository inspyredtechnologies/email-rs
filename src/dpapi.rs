//! Windows Data Protection API: encrypt data so only the current Windows user
//! (on this PC) can decrypt it. Used for saved passwords and OAuth tokens.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};

/// Extra entropy so other programs can't decrypt our blobs with a bare CryptUnprotectData call.
/// Keeps the app's original name: changing it would make every saved sign-in unreadable.
const ENTROPY: &[u8] = b"Outlook RS secret v1";

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// Copy out and free a blob that Windows allocated.
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData.cast()) };
    bytes
}

pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    let (input, entropy) = (blob(data), blob(ENTROPY));
    let mut out = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe { CryptProtectData(&input, null(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) };
    if ok == 0 {
        return Err(format!("Windows couldn't encrypt the secret (error {}).", std::io::Error::last_os_error()));
    }
    Ok(take(out))
}

pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    let (input, entropy) = (blob(data), blob(ENTROPY));
    let mut out = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe { CryptUnprotectData(&input, null_mut(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) };
    if ok == 0 {
        return Err(format!("Windows couldn't decrypt the secret (error {}).", std::io::Error::last_os_error()));
    }
    Ok(take(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_long_secrets() {
        // Larger than Credential Manager's 2560-byte limit, like a Microsoft 365 refresh token.
        let secret: String = (0..6000).map(|i| char::from(b'A' + (i % 26) as u8)).collect();
        let sealed = protect(secret.as_bytes()).unwrap();
        assert!(!sealed.windows(16).any(|w| w == &secret.as_bytes()[..16]), "stored encrypted");
        assert_eq!(unprotect(&sealed).unwrap(), secret.as_bytes());
        // Tampered data is rejected rather than returning garbage.
        let mut bad = sealed.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xFF;
        assert!(unprotect(&bad).is_err());
    }
}
