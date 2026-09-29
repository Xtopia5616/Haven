//! Operating-system credential-store adapter.

use anyhow::{Context, Result};
use haven_common::config::{CredentialStore, validate_credential_reference};

/// Platform credential-store adapter. Windows uses Credential Manager;
/// non-Windows builds fail credential operations explicitly rather than
/// storing secrets in plaintext or volatile memory.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformCredentialStore;

#[cfg(windows)]
struct SecretBuffer(Vec<u8>);

#[cfg(windows)]
impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(windows)]
impl CredentialStore for PlatformCredentialStore {
    fn read(&self, reference: &str) -> Result<Option<String>> {
        use std::ptr;
        use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
        use windows_sys::Win32::Security::Credentials::{
            CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
        };

        validate_credential_reference(reference)?;
        let target = target_name(reference);
        let mut credential: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: target is NUL-terminated and credential points to a valid
        // out-pointer. CredReadW owns the returned allocation until CredFree.
        let success = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if success == 0 {
            // SAFETY: GetLastError is thread-local and queried immediately
            // after CredReadW returned failure.
            let error = unsafe { GetLastError() };
            if error == ERROR_NOT_FOUND {
                return Ok(None);
            }
            anyhow::bail!("Windows Credential Manager read failed with code {error}");
        }
        if credential.is_null() {
            anyhow::bail!("Windows Credential Manager returned an invalid credential");
        }

        struct CredentialGuard(*mut CREDENTIALW);
        impl Drop for CredentialGuard {
            fn drop(&mut self) {
                // Clear the copied credential blob before releasing the
                // Credential Manager allocation.
                // SAFETY: this pointer was returned by CredReadW and is freed
                // exactly once, including when UTF-8 validation fails.
                unsafe {
                    if !(*self.0).CredentialBlob.is_null() {
                        std::ptr::write_bytes(
                            (*self.0).CredentialBlob,
                            0,
                            (*self.0).CredentialBlobSize as usize,
                        );
                    }
                    CredFree(self.0.cast());
                }
            }
        }

        // SAFETY: a successful CredReadW provides a non-null credential
        // allocation valid until the guard calls CredFree.
        let credential_ref = unsafe { &*credential };
        let _guard = CredentialGuard(credential);
        if credential_ref.CredentialBlobSize == 0 {
            return Ok(Some(String::new()));
        }
        if credential_ref.CredentialBlob.is_null() {
            anyhow::bail!("Windows Credential Manager returned an invalid credential blob");
        }
        // SAFETY: the credential blob is an owned byte range described by
        // CredentialBlobSize and remains valid until the guard calls CredFree.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                credential_ref.CredentialBlob,
                credential_ref.CredentialBlobSize as usize,
            )
        };
        let value = std::str::from_utf8(bytes)
            .context("Windows Credential Manager returned non-UTF-8 credential data")?;
        Ok(Some(value.to_string()))
    }

    fn write(&self, reference: &str, value: &str) -> Result<()> {
        use windows_sys::Win32::Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
        };

        validate_credential_reference(reference)?;
        let mut target = target_name(reference);
        let mut blob = SecretBuffer(value.as_bytes().to_vec());
        let blob_size = u32::try_from(blob.0.len())
            .context("credential exceeds Windows Credential Manager size limit")?;
        // SAFETY: CREDENTIALW is a C FFI struct where all-zero is a valid
        // initial state before its documented fields are filled.
        let mut credential: CREDENTIALW = unsafe { std::mem::zeroed() };
        credential.Type = CRED_TYPE_GENERIC;
        credential.TargetName = target.as_mut_ptr();
        credential.CredentialBlobSize = blob_size;
        credential.CredentialBlob = if blob.0.is_empty() {
            std::ptr::null_mut()
        } else {
            blob.0.as_mut_ptr()
        };
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE;

        // SAFETY: all pointer fields either point to live, NUL-terminated
        // buffers held through this call or are null. CredWriteW copies the
        // credential before returning. SecretBuffer zeroes its local bytes on
        // both success and error paths.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            use windows_sys::Win32::Foundation::GetLastError;
            // SAFETY: queried immediately after the failed CredWriteW call.
            let error = unsafe { GetLastError() };
            anyhow::bail!("Windows Credential Manager write failed with code {error}");
        }
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<()> {
        use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
        use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};

        validate_credential_reference(reference)?;
        let target = target_name(reference);
        // SAFETY: target is a live, NUL-terminated string for this call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
            return Ok(());
        }
        // SAFETY: queried immediately after the failed CredDeleteW call.
        let error = unsafe { GetLastError() };
        if error == ERROR_NOT_FOUND {
            Ok(())
        } else {
            anyhow::bail!("Windows Credential Manager delete failed with code {error}");
        }
    }
}

#[cfg(not(windows))]
impl CredentialStore for PlatformCredentialStore {
    fn read(&self, _reference: &str) -> Result<Option<String>> {
        anyhow::bail!("persistent credential storage is unavailable on this platform")
    }

    fn write(&self, _reference: &str, _value: &str) -> Result<()> {
        anyhow::bail!("persistent credential storage is unavailable on this platform")
    }

    fn delete(&self, _reference: &str) -> Result<()> {
        anyhow::bail!("persistent credential storage is unavailable on this platform")
    }
}

#[cfg(windows)]
fn target_name(reference: &str) -> Vec<u16> {
    format!("Haven/{reference}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}
