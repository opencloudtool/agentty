use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;

use x11rb::protocol::xproto::{
    CONVERT_SELECTION_REQUEST, DELETE_PROPERTY_REQUEST, GET_PROPERTY_REQUEST,
    PROPERTY_NOTIFY_EVENT, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, Screen,
    SelectionNotifyEvent, Setup,
};
use x11rb::rust_connection::DefaultStream;
use x11rb::x11_utils::Serialize;

use super::*;

const MAX_CLIPBOARD_BYTE_COUNT_U32: u32 = 64 * 1024 * 1024;
const TEST_WINDOW_ID: u32 = 42;

struct X11TestServer {
    server_thread: thread::JoinHandle<()>,
    writer: UnixStream,
}

impl X11TestServer {
    fn start(steps: Vec<X11ServerStep>) -> (Self, RustConnection) {
        let (client_stream, server_stream) =
            UnixStream::pair().expect("test X11 socket pair should open");
        let writer = server_stream
            .try_clone()
            .expect("test X11 server socket should clone");
        let server_thread = thread::spawn(move || Self::run(server_stream, steps));
        let (client_stream, _) = DefaultStream::from_unix_stream(client_stream)
            .expect("test X11 client stream should initialize");
        let connection = RustConnection::connect_to_stream(client_stream, 0)
            .expect("test X11 connection should complete setup");

        (
            Self {
                server_thread,
                writer,
            },
            connection,
        )
    }

    fn run(mut server_stream: UnixStream, steps: Vec<X11ServerStep>) {
        let mut setup_request = [0; 12];
        server_stream
            .read_exact(&mut setup_request)
            .expect("test X11 server should receive setup request");
        server_stream
            .write_all(&Self::setup_bytes())
            .expect("test X11 server should send setup response");

        for (step_index, step) in steps.into_iter().enumerate() {
            let opcode = Self::read_request_opcode(&mut server_stream);
            assert_eq!(opcode, step.expected_opcode);
            let sequence =
                u16::try_from(step_index + 1).expect("test X11 request sequence should fit in u16");
            for response in step.responses {
                response.write_to(&mut server_stream, sequence);
            }
        }
    }

    fn setup_bytes() -> Vec<u8> {
        let mut setup = Setup {
            maximum_request_length: u16::MAX,
            protocol_major_version: 11,
            resource_id_base: 0x0100_0000,
            resource_id_mask: 0x00FF_FFFF,
            roots: vec![Screen {
                root: 1,
                ..Screen::default()
            }],
            status: 1,
            ..Setup::default()
        };
        setup.length = u16::try_from((setup.serialize().len() - 8) / 4)
            .expect("test X11 setup length should fit in u16");

        setup.serialize()
    }

    fn read_request_opcode(server_stream: &mut UnixStream) -> u8 {
        let mut header = [0; 4];
        server_stream
            .read_exact(&mut header)
            .expect("test X11 server should receive request header");
        let request_byte_count = usize::from(u16::from_ne_bytes([header[2], header[3]])) * 4;
        let body_byte_count = request_byte_count
            .checked_sub(header.len())
            .expect("test X11 request should include its header");
        let mut body = vec![0; body_byte_count];
        server_stream
            .read_exact(&mut body)
            .expect("test X11 server should receive request body");

        header[0]
    }

    fn send_response(&mut self, response: X11ServerResponse) {
        response.write_to(&mut self.writer, 0);
    }

    fn finish(self) {
        drop(self.writer);
        self.server_thread
            .join()
            .expect("test X11 server thread should finish");
    }
}

struct X11ServerStep {
    expected_opcode: u8,
    responses: Vec<X11ServerResponse>,
}

enum X11ServerResponse {
    GetProperty(GetPropertyReply),
    PropertyNotify(PropertyNotifyEvent),
    SelectionNotify(SelectionNotifyEvent),
}

impl X11ServerResponse {
    fn write_to(self, server_stream: &mut UnixStream, sequence: u16) {
        let bytes = match self {
            Self::GetProperty(mut reply) => {
                reply.sequence = sequence;
                let reply_byte_count = 32 + reply.length as usize * 4;
                let mut bytes = reply.serialize();
                bytes.resize(reply_byte_count, 0);

                bytes
            }
            Self::PropertyNotify(mut event) => {
                event.sequence = sequence;
                let mut bytes = event.serialize().to_vec();
                bytes.resize(32, 0);

                bytes
            }
            Self::SelectionNotify(mut event) => {
                event.sequence = sequence;
                let mut bytes = event.serialize().to_vec();
                bytes.resize(32, 0);

                bytes
            }
        };
        server_stream
            .write_all(&bytes)
            .expect("test X11 server should send scripted response");
    }
}

