/// A single file or directory entry for the `@` mention dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Relative path from the listing root (e.g., `src/main.rs`).
    pub path: String,
}
