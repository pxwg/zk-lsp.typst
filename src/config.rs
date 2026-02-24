use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WikiConfig {
    pub root: PathBuf,
    pub note_dir: PathBuf,
    pub link_file: PathBuf,
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
        WikiConfig { root, note_dir, link_file }
    }
}