#[test]
fn test_read_target_waits_for_delayed_selection_event() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(CONVERT_SELECTION_REQUEST, Vec::new()),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, target_format, b"delayed".to_vec())],
        ),
    ];
    let (mut server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };
    let mut wait_call_count = 0;

    // Act
    let bytes = clipboard
        .read_target_with_event_waiter(target_format, |_, _| {
            wait_call_count += 1;
            server.send_response(selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            ));

            Ok(true)
        })
        .expect("delayed X11 selection should be read");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(bytes, b"delayed");
    assert_eq!(wait_call_count, 1);
}

#[test]
fn test_read_target_reassembles_incremental_transfer() {
    // Arrange
    let atoms = test_atoms();
    let target_format = atoms.UTF8_STRING;
    let steps = vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            )],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, atoms.INCR, Vec::new())],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(32, atoms.INCR, 8_u32.to_ne_bytes().to_vec()),
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![
                property_response(8, target_format, b"incremental".to_vec()),
                property_notify_response(atoms),
            ],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, target_format, Vec::new())],
        ),
    ];
    let (server, connection) = X11TestServer::start(steps);
    let clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let bytes = clipboard
        .read_target(target_format)
        .expect("scripted X11 INCR transfer should complete");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(bytes, b"incremental");
}

#[test]
fn test_wait_for_x11_event_reports_ready_connection() {
    // Arrange
    let atoms = test_atoms();
    let (mut server, connection) = X11TestServer::start(Vec::new());
    server.send_response(selection_response(
        atoms,
        atoms.UTF8_STRING,
        atoms.AGENTTY_CLIPBOARD,
    ));
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event(&connection, timeout_end)
        .expect("readable test connection should poll successfully");
    drop(connection);
    server.finish();

    // Assert
    assert!(is_ready);
}

#[test]
fn test_wait_for_x11_event_returns_false_without_ready_events() {
    // Arrange
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, empty_poller)
        .expect("empty poll result should not fail");

    // Assert
    assert!(!is_ready);
}

#[test]
fn test_wait_for_x11_event_returns_false_after_deadline() {
    // Arrange
    let timeout_end = Instant::now();

    // Act
    let is_ready = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, empty_poller)
        .expect("expired deadline should not fail");

    // Assert
    assert!(!is_ready);
}

#[test]
fn test_wait_for_x11_event_reports_poll_failure() {
    // Arrange
    let timeout_end = Instant::now() + Duration::from_secs(1);

    // Act
    let result = X11Clipboard::wait_for_x11_event_with_poller(timeout_end, |_| {
        Err(rustix::io::Errno::INVAL)
    });

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to wait for X11 clipboard events")
    ));
}

#[test]
fn test_poll_timeout_until_returns_remaining_duration() {
    // Arrange
    let now = Instant::now();
    let timeout_end = now + Duration::from_millis(1500);

    // Act
    let timeout =
        X11Clipboard::poll_timeout_until(now, timeout_end).expect("deadline should be in future");

    // Assert
    assert_eq!(timeout.tv_sec, 1);
    assert_eq!(timeout.tv_nsec, 500_000_000);
}

#[test]
fn test_poll_timeout_until_returns_none_after_deadline() {
    // Arrange
    let now = Instant::now();
    let timeout_end = now;

    // Act
    let timeout = X11Clipboard::poll_timeout_until(now, timeout_end);

    // Assert
    assert_eq!(timeout, None);
}

#[test]
fn test_next_incr_timeout_uses_segment_timeout_without_transfer_cap() {
    // Arrange
    let now = Instant::now();

    // Act
    let timeout_end = X11Clipboard::next_incr_timeout(now, None);

    // Assert
    assert_eq!(timeout_end, now + INCR_SEGMENT_TIMEOUT);
}

#[test]
fn test_next_incr_timeout_caps_segment_timeout_to_transfer_deadline() {
    // Arrange
    let now = Instant::now();
    let transfer_timeout_end = now + Duration::from_millis(50);

    // Act
    let timeout_end = X11Clipboard::next_incr_timeout(now, Some(transfer_timeout_end));

    // Assert
    assert_eq!(timeout_end, transfer_timeout_end);
}

