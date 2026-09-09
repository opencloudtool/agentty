use tempfile::tempdir;

use super::*;

#[test]
fn test_check_prefixes_no_duplicates() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");
    fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
    fs::write(dir.path().join("002_create_orders.sql"), "").expect("write");

    // Act
    let result = check_prefixes(dir.path());

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_check_prefixes_with_duplicates() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");
    fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
    fs::write(dir.path().join("001_create_orders.sql"), "").expect("write");

    // Act
    let result = check_prefixes(dir.path());

    // Assert
    assert!(result.is_err());
    let err = result.expect_err("expected duplicate prefix error");
    assert!(err.contains("Duplicate migration prefix `001`"), "{err}");
    assert!(err.contains("001_create_orders.sql"), "{err}");
    assert!(err.contains("001_create_users.sql"), "{err}");
}

#[test]
fn test_check_prefixes_ignores_non_sql() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");
    fs::write(dir.path().join("001_create_users.sql"), "").expect("write");
    fs::write(dir.path().join("001_readme.md"), "").expect("write");

    // Act
    let result = check_prefixes(dir.path());

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_check_prefixes_empty_dir() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");

    // Act
    let result = check_prefixes(dir.path());

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_find_migration_dirs() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");
    let crate_dir = dir.path().join("my-crate");
    fs::create_dir_all(crate_dir.join("migrations")).expect("mkdir");

    // Act
    let dirs = find_migration_dirs(dir.path());

    // Assert
    assert_eq!(dirs.len(), 1);
    assert!(dirs[0].ends_with("migrations"));
}

#[test]
fn test_find_migration_dirs_no_migrations() {
    // Arrange
    let dir = tempdir().expect("Failed to create temp dir");
    fs::create_dir_all(dir.path().join("my-crate/src")).expect("mkdir");

    // Act
    let dirs = find_migration_dirs(dir.path());

    // Assert
    assert_eq!(dirs, Vec::<PathBuf>::new());
}
