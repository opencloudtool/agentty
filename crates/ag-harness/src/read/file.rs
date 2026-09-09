use std::path::Path;

use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader};

use super::runtime::{MAX_READ_BYTES, MAX_READ_LINES, MAX_SCAN_BYTES, ReadTool};
use super::{ReadError, ReadOutput};
use crate::tool::ReadArguments;

impl ReadTool {
    pub(super) async fn execute_file(
        &self,
        arguments: &ReadArguments,
        requested_path: &str,
    ) -> Result<ReadOutput, ReadError> {
        let root = self
            .file_system
            .canonicalize(&self.repository_root)
            .await
            .map_err(|source| ReadError::RepositoryRoot { source })?;
        let path = requested_path.to_string();
        let candidate = root.join(Path::new(&path));
        let canonical_path = self
            .file_system
            .canonicalize(&candidate)
            .await
            .map_err(|source| ReadError::ResolvePath {
                path: path.clone(),
                source,
            })?;
        if !canonical_path.starts_with(&root) || canonical_path == root {
            return Err(ReadError::OutsideRepository { path });
        }
        let file = self
            .file_system
            .open_beneath(&root, Path::new(&path))
            .await
            .map_err(|source| ReadError::Open {
                path: path.clone(),
                source,
            })?;

        Self::read(file, arguments, path, false).await
    }

    pub(super) async fn read(
        file: Box<dyn AsyncRead + Send + Unpin>,
        arguments: &ReadArguments,
        path: String,
        source_truncated: bool,
    ) -> Result<ReadOutput, ReadError> {
        let start_line = arguments.offset().unwrap_or(1);
        let requested_lines = arguments.limit().unwrap_or(MAX_READ_LINES);
        let selected_lines = requested_lines.min(MAX_READ_LINES);
        let file: Box<dyn AsyncRead + Send + Unpin> =
            Box::new(file.take((MAX_SCAN_BYTES + 1) as u64));
        let mut reader = BufReader::new(file);
        let mut remaining_scan_bytes = MAX_SCAN_BYTES;

        Self::skip_to_line(
            &mut reader,
            start_line,
            &path,
            source_truncated,
            &mut remaining_scan_bytes,
        )
        .await?;

        let mut content = String::new();
        let mut current_line = start_line;
        let mut lines_read = 0_u64;
        let mut next_offset = None;
        while lines_read < selected_lines {
            let Some(line) =
                Self::next_line(&mut reader, current_line, &path, &mut remaining_scan_bytes)
                    .await?
            else {
                if source_truncated {
                    return Err(ReadError::ScanLimitExceeded {
                        limit: MAX_SCAN_BYTES,
                        path,
                    });
                }
                break;
            };
            let line = Self::decode_line(line, current_line, &path)?;
            let separator_bytes = usize::from(lines_read > 0);
            if content
                .len()
                .checked_add(separator_bytes)
                .and_then(|bytes| bytes.checked_add(line.len()))
                .is_none_or(|bytes| bytes > MAX_READ_BYTES)
            {
                next_offset = Some(current_line);
                break;
            }
            if separator_bytes > 0 {
                content.push('\n');
            }
            content.push_str(&line);
            lines_read += 1;
            current_line += 1;
        }

        if next_offset.is_none()
            && lines_read == selected_lines
            && (source_truncated || Self::has_more(&mut reader, &path).await?)
        {
            next_offset = Some(current_line);
        }
        if lines_read == 0 && start_line > 1 {
            return Err(ReadError::OffsetBeyondEnd {
                offset: start_line,
                path,
            });
        }
        let end_line = lines_read
            .checked_sub(1)
            .and_then(|additional_lines| start_line.checked_add(additional_lines));

        Ok(ReadOutput {
            content,
            end_line,
            next_offset,
            path,
            start_line,
            truncated: next_offset.is_some(),
        })
    }

    async fn skip_to_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        start_line: u64,
        path: &str,
        source_truncated: bool,
        remaining_scan_bytes: &mut usize,
    ) -> Result<(), ReadError> {
        let mut current_line = 1_u64;
        while current_line < start_line {
            if !Self::skip_line(reader, path, remaining_scan_bytes).await? {
                return Err(if source_truncated {
                    ReadError::ScanLimitExceeded {
                        limit: MAX_SCAN_BYTES,
                        path: path.to_string(),
                    }
                } else {
                    ReadError::OffsetBeyondEnd {
                        offset: start_line,
                        path: path.to_string(),
                    }
                });
            }
            current_line += 1;
        }

        Ok(())
    }

    async fn next_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        line: u64,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<Option<Vec<u8>>, ReadError> {
        let mut bytes = Vec::new();
        let mut limited = (&mut *reader).take((MAX_READ_BYTES + 3) as u64);
        let bytes_read = limited
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })?;
        if bytes_read == 0 {
            return Ok(None);
        }
        Self::consume_scan_budget(remaining_scan_bytes, bytes.len(), path)?;
        let line_content_bytes = if let Some(line) = bytes.strip_suffix(b"\n") {
            line.strip_suffix(b"\r").unwrap_or(line)
        } else {
            &bytes
        };
        if line_content_bytes.len() > MAX_READ_BYTES {
            return Err(ReadError::LineTooLong {
                line,
                path: path.to_string(),
            });
        }

        Ok(Some(bytes))
    }

    async fn skip_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<bool, ReadError> {
        let mut saw_bytes = false;
        loop {
            let (bytes_to_consume, reached_newline) = {
                let bytes = reader.fill_buf().await.map_err(|source| ReadError::Read {
                    path: path.to_string(),
                    source,
                })?;
                if bytes.is_empty() {
                    return Ok(saw_bytes);
                }
                saw_bytes = true;

                bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or((bytes.len(), false), |index| (index + 1, true))
            };
            Self::consume_scan_budget(remaining_scan_bytes, bytes_to_consume, path)?;
            reader.consume(bytes_to_consume);
            if reached_newline {
                return Ok(true);
            }
        }
    }

    fn consume_scan_budget(
        remaining_scan_bytes: &mut usize,
        bytes: usize,
        path: &str,
    ) -> Result<(), ReadError> {
        if bytes > *remaining_scan_bytes {
            return Err(ReadError::ScanLimitExceeded {
                limit: MAX_SCAN_BYTES,
                path: path.to_string(),
            });
        }
        *remaining_scan_bytes -= bytes;

        Ok(())
    }

    async fn has_more(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
    ) -> Result<bool, ReadError> {
        reader
            .fill_buf()
            .await
            .map(|bytes| !bytes.is_empty())
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })
    }

    fn decode_line(mut line: Vec<u8>, line_number: u64, path: &str) -> Result<String, ReadError> {
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }

        String::from_utf8(line).map_err(|_| ReadError::InvalidUtf8 {
            line: line_number,
            path: path.to_string(),
        })
    }
}