#[test]
fn test_property_payload_limit_accepts_payload_within_limit() {
    // Arrange
    let property = GetPropertyReply {
        bytes_after: 4,
        format: 8,
        length: 1,
        sequence: 0,
        type_: 0,
        value: vec![0],
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::ensure_property_payload_within_limit(&property);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_property_payload_limit_rejects_payload_above_limit() {
    // Arrange
    let property = GetPropertyReply {
        bytes_after: MAX_CLIPBOARD_BYTE_COUNT_U32,
        format: 8,
        length: 0,
        sequence: 0,
        type_: 0,
        value: vec![0],
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::ensure_property_payload_within_limit(&property);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

#[test]
fn test_minimum_incr_byte_count_reads_first_header_value() {
    // Arrange
    let minimum_byte_count = 4096_u32;
    let property = GetPropertyReply {
        bytes_after: 0,
        format: 32,
        length: 1,
        sequence: 0,
        type_: 0,
        value: minimum_byte_count.to_ne_bytes().to_vec(),
        value_len: 1,
    };

    // Act
    let result = X11Clipboard::minimum_incr_byte_count(&property);

    // Assert
    assert_eq!(result, Some(minimum_byte_count));
}

#[test]
fn test_latin1_bytes_to_string_maps_bytes_directly_to_unicode_scalars() {
    // Arrange
    let bytes = vec![b'a', 0xE9, b'z'];

    // Act
    let text = X11Clipboard::latin1_bytes_to_string(bytes);

    // Assert
    assert_eq!(text, "a\u{e9}z");
}

#[test]
fn test_utf8_bytes_to_string_decodes_valid_utf8() {
    // Arrange
    let bytes = "clipboard text".as_bytes().to_vec();

    // Act
    let text = X11Clipboard::utf8_bytes_to_string(bytes)
        .expect("valid UTF-8 clipboard text should decode");

    // Assert
    assert_eq!(text, "clipboard text");
}

#[test]
fn test_utf8_bytes_to_string_reports_backend_failure_for_invalid_utf8() {
    // Arrange
    let bytes = vec![0xFF];

    // Act
    let result = X11Clipboard::utf8_bytes_to_string(bytes);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::Backend { reason })
            if reason.starts_with("failed to decode X11 clipboard text as UTF-8")
    ));
}

#[test]
fn test_read_text_decodes_utf8_target() {
    // Arrange
    let atoms = test_atoms();
    let steps = available_target_steps(
        atoms,
        atoms.UTF8_STRING,
        atoms.UTF8_STRING,
        "clipboard text".as_bytes().to_vec(),
    );
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let text = clipboard
        .read_text()
        .expect("scripted UTF-8 X11 text should decode");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(text, "clipboard text");
}

#[test]
fn test_read_text_decodes_string_target_as_latin1() {
    // Arrange
    let atoms = test_atoms();
    let mut steps = Vec::new();
    for target_format in [
        atoms.UTF8_STRING,
        atoms.UTF8_MIME_LOWER,
        atoms.UTF8_MIME_UPPER,
    ] {
        steps.extend(unavailable_target_steps(atoms, target_format));
    }
    steps.extend(available_target_steps(
        atoms,
        atoms.STRING,
        atoms.STRING,
        vec![0xE9],
    ));
    let (server, connection) = X11TestServer::start(steps);
    let mut clipboard = X11Clipboard {
        atoms,
        connection,
        window_id: TEST_WINDOW_ID,
    };

    // Act
    let text = clipboard
        .read_text()
        .expect("scripted Latin-1 X11 text should decode");
    drop(clipboard);
    server.finish();

    // Assert
    assert_eq!(text, "\u{e9}");
}

#[test]
fn test_incr_transfer_reassembles_chunks_and_resets_on_finish() {
    // Arrange
    let mut transfer = IncrTransfer::default();
    transfer
        .reserve_at_least(8)
        .expect("small reservation should fit");

    // Act
    transfer
        .push_chunk(b"abc".to_vec())
        .expect("first chunk should fit");
    transfer
        .push_chunk(b"def".to_vec())
        .expect("second chunk should fit");
    let bytes = transfer.finish();

    // Assert
    assert_eq!(bytes, b"abcdef");
    assert_eq!(transfer.finish(), Vec::<u8>::new());
}

#[test]
fn test_incr_transfer_caps_advertised_reservation() {
    // Arrange
    let advertised_byte_count = MAX_CLIPBOARD_BYTE_COUNT;

    // Act
    let reservation_byte_count = IncrTransfer::capped_reservation(advertised_byte_count);

    // Assert
    assert_eq!(reservation_byte_count, INCR_RESERVATION_BYTE_CAP);
}

#[test]
fn test_incr_transfer_rejects_advertised_payload_above_limit() {
    // Arrange
    let mut transfer = IncrTransfer::default();
    let advertised_byte_count = MAX_CLIPBOARD_BYTE_COUNT_U32 + 1;

    // Act
    let result = transfer.reserve_at_least(advertised_byte_count);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

#[test]
fn test_checked_clipboard_byte_count_rejects_payload_above_limit() {
    // Arrange
    let current_byte_count = MAX_CLIPBOARD_BYTE_COUNT;
    let additional_byte_count = 1;

    // Act
    let result = checked_clipboard_byte_count(current_byte_count, additional_byte_count);

    // Assert
    assert!(matches!(result, Err(ClipboardError::Backend { .. })));
}

fn test_atoms() -> AtomCollection {
    AtomCollection {
        AGENTTY_CLIPBOARD: 12,
        CLIPBOARD: 1,
        INCR: 3,
        PNG_MIME: 11,
        STRING: 7,
        TARGETS: 2,
        TEXT: 8,
        TEXT_MIME: 9,
        URI_LIST: 10,
        UTF8_MIME_LOWER: 5,
        UTF8_MIME_UPPER: 6,
        UTF8_STRING: 4,
    }
}

fn server_step(expected_opcode: u8, responses: Vec<X11ServerResponse>) -> X11ServerStep {
    X11ServerStep {
        expected_opcode,
        responses,
    }
}

fn property_response(format: u8, type_: Atom, value: Vec<u8>) -> X11ServerResponse {
    let bytes_per_value = usize::from(format) / 8;
    assert_eq!(value.len() % bytes_per_value, 0);
    let value_len = u32::try_from(value.len() / bytes_per_value)
        .expect("test X11 property value length should fit in u32");
    let length = u32::try_from(value.len().div_ceil(4))
        .expect("test X11 property reply length should fit in u32");

    X11ServerResponse::GetProperty(GetPropertyReply {
        bytes_after: 0,
        format,
        length,
        sequence: 0,
        type_,
        value,
        value_len,
    })
}

fn selection_response(atoms: AtomCollection, target: Atom, property: Atom) -> X11ServerResponse {
    X11ServerResponse::SelectionNotify(SelectionNotifyEvent {
        property,
        requestor: TEST_WINDOW_ID,
        response_type: SELECTION_NOTIFY_EVENT,
        selection: atoms.CLIPBOARD,
        target,
        ..SelectionNotifyEvent::default()
    })
}

fn property_notify_response(atoms: AtomCollection) -> X11ServerResponse {
    X11ServerResponse::PropertyNotify(PropertyNotifyEvent {
        atom: atoms.AGENTTY_CLIPBOARD,
        response_type: PROPERTY_NOTIFY_EVENT,
        state: Property::NEW_VALUE,
        window: TEST_WINDOW_ID,
        ..PropertyNotifyEvent::default()
    })
}

fn available_target_steps(
    atoms: AtomCollection,
    target_format: Atom,
    property_type: Atom,
    value: Vec<u8>,
) -> Vec<X11ServerStep> {
    vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(
                atoms,
                target_format,
                atoms.AGENTTY_CLIPBOARD,
            )],
        ),
        server_step(
            GET_PROPERTY_REQUEST,
            vec![property_response(8, property_type, value)],
        ),
    ]
}

fn unavailable_target_steps(atoms: AtomCollection, target_format: Atom) -> Vec<X11ServerStep> {
    vec![
        server_step(DELETE_PROPERTY_REQUEST, Vec::new()),
        server_step(
            CONVERT_SELECTION_REQUEST,
            vec![selection_response(atoms, target_format, NONE)],
        ),
    ]
}

fn empty_poller(_: &Timespec) -> rustix::io::Result<usize> {
    let immediate_timeout = Timespec {
        tv_nsec: 0,
        tv_sec: 0,
    };

    event::poll(&mut [], Some(&immediate_timeout))
}
