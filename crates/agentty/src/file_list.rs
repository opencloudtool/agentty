use std::path::Path;

use ignore::WalkBuilder;

const MAX_DEPTH: usize = 10;
const MAX_FILES: usize = 500;

/// A single file entry for the `@` mention dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path from the listing root (e.g., `src/main.rs`).
    pub path: String,
}

/// Lists files recursively under `root`, respecting `.gitignore`.
///
/// Returns at most [`MAX_FILES`] entries with a maximum depth of
/// [`MAX_DEPTH`]. Results are sorted alphabetically by path.
pub fn list_files(root: &Path) -> Vec<FileEntry> {
    let walker = WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH))
        .hidden(false)
        .build();

    let mut entries: Vec<FileEntry> = walker
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .ok()
                .map(|relative| FileEntry {
                    path: relative.to_string_lossy().to_string(),
                })
        })
        .collect();

    sort_and_limit_entries(&mut entries);

    entries
}

fn sort_and_limit_entries(entries: &mut Vec<FileEntry>) {
    entries.sort_by(|first, second| first.path.cmp(&second.path));
    entries.truncate(MAX_FILES);
}

/// Fuzzy-filters entries and returns them sorted by best match.
///
/// Query characters must appear in order (case-insensitive) within the
/// path. Results are ranked by: consecutive-character runs, matches at
/// the start of path segments (`/`, `.`), and filename matches.
pub fn filter_entries<'a>(entries: &'a [FileEntry], query: &str) -> Vec<&'a FileEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }

    let query_chars: Vec<char> = query.to_lowercase().chars().collect();
    let mut scored: Vec<(&FileEntry, i32)> = entries
        .iter()
        .filter_map(|entry| fuzzy_score(&entry.path, &query_chars).map(|score| (entry, score)))
        .collect();

    scored.sort_by(|first, second| {
        second
            .1
            .cmp(&first.1)
            .then(first.0.path.cmp(&second.0.path))
    });

    scored.into_iter().map(|(entry, _)| entry).collect()
}

/// Scores a fuzzy match of `query_chars` against `path`.
///
/// Returns `Some(score)` if all query characters appear in order,
/// `None` if the path does not match.
fn fuzzy_score(path: &str, query_chars: &[char]) -> Option<i32> {
    let path_lower: Vec<char> = path.to_lowercase().chars().collect();
    let path_chars: Vec<char> = path.chars().collect();

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
        Some(score)
    } else {
        None
    }
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
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "src/main.rs");
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
    fn test_list_files_excludes_directories() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        fs::create_dir_all(temp_dir.path().join("subdir")).expect("test expectation should hold");
        fs::write(temp_dir.path().join("file.txt"), "").expect("test expectation should hold");

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "file.txt");
    }

    #[test]
    fn test_list_files_respects_max_files() {
        // Arrange
        let temp_dir = TempDir::new().expect("test expectation should hold");
        for index in 0..MAX_FILES + 50 {
            fs::write(temp_dir.path().join(format!("file_{index:04}.txt")), "")
                .expect("test expectation should hold");
        }

        // Act
        let entries = list_files(temp_dir.path());

        // Assert
        assert_eq!(entries.len(), MAX_FILES);
    }

    #[test]
    fn test_sort_and_limit_entries_truncates_after_sort() {
        // Arrange
        let mut entries: Vec<FileEntry> = (0..MAX_FILES + 20)
            .rev()
            .map(|index| FileEntry {
                path: format!("file_{index:04}.txt"),
            })
            .collect();

        // Act
        sort_and_limit_entries(&mut entries);

        // Assert
        assert_eq!(entries.len(), MAX_FILES);
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
                path: "src/Main.rs".to_string(),
            },
            FileEntry {
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
                path: "a.txt".to_string(),
            },
            FileEntry {
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
                path: "src/main.rs".to_string(),
            },
            FileEntry {
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
                path: "src/xmxaxixn.rs".to_string(),
            },
            FileEntry {
                path: "src/main.rs".to_string(),
            },
        ];

        // Act — "main" is consecutive in "src/main.rs" but scattered in the other
        let filtered = filter_entries(&entries, "main");

        // Assert — consecutive match ranked first
        assert_eq!(filtered[0].path, "src/main.rs");
    }

    #[test]
    fn test_filter_entries_fuzzy_ranks_segment_start_higher() {
        // Arrange
        let entries = vec![
            FileEntry {
                path: "docs/domain.rs".to_string(),
            },
            FileEntry {
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
                path: "src/app/mod.rs".to_string(),
            },
            FileEntry {
                path: "tests/unit.rs".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "app/mod");

        // Assert
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "src/app/mod.rs");
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
