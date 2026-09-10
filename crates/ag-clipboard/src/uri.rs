use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use percent_encoding::percent_decode_str;

#[cfg(any(target_os = "linux", test))]
pub(crate) fn paths_from_uri_list(uri_list_bytes: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(uri_list_bytes)
        .lines()
        .filter_map(|line| path_from_file_url_text(line.trim_end_matches('\r')))
        .collect()
}

pub(crate) fn path_from_file_url_text(file_url: &str) -> Option<PathBuf> {
    let file_url = file_url.trim();
    if file_url.is_empty() || file_url.starts_with('#') {
        return None;
    }

    let path_fragment = file_url.strip_prefix("file://")?;
    let path_fragment = path_fragment
        .strip_prefix("localhost")
        .unwrap_or(path_fragment);
    if !path_fragment.starts_with('/') {
        return None;
    }

    let decoded_path = percent_decode_str(path_fragment).collect::<Vec<_>>();

    Some(PathBuf::from(os_string_from_bytes(decoded_path)))
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
#[path = "uri_test.rs"]
mod tests;
