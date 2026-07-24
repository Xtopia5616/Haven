use std::collections::HashMap;

/// §2.7: Auth resolver with environment variable → settings → API key input chain
pub struct AuthResolver {
    env_prefix: String,
    overrides: HashMap<String, String>,
}

impl AuthResolver {
    pub fn new(env_prefix: &str) -> Self {
        Self {
            env_prefix: env_prefix.to_string(),
            overrides: HashMap::new(),
        }
    }

    /// Set a runtime override (e.g. from settings page API key input)
    pub fn set_override(&mut self, provider: &str, key: String) {
        self.overrides.insert(provider.to_string(), key);
    }

    fn env_var_name(&self, provider: &str) -> String {
        let upper = provider.to_uppercase().replace('-', "_");
        format!("{}_{}_API_KEY", self.env_prefix, upper)
    }

    pub fn resolve(&self, provider: &str, default: &str) -> String {
        let env_name = self.env_var_name(provider);
        if let Some(key) = self.overrides.get(provider)
            && !key.is_empty()
        {
            return key.clone();
        }
        std::env::var(&env_name)
            .ok()
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// Clear all overrides
    pub fn clear_overrides(&mut self) {
        self.overrides.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_override_first() {
        let mut resolver = AuthResolver::new("HAVEN");
        resolver.set_override("openai", "sk-override".into());
        let key = resolver.resolve("openai", "default");
        assert_eq!(key, "sk-override");
    }

    #[test]
    fn resolve_falls_back_to_default() {
        let resolver = AuthResolver::new("HAVEN");
        let key = resolver.resolve("openai", "default-key");
        assert_eq!(key, "default-key");
    }

    #[test]
    fn env_var_name_constructed_correctly() {
        let resolver = AuthResolver::new("HAVEN");
        assert_eq!(
            resolver.env_var_name("openai"),
            "HAVEN_OPENAI_API_KEY"
        );
        assert_eq!(
            resolver.env_var_name("deep-seek"),
            "HAVEN_DEEP_SEEK_API_KEY"
        );
    }
}