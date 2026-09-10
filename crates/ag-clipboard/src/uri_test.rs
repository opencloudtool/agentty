use super::*;

#[test]
fn test_paths_from_uri_list_ignores_comments_and_decodes_file_paths() {
    // Arrange
    let uri_list =
        b"# copied files\r\nfile:///tmp/image%201.png\r\nfile://localhost/tmp/second.png\r\n";

    // Act
    let paths = paths_from_uri_list(uri_list);

    // Assert
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/tmp/image 1.png"),
            PathBuf::from("/tmp/second.png")
        ]
    );
}

#[test]
fn test_path_from_file_url_text_rejects_non_file_and_remote_urls() {
    // Arrange
    let http_url = "https://example.com/image.png";
    let remote_file_url = "file://example.com/tmp/image.png";

    // Act
    let http_path = path_from_file_url_text(http_url);
    let remote_file_path = path_from_file_url_text(remote_file_url);

    // Assert
    assert_eq!(http_path, None);
    assert_eq!(remote_file_path, None);
}
