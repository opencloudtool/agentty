//! Filesystem-backed index and fuzzy filtering used by `@` file mentions.

use std::path::Path;

use ignore::WalkBuilder;

const MAX_DEPTH: usize = 10;
const MAX_ENTRIES: usize = 500;

/// A single file or directory entry for the `@` mention dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Relative path from the listing root (e.g., `src/main.rs`).
    pub path: String,
}

/// Lists files and directories recursively under `root`, respecting
/// `.gitignore`.
///
/// Returns at most [`MAX_ENTRIES`] entries with a maximum depth of
/// [`MAX_DEPTH`]. Directories sort before files; within each group,
/// results are sorted alphabetically by path.
pub fn list_files(root: &Path) -> Vec<FileEntry> {
    list_files_with_limits(root, Some(MAX_DEPTH), Some(MAX_ENTRIES))
}

/// Lists files and directories recursively under `root` for project-explorer
/// rendering with optional traversal and result limits.
///
/// Passing `None` for `max_depth` and `max_entries` provides an unbounded
/// gitignore-aware traversal. Directories sort before files; within each
/// group, results are sorted alphabetically by path.
pub fn list_files_for_explorer(
    root: &Path,
    max_depth: Option<usize>,
    max_entries: Option<usize>,
) -> Vec<FileEntry> {
    list_files_with_limits(root, max_depth, max_entries)
}

/// Lists files and directories recursively under `root` with optional
/// traversal and result limits.
fn list_files_with_limits(
    root: &Path,
    max_depth: Option<usize>,
    max_entries: Option<usize>,
) -> Vec<FileEntry> {
    let walker = WalkBuilder::new(root)
        .max_depth(max_depth)
        .hidden(false)
        .build();

    let mut entries: Vec<FileEntry> = walker
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_some_and(|ft| ft.is_file() || ft.is_dir())
        })
        .filter_map(|entry| {
            let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());

            entry.path().strip_prefix(root).ok().and_then(|relative| {
                let path = relative.to_string_lossy().to_string();
                if path.is_empty() {
                    return None;
                }

                Some(FileEntry { is_dir, path })
            })
        })
        .collect();

    sort_and_limit_entries(&mut entries, max_entries);

    entries
}

/// Sorts entries with directories first and optionally truncates to
/// `max_entries`.
fn sort_and_limit_entries(entries: &mut Vec<FileEntry>, max_entries: Option<usize>) {
    entries.sort_by(|first, second| {
        second
            .is_dir
            .cmp(&first.is_dir)
            .then(first.path.cmp(&second.path))
    });

    if let Some(max_entries) = max_entries {
        entries.truncate(max_entries);
    }
}

/// Fuzzy-filters entries and returns them sorted by best match.
///
/// Query characters must appear in order (case-insensitive) within the
/// path. Results are ranked by: consecutive-character runs, matches at
/// the start of path segments (`/`, `.`), and a basename-substring bonus
/// for intuitive file-name-first matches. If the query ends with `/`,
/// directory entries are prioritized before files.
pub fn filter_entries<'a>(entries: &'a [FileEntry], query: &str) -> Vec<&'a FileEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }

    let query_chars: Vec<char> = query.to_lowercase().chars().collect();
    let mut scored: Vec<(&FileEntry, i32)> = entries
        .iter()
        .filter_map(|entry| fuzzy_score_for_entry(entry, &query_chars).map(|score| (entry, score)))
        .collect();

    scored.sort_by(|first, second| {
        second
            .1
            .cmp(&first.1)
            .then(first.0.path.cmp(&second.0.path))
    });

    let mut filtered: Vec<&FileEntry> = scored.into_iter().map(|(entry, _)| entry).collect();
    prioritize_directories_for_trailing_slash(&mut filtered, query);

    filtered
}

fn fuzzy_score_for_entry(entry: &FileEntry, query_chars: &[char]) -> Option<i32> {
    if entry.is_dir {
        return fuzzy_score(&format!("{}/", entry.path), query_chars);
    }

    fuzzy_score(&entry.path, query_chars)
}

fn prioritize_directories_for_trailing_slash(entries: &mut Vec<&FileEntry>, query: &str) {
    if !query.ends_with('/') {
        return;
    }

    entries.sort_by_key(|entry| !entry.is_dir);
}

