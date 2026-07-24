use thiserror::Error;

#[derive(Debug, Error)]
pub enum HavenError {
    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("database error: {0}")]
    Database(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type HavenResult<T> = Result<T, HavenError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_display() {
        let err = HavenError::Config("missing config key".into());
        assert!(err.to_string().contains("missing config key"));
        assert!(err.to_string().contains("config error"));
    }

    #[test]
    fn io_error_display_and_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let haven_err: HavenError = io_err.into();
        assert!(haven_err.to_string().contains("file not found"));
        assert!(haven_err.to_string().contains("io error"));
    }

    #[test]
    fn serde_error_display_and_from() {
        let serde_err = serde_json::from_str::<i32>("\"not a number\"")
            .unwrap_err();
        let haven_err: HavenError = serde_err.into();
        assert!(haven_err.to_string().contains("serialization error"));
        let displayed = haven_err.to_string();
        assert!(!displayed.is_empty());
    }

    #[test]
    fn toml_parse_error_display_and_from() {
        let toml_err = "=invalid".parse::<toml::Value>().unwrap_err();
        let haven_err: HavenError = toml_err.into();
        assert!(haven_err.to_string().contains("toml parse error"));
    }

    #[test]
    fn toml_serialize_error_display_and_from() {
        use serde::ser::Error as _;
        let err = HavenError::TomlSerialize(toml::ser::Error::custom("custom_type"));
        assert!(err.to_string().contains("toml serialize error"));
        assert!(err.to_string().contains("custom_type"));
    }

    #[test]
    fn database_error_display() {
        let err = HavenError::Database("connection lost".into());
        assert!(err.to_string().contains("connection lost"));
        assert!(err.to_string().contains("database error"));
    }

    #[test]
    fn not_found_error_display() {
        let err = HavenError::NotFound("task_123".into());
        assert!(err.to_string().contains("task_123"));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn invalid_input_error_display() {
        let err = HavenError::InvalidInput("negative timeout".into());
        assert!(err.to_string().contains("negative timeout"));
        assert!(err.to_string().contains("invalid input"));
    }

    #[test]
    fn internal_error_display() {
        let err = HavenError::Internal("unexpected state".into());
        assert!(err.to_string().contains("unexpected state"));
        assert!(err.to_string().contains("internal error"));
    }

    #[test]
    fn from_io_error_conversion() {
        let io_err =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let haven_err: HavenError = io_err.into();
        match haven_err {
            HavenError::Io(_) => {}
            other => panic!("expected Io variant, got: {:?}", other),
        }
    }

    #[test]
    fn from_serde_json_error_conversion() {
        let json_err =
            serde_json::from_str::<i32>("not a number").unwrap_err();
        let haven_err: HavenError = json_err.into();
        match haven_err {
            HavenError::Serde(_) => {}
            other => panic!("expected Serde variant, got: {:?}", other),
        }
    }

    #[test]
    fn from_toml_de_error_conversion() {
        let toml_err = "=bad toml".parse::<toml::Table>().unwrap_err();
        let haven_err: HavenError = toml_err.into();
        match haven_err {
            HavenError::TomlParse(_) => {}
            other => panic!("expected TomlParse variant, got: {:?}", other),
        }
    }

    #[test]
    fn from_toml_ser_error_conversion() {
        use serde::ser::Error as _;
        let toml_err = toml::ser::Error::custom("custom_type");
        let haven_err: HavenError = toml_err.into();
        match haven_err {
            HavenError::TomlSerialize(_) => {}
            other => panic!("expected TomlSerialize variant, got: {:?}", other),
        }
    }

    #[test]
    fn all_variants_implement_display() {
        use serde::ser::Error as _;
        let errors: Vec<HavenError> = vec![
            HavenError::Config("cfg".into()),
            HavenError::Io(std::io::Error::new(std::io::ErrorKind::Other, "io")),
            HavenError::Serde(serde_json::from_str::<i32>("\"\"").unwrap_err()),
            HavenError::TomlParse("key=value".parse::<toml::Value>().unwrap_err()),
            HavenError::TomlSerialize(toml::ser::Error::custom("x")),
            HavenError::Database("db".into()),
            HavenError::NotFound("nf".into()),
            HavenError::InvalidInput("ii".into()),
            HavenError::Internal("int".into()),
        ];
        for err in &errors {
            let s = err.to_string();
            assert!(!s.is_empty(), "Display should produce non-empty string");
        }
    }
}
