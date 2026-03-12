use std::path::{Path, PathBuf};

/// Merged zk-lsp configuration.
///
/// Load order (later overrides earlier):
/// 1. `$XDG_CONFIG_HOME/zk-lsp/config.toml`  (user-level)
/// 2. `<wiki-root>/zk-lsp.toml`               (project-level)
#[derive(Debug, Clone, Default)]
pub struct ZkLspConfig {
    /// Custom template for `zk-lsp new`. Supports `{{id}}` and `{{metadata}}`.
    pub new_note_template: Option<String>,
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
        let Ok(raw) = std::fs::read_to_string(path) else { return Self::default() };
        let Ok(table) = raw.parse::<toml::Table>() else { return Self::default() };
        Self {
            new_note_template: table
                .get("new_note")
                .and_then(|v| v.get("template"))
                .and_then(|v| v.as_str())
                .map(String::from),
        }
    }

    /// Load and merge user-level then project-level config.
    pub fn load(wiki_root: &Path) -> Self {
        let user = Self::from_path(&Self::user_config_path());
        let project = Self::from_path(&wiki_root.join("zk-lsp.toml"));
        Self {
            new_note_template: project.new_note_template.or(user.new_note_template),
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
