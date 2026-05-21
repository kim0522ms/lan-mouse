use input_event::{Event as InputEvent, KeyboardEvent, PointerEvent};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use paste::paste;
use std::{
    fmt::{Debug, Display, Formatter},
    mem::size_of,
};
use thiserror::Error;

/// defines the maximum size an encoded protocol datagram can take up.
pub const MAX_EVENT_SIZE: usize = 1200;

pub const CLIPBOARD_MIME_TEXT: &str = "text/plain;charset=utf-8";
pub const CAP_CLIPBOARD: u32 = 1 << 0;
pub const CAP_SYNC_LOCK: u32 = 1 << 1;
pub const CAP_PLATFORM_LINUX: u32 = 1 << 28;
pub const CAP_PLATFORM_MACOS: u32 = 1 << 29;
pub const CAP_PLATFORM_WINDOWS: u32 = 1 << 30;
pub const LOCAL_CAPABILITIES: u32 = CAP_CLIPBOARD | CAP_SYNC_LOCK | LOCAL_PLATFORM_CAPABILITY;

#[cfg(target_os = "linux")]
pub const LOCAL_PLATFORM_CAPABILITY: u32 = CAP_PLATFORM_LINUX;
#[cfg(target_os = "macos")]
pub const LOCAL_PLATFORM_CAPABILITY: u32 = CAP_PLATFORM_MACOS;
#[cfg(target_os = "windows")]
pub const LOCAL_PLATFORM_CAPABILITY: u32 = CAP_PLATFORM_WINDOWS;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub const LOCAL_PLATFORM_CAPABILITY: u32 = 0;

const CLIPBOARD_HEADER_SIZE: usize = size_of::<u8>() + size_of::<u32>() + 4 * size_of::<u16>();

pub fn max_clipboard_payload_size(mime: &str) -> usize {
    MAX_EVENT_SIZE
        .saturating_sub(CLIPBOARD_HEADER_SIZE)
        .saturating_sub(mime.len())
}

pub fn local_capabilities(clipboard_sharing: bool, sync_lock: bool) -> u32 {
    LOCAL_PLATFORM_CAPABILITY
        | if clipboard_sharing { CAP_CLIPBOARD } else { 0 }
        | if sync_lock { CAP_SYNC_LOCK } else { 0 }
}

pub fn capabilities_indicate_linux_or_windows(capabilities: u32) -> bool {
    capabilities & (CAP_PLATFORM_LINUX | CAP_PLATFORM_WINDOWS) != 0
}

/// error type for protocol violations
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// event type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidEventId(#[from] TryFromPrimitiveError<EventType>),
    /// position type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidPosition(#[from] TryFromPrimitiveError<Position>),
    #[error("truncated protocol event")]
    UnexpectedEof,
    #[error("invalid clipboard event")]
    InvalidClipboard,
}

/// Position of a client
#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Display for Position {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

/// main lan-mouse protocol event type
#[derive(Clone, Debug)]
pub enum ProtoEvent {
    /// notify a client that the cursor entered its region at the given position
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Enter { pos: Position, capabilities: u32 },
    /// notify a client that the cursor left its region
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Leave(u32),
    /// acknowledge of an [`ProtoEvent::Enter`] or [`ProtoEvent::Leave`] event
    Ack { serial: u32, capabilities: u32 },
    /// Input event
    Input(InputEvent),
    /// Ping event for tracking unresponsive clients.
    /// A client has to respond with [`ProtoEvent::Pong`].
    Ping,
    /// Response to [`ProtoEvent::Ping`], true if emulation is enabled / available
    Pong { alive: bool, capabilities: u32 },
    /// Clipboard or other shared data payload.
    Clipboard(ClipboardData),
    /// Request that the peer locks its local login session.
    Lock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardData {
    pub transfer_id: u32,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub mime: String,
    pub payload: Vec<u8>,
}

impl Display for ProtoEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoEvent::Enter { pos, .. } => write!(f, "Enter({pos})"),
            ProtoEvent::Leave(s) => write!(f, "Leave({s})"),
            ProtoEvent::Ack { serial, .. } => write!(f, "Ack({serial})"),
            ProtoEvent::Input(e) => write!(f, "{e}"),
            ProtoEvent::Ping => write!(f, "ping"),
            ProtoEvent::Pong { alive, .. } => {
                write!(
                    f,
                    "pong: {}",
                    if *alive { "alive" } else { "not available" }
                )
            }
            ProtoEvent::Clipboard(data) => write!(
                f,
                "Clipboard({}, {}/{}, {} bytes)",
                data.mime,
                data.chunk_index + 1,
                data.chunk_count,
                data.payload.len()
            ),
            ProtoEvent::Lock => write!(f, "Lock"),
        }
    }
}

