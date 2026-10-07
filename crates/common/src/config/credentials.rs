//! Credential storage port and a process-local implementation for isolated
//! tests. Production Windows builds provide Credential Manager through
//! `haven-platform`; non-Windows production builds intentionally fail closed.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::{AppConfig, McpEnvironmentCredentialRef, McpServerConfig, OcrConfig};
use crate::types::new_id;

/// Opaque references are persisted in config; secret values stay behind this
/// interface and are hydrated only into the backend runtime snapshot.
pub trait CredentialStore: Send + Sync {
    fn read(&self, reference: &str) -> anyhow::Result<Option<String>>;
    fn write(&self, reference: &str, value: &str) -> anyhow::Result<()>;
    fn delete(&self, reference: &str) -> anyhow::Result<()>;
}

/// Fail-closed default for builds that have not installed a persistent
/// platform credential adapter. It permits configurations with no credential
/// references, while making reads and writes fail explicitly when credentials
/// are configured or changed.
#[derive(Default)]
pub(crate) struct UnavailableCredentialStore;

impl CredentialStore for UnavailableCredentialStore {
    fn read(&self, _reference: &str) -> anyhow::Result<Option<String>> {
        anyhow::bail!("persistent credential storage is unavailable")
    }

    fn write(&self, _reference: &str, _value: &str) -> anyhow::Result<()> {
        anyhow::bail!("persistent credential storage is unavailable")
    }

    fn delete(&self, _reference: &str) -> anyhow::Result<()> {
        anyhow::bail!("persistent credential storage is unavailable")
    }
}

/// Validate the stable reference format before using it as an OS credential
/// target. References are identifiers, never secret values.
pub fn validate_credential_reference(reference: &str) -> anyhow::Result<()> {
    let Some(suffix) = reference.strip_prefix("cred-") else {
        anyhow::bail!("invalid credential reference");
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("invalid credential reference");
    }
    Ok(())
}

/// Process-local credential storage. Values never reach disk; callers must
/// inject it explicitly, typically in isolated tests. It is unsuitable for
/// default production or development startup because references will not
/// survive a process restart.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    values: Mutex<HashMap<String, String>>,
}

impl CredentialStore for InMemoryCredentialStore {
    fn read(&self, reference: &str) -> anyhow::Result<Option<String>> {
        validate_credential_reference(reference)?;
        self.values
            .lock()
            .map(|values| values.get(reference).cloned())
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))
    }

    fn write(&self, reference: &str, value: &str) -> anyhow::Result<()> {
        validate_credential_reference(reference)?;
        self.values
            .lock()
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))?
            .insert(reference.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, reference: &str) -> anyhow::Result<()> {
        validate_credential_reference(reference)?;
        self.values
            .lock()
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))?
            .remove(reference);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum CredentialSlot {
    ProviderApiKey(String),
    OcrApiKey,
    OcrApiSecret,
}

/// Hydrate runtime-only secret values from the current secure references.
pub(crate) fn hydrate_from_store(
    config: &mut AppConfig,
    store: &dyn CredentialStore,
) -> anyhow::Result<()> {
    validate_reference_syntax(config)?;
    for provider in &mut config.llm.providers {
        hydrate_reference(
            &mut provider.api_key,
            provider.api_key_ref.as_deref(),
            store,
            "provider API key",
        )?;
    }
    let api_key_ref = config.media.ocr.api_key_ref.clone();
    hydrate_reference(
        &mut config.media.ocr.api_key,
        api_key_ref.as_deref(),
        store,
        "OCR API key",
    )?;
    let api_secret_ref = config.media.ocr.api_secret_ref.clone();
    hydrate_reference(
        &mut config.media.ocr.api_secret,
        api_secret_ref.as_deref(),
        store,
        "OCR API secret",
    )?;
    for server in &mut config.mcp_servers {
        hydrate_mcp_environment(server, store)?;
    }
    validate_config_references(config)?;
    Ok(())
}
fn validate_reference_syntax(config: &AppConfig) -> anyhow::Result<()> {
    let mut references = HashSet::<String>::new();
    let mut insert = |reference: &str| -> anyhow::Result<()> {
        validate_credential_reference(reference)?;
        if !references.insert(reference.to_string()) {
            anyhow::bail!("credential reference is assigned more than once");
        }
        Ok(())
    };
    for provider in &config.llm.providers {
        if let Some(reference) = provider.api_key_ref.as_deref() {
            insert(reference)?;
        }
    }
    if let Some(reference) = config.media.ocr.api_key_ref.as_deref() {
        insert(reference)?;
    }
    if let Some(reference) = config.media.ocr.api_secret_ref.as_deref() {
        insert(reference)?;
    }
    for server in &config.mcp_servers {
        let mut names = HashSet::new();
        for item in &server.env_refs {
            if !names.insert(item.name.as_str()) {
                anyhow::bail!("MCP environment variable is configured more than once");
            }
            match (item.has_value, item.credential_ref.as_deref()) {
                (true, Some(reference)) => insert(reference)?,
                (true, None) => anyhow::bail!("MCP environment value has no credential reference"),
                (false, Some(_)) => {
                    anyhow::bail!("MCP name-only environment entry has a credential reference")
                }
                (false, None) => {}
            }
        }
    }
    Ok(())
}

