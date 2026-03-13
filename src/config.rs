use std::path::{Path, PathBuf};

/// Core TOML metadata fields that cannot be overridden by user-defined fields.
const CORE_METADATA_FIELDS: &[&str] = &[
    "schema-version",
    "aliases",
    "abstract",
    "keywords",
    "generated",
    "checklist-status",
    "relation",
    "relation-target",
];

#[derive(Debug, Clone, PartialEq)]
pub enum MetadataFieldKind {
    String,
    Boolean,
    ArrayString,
}

#[derive(Debug, Clone)]
pub struct MetadataFieldConfig {
    pub path: String, // e.g., "user.course"
    #[allow(dead_code)]
    pub kind: MetadataFieldKind,
    pub default: toml::Value,
}

#[derive(Debug, Clone, Default)]
pub struct MetadataConfig {
    pub fields: Vec<MetadataFieldConfig>,
}

fn parse_metadata_config(table: &toml::Table) -> MetadataConfig {
    let fields_arr = match table
        .get("metadata")
        .and_then(|v| v.get("field"))
        .and_then(|v| v.as_array())
    {
        Some(a) => a.clone(),
        None => return MetadataConfig::default(),
    };

    let mut fields = Vec::new();
    for entry in &fields_arr {
        let t = match entry.as_table() {
            Some(t) => t,
            None => continue,
        };

        let path = match t.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                eprintln!("zk-lsp config: metadata.field missing 'path'");
                continue;
            }
        };

        // Validate: must be "user.<key>" with no further nesting
        if !path.starts_with("user.") || path.matches('.').count() != 1 {
            eprintln!(
                "zk-lsp config: metadata.field path '{path}' must be in 'user.*' namespace (e.g., 'user.course')"
            );
            continue;
        }
        let sub_key = &path["user.".len()..];
        if sub_key.is_empty() {
            eprintln!("zk-lsp config: metadata.field path '{path}' has empty sub-key");
            continue;
        }

        // Cannot override core fields
        if CORE_METADATA_FIELDS.contains(&path.as_str()) {
            eprintln!("zk-lsp config: metadata.field path '{path}' overrides a core field");
            continue;
        }

        let kind_str = match t.get("kind").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                eprintln!("zk-lsp config: metadata.field '{path}' missing 'kind'");
                continue;
            }
        };

        let kind = match kind_str {
            "string" => MetadataFieldKind::String,
            "boolean" => MetadataFieldKind::Boolean,
            "array-string" => MetadataFieldKind::ArrayString,
            other => {
                eprintln!(
                    "zk-lsp config: metadata.field '{path}' has unknown kind '{other}' (valid: string, boolean, array-string)"
                );
                continue;
            }
        };

        let default = match t.get("default") {
            Some(d) => d.clone(),
            None => match &kind {
                MetadataFieldKind::String => toml::Value::String(String::new()),
                MetadataFieldKind::Boolean => toml::Value::Boolean(false),
                MetadataFieldKind::ArrayString => toml::Value::Array(Vec::new()),
            },
        };

        // Validate default type matches kind
        let valid = match &kind {
            MetadataFieldKind::String => default.as_str().is_some(),
            MetadataFieldKind::Boolean => default.as_bool().is_some(),
            MetadataFieldKind::ArrayString => default
                .as_array()
                .map(|a| a.iter().all(|v| v.as_str().is_some()))
                .unwrap_or(false),
        };
        if !valid {
            eprintln!(
                "zk-lsp config: metadata.field '{path}' default type doesn't match kind '{kind_str}'"
            );
            continue;
        }

        fields.push(MetadataFieldConfig { path, kind, default });
    }

    MetadataConfig { fields }
}

fn merge_metadata(mut user: MetadataConfig, project: MetadataConfig) -> MetadataConfig {
    for pf in project.fields {
        if let Some(existing) = user.fields.iter_mut().find(|f| f.path == pf.path) {
            *existing = pf;
        } else {
            user.fields.push(pf);
        }
    }
    user
}

/// Merged zk-lsp configuration.
///
/// Load order (later overrides earlier):
/// 1. `$XDG_CONFIG_HOME/zk-lsp/config.toml`  (user-level)
/// 2. `<wiki-root>/zk-lsp.toml`               (project-level)
#[derive(Debug, Clone, Default)]
pub struct ZkLspConfig {
    /// Custom template for `zk-lsp new`. Supports `{{id}}` and `{{metadata}}`.
    pub new_note_template: Option<String>,
    /// User-defined metadata fields added to new notes.
    pub metadata: MetadataConfig,
}