#[derive(TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum EventType {
    PointerMotion,
    PointerButton,
    PointerAxis,
    PointerAxisValue120,
    KeyboardKey,
    KeyboardModifiers,
    Ping,
    Pong,
    Enter,
    Leave,
    Ack,
    Clipboard,
    Lock,
}

impl ProtoEvent {
    fn event_type(&self) -> EventType {
        match self {
            ProtoEvent::Input(e) => match e {
                InputEvent::Pointer(p) => match p {
                    PointerEvent::Motion { .. } => EventType::PointerMotion,
                    PointerEvent::Button { .. } => EventType::PointerButton,
                    PointerEvent::Axis { .. } => EventType::PointerAxis,
                    PointerEvent::AxisDiscrete120 { .. } => EventType::PointerAxisValue120,
                },
                InputEvent::Keyboard(k) => match k {
                    KeyboardEvent::Key { .. } => EventType::KeyboardKey,
                    KeyboardEvent::Modifiers { .. } => EventType::KeyboardModifiers,
                },
            },
            ProtoEvent::Ping => EventType::Ping,
            ProtoEvent::Pong { .. } => EventType::Pong,
            ProtoEvent::Enter { .. } => EventType::Enter,
            ProtoEvent::Leave(_) => EventType::Leave,
            ProtoEvent::Ack { .. } => EventType::Ack,
            ProtoEvent::Clipboard(_) => EventType::Clipboard,
            ProtoEvent::Lock => EventType::Lock,
        }
    }

    pub fn capabilities(&self) -> u32 {
        match self {
            ProtoEvent::Enter { capabilities, .. }
            | ProtoEvent::Ack { capabilities, .. }
            | ProtoEvent::Pong { capabilities, .. } => *capabilities,
            _ => 0,
        }
    }
}

impl TryFrom<&[u8]> for ProtoEvent {
    type Error = ProtocolError;

    fn try_from(mut buf: &[u8]) -> Result<Self, Self::Error> {
        let event_type = decode_u8(&mut buf)?;
        match EventType::try_from(event_type)? {
            EventType::PointerMotion => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Motion {
                    time: decode_u32(&mut buf)?,
                    dx: decode_f64(&mut buf)?,
                    dy: decode_f64(&mut buf)?,
                })))
            }
            EventType::PointerButton => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Button {
                    time: decode_u32(&mut buf)?,
                    button: decode_u32(&mut buf)?,
                    state: decode_u32(&mut buf)?,
                })))
            }
            EventType::PointerAxis => Ok(Self::Input(InputEvent::Pointer(PointerEvent::Axis {
                time: decode_u32(&mut buf)?,
                axis: decode_u8(&mut buf)?,
                value: decode_f64(&mut buf)?,
            }))),
            EventType::PointerAxisValue120 => Ok(Self::Input(InputEvent::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: decode_u8(&mut buf)?,
                    value: decode_i32(&mut buf)?,
                },
            ))),
            EventType::KeyboardKey => Ok(Self::Input(InputEvent::Keyboard(KeyboardEvent::Key {
                time: decode_u32(&mut buf)?,
                key: decode_u32(&mut buf)?,
                state: decode_u8(&mut buf)?,
            }))),
            EventType::KeyboardModifiers => Ok(Self::Input(InputEvent::Keyboard(
                KeyboardEvent::Modifiers {
                    depressed: decode_u32(&mut buf)?,
                    latched: decode_u32(&mut buf)?,
                    locked: decode_u32(&mut buf)?,
                    group: decode_u32(&mut buf)?,
                },
            ))),
            EventType::Ping => Ok(Self::Ping),
            EventType::Pong => Ok(Self::Pong {
                alive: decode_u8(&mut buf)? != 0,
                capabilities: decode_optional_u32(&mut buf)?,
            }),
            EventType::Enter => Ok(Self::Enter {
                pos: decode_u8(&mut buf)?.try_into()?,
                capabilities: decode_optional_u32(&mut buf)?,
            }),
            EventType::Leave => Ok(Self::Leave(decode_u32(&mut buf)?)),
            EventType::Ack => Ok(Self::Ack {
                serial: decode_u32(&mut buf)?,
                capabilities: decode_optional_u32(&mut buf)?,
            }),
            EventType::Clipboard => {
                let transfer_id = decode_u32(&mut buf)?;
                let chunk_index = decode_u16(&mut buf)?;
                let chunk_count = decode_u16(&mut buf)?;
                let mime_len = decode_u16(&mut buf)? as usize;
                let payload_len = decode_u16(&mut buf)? as usize;
                if chunk_count == 0
                    || chunk_index >= chunk_count
                    || mime_len > buf.len()
                    || payload_len > buf.len().saturating_sub(mime_len)
                {
                    return Err(ProtocolError::InvalidClipboard);
                }
                let (mime, rest) = buf.split_at(mime_len);
                let (payload, _) = rest.split_at(payload_len);
                let mime = String::from_utf8(mime.to_vec())
                    .map_err(|_| ProtocolError::InvalidClipboard)?;
                Ok(Self::Clipboard(ClipboardData {
                    transfer_id,
                    chunk_index,
                    chunk_count,
                    mime,
                    payload: payload.to_vec(),
                }))
            }
            EventType::Lock => Ok(Self::Lock),
        }
    }
}

