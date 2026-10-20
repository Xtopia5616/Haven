//! Operating-system credential-store adapter.

use anyhow::{Context, Result};
use haven_common::config::{CredentialStore, validate_credential_reference};

#[cfg(any(windows, test))]
const WINDOWS_CREDENTIAL_BLOB_MAX_BYTES: usize = 5 * 512;
#[cfg(any(windows, test))]
const CREDENTIAL_CHUNK_BYTES: usize = 2 * 1024;
#[cfg(any(windows, test))]
const MAX_CREDENTIAL_CHUNKS: usize = 32;
#[cfg(any(windows, test))]
// Leave three bytes of per-chunk headroom for a UTF-8 code point that
// straddles the nominal boundary, so every accepted value fits in 32 chunks.
const MAX_CREDENTIAL_VALUE_BYTES: usize = (CREDENTIAL_CHUNK_BYTES - 3) * MAX_CREDENTIAL_CHUNKS;
#[cfg(any(windows, test))]
const CHUNK_MANIFEST_PREFIX: &str = "HAVEN_CREDENTIAL_CHUNKS_V1:";

#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};
#[cfg(any(windows, test))]
use std::ops::Range;

/// Platform credential-store adapter. Windows uses Credential Manager;
/// non-Windows builds fail credential operations explicitly rather than
/// storing secrets in plaintext or volatile memory.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformCredentialStore;

#[cfg(any(windows, test))]
struct SecretBuffer(Vec<u8>);

#[cfg(any(windows, test))]
impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChunkManifest {
    active_slot: char,
    chunk_count: usize,
    digest: String,
}

/// Private adapter boundary used by production Credential Manager and the
/// isolated storage-layout tests. Targets contain only opaque credential
/// references and chunk indexes; values remain encrypted by the OS store.
#[cfg(any(windows, test))]
trait CredentialTargetStore {
    fn read_blob(&self, target: &str) -> Result<Option<SecretBuffer>>;
    fn write_blob(&self, target: &str, value: &[u8]) -> Result<()>;
    fn delete_blob(&self, target: &str) -> Result<()>;
}

#[cfg(windows)]
struct WindowsCredentialTargetStore;

#[cfg(windows)]
impl CredentialTargetStore for WindowsCredentialTargetStore {
    fn read_blob(&self, target: &str) -> Result<Option<SecretBuffer>> {
        read_raw_credential(target)
    }

    fn write_blob(&self, target: &str, value: &[u8]) -> Result<()> {
        write_raw_credential(target, value)
    }

    fn delete_blob(&self, target: &str) -> Result<()> {
        delete_raw_credential(target)
    }
}

#[cfg(windows)]
impl CredentialStore for PlatformCredentialStore {
    fn read(&self, reference: &str) -> Result<Option<String>> {
        validate_credential_reference(reference)?;
        read_stored_value(&WindowsCredentialTargetStore, reference)
    }

    fn write(&self, reference: &str, value: &str) -> Result<()> {
        validate_credential_reference(reference)?;
        write_stored_value(&WindowsCredentialTargetStore, reference, value)
    }