impl ZkLspConfig {
    fn user_config_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_else(|_| PathBuf::from(".config"))
            });
        base.join("zk-lsp").join("config.toml")
    }

    fn from_path(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            return Self::default();
        };
        Self {
            new_note_template: table
                .get("new_note")
                .and_then(|v| v.get("template"))
                .and_then(|v| v.as_str())
                .map(String::from),
            metadata: parse_metadata_config(&table),
        }
    }

    /// Load and merge user-level then project-level config.
    pub fn load(wiki_root: &Path) -> Self {
        let user = Self::from_path(&Self::user_config_path());
        let project = Self::from_path(&wiki_root.join("zk-lsp.toml"));
        Self {
            new_note_template: project.new_note_template.or(user.new_note_template),
            metadata: merge_metadata(user.metadata, project.metadata),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WikiConfig {
    #[allow(dead_code)]
    pub root: PathBuf,
    pub note_dir: PathBuf,
    pub link_file: PathBuf,
    pub zk_config: ZkLspConfig,
}

impl WikiConfig {
    /// Resolution order: CLI flag → WIKI_ROOT env → initializationOptions → ~/wiki fallback
    pub fn resolve(cli_root: Option<PathBuf>, init_root: Option<PathBuf>) -> Self {
        let root = cli_root
            .or_else(|| std::env::var("WIKI_ROOT").ok().map(PathBuf::from))
            .or(init_root)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("wiki")
            });
        Self::from_root(root)
    }

    pub fn from_root(root: PathBuf) -> Self {
        let note_dir = root.join("note");
        let link_file = root.join("link.typ");
        let zk_config = ZkLspConfig::load(&root);
        WikiConfig { root, note_dir, link_file, zk_config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(toml_str: &str) -> ZkLspConfig {
        let table = toml_str.parse::<toml::Table>().unwrap();
        ZkLspConfig {
            new_note_template: None,
            metadata: parse_metadata_config(&table),
        }
    }

    #[test]
    fn test_valid_metadata_fields() {
        let cfg = parse_config(
            r#"
[metadata]
version = 1

[[metadata.field]]
path = "user.course"
kind = "string"
default = ""

[[metadata.field]]
path = "user.priority"
kind = "string"
default = "normal"

[[metadata.field]]
path = "user.tags"
kind = "array-string"
default = []

[[metadata.field]]
path = "user.done"
kind = "boolean"
default = false
"#,
        );
        assert_eq!(cfg.metadata.fields.len(), 4);
        assert_eq!(cfg.metadata.fields[0].path, "user.course");
        assert_eq!(cfg.metadata.fields[0].kind, MetadataFieldKind::String);
        assert_eq!(cfg.metadata.fields[2].path, "user.tags");
        assert_eq!(cfg.metadata.fields[2].kind, MetadataFieldKind::ArrayString);
        assert_eq!(cfg.metadata.fields[3].kind, MetadataFieldKind::Boolean);
    }

    #[test]
    fn test_invalid_path_no_user_prefix() {
        let cfg = parse_config(
            r#"
[[metadata.field]]
path = "course"
kind = "string"
default = ""
"#,
        );
        assert!(cfg.metadata.fields.is_empty(), "path without user. prefix should be rejected");
    }

    #[test]
    fn test_invalid_path_nested() {
        let cfg = parse_config(
            r#"
[[metadata.field]]
path = "user.a.b"
kind = "string"
default = ""
"#,
        );
        assert!(cfg.metadata.fields.is_empty(), "nested path should be rejected");
    }

    #[test]
    fn test_invalid_kind() {
        let cfg = parse_config(
            r#"
[[metadata.field]]
path = "user.x"
kind = "enum"
default = "a"
"#,
        );
        assert!(cfg.metadata.fields.is_empty(), "unknown kind should be rejected");
    }

    #[test]
    fn test_default_type_mismatch() {
        let cfg = parse_config(
            r#"
[[metadata.field]]
path = "user.x"
kind = "boolean"
default = "not-a-bool"
"#,
        );
        assert!(cfg.metadata.fields.is_empty(), "default type mismatch should be rejected");
    }

    #[test]
    fn test_cannot_override_core_fields() {
        // "schema-version" is a core field — but also, it wouldn't pass the user.* path check
        // The path must start with "user." so any core field name won't pass anyway.
        // Still test explicitly that a path matching a core field name is rejected by the namespace rule.
        let cfg = parse_config(
            r#"
[[metadata.field]]
path = "relation"
kind = "string"
default = ""
"#,
        );
        assert!(cfg.metadata.fields.is_empty(), "core field path should be rejected");
    }

    #[test]
    fn test_merge_metadata_project_wins() {
        let user = MetadataConfig {
            fields: vec![MetadataFieldConfig {
                path: "user.course".into(),
                kind: MetadataFieldKind::String,
                default: toml::Value::String("default".into()),
            }],
        };
        let project = MetadataConfig {
            fields: vec![MetadataFieldConfig {
                path: "user.course".into(),
                kind: MetadataFieldKind::String,
                default: toml::Value::String("project".into()),
            }],
        };
        let merged = merge_metadata(user, project);
        assert_eq!(merged.fields.len(), 1);
        assert_eq!(merged.fields[0].default.as_str(), Some("project"));
    }
}