impl TryFrom<[u8; MAX_EVENT_SIZE]> for ProtoEvent {
    type Error = ProtocolError;

    fn try_from(buf: [u8; MAX_EVENT_SIZE]) -> Result<Self, Self::Error> {
        Self::try_from(&buf[..])
    }
}

impl From<ProtoEvent> for ([u8; MAX_EVENT_SIZE], usize) {
    fn from(event: ProtoEvent) -> Self {
        let mut buf = [0u8; MAX_EVENT_SIZE];
        let mut len = 0usize;
        {
            let mut buf = &mut buf[..];
            let buf = &mut buf;
            let len = &mut len;
            encode_u8(buf, len, event.event_type() as u8);
            match event {
                ProtoEvent::Input(event) => match event {
                    InputEvent::Pointer(p) => match p {
                        PointerEvent::Motion { time, dx, dy } => {
                            encode_u32(buf, len, time);
                            encode_f64(buf, len, dx);
                            encode_f64(buf, len, dy);
                        }
                        PointerEvent::Button {
                            time,
                            button,
                            state,
                        } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, button);
                            encode_u32(buf, len, state);
                        }
                        PointerEvent::Axis { time, axis, value } => {
                            encode_u32(buf, len, time);
                            encode_u8(buf, len, axis);
                            encode_f64(buf, len, value);
                        }
                        PointerEvent::AxisDiscrete120 { axis, value } => {
                            encode_u8(buf, len, axis);
                            encode_i32(buf, len, value);
                        }
                    },
                    InputEvent::Keyboard(k) => match k {
                        KeyboardEvent::Key { time, key, state } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, key);
                            encode_u8(buf, len, state);
                        }
                        KeyboardEvent::Modifiers {
                            depressed,
                            latched,
                            locked,
                            group,
                        } => {
                            encode_u32(buf, len, depressed);
                            encode_u32(buf, len, latched);
                            encode_u32(buf, len, locked);
                            encode_u32(buf, len, group);
                        }
                    },
                },
                ProtoEvent::Ping => {}
                ProtoEvent::Pong {
                    alive,
                    capabilities,
                } => {
                    encode_u8(buf, len, alive as u8);
                    encode_u32(buf, len, capabilities);
                }
                ProtoEvent::Enter { pos, capabilities } => {
                    encode_u8(buf, len, pos as u8);
                    encode_u32(buf, len, capabilities);
                }
                ProtoEvent::Leave(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Ack {
                    serial,
                    capabilities,
                } => {
                    encode_u32(buf, len, serial);
                    encode_u32(buf, len, capabilities);
                }
                ProtoEvent::Clipboard(data) => {
                    encode_u32(buf, len, data.transfer_id);
                    encode_u16(buf, len, data.chunk_index);
                    encode_u16(buf, len, data.chunk_count);
                    encode_u16(buf, len, data.mime.len() as u16);
                    encode_u16(buf, len, data.payload.len() as u16);
                    encode_bytes(buf, len, data.mime.as_bytes());
                    encode_bytes(buf, len, &data.payload);
                }
                ProtoEvent::Lock => {}
            }
        }
        (buf, len)
    }
}