fn hydrate_mcp_environment(
    server: &mut McpServerConfig,
    store: &dyn CredentialStore,
) -> anyhow::Result<()> {
    if !server.env.is_empty() {
        anyhow::bail!("plaintext MCP environment values cannot be loaded from config.toml");
    }
    for reference in &server.env_refs {
        if reference.has_value {
            let value = match reference.credential_ref.as_deref() {
                Some(credential_ref) => match secure_read(store, credential_ref)? {
                    Some(value) => value,
                    None => anyhow::bail!(
                        "configured MCP environment credential is missing from secure storage"
                    ),
                },
                None => {
                    anyhow::bail!("configured MCP environment credential has no secure reference")
                }
            };
            server.env.push(format!("{}={value}", reference.name));
        } else {
            server.env.push(reference.name.clone());
        }
    }
    Ok(())
}

fn persist_mcp_environment(
    server: &mut McpServerConfig,
    store: &dyn CredentialStore,
) -> anyhow::Result<()> {
    if server.env.is_empty() {
        server.env_refs.clear();
        return Ok(());
    }

    let previous = server.env_refs.clone();
    let mut refs = Vec::with_capacity(server.env.len());
    for entry in &server.env {
        let Some((name, value)) = entry.split_once('=') else {
            refs.push(McpEnvironmentCredentialRef {
                name: entry.clone(),
                credential_ref: None,
                has_value: false,
            });
            continue;
        };
        if name.trim().is_empty() {
            anyhow::bail!("MCP environment variable name cannot be empty");
        }
        let existing = previous
            .iter()
            .find(|item| item.name == name && item.has_value);
        let reused =
            if let Some(reference) = existing.and_then(|item| item.credential_ref.as_deref()) {
                (secure_read(store, reference)?.as_deref() == Some(value))
                    .then_some(reference.to_string())
            } else {
                None
            };
        let credential_ref = match reused {
            Some(reference) => reference,
            None => {
                let reference = new_id("cred");
                secure_write(store, &reference, value, "MCP environment value")?;
                reference
            }
        };
        refs.push(McpEnvironmentCredentialRef {
            name: name.to_string(),
            credential_ref: Some(credential_ref),
            has_value: true,
        });
    }
    if refs != server.env_refs {
        server.env_refs = refs;
    }
    Ok(())
}

pub(crate) fn prepare_after_edit(
    before: &AppConfig,
    after: &mut AppConfig,
    store: &dyn CredentialStore,
    staged: &mut HashMap<CredentialSlot, String>,
) -> anyhow::Result<HashSet<CredentialSlot>> {
    validate_reference_changes(before, after, staged)?;
    let mut consumed = HashSet::new();
    for provider in &mut after.llm.providers {
        let slot = CredentialSlot::ProviderApiKey(provider.name.clone());
        if let Some(reference) = staged
            .get(&slot)
            .filter(|reference| provider.api_key_ref.as_deref() == Some(reference.as_str()))
        {
            provider.api_key_ref = Some(reference.clone());
            provider.api_key = read_staged(store, reference, "provider API key")?;
            consumed.insert(slot);
        } else if provider.api_key.is_empty() {
            if provider.api_key_ref.is_none()
                && let Some(previous) = before.llm.provider_config_by_name(&provider.name)
            {
                provider.api_key_ref = previous.api_key_ref.clone();
                provider.api_key = previous.api_key.clone();
            }
            hydrate_reference(
                &mut provider.api_key,
                provider.api_key_ref.as_deref(),
                store,
                "provider API key",
            )?;
        } else {
            provider.api_key_ref = Some(store_secret_if_needed(
                before
                    .llm
                    .provider_config_by_name(&provider.name)
                    .and_then(|previous| previous.api_key_ref.as_deref()),
                &provider.api_key,
                store,
                "provider API key",
            )?);
        }
    }

    prepare_ocr_field(
        &mut after.media.ocr,
        OcrCredentialField::ApiKey,
        before.media.ocr.api_key_ref.as_deref(),
        CredentialSlot::OcrApiKey,
        store,
        staged,
        &mut consumed,
    )?;
    prepare_ocr_field(
        &mut after.media.ocr,
        OcrCredentialField::ApiSecret,
        before.media.ocr.api_secret_ref.as_deref(),
        CredentialSlot::OcrApiSecret,
        store,
        staged,
        &mut consumed,
    )?;

    for server in &mut after.mcp_servers {
        persist_mcp_environment(server, store)?;
    }

    validate_config_references(after)?;
    Ok(consumed)
}

