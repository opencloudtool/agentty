use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::error;

/// Validates that each crate migration directory has unique numeric prefixes.
///
/// # Errors
/// Returns an error when duplicate migration prefixes are found.
pub(crate) fn run() -> Result<(), String> {
    let migration_dirs = find_migration_dirs(Path::new("crates"));

    for dir in migration_dirs {
        check_prefixes(&dir)?;
    }

    Ok(())
}

fn find_migration_dirs(base: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(err) => {
            error!("Failed to read {}: {}", base.display(), err);

            return Vec::new();
        }
    };

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let migrations_path = entry.path().join("migrations");
        if migrations_path.is_dir() {
            dirs.push(migrations_path);
        }
    }
    dirs.sort();

    dirs
}

fn check_prefixes(dir: &Path) -> Result<(), String> {
    let entries =
        fs::read_dir(dir).map_err(|err| format!("Failed to read {}: {err}", dir.display()))?;

    let mut prefix_map: HashMap<String, Vec<String>> = HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sql") {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if let Some(prefix) = file_name.split('_').next() {
                prefix_map
                    .entry(prefix.to_string())
                    .or_default()
                    .push(file_name);
            }
        }
    }

    for (prefix, files) in &mut prefix_map {
        files.sort();
        if files.len() > 1 {
            return Err(format!(
                "Duplicate migration prefix `{prefix}` in {}: {}",
                dir.display(),
                files.join(", ")
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "check_migration_test.rs"]
mod tests;