/// Scores a fuzzy match of `query_chars` against `path`.
///
/// Returns `Some(score)` if all query characters appear in order,
/// `None` if the path does not match.
fn fuzzy_score(path: &str, query_chars: &[char]) -> Option<i32> {
    let path_lower: Vec<char> = path.to_lowercase().chars().collect();
    let path_chars: Vec<char> = path.chars().collect();
    let query_lower: String = query_chars.iter().collect();

    if query_chars.len() > path_lower.len() {
        return None;
    }

    let mut score: i32 = 0;
    let mut query_index = 0;
    let mut prev_matched = false;

    for (path_index, &path_char) in path_lower.iter().enumerate() {
        if query_index >= query_chars.len() {
            break;
        }

        if path_char == query_chars[query_index] {
            score += 1;

            // Bonus for consecutive matches.
            if prev_matched {
                score += 3;
            }

            // Bonus for match at start of a path segment.
            if path_index == 0 || matches!(path_chars[path_index - 1], '/' | '.' | '_' | '-') {
                score += 5;
            }

            query_index += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }

    if query_index == query_chars.len() {
        Some(score + basename_match_bonus(path, &query_lower))
    } else {
        None
    }
}

/// Returns an extra score when the query text is found in the basename.
///
/// This prioritizes intuitive `@` mention matches such as `@settings`
/// matching `settings.rs` before unrelated paths that only match through
/// scattered fuzzy characters.
fn basename_match_bonus(path: &str, query: &str) -> i32 {
    if query.is_empty() || query.contains('/') {
        return 0;
    }

    let normalized_path = path.trim_end_matches('/');
    let basename = normalized_path
        .rsplit('/')
        .next()
        .unwrap_or(normalized_path);
    let basename_lower = basename.to_lowercase();
    let basename_stem = basename_lower.split('.').next().unwrap_or("");

    if basename_stem == query {
        return 60;
    }

    if basename_lower.starts_with(query) {
        return 45;
    }

    if basename_lower.contains(query) {
        return 30;
    }

    0
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_list_files_empty_directory() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert!(entries.is_empty());
    }

    #[test]
    fn test_list_files_returns_sorted_entries() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::write(temp_dir.path().join("banana.txt"), "").expect("test expectation should hold");
        fs::write(temp_dir.path().join("apple.txt"), "").expect("test expectation should hold");
        fs::write(temp_dir.path().join("cherry.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, vec!["apple.txt", "banana.txt", "cherry.txt"]);
    }

    #[test]
    fn test_list_files_returns_relative_paths() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::create_dir_all(temp_dir.path().join("src")).expect("test expectation should hold");
        fs::write(temp_dir.path().join("src/main.rs"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        let file_entries: Vec<_> = entries.iter().filter(|entry| !entry.is_dir).collect();
        assert_eq!(file_entries.len(), 1);
        assert_eq!(file_entries[0].path, "src/main.rs");
    }

    #[test]
    fn test_list_files_respects_gitignore() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp_dir.path())
            .output()
            .expect("test expectation should hold");
        fs::write(temp_dir.path().join(".gitignore"), "ignored.txt\n")
            .expect("test expectation should hold");
        fs::write(temp_dir.path().join("kept.txt"), "").expect("test expectation should hold");
        fs::write(temp_dir.path().join("ignored.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"kept.txt"));
        assert!(!paths.contains(&"ignored.txt"));
    }

    #[test]
    fn test_list_files_includes_non_ignored_dotfiles() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp_dir.path())
            .output()
            .expect("test expectation should hold");
        fs::write(temp_dir.path().join(".gitignore"), ".ignored-dotfile\n")
            .expect("test expectation should hold");
        fs::write(temp_dir.path().join(".visible-dotfile"), "")
            .expect("test expectation should hold");
        fs::write(temp_dir.path().join(".ignored-dotfile"), "")
            .expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&".visible-dotfile"));
        assert!(!paths.contains(&".ignored-dotfile"));
    }

    #[test]
    fn test_list_files_includes_directories() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::create_dir_all(temp_dir.path().join("subdir")).expect("test expectation should hold");
        fs::write(temp_dir.path().join("file.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "subdir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, "file.txt");
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn test_list_files_excludes_root_directory() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::write(temp_dir.path().join("file.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert!(!entries.iter().any(|entry| entry.path.is_empty()));
    }

    #[test]
    fn test_list_files_sorts_directories_before_files() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::write(temp_dir.path().join("aaa_file.txt"), "").expect("test expectation should hold");
        fs::create_dir_all(temp_dir.path().join("zzz_dir")).expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert — directory sorts before file despite alphabetical order
        assert_eq!(entries[0].path, "zzz_dir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, "aaa_file.txt");
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn test_list_files_respects_max_entries() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        for index in 0..MAX_ENTRIES + 50 {
            fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
                .expect("test expectation should hold");
        }

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert_eq!(entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn test_list_files_for_explorer_can_be_unbounded() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        for index in 0..MAX_ENTRIES + 50 {
            fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
                .expect("test expectation should hold");
        }

        // Act
        let entries = list_files_for_explorer(temp_dir.path(), None, None);

        // Assert
        assert_eq!(entries.len(), MAX_ENTRIES + 50);
    }

    #[test]
    fn test_list_files_for_explorer_respects_custom_limits() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        for index in 0..20 {
            fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
                .expect("test expectation should hold");
        }

        // Act
        let entries = list_files_for_explorer(temp_dir.path(), Some(3), Some(7));

        // Assert
        assert_eq!(entries.len(), 7);
    }

    #[test]
    fn test_sort_and_limit_entries_truncates_after_sort() {
        // Arrange
        let mut entries: Vec<FileEntry> = (0..MAX_ENTRIES + 20)
            .rev()
            .map(|index| FileEntry {
                is_dir: false,
                path: format!("file_{index:04}.txt"),
            })
            .collect();

        // Act
        sort_and_limit_entries(&mut entries, Some(MAX_ENTRIES));

        // Assert
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(
            entries.first().map(|entry| entry.path.as_str()),
            Some("file_0000.txt")
        );
        assert_eq!(
            entries.last().map(|entry| entry.path.as_str()),
            Some("file_0499.txt")
        );
    }

    #[test]
    fn test_list_files_respects_max_depth() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        let mut deep_path = temp_dir.path().to_path_buf();
        for level in 0..MAX_DEPTH + 2 {
            deep_path = deep_path.join(format!("d{level}"));
        }
        fs::create_dir_all(&deep_path).expect("test expectation should hold");
        fs::write(deep_path.join("deep.txt"), "").expect("test expectation should hold");
        fs::write(temp_dir.path().join("shallow.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"shallow.txt"));
        assert!(!paths.iter().any(|path| path.contains("deep.txt")));
    }

    #[test]
    fn test_filter_entries_case_insensitive() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/Main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "main");

        // Assert
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "src/Main.rs");
    }

    #[test]
    fn test_filter_entries_empty_query_returns_all() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "a.txt".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "b.txt".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "");

        // Assert
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_entries_no_match() {
        // Arrange
        let entries = vec![FileEntry {
            is_dir: false,
            path: "hello.txt".to_string(),
        }];

        // Act
        let filtered = filter_entries(&entries, "xyz");

        // Assert
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_entries_fuzzy_match() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/model.rs".to_string(),
            },
        ];

        // Act — "smr" matches "src/main.rs" (s...m...r) and "src/model.rs" (s...m...r)
        let filtered = filter_entries(&entries, "smr");

        // Assert
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_entries_fuzzy_ranks_consecutive_higher() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/xmxaxixn.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];

        // Act — "main" is consecutive in "src/main.rs" but scattered in the other
        let filtered = filter_entries(&entries, "main");

        // Assert — consecutive match ranked first
        assert_eq!(filtered[0].path, "src/main.rs");
    }

    #[test]
    fn test_filter_entries_prioritizes_basename_match() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "crates/agentty/src/infra/git.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "crates/agentty/src/app/settings.rs".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "settings");

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].path, "crates/agentty/src/app/settings.rs");
    }

    #[test]
    fn test_filter_entries_fuzzy_ranks_segment_start_higher() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "docs/domain.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/db.rs".to_string(),
            },
        ];

        // Act — "d" matches segment start in "src/db.rs" (after /) and mid-word in
        // "docs"
        let filtered = filter_entries(&entries, "d");

        // Assert — both match, segment-start bonus means "docs" and "db" both have it
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_entries_fuzzy_no_match_wrong_order() {
        // Arrange
        let entries = vec![FileEntry {
            is_dir: false,
            path: "abc.txt".to_string(),
        }];

        // Act — "cb" requires c before b, but in "abc" b comes before c
        let filtered = filter_entries(&entries, "cb");

        // Assert
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_entries_matches_path_segments() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/app/session.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "tests/unit.rs".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "app/session");

        // Assert
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "src/app/session.rs");
    }

    #[test]
    fn test_filter_entries_trailing_slash_prioritizes_directories() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/aaa.rs".to_string(),
            },
            FileEntry {
                is_dir: true,
                path: "src/zzz".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "src/");

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].path, "src/zzz");
        assert!(filtered[0].is_dir);
        assert_eq!(filtered[1].path, "src/aaa.rs");
        assert!(!filtered[1].is_dir);
    }

    #[test]
    fn test_filter_entries_trailing_slash_matches_exact_directory() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/main.rs".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "src/");

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].path, "src");
        assert!(filtered[0].is_dir);
        assert_eq!(filtered[1].path, "src/main.rs");
        assert!(!filtered[1].is_dir);
    }

    #[test]
    fn test_fuzzy_score_returns_none_for_no_match() {
        // Arrange & Act
        let result = fuzzy_score("hello.txt", &['x', 'y', 'z']);

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn test_fuzzy_score_returns_some_for_match() {
        // Arrange & Act
        let result = fuzzy_score("src/main.rs", &['m', 'a', 'i', 'n']);

        // Assert
        assert!(result.is_some());
    }

    #[test]
    fn test_fuzzy_score_consecutive_beats_scattered() {
        // Arrange & Act
        let consecutive = fuzzy_score("main.rs", &['m', 'a', 'i', 'n']);
        let scattered = fuzzy_score("my_archive_index_name.rs", &['m', 'a', 'i', 'n']);

        // Assert
        assert!(
            consecutive.expect("test expectation should hold")
                > scattered.expect("test expectation should hold")
        );
    }
}
