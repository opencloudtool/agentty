use std::path::PathBuf;
use std::time::{Duration, Instant};

use image::ImageFormat;
use rustix::event::{self, PollFd, PollFlags, Timespec};
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, ConnectionExt, GetPropertyReply, Property, PropertyNotifyEvent, SelectionNotifyEvent,
    Time,
};
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{CreateWindowAux, EventMask, WindowClass};
use x11rb::rust_connection::RustConnection;
#[cfg(target_os = "linux")]
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

use super::ClipboardBackend;
use crate::{ClipboardError, RgbaImageData, format, uri};

const INCR_RESERVATION_BYTE_CAP: usize = 16 * 1024 * 1024;
const INCR_SEGMENT_TIMEOUT: Duration = Duration::from_secs(1);
const INCR_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const INCR_HEADER_LONG_LENGTH: u32 = 1;
const MAX_CLIPBOARD_BYTE_COUNT: usize = 64 * 1024 * 1024;
const MAX_CLIPBOARD_PROPERTY_LONG_LENGTH: u32 = 16 * 1024 * 1024;
const SELECTION_TIMEOUT: Duration = Duration::from_secs(4);

x11rb::atom_manager! {
    AtomCollection: AtomCollectionCookie {
        CLIPBOARD,
        TARGETS,
        INCR,
        UTF8_STRING,
        UTF8_MIME_LOWER: b"text/plain;charset=utf-8",
        UTF8_MIME_UPPER: b"text/plain;charset=UTF-8",
        STRING,
        TEXT,
        TEXT_MIME: b"text/plain",
        URI_LIST: b"text/uri-list",
        PNG_MIME: b"image/png",
        AGENTTY_CLIPBOARD,
    }
}

pub(crate) struct X11Clipboard {
    atoms: AtomCollection,
    connection: RustConnection,
    window_id: u32,
}