macro_rules! decode_impl {
    ($t:ty) => {
        paste! {
            fn [<decode_ $t>](data: &mut &[u8]) -> Result<$t, ProtocolError> {
                if data.len() < size_of::<$t>() {
                    return Err(ProtocolError::UnexpectedEof);
                }
                let (int_bytes, rest) = data.split_at(size_of::<$t>());
                *data = rest;
                let int_bytes = int_bytes
                    .try_into()
                    .map_err(|_| ProtocolError::UnexpectedEof)?;
                Ok($t::from_be_bytes(int_bytes))
            }
        }
    };
}

decode_impl!(u8);
decode_impl!(u16);
decode_impl!(u32);
decode_impl!(i32);
decode_impl!(f64);

fn decode_optional_u32(data: &mut &[u8]) -> Result<u32, ProtocolError> {
    if data.is_empty() {
        return Ok(0);
    }
    decode_u32(data)
}

macro_rules! encode_impl {
    ($t:ty) => {
        paste! {
            fn [<encode_ $t>](buf: &mut &mut [u8], amt: &mut usize, n: $t) {
                let src = n.to_be_bytes();
                let data = std::mem::take(buf);
                let (int_bytes, rest) = data.split_at_mut(size_of::<$t>());
                int_bytes.copy_from_slice(&src);
                *amt += size_of::<$t>();
                *buf = rest
            }
        }
    };
}

encode_impl!(u8);
encode_impl!(u16);
encode_impl!(u32);
encode_impl!(i32);
encode_impl!(f64);