    fn delete(&self, reference: &str) -> Result<()> {
        validate_credential_reference(reference)?;
        delete_stored_value(&WindowsCredentialTargetStore, reference)
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

#[cfg(any(windows, test))]
fn credential_chunk_ranges(value: &str, max_bytes: usize) -> Vec<Range<usize>> {
    assert!(max_bytes > 0, "credential chunk size must be non-zero");
    if value.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = (start + max_bytes).min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
}

#[cfg(any(windows, test))]
fn credential_digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(any(windows, test))]
fn chunk_manifest_text(active_slot: char, chunk_count: usize, value: &str) -> String {
    format!(
        "{CHUNK_MANIFEST_PREFIX}{active_slot}:{chunk_count}:{}",
        credential_digest(value)
    )
}

#[cfg(any(windows, test))]
fn parse_chunk_manifest(value: &str) -> Option<ChunkManifest> {
    let mut fields = value.strip_prefix(CHUNK_MANIFEST_PREFIX)?.split(':');
    let active_slot = match fields.next()? {
        "a" => 'a',
        "b" => 'b',
        _ => return None,
    };
    let chunk_count = fields.next()?.parse::<usize>().ok()?;
    if !(1..=MAX_CREDENTIAL_CHUNKS).contains(&chunk_count) {
        return None;
    }
    let digest = fields.next()?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    if fields.next().is_some() {
        return None;
    }
    Some(ChunkManifest {
        active_slot,
        chunk_count,
        digest: digest.to_ascii_lowercase(),
    })
}

#[cfg(any(windows, test))]
fn read_stored_value(
    store: &impl CredentialTargetStore,
    reference: &str,
) -> Result<Option<String>> {
    let base_target = target_name(reference);
    let Some(root) = store.read_blob(&base_target)? else {
        return Ok(None);
    };
    let root_text = std::str::from_utf8(&root.0)
        .context("Windows Credential Manager returned non-UTF-8 credential data")?;
    let Some(manifest) = parse_chunk_manifest(root_text) else {
        anyhow::bail!("Windows Credential Manager credential manifest is invalid");
    };

    let mut assembled = SecretBuffer(Vec::new());
    for index in 0..manifest.chunk_count {
        let target = chunk_target_name(reference, manifest.active_slot, index);
        let Some(chunk) = store.read_blob(&target)? else {
            anyhow::bail!("Windows Credential Manager returned incomplete credential data");
        };
        if chunk.0.len() > CREDENTIAL_CHUNK_BYTES {
            anyhow::bail!("Windows Credential Manager returned an oversized credential chunk");
        }
        if assembled.0.len().saturating_add(chunk.0.len()) > MAX_CREDENTIAL_VALUE_BYTES {
            anyhow::bail!("Windows Credential Manager credential data exceeds the supported size");
        }
        assembled.0.extend_from_slice(&chunk.0);
    }
    let value = std::str::from_utf8(&assembled.0)
        .context("Windows Credential Manager returned invalid credential data")?;
    if credential_digest(value) != manifest.digest {
        anyhow::bail!("Windows Credential Manager credential data failed integrity validation");
    }
    Ok(Some(value.to_string()))
}

#[cfg(any(windows, test))]
fn write_stored_value(
    store: &impl CredentialTargetStore,
    reference: &str,
    value: &str,
) -> Result<()> {
    if value.len() > MAX_CREDENTIAL_VALUE_BYTES {
        anyhow::bail!("credential exceeds the supported Windows secure-storage size");
    }

    let base_target = target_name(reference);
    let previous_manifest = read_manifest(store, &base_target)?;
    let ranges = credential_chunk_ranges(value, CREDENTIAL_CHUNK_BYTES);
    if ranges.is_empty() || ranges.len() > MAX_CREDENTIAL_CHUNKS {
        anyhow::bail!("credential exceeds the supported Windows secure-storage size");
    }
    let active_slot = previous_manifest
        .as_ref()
        .map(|manifest| {
            if manifest.active_slot == 'a' {
                'b'
            } else {
                'a'
            }
        })
        .unwrap_or('a');
    if previous_manifest.is_some() {
        cleanup_chunk_slot(store, reference, active_slot)?;
    } else {
        for slot in ['a', 'b'] {
            cleanup_chunk_slot(store, reference, slot)?;
        }
    }

    for (index, range) in ranges.iter().enumerate() {
        let target = chunk_target_name(reference, active_slot, index);
        if let Err(error) = store.write_blob(&target, value[range.clone()].as_bytes()) {
            warn_on_chunk_cleanup_failure(cleanup_chunk_slot(store, reference, active_slot));
            return Err(error);
        }
    }

    let manifest = chunk_manifest_text(active_slot, ranges.len(), value);
    if let Err(error) = store.write_blob(&base_target, manifest.as_bytes()) {
        warn_on_chunk_cleanup_failure(cleanup_chunk_slot(store, reference, active_slot));
        return Err(error);
    }

    if let Some(previous_manifest) = previous_manifest {
        warn_on_chunk_cleanup_failure(cleanup_chunk_slot(
            store,
            reference,
            previous_manifest.active_slot,
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn read_manifest(
    store: &impl CredentialTargetStore,
    base_target: &str,
) -> Result<Option<ChunkManifest>> {
    let Some(value) = store.read_blob(base_target)? else {
        return Ok(None);
    };
    let value = std::str::from_utf8(&value.0)
        .context("Windows Credential Manager returned non-UTF-8 credential data")?;
    if value.starts_with(CHUNK_MANIFEST_PREFIX) {
        return parse_chunk_manifest(value).map(Some).ok_or_else(|| {
            anyhow::anyhow!("Windows Credential Manager credential manifest is invalid")
        });
    }
    anyhow::bail!("Windows Credential Manager credential manifest is invalid");
}

#[cfg(any(windows, test))]
fn delete_stored_value(store: &impl CredentialTargetStore, reference: &str) -> Result<()> {
    store.delete_blob(&target_name(reference))?;
    let mut first_error = None;
    for slot in ['a', 'b'] {
        if let Err(error) = cleanup_chunk_slot(store, reference, slot)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn cleanup_chunk_slot(
    store: &impl CredentialTargetStore,
    reference: &str,
    slot: char,
) -> Result<()> {
    let mut first_error = None;
    for index in 0..MAX_CREDENTIAL_CHUNKS {
        if let Err(error) = store.delete_blob(&chunk_target_name(reference, slot, index))
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn warn_on_chunk_cleanup_failure(result: Result<()>) {
    if result.is_err() {
        tracing::warn!("failed to remove stale Windows Credential Manager credential chunks");
    }
}

fn target_name(reference: &str) -> String {
    format!("haven/{reference}")
}

#[cfg(any(windows, test))]
fn chunk_target_name(reference: &str, slot: char, index: usize) -> String {
    format!("haven/{reference}/chunks/{slot}/{index}")
}

#[cfg(windows)]
fn wide_target_name(target: &str) -> Vec<u16> {
    target.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn read_raw_credential(target: &str) -> Result<Option<SecretBuffer>> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    let target = wide_target_name(target);
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
            // exactly once, including when data validation fails.
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
    if credential_ref.CredentialBlobSize as usize > WINDOWS_CREDENTIAL_BLOB_MAX_BYTES {
        anyhow::bail!("Windows Credential Manager returned an oversized credential blob");
    }
    if credential_ref.CredentialBlobSize == 0 {
        return Ok(Some(SecretBuffer(Vec::new())));
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
    Ok(Some(SecretBuffer(bytes.to_vec())))
}

#[cfg(windows)]
fn write_raw_credential(target: &str, value: &[u8]) -> Result<()> {
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    if value.len() > WINDOWS_CREDENTIAL_BLOB_MAX_BYTES {
        anyhow::bail!("credential chunk exceeds Windows Credential Manager size limit");
    }
    let mut target = wide_target_name(target);
    let mut blob = SecretBuffer(value.to_vec());
    let blob_size = u32::try_from(blob.0.len())
        .context("credential chunk exceeds Windows Credential Manager size limit")?;
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

#[cfg(windows)]
fn delete_raw_credential(target: &str) -> Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};

    let target = wide_target_name(target);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const REFERENCE: &str = "cred-00000000000000000000000000000000";

    #[derive(Default)]
    struct FakeCredentialTargetStore {
        values: Mutex<HashMap<String, Vec<u8>>>,
        fail_next_write_to: Mutex<Option<String>>,
    }

    impl FakeCredentialTargetStore {
        fn seed(&self, target: &str, value: &[u8]) {
            self.values
                .lock()
                .unwrap()
                .insert(target.to_string(), value.to_vec());
        }

        fn contains(&self, target: &str) -> bool {
            self.values.lock().unwrap().contains_key(target)
        }

        fn fail_next_write_to(&self, target: &str) {
            *self.fail_next_write_to.lock().unwrap() = Some(target.to_string());
        }
    }

    impl CredentialTargetStore for FakeCredentialTargetStore {
        fn read_blob(&self, target: &str) -> Result<Option<SecretBuffer>> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(target)
                .cloned()
                .map(SecretBuffer))
        }

        fn write_blob(&self, target: &str, value: &[u8]) -> Result<()> {
            let should_fail = {
                let mut failure = self.fail_next_write_to.lock().unwrap();
                if failure.as_deref() == Some(target) {
                    *failure = None;
                    true
                } else {
                    false
                }
            };
            if should_fail {
                anyhow::bail!("injected credential target write failure");
            }
            if value.len() > WINDOWS_CREDENTIAL_BLOB_MAX_BYTES {
                anyhow::bail!("fake credential target exceeded platform blob limit");
            }
            self.values
                .lock()
                .unwrap()
                .insert(target.to_string(), value.to_vec());
            Ok(())
        }

        fn delete_blob(&self, target: &str) -> Result<()> {
            self.values.lock().unwrap().remove(target);
            Ok(())
        }
    }

    #[test]
    fn credential_chunks_preserve_utf8_boundaries_and_round_trip() {
        let value = format!("{}🙂{}", "a".repeat(2_047), "数据库凭据".repeat(800));
        let ranges = credential_chunk_ranges(&value, CREDENTIAL_CHUNK_BYTES);
        assert!(ranges.len() > 1);
        assert!(ranges.iter().all(|range| {
            range.len() <= CREDENTIAL_CHUNK_BYTES
                && value.is_char_boundary(range.start)
                && value.is_char_boundary(range.end)
        }));
        let rebuilt = ranges
            .iter()
            .map(|range| &value[range.clone()])
            .collect::<String>();
        assert_eq!(rebuilt, value);
    }

    #[test]
    fn chunk_manifest_round_trips_and_rejects_invalid_shapes() {
        let value = "secret-value".repeat(400);
        let manifest = chunk_manifest_text('b', 3, &value);
        assert_eq!(
            parse_chunk_manifest(&manifest),
            Some(ChunkManifest {
                active_slot: 'b',
                chunk_count: 3,
                digest: credential_digest(&value),
            })
        );
        assert!(parse_chunk_manifest("HAVEN_CREDENTIAL_CHUNKS_V1:c:3:bad").is_none());
        assert!(parse_chunk_manifest("HAVEN_CREDENTIAL_CHUNKS_V1:a:0:bad").is_none());
    }

    #[test]
    fn rejects_chunks_that_exceed_the_configured_chunk_size() {
        let store = FakeCredentialTargetStore::default();
        let value = "x".repeat(CREDENTIAL_CHUNK_BYTES + 1);
        store.seed(
            &target_name(REFERENCE),
            chunk_manifest_text('a', 1, &value).as_bytes(),
        );
        store.seed(&chunk_target_name(REFERENCE, 'a', 0), value.as_bytes());

        assert!(read_stored_value(&store, REFERENCE).is_err());
    }

    #[test]
    fn rejects_unversioned_single_blob_credentials() {
        let store = FakeCredentialTargetStore::default();
        store.seed(&target_name(REFERENCE), br#"{"refresh_token":"legacy"}"#);

        assert!(read_stored_value(&store, REFERENCE).is_err());
        assert!(write_stored_value(&store, REFERENCE, "replacement").is_err());
    }

    #[test]
    fn small_and_empty_credentials_use_the_versioned_chunk_format() {
        let store = FakeCredentialTargetStore::default();
        for value in ["short-token", ""] {
            write_stored_value(&store, REFERENCE, value).unwrap();
            let root = store.read_blob(&target_name(REFERENCE)).unwrap().unwrap();
            let root_text = std::str::from_utf8(&root.0).unwrap();
            let manifest = parse_chunk_manifest(root_text).unwrap();
            assert_eq!(manifest.chunk_count, 1);
            assert_eq!(
                read_stored_value(&store, REFERENCE).unwrap().as_deref(),
                Some(value)
            );
        }
    }

    #[test]
    fn large_credentials_round_trip_and_updates_use_a_second_slot() {
        let store = FakeCredentialTargetStore::default();
        let first = "token🙂".repeat(1_000);
        let second = "updated-token数据库".repeat(900);

        write_stored_value(&store, REFERENCE, &first).unwrap();
        let first_manifest = parse_chunk_manifest(
            std::str::from_utf8(&store.read_blob(&target_name(REFERENCE)).unwrap().unwrap().0)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some(first.as_str())
        );

        write_stored_value(&store, REFERENCE, &second).unwrap();
        let second_manifest = parse_chunk_manifest(
            std::str::from_utf8(&store.read_blob(&target_name(REFERENCE)).unwrap().unwrap().0)
                .unwrap(),
        )
        .unwrap();
        assert_ne!(first_manifest.active_slot, second_manifest.active_slot);
        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some(second.as_str())
        );
    }

    #[test]
    fn failed_manifest_commit_preserves_the_previous_credential() {
        let store = FakeCredentialTargetStore::default();
        let previous = "previous-token";
        let replacement = "replacement-token🙂".repeat(500);
        write_stored_value(&store, REFERENCE, previous).unwrap();
        let previous_manifest = parse_chunk_manifest(
            std::str::from_utf8(&store.read_blob(&target_name(REFERENCE)).unwrap().unwrap().0)
                .unwrap(),
        )
        .unwrap();
        let failed_slot = if previous_manifest.active_slot == 'a' {
            'b'
        } else {
            'a'
        };
        store.fail_next_write_to(&target_name(REFERENCE));

        assert!(write_stored_value(&store, REFERENCE, &replacement).is_err());
        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some(previous)
        );
        assert!(store.contains(&chunk_target_name(
            REFERENCE,
            previous_manifest.active_slot,
            0
        )));
        assert!(!store.contains(&chunk_target_name(REFERENCE, failed_slot, 0)));
    }

    #[test]
    fn detects_missing_or_corrupt_chunks_and_cleans_them_on_delete() {
        let store = FakeCredentialTargetStore::default();
        let value = "long-token".repeat(400);
        write_stored_value(&store, REFERENCE, &value).unwrap();
        let manifest = parse_chunk_manifest(
            std::str::from_utf8(&store.read_blob(&target_name(REFERENCE)).unwrap().unwrap().0)
                .unwrap(),
        )
        .unwrap();
        let first_chunk = chunk_target_name(REFERENCE, manifest.active_slot, 0);
        let original_chunk = store.read_blob(&first_chunk).unwrap().unwrap().0.clone();
        store.values.lock().unwrap().remove(&first_chunk);
        assert!(read_stored_value(&store, REFERENCE).is_err());
        store.seed(&first_chunk, &original_chunk);
        store.seed(&first_chunk, b"tampered");
        assert!(read_stored_value(&store, REFERENCE).is_err());

        delete_stored_value(&store, REFERENCE).unwrap();
        assert!(!store.contains(&target_name(REFERENCE)));
        assert!(!store.contains(&first_chunk));
    }

    #[test]
    fn replacing_a_chunked_value_with_a_small_value_removes_old_chunks() {
        let store = FakeCredentialTargetStore::default();
        write_stored_value(&store, REFERENCE, &"long-token".repeat(400)).unwrap();
        assert!(store.contains(&chunk_target_name(REFERENCE, 'a', 0)));

        write_stored_value(&store, REFERENCE, "short-token").unwrap();

        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some("short-token")
        );
        assert!(!store.contains(&chunk_target_name(REFERENCE, 'a', 0)));
        assert!(store.contains(&chunk_target_name(REFERENCE, 'b', 0)));
    }

    #[test]
    fn rejects_oversized_values_without_changing_existing_credentials() {
        let store = FakeCredentialTargetStore::default();
        write_stored_value(&store, REFERENCE, "keep-me").unwrap();
        let too_large = "x".repeat(MAX_CREDENTIAL_VALUE_BYTES + 1);

        assert!(write_stored_value(&store, REFERENCE, &too_large).is_err());
        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some("keep-me")
        );
    }

    #[test]
    fn maximum_utf8_boundary_heavy_value_fits_the_chunk_limit() {
        let store = FakeCredentialTargetStore::default();
        let value = "界".repeat(MAX_CREDENTIAL_VALUE_BYTES / "界".len());

        write_stored_value(&store, REFERENCE, &value).unwrap();

        let manifest = parse_chunk_manifest(
            std::str::from_utf8(&store.read_blob(&target_name(REFERENCE)).unwrap().unwrap().0)
                .unwrap(),
        )
        .unwrap();
        assert!(manifest.chunk_count <= MAX_CREDENTIAL_CHUNKS);
        assert_eq!(
            read_stored_value(&store, REFERENCE).unwrap().as_deref(),
            Some(value.as_str())
        );
    }

    #[test]
    fn credential_manager_targets_use_the_haven_namespace() {
        assert_eq!(
            target_name(REFERENCE),
            "haven/cred-00000000000000000000000000000000"
        );
        assert_eq!(
            chunk_target_name(REFERENCE, 'b', 12),
            "haven/cred-00000000000000000000000000000000/chunks/b/12"
        );
    }
}