impl X11Clipboard {
    #[cfg(target_os = "linux")]
    pub(crate) fn new() -> Result<Self, ClipboardError> {
        let (connection, screen_number) =
            RustConnection::connect(None).map_err(|error| ClipboardError::Unavailable {
                reason: format!("X11 clipboard connection failed: {error}"),
            })?;
        let screen =
            connection
                .setup()
                .roots
                .get(screen_number)
                .ok_or_else(|| ClipboardError::Backend {
                    reason: "X11 screen was not found".to_string(),
                })?;
        let window_id = connection
            .generate_id()
            .map_err(|error| ClipboardError::backend("failed to allocate X11 window id", error))?;
        let event_mask = EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY;

        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window_id,
                screen.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::COPY_FROM_PARENT,
                COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(event_mask),
            )
            .map_err(|error| {
                ClipboardError::backend("failed to create X11 clipboard window", error)
            })?;
        connection.flush().map_err(|error| {
            ClipboardError::backend("failed to flush X11 clipboard window", error)
        })?;
        let atoms = AtomCollection::new(&connection)
            .map_err(|error| {
                ClipboardError::backend("failed to request X11 clipboard atoms", error)
            })?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to load X11 clipboard atoms", error)
            })?;

        Ok(Self {
            atoms,
            connection,
            window_id,
        })
    }

    fn read_clipboard_data(
        &self,
        target_formats: &[Atom],
    ) -> Result<ClipboardData, ClipboardError> {
        for target_format in target_formats {
            match self.read_target(*target_format) {
                Ok(bytes) => {
                    return Ok(ClipboardData {
                        bytes,
                        format: *target_format,
                    });
                }
                Err(ClipboardError::ContentUnavailable) => {}
                Err(error) => return Err(error),
            }
        }

        Err(ClipboardError::ContentUnavailable)
    }

    fn read_target(&self, target_format: Atom) -> Result<Vec<u8>, ClipboardError> {
        self.read_target_with_event_waiter(target_format, Self::wait_for_x11_event)
    }

    fn read_target_with_event_waiter(
        &self,
        target_format: Atom,
        mut wait_for_event: impl FnMut(&RustConnection, Instant) -> Result<bool, ClipboardError>,
    ) -> Result<Vec<u8>, ClipboardError> {
        self.connection
            .delete_property(self.window_id, self.atoms.AGENTTY_CLIPBOARD)
            .map_err(|error| {
                ClipboardError::backend("failed to clear X11 clipboard property", error)
            })?;
        self.connection
            .convert_selection(
                self.window_id,
                self.atoms.CLIPBOARD,
                target_format,
                self.atoms.AGENTTY_CLIPBOARD,
                Time::CURRENT_TIME,
            )
            .map_err(|error| {
                ClipboardError::backend("failed to request X11 clipboard selection", error)
            })?;
        self.connection.flush().map_err(|error| {
            ClipboardError::backend("failed to flush X11 clipboard request", error)
        })?;

        let mut timeout_end = Instant::now() + SELECTION_TIMEOUT;
        let mut is_incr_transfer = false;
        let mut incr_transfer_timeout_end = None;
        let mut incr_transfer = IncrTransfer::default();

        while Instant::now() < timeout_end {
            let Some(event) = self.connection.poll_for_event().map_err(|error| {
                ClipboardError::backend("failed to poll X11 clipboard events", error)
            })?
            else {
                if !wait_for_event(&self.connection, timeout_end)? {
                    break;
                }
                continue;
            };

            match event {
                Event::SelectionNotify(event) => match self.handle_selection_notify(
                    event,
                    target_format,
                    &mut is_incr_transfer,
                    &mut incr_transfer,
                )? {
                    SelectionRead::Complete(bytes) => return Ok(bytes),
                    SelectionRead::IncrStarted => {
                        let now = Instant::now();
                        incr_transfer_timeout_end = Some(now + INCR_TRANSFER_TIMEOUT);
                        timeout_end = Self::next_incr_timeout(now, incr_transfer_timeout_end);
                    }
                    SelectionRead::Ignored => {}
                },
                Event::PropertyNotify(event)
                    if self.handle_property_notify(
                        &event,
                        target_format,
                        is_incr_transfer,
                        &mut incr_transfer,
                        incr_transfer_timeout_end,
                        &mut timeout_end,
                    )? =>
                {
                    return Ok(incr_transfer.finish());
                }
                _ => {}
            }
        }

        Err(ClipboardError::ContentUnavailable)
    }

    fn handle_selection_notify(
        &self,
        event: SelectionNotifyEvent,
        target_format: Atom,
        is_incr_transfer: &mut bool,
        incr_transfer: &mut IncrTransfer,
    ) -> Result<SelectionRead, ClipboardError> {
        if event.property == NONE || event.target != target_format {
            return Err(ClipboardError::ContentUnavailable);
        }
        if event.selection != self.atoms.CLIPBOARD {
            return Ok(SelectionRead::Ignored);
        }
        if *is_incr_transfer {
            return Ok(SelectionRead::Ignored);
        }

        let mut property = self
            .connection
            .get_property(
                true,
                event.requestor,
                event.property,
                event.target,
                0,
                MAX_CLIPBOARD_PROPERTY_LONG_LENGTH,
            )
            .map_err(|error| {
                ClipboardError::backend("failed to read X11 clipboard property", error)
            })?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to receive X11 clipboard property", error)
            })?;
        Self::ensure_property_payload_within_limit(&property)?;

        if property.type_ == target_format {
            return Ok(SelectionRead::Complete(property.value));
        }
        if property.type_ != self.atoms.INCR {
            return Err(ClipboardError::Backend {
                reason: "X11 clipboard owner returned an unexpected property type".to_string(),
            });
        }

        property = self
            .connection
            .get_property(
                true,
                event.requestor,
                event.property,
                self.atoms.INCR,
                0,
                INCR_HEADER_LONG_LENGTH,
            )
            .map_err(|error| ClipboardError::backend("failed to read X11 INCR header", error))?
            .reply()
            .map_err(|error| ClipboardError::backend("failed to receive X11 INCR header", error))?;
        if let Some(minimum_byte_count) = Self::minimum_incr_byte_count(&property) {
            incr_transfer.reserve_at_least(minimum_byte_count)?;
        }
        *is_incr_transfer = true;

        Ok(SelectionRead::IncrStarted)
    }

    fn handle_property_notify(
        &self,
        event: &PropertyNotifyEvent,
        target_format: Atom,
        is_incr_transfer: bool,
        incr_transfer: &mut IncrTransfer,
        incr_transfer_timeout_end: Option<Instant>,
        timeout_end: &mut Instant,
    ) -> Result<bool, ClipboardError> {
        if event.atom != self.atoms.AGENTTY_CLIPBOARD || event.state != Property::NEW_VALUE {
            return Ok(false);
        }
        if !is_incr_transfer {
            return Ok(false);
        }

        let property = self
            .connection
            .get_property(
                true,
                event.window,
                event.atom,
                target_format,
                0,
                MAX_CLIPBOARD_PROPERTY_LONG_LENGTH,
            )
            .map_err(|error| ClipboardError::backend("failed to read X11 INCR segment", error))?
            .reply()
            .map_err(|error| {
                ClipboardError::backend("failed to receive X11 INCR segment", error)
            })?;
        Self::ensure_property_payload_within_limit(&property)?;
        if property.value_len == 0 {
            return Ok(true);
        }

        incr_transfer.push_chunk(property.value)?;
        *timeout_end = Self::next_incr_timeout(Instant::now(), incr_transfer_timeout_end);

        Ok(false)
    }

    fn wait_for_x11_event(
        connection: &RustConnection,
        timeout_end: Instant,
    ) -> Result<bool, ClipboardError> {
        let mut poll_fds = [PollFd::new(connection.stream(), PollFlags::IN)];

        Self::wait_for_x11_event_with_poller(timeout_end, |timeout| {
            event::poll(&mut poll_fds, Some(timeout))
        })
    }

    fn wait_for_x11_event_with_poller(
        timeout_end: Instant,
        poll_events: impl FnOnce(&Timespec) -> rustix::io::Result<usize>,
    ) -> Result<bool, ClipboardError> {
        let Some(timeout) = Self::poll_timeout_until(Instant::now(), timeout_end) else {
            return Ok(false);
        };
        let ready_count = poll_events(&timeout).map_err(|error| {
            ClipboardError::backend("failed to wait for X11 clipboard events", error)
        })?;

        Ok(ready_count > 0)
    }

    fn poll_timeout_until(now: Instant, timeout_end: Instant) -> Option<Timespec> {
        if now >= timeout_end {
            return None;
        }
        let duration = timeout_end.checked_duration_since(now)?;

        Some(Self::duration_to_timespec(duration))
    }

    fn duration_to_timespec(duration: Duration) -> Timespec {
        Timespec::try_from(duration).unwrap_or(Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 999_999_999,
        })
    }

    fn next_incr_timeout(now: Instant, transfer_timeout_end: Option<Instant>) -> Instant {
        let segment_timeout_end = now + INCR_SEGMENT_TIMEOUT;

        transfer_timeout_end.map_or(segment_timeout_end, |deadline| {
            segment_timeout_end.min(deadline)
        })
    }

    fn ensure_property_payload_within_limit(
        property: &GetPropertyReply,
    ) -> Result<(), ClipboardError> {
        checked_clipboard_byte_count(property.value.len(), property.bytes_after as usize)?;

        Ok(())
    }

    fn minimum_incr_byte_count(property: &GetPropertyReply) -> Option<u32> {
        property.value32().and_then(|mut values| values.next())
    }

    fn latin1_bytes_to_string(bytes: Vec<u8>) -> String {
        bytes.into_iter().map(char::from).collect()
    }

    fn utf8_bytes_to_string(bytes: Vec<u8>) -> Result<String, ClipboardError> {
        String::from_utf8(bytes).map_err(|error| {
            ClipboardError::backend("failed to decode X11 clipboard text as UTF-8", error)
        })
    }
}