fn validate_reference_changes(
    before: &AppConfig,
    after: &AppConfig,
    staged: &HashMap<CredentialSlot, String>,
) -> anyhow::Result<()> {
    let old_provider_references: HashMap<&str, &str> = before
        .llm
        .providers
        .iter()
        .filter_map(|provider| {
            provider
                .api_key_ref
                .as_deref()
                .map(|reference| (reference, provider.name.as_str()))
        })
        .collect();
    let new_provider_names: HashSet<&str> = after
        .llm
        .providers
        .iter()
        .map(|provider| provider.name.as_str())
        .collect();
    let mut references = HashSet::new();
    for provider in &after.llm.providers {
        let Some(reference) = provider.api_key_ref.as_deref() else {
            continue;
        };
        let same_provider = before
            .llm
            .provider_config_by_name(&provider.name)
            .and_then(|previous| previous.api_key_ref.as_deref())
            == Some(reference);
        let staged_for_provider = staged
            .get(&CredentialSlot::ProviderApiKey(provider.name.clone()))
            .is_some_and(|staged_reference| staged_reference == reference);
        let moved_from_removed_provider = old_provider_references
            .get(reference)
            .is_some_and(|old_name| !new_provider_names.contains(old_name));
        if !same_provider && !staged_for_provider && !moved_from_removed_provider {
            anyhow::bail!("provider credential reference does not belong to this configuration");
        }
        if !references.insert(reference) {
            anyhow::bail!("a credential reference cannot be assigned to multiple providers");
        }
    }

    for (reference, previous_reference, staged_slot) in [
        (
            after.media.ocr.api_key_ref.as_deref(),
            before.media.ocr.api_key_ref.as_deref(),
            CredentialSlot::OcrApiKey,
        ),
        (
            after.media.ocr.api_secret_ref.as_deref(),
            before.media.ocr.api_secret_ref.as_deref(),
            CredentialSlot::OcrApiSecret,
        ),
    ] {
        let Some(reference) = reference else {
            continue;
        };
        let staged_for_field = staged
            .get(&staged_slot)
            .is_some_and(|staged_reference| staged_reference == reference);
        if Some(reference) != previous_reference && !staged_for_field {
            anyhow::bail!("OCR credential reference does not belong to this configuration");
        }
        if !references.insert(reference) {
            anyhow::bail!("a credential reference cannot be assigned to multiple settings");
        }
    }

    for server in &after.mcp_servers {
        let previous = before
            .mcp_servers
            .iter()
            .find(|candidate| candidate.name == server.name);
        for item in &server.env_refs {
            let Some(reference) = item.credential_ref.as_deref() else {
                continue;
            };
            let owned_by_existing_variable = previous.is_some_and(|previous| {
                previous.env_refs.iter().any(|old| {
                    old.name == item.name && old.credential_ref.as_deref() == Some(reference)
                })
            });
            if !owned_by_existing_variable {
                anyhow::bail!(
                    "MCP credential reference does not belong to this environment variable"
                );
            }
            if !references.insert(reference) {
                anyhow::bail!("a credential reference cannot be assigned to multiple settings");
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum OcrCredentialField {
    ApiKey,
    ApiSecret,
}

fn prepare_ocr_field(
    ocr: &mut OcrConfig,
    field: OcrCredentialField,
    previous_reference: Option<&str>,
    slot: CredentialSlot,
    store: &dyn CredentialStore,
    staged: &HashMap<CredentialSlot, String>,
    consumed: &mut HashSet<CredentialSlot>,
) -> anyhow::Result<()> {
    let (value, reference, label) = match field {
        OcrCredentialField::ApiKey => (&mut ocr.api_key, &mut ocr.api_key_ref, "OCR API key"),
        OcrCredentialField::ApiSecret => (
            &mut ocr.api_secret,
            &mut ocr.api_secret_ref,
            "OCR API secret",
        ),
    };
    if let Some(next_reference) = staged
        .get(&slot)
        .filter(|next_reference| reference.as_deref() == Some(next_reference.as_str()))
    {
        *reference = Some(next_reference.clone());
        *value = read_staged(store, next_reference, label)?;
        consumed.insert(slot);
    } else if value.is_empty() {
        if reference.is_none() {
            *reference = previous_reference.map(ToOwned::to_owned);
        }
        hydrate_reference(value, reference.as_deref(), store, label)?;
    } else {
        *reference = Some(store_secret_if_needed(
            previous_reference,
            value,
            store,
            label,
        )?);
    }
    Ok(())
}

fn read_staged(
    store: &dyn CredentialStore,
    reference: &str,
    label: &str,
) -> anyhow::Result<String> {
    store
        .read(reference)?
        .ok_or_else(|| anyhow::anyhow!("staged {label} is missing from secure storage"))
}

fn hydrate_reference(
    value: &mut String,
    reference: Option<&str>,
    store: &dyn CredentialStore,
    label: &str,
) -> anyhow::Result<()> {
    if value.is_empty()
        && let Some(reference) = reference
    {
        *value = secure_read(store, reference)?
            .ok_or_else(|| anyhow::anyhow!("configured {label} is missing from secure storage"))?;
    }
    Ok(())
}

fn store_secret_if_needed(
    previous_reference: Option<&str>,
    value: &str,
    store: &dyn CredentialStore,
    label: &str,
) -> anyhow::Result<String> {
    if let Some(reference) = previous_reference
        && secure_read(store, reference)?.as_deref() == Some(value)
    {
        return Ok(reference.to_string());
    }
    let reference = new_id("cred");
    secure_write(store, &reference, value, label)?;
    Ok(reference)
}

fn secure_read(store: &dyn CredentialStore, reference: &str) -> anyhow::Result<Option<String>> {
    validate_credential_reference(reference)?;
    store
        .read(reference)
        .map_err(|_| anyhow::anyhow!("failed to read credential from secure storage"))
}

fn secure_write(
    store: &dyn CredentialStore,
    reference: &str,
    value: &str,
    label: &str,
) -> anyhow::Result<()> {
    validate_credential_reference(reference)?;
    store
        .write(reference, value)
        .map_err(|_| anyhow::anyhow!("failed to store {label} securely"))
}

pub(crate) fn validate_config_references(config: &AppConfig) -> anyhow::Result<()> {
    validate_reference_syntax(config)?;
    for provider in &config.llm.providers {
        if !provider.api_key.is_empty() && provider.api_key_ref.is_none() {
            anyhow::bail!("provider API key has no secure storage reference");
        }
    }
    if !config.media.ocr.api_key.is_empty() && config.media.ocr.api_key_ref.is_none() {
        anyhow::bail!("OCR API key has no secure storage reference");
    }
    if !config.media.ocr.api_secret.is_empty() && config.media.ocr.api_secret_ref.is_none() {
        anyhow::bail!("OCR API secret has no secure storage reference");
    }
    for server in &config.mcp_servers {
        for entry in &server.env {
            if let Some((name, _)) = entry.split_once('=')
                && !server.env_refs.iter().any(|item| {
                    item.name == name && item.has_value && item.credential_ref.is_some()
                })
            {
                anyhow::bail!("MCP environment value has no secure storage reference");
            }
        }
    }
    Ok(())
}

pub(crate) fn credential_references(config: &AppConfig) -> HashSet<String> {
    let mut references = HashSet::new();
    references.extend(
        config
            .llm
            .providers
            .iter()
            .filter_map(|provider| provider.api_key_ref.clone()),
    );
    references.extend(config.media.ocr.api_key_ref.clone());
    references.extend(config.media.ocr.api_secret_ref.clone());
    references.extend(config.mcp_servers.iter().flat_map(|server| {
        server
            .env_refs
            .iter()
            .filter_map(|item| item.credential_ref.clone())
    }));
    references
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpEnvironmentCredentialRef, McpServerConfig, ProviderConfig};

    #[test]
    fn settings_credentials_are_persisted_only_as_secure_references() {
        let store = InMemoryCredentialStore::default();
        let before = AppConfig::default();
        let mut after = AppConfig::default();
        after.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_key: "provider-secret-marker".into(),
            ..Default::default()
        });
        after.media.ocr.api_key = "ocr-key-marker".into();
        after.media.ocr.api_secret = "ocr-secret-marker".into();
        after.mcp_servers.push(McpServerConfig {
            name: "example".into(),
            env: vec![
                "TOKEN=mcp-token-marker".into(),
                "MODE=production".into(),
                "FLAG".into(),
                "EMPTY=".into(),
            ],
            ..Default::default()
        });

        prepare_after_edit(&before, &mut after, &store, &mut HashMap::new()).unwrap();
        assert_eq!(after.mcp_servers[0].env_refs.len(), 4);
        assert!(after.mcp_servers[0].env_refs[0].has_value);
        assert!(!after.mcp_servers[0].env_refs[2].has_value);
        assert!(after.mcp_servers[0].env_refs[3].has_value);

        let toml = toml::to_string(&after).unwrap();
        for marker in [
            "provider-secret-marker",
            "ocr-key-marker",
            "ocr-secret-marker",
            "mcp-token-marker",
            "production",
        ] {
            assert!(!toml.contains(marker), "serialized config leaked {marker}");
        }
        assert!(toml.contains("credential_ref"));
        assert!(toml.contains("name = \"MODE\""));

        let settings = crate::config::Settings::from(&after);
        assert!(settings.llm.providers[0].api_key.is_empty());
        assert!(settings.media.ocr.api_key.is_empty());
        assert!(settings.media.ocr.api_secret.is_empty());
        assert!(settings.mcp_servers[0].env.is_empty());
        let settings_wire = serde_json::to_string(&settings).unwrap();
        let debug = format!("{after:?} {settings:?}");
        for marker in [
            "provider-secret-marker",
            "ocr-key-marker",
            "ocr-secret-marker",
            "mcp-token-marker",
            "production",
        ] {
            assert!(!settings_wire.contains(marker));
            assert!(!debug.contains(marker));
        }
    }

    #[test]
    fn startup_hydrates_current_references_and_fails_when_missing() {
        let store = InMemoryCredentialStore::default();
        let provider_ref = new_id("cred");
        let mcp_ref = new_id("cred");
        store.write(&provider_ref, "provider-secret").unwrap();
        store.write(&mcp_ref, "mcp-secret").unwrap();

        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_key_ref: Some(provider_ref),
            ..Default::default()
        });
        config.mcp_servers.push(McpServerConfig {
            name: "example".into(),
            env_refs: vec![McpEnvironmentCredentialRef {
                name: "TOKEN".into(),
                credential_ref: Some(mcp_ref),
                has_value: true,
            }],
            ..Default::default()
        });

        hydrate_from_store(&mut config, &store).unwrap();
        assert_eq!(config.llm.providers[0].api_key, "provider-secret");
        assert_eq!(config.mcp_servers[0].env, ["TOKEN=mcp-secret"]);

        let mut missing = AppConfig::default();
        missing.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_key_ref: Some(new_id("cred")),
            ..Default::default()
        });
        assert!(hydrate_from_store(&mut missing, &store).is_err());
    }

    #[test]
    fn failed_settings_secret_write_leaves_the_edit_uncommitted() {
        struct FailingStore;
        impl CredentialStore for FailingStore {
            fn read(&self, _reference: &str) -> anyhow::Result<Option<String>> {
                Ok(None)
            }
            fn write(&self, _reference: &str, _value: &str) -> anyhow::Result<()> {
                anyhow::bail!("simulated secure-store write failure")
            }
            fn delete(&self, _reference: &str) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let before = AppConfig::default();
        let mut after = AppConfig::default();
        after.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_key: "must-remain-in-memory".into(),
            ..Default::default()
        });
        let error = prepare_after_edit(&before, &mut after, &FailingStore, &mut HashMap::new())
            .unwrap_err();
        assert!(error.to_string().contains("securely"));
        assert_eq!(after.llm.providers[0].api_key, "must-remain-in-memory");
        assert!(after.llm.providers[0].api_key_ref.is_none());
    }

    #[test]
    fn malformed_references_are_rejected_before_store_access() {
        let mut config = AppConfig::default();
        config.llm.providers.push(ProviderConfig {
            name: "primary".into(),
            api_key_ref: Some("Haven/../../other-app-secret".into()),
            ..Default::default()
        });
        let error =
            hydrate_from_store(&mut config, &InMemoryCredentialStore::default()).unwrap_err();
        assert!(error.to_string().contains("invalid credential reference"));
    }
}