fn encode_bytes(buf: &mut &mut [u8], amt: &mut usize, bytes: &[u8]) {
    let data = std::mem::take(buf);
    let (dst, rest) = data.split_at_mut(bytes.len());
    dst.copy_from_slice(bytes);
    *amt += bytes.len();
    *buf = rest;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_events_decode_from_legacy_short_messages() {
        let enter = [EventType::Enter as u8, Position::Right as u8];
        assert!(matches!(
            ProtoEvent::try_from(&enter[..]).expect("enter"),
            ProtoEvent::Enter {
                pos: Position::Right,
                capabilities: 0,
            }
        ));

        let mut ack = vec![EventType::Ack as u8];
        ack.extend(7u32.to_be_bytes());
        assert!(matches!(
            ProtoEvent::try_from(&ack[..]).expect("ack"),
            ProtoEvent::Ack {
                serial: 7,
                capabilities: 0,
            }
        ));

        let pong = [EventType::Pong as u8, 1];
        assert!(matches!(
            ProtoEvent::try_from(&pong[..]).expect("pong"),
            ProtoEvent::Pong {
                alive: true,
                capabilities: 0,
            }
        ));
    }

    #[test]
    fn partial_capability_events_return_error() {
        for event in [
            vec![EventType::Enter as u8, Position::Right as u8, 0],
            vec![EventType::Enter as u8, Position::Right as u8, 0, 0],
            vec![EventType::Enter as u8, Position::Right as u8, 0, 0, 0],
            vec![EventType::Ack as u8, 0, 0, 0, 7, 0],
            vec![EventType::Ack as u8, 0, 0, 0, 7, 0, 0],
            vec![EventType::Ack as u8, 0, 0, 0, 7, 0, 0, 0],
            vec![EventType::Pong as u8, 1, 0],
            vec![EventType::Pong as u8, 1, 0, 0],
            vec![EventType::Pong as u8, 1, 0, 0, 0],
        ] {
            assert!(matches!(
                ProtoEvent::try_from(event.as_slice()),
                Err(ProtocolError::UnexpectedEof)
            ));
        }
    }

    #[test]
    fn capability_events_round_trip() {
        for event in [
            ProtoEvent::Enter {
                pos: Position::Left,
                capabilities: LOCAL_CAPABILITIES,
            },
            ProtoEvent::Ack {
                serial: 3,
                capabilities: LOCAL_CAPABILITIES,
            },
            ProtoEvent::Pong {
                alive: true,
                capabilities: LOCAL_CAPABILITIES,
            },
        ] {
            let expected = event.capabilities();
            let (buf, len) = event.into();
            let decoded = ProtoEvent::try_from(&buf[..len]).expect("decode event");
            assert_eq!(decoded.capabilities(), expected);
        }
    }

    #[test]
    fn local_capabilities_follow_clipboard_toggle() {
        assert_eq!(local_capabilities(false, false) & CAP_CLIPBOARD, 0);
        assert_eq!(
            local_capabilities(true, false) & CAP_CLIPBOARD,
            CAP_CLIPBOARD
        );
    }

    #[test]
    fn local_capabilities_follow_sync_lock_toggle() {
        assert_eq!(local_capabilities(false, false) & CAP_SYNC_LOCK, 0);
        assert_eq!(
            local_capabilities(false, true) & CAP_SYNC_LOCK,
            CAP_SYNC_LOCK
        );
    }

    #[test]
    fn local_capabilities_include_platform() {
        assert_eq!(
            local_capabilities(false, false) & LOCAL_PLATFORM_CAPABILITY,
            LOCAL_PLATFORM_CAPABILITY
        );
    }

    #[test]
    fn lock_event_round_trips() {
        let (buf, len) = ProtoEvent::Lock.into();
        assert!(matches!(
            ProtoEvent::try_from(&buf[..len]).expect("decode lock"),
            ProtoEvent::Lock
        ));
    }

    #[test]
    fn clipboard_event_round_trips_with_actual_datagram_len() {
        let event = ProtoEvent::Clipboard(ClipboardData {
            transfer_id: 42,
            chunk_index: 1,
            chunk_count: 2,
            mime: CLIPBOARD_MIME_TEXT.to_owned(),
            payload: b"hello clipboard".to_vec(),
        });

        let (buf, len) = event.into();
        assert!(len <= MAX_EVENT_SIZE);
        let decoded = ProtoEvent::try_from(&buf[..len]).expect("decode event");
        let ProtoEvent::Clipboard(decoded) = decoded else {
            panic!("expected clipboard event");
        };
        assert_eq!(
            decoded,
            ClipboardData {
                transfer_id: 42,
                chunk_index: 1,
                chunk_count: 2,
                mime: CLIPBOARD_MIME_TEXT.to_owned(),
                payload: b"hello clipboard".to_vec(),
            }
        );
    }

    #[test]
    fn invalid_clipboard_events_return_protocol_error() {
        let mut zero_chunks = vec![EventType::Clipboard as u8];
        zero_chunks.extend(42u32.to_be_bytes());
        zero_chunks.extend(0u16.to_be_bytes());
        zero_chunks.extend(0u16.to_be_bytes());
        zero_chunks.extend(0u16.to_be_bytes());
        zero_chunks.extend(0u16.to_be_bytes());

        let mut invalid_index = vec![EventType::Clipboard as u8];
        invalid_index.extend(42u32.to_be_bytes());
        invalid_index.extend(2u16.to_be_bytes());
        invalid_index.extend(2u16.to_be_bytes());
        invalid_index.extend(0u16.to_be_bytes());
        invalid_index.extend(0u16.to_be_bytes());

        let mut invalid_mime = vec![EventType::Clipboard as u8];
        invalid_mime.extend(42u32.to_be_bytes());
        invalid_mime.extend(0u16.to_be_bytes());
        invalid_mime.extend(1u16.to_be_bytes());
        invalid_mime.extend(1u16.to_be_bytes());
        invalid_mime.extend(0u16.to_be_bytes());
        invalid_mime.push(0xff);

        for event in [zero_chunks, invalid_index, invalid_mime] {
            assert!(matches!(
                ProtoEvent::try_from(event.as_slice()),
                Err(ProtocolError::InvalidClipboard)
            ));
        }
    }

    #[test]
    fn truncated_events_return_error() {
        for event in [
            vec![],
            vec![EventType::Enter as u8],
            vec![EventType::Ack as u8, 0, 0],
            vec![EventType::Pong as u8],
            vec![EventType::Clipboard as u8, 0, 0, 0],
        ] {
            assert!(matches!(
                ProtoEvent::try_from(event.as_slice()),
                Err(ProtocolError::UnexpectedEof)
            ));
        }
    }
}