impl ClipboardBackend for X11Clipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        let target_formats = [
            self.atoms.UTF8_STRING,
            self.atoms.UTF8_MIME_LOWER,
            self.atoms.UTF8_MIME_UPPER,
            self.atoms.STRING,
            self.atoms.TEXT,
            self.atoms.TEXT_MIME,
        ];
        let clipboard_data = self.read_clipboard_data(&target_formats)?;
        if clipboard_data.format == self.atoms.STRING {
            return Ok(Self::latin1_bytes_to_string(clipboard_data.bytes));
        }

        Self::utf8_bytes_to_string(clipboard_data.bytes)
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        let clipboard_data = self.read_clipboard_data(&[self.atoms.URI_LIST])?;
        let paths = uri::paths_from_uri_list(&clipboard_data.bytes);
        if paths.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(paths)
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        let clipboard_data = self.read_clipboard_data(&[self.atoms.PNG_MIME])?;

        format::decode_image_rgba(&clipboard_data.bytes, ImageFormat::Png)
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        if self.connection.destroy_window(self.window_id).is_ok() {
            let _ = self.connection.flush();
        }
    }
}

struct ClipboardData {
    bytes: Vec<u8>,
    format: Atom,
}

enum SelectionRead {
    Complete(Vec<u8>),
    Ignored,
    IncrStarted,
}

#[derive(Default)]
struct IncrTransfer {
    bytes: Vec<u8>,
}

impl IncrTransfer {
    fn reserve_at_least(&mut self, minimum_byte_count: u32) -> Result<(), ClipboardError> {
        let minimum_byte_count = checked_clipboard_byte_count(0, minimum_byte_count as usize)?;

        self.bytes
            .reserve_exact(Self::capped_reservation(minimum_byte_count));

        Ok(())
    }

    fn push_chunk(&mut self, chunk: Vec<u8>) -> Result<(), ClipboardError> {
        checked_clipboard_byte_count(self.bytes.len(), chunk.len())?;
        self.bytes.extend(chunk);

        Ok(())
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    fn capped_reservation(minimum_byte_count: usize) -> usize {
        minimum_byte_count.min(INCR_RESERVATION_BYTE_CAP)
    }
}

fn checked_clipboard_byte_count(
    current_byte_count: usize,
    additional_byte_count: usize,
) -> Result<usize, ClipboardError> {
    let byte_count = current_byte_count
        .checked_add(additional_byte_count)
        .ok_or_else(|| clipboard_payload_too_large(usize::MAX))?;
    if byte_count > MAX_CLIPBOARD_BYTE_COUNT {
        return Err(clipboard_payload_too_large(byte_count));
    }

    Ok(byte_count)
}

fn clipboard_payload_too_large(byte_count: usize) -> ClipboardError {
    ClipboardError::Backend {
        reason: format!(
            "X11 clipboard payload exceeds {MAX_CLIPBOARD_BYTE_COUNT} byte limit ({byte_count} \
             bytes)"
        ),
    }
}

#[cfg(test)]
#[path = "x11_test.rs"]
mod tests;
