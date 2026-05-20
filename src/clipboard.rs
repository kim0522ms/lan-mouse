use std::{
    cell::RefCell,
    sync::atomic::{AtomicU32, Ordering},
};

use lan_mouse_proto::{CLIPBOARD_MIME_TEXT, ClipboardData, ProtoEvent, max_clipboard_payload_size};
use thiserror::Error;

const MAX_CLIPBOARD_TRANSFER_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ClipboardError {
    #[error(transparent)]
    Backend(#[from] arboard::Error),
    #[error("clipboard payload is not valid UTF-8")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("clipboard chunk sequence is incomplete")]
    Incomplete,
    #[error("clipboard payload is too large")]
    TooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SharedClipboardData {
    pub mime: String,
    pub payload: Vec<u8>,
}

impl SharedClipboardData {
    pub(crate) fn plain_text(text: String) -> Self {
        Self {
            mime: CLIPBOARD_MIME_TEXT.to_owned(),
            payload: text.into_bytes(),
        }
    }

    fn as_text(self) -> Result<Option<String>, ClipboardError> {
        if self.mime == CLIPBOARD_MIME_TEXT {
            return Ok(Some(String::from_utf8(self.payload)?));
        }
        Ok(None)
    }
}

pub(crate) struct SystemClipboard;

impl SystemClipboard {
    pub(crate) fn get_plain_text() -> Result<SharedClipboardData, ClipboardError> {
        with_system_clipboard(|clipboard| {
            let text = clipboard.get_text()?;
            Ok(SharedClipboardData::plain_text(text))
        })
    }

    pub(crate) fn set(data: SharedClipboardData) -> Result<(), ClipboardError> {
        let Some(text) = data.as_text()? else {
            return Ok(());
        };
        with_system_clipboard(|clipboard| {
            clipboard.set_text(text)?;
            Ok(())
        })
    }
}

thread_local! {
    static SYSTEM_CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn with_system_clipboard<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, ClipboardError>,
) -> Result<T, ClipboardError> {
    SYSTEM_CLIPBOARD.with(|clipboard| {
        let mut clipboard = clipboard.borrow_mut();
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new()?);
        }
        let result = f(clipboard.as_mut().expect("clipboard"));
        if matches!(result, Err(ClipboardError::Backend(_))) {
            *clipboard = None;
        }
        result
    })
}

#[derive(Default)]
pub(crate) struct ClipboardTransferReceiver {
    current: Option<IncomingClipboardTransfer>,
}

impl ClipboardTransferReceiver {
    pub(crate) fn push(
        &mut self,
        chunk: ClipboardData,
    ) -> Result<Option<SharedClipboardData>, ClipboardError> {
        validate_chunk_shape(&chunk)?;
        let reset = self
            .current
            .as_ref()
            .map(|transfer| {
                transfer.transfer_id != chunk.transfer_id
                    || transfer.mime != chunk.mime
                    || transfer.chunk_count != chunk.chunk_count
            })
            .unwrap_or(true);
        if reset {
            self.current = Some(IncomingClipboardTransfer::new(
                chunk.transfer_id,
                chunk.chunk_count,
                chunk.mime.clone(),
            )?);
        }

        let transfer = self.current.as_mut().expect("transfer");
        transfer.push(chunk)?;
        if transfer.is_complete() {
            let transfer = self.current.take().expect("transfer");
            return transfer.finish().map(Some);
        }
        Ok(None)
    }
}

struct IncomingClipboardTransfer {
    transfer_id: u32,
    chunk_count: u16,
    mime: String,
    chunks: Vec<Option<Vec<u8>>>,
}

impl IncomingClipboardTransfer {
    fn new(transfer_id: u32, chunk_count: u16, mime: String) -> Result<Self, ClipboardError> {
        if chunk_count == 0 {
            return Err(ClipboardError::Incomplete);
        }
        let max_payload = max_clipboard_payload_size(&mime);
        if max_payload == 0 || chunk_count as usize * max_payload > MAX_CLIPBOARD_TRANSFER_SIZE {
            return Err(ClipboardError::TooLarge);
        }
        Ok(Self {
            transfer_id,
            chunk_count,
            mime,
            chunks: vec![None; chunk_count as usize],
        })
    }

    fn push(&mut self, chunk: ClipboardData) -> Result<(), ClipboardError> {
        let Some(slot) = self.chunks.get_mut(chunk.chunk_index as usize) else {
            return Err(ClipboardError::Incomplete);
        };
        *slot = Some(chunk.payload);
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.chunks.iter().all(Option::is_some)
    }

    fn finish(self) -> Result<SharedClipboardData, ClipboardError> {
        let mut payload = Vec::new();
        for chunk in self.chunks {
            let Some(chunk) = chunk else {
                return Err(ClipboardError::Incomplete);
            };
            payload.extend(chunk);
        }
        Ok(SharedClipboardData {
            mime: self.mime,
            payload,
        })
    }
}

pub(crate) fn encode_clipboard_events(
    data: SharedClipboardData,
) -> Result<Vec<ProtoEvent>, ClipboardError> {
    let transfer_id = next_transfer_id();
    let max_payload = max_clipboard_payload_size(&data.mime);
    if max_payload == 0 || data.mime.len() > u16::MAX as usize {
        return Err(ClipboardError::TooLarge);
    }
    if data.payload.len() > MAX_CLIPBOARD_TRANSFER_SIZE {
        return Err(ClipboardError::TooLarge);
    }
    let chunk_count = data.payload.len().div_ceil(max_payload).max(1);
    if chunk_count > u16::MAX as usize {
        return Err(ClipboardError::TooLarge);
    }

    Ok((0..chunk_count)
        .map(|index| {
            let start = index * max_payload;
            let end = ((index + 1) * max_payload).min(data.payload.len());
            ProtoEvent::Clipboard(ClipboardData {
                transfer_id,
                chunk_index: index as u16,
                chunk_count: chunk_count as u16,
                mime: data.mime.clone(),
                payload: data.payload[start..end].to_vec(),
            })
        })
        .collect())
}

fn validate_chunk_shape(chunk: &ClipboardData) -> Result<(), ClipboardError> {
    if chunk.chunk_count == 0 || chunk.chunk_index >= chunk.chunk_count {
        return Err(ClipboardError::Incomplete);
    }
    let max_payload = max_clipboard_payload_size(&chunk.mime);
    if max_payload == 0
        || chunk.payload.len() > max_payload
        || chunk.chunk_count as usize * max_payload > MAX_CLIPBOARD_TRANSFER_SIZE
    {
        return Err(ClipboardError::TooLarge);
    }
    Ok(())
}

fn next_transfer_id() -> u32 {
    static NEXT_TRANSFER_ID: AtomicU32 = AtomicU32::new(1);
    NEXT_TRANSFER_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lan_mouse_proto::MAX_EVENT_SIZE;

    #[test]
    fn chunks_round_trip_large_plain_text_payload() {
        let text = "clipboard ".repeat(MAX_EVENT_SIZE * 3);
        let data = SharedClipboardData::plain_text(text.clone());
        let events = encode_clipboard_events(data).expect("encode clipboard");

        assert!(events.len() > 1);
        let mut receiver = ClipboardTransferReceiver::default();
        let mut decoded = None;
        for event in events {
            let (buf, len) = event.into();
            assert!(len <= MAX_EVENT_SIZE);
            let event = ProtoEvent::try_from(&buf[..len]).expect("decode clipboard event");
            let ProtoEvent::Clipboard(chunk) = event else {
                panic!("expected clipboard event");
            };
            decoded = receiver.push(chunk).expect("push chunk");
        }

        let decoded = decoded.expect("complete transfer");
        assert_eq!(decoded.mime, CLIPBOARD_MIME_TEXT);
        assert_eq!(String::from_utf8(decoded.payload).expect("utf8"), text);
    }

    #[test]
    fn chunks_round_trip_out_of_order() {
        let data = SharedClipboardData::plain_text("out-of-order clipboard chunks".repeat(100));
        let mut chunks = encode_clipboard_events(data)
            .expect("encode clipboard")
            .into_iter()
            .map(|event| {
                let ProtoEvent::Clipboard(chunk) = event else {
                    panic!("expected clipboard event");
                };
                chunk
            })
            .collect::<Vec<_>>();
        chunks.reverse();

        let mut receiver = ClipboardTransferReceiver::default();
        let mut decoded = None;
        for chunk in chunks {
            decoded = receiver.push(chunk).expect("push chunk");
        }

        let decoded = decoded.expect("complete transfer");
        assert_eq!(
            String::from_utf8(decoded.payload).expect("utf8"),
            "out-of-order clipboard chunks".repeat(100)
        );
    }

    #[test]
    fn receiver_resets_when_new_transfer_starts() {
        let first = encode_clipboard_events(SharedClipboardData::plain_text(
            "first transfer ".repeat(MAX_EVENT_SIZE),
        ))
        .expect("encode first")
        .into_iter()
        .map(|event| {
            let ProtoEvent::Clipboard(chunk) = event else {
                panic!("expected clipboard event");
            };
            chunk
        })
        .collect::<Vec<_>>();
        let second = encode_clipboard_events(SharedClipboardData::plain_text("second".to_owned()))
            .expect("encode second");
        let ProtoEvent::Clipboard(second) = second.into_iter().next().expect("second chunk") else {
            panic!("expected clipboard event");
        };

        let mut receiver = ClipboardTransferReceiver::default();
        assert!(
            receiver
                .push(first[0].clone())
                .expect("first chunk")
                .is_none()
        );
        let decoded = receiver
            .push(second)
            .expect("second transfer")
            .expect("complete second transfer");

        assert_eq!(String::from_utf8(decoded.payload).expect("utf8"), "second");
    }

    #[test]
    fn empty_plain_text_payload_is_one_chunk() {
        let data = SharedClipboardData::plain_text(String::new());
        let events = encode_clipboard_events(data).expect("encode clipboard");

        assert_eq!(events.len(), 1);
        let ProtoEvent::Clipboard(chunk) = &events[0] else {
            panic!("expected clipboard event");
        };
        assert_eq!(chunk.chunk_count, 1);
        assert_eq!(chunk.payload, b"");
    }

    #[test]
    fn consecutive_transfers_use_distinct_ids() {
        let first = encode_clipboard_events(SharedClipboardData::plain_text("first".to_owned()))
            .expect("encode first");
        let second = encode_clipboard_events(SharedClipboardData::plain_text("second".to_owned()))
            .expect("encode second");

        let ProtoEvent::Clipboard(first) = &first[0] else {
            panic!("expected clipboard event");
        };
        let ProtoEvent::Clipboard(second) = &second[0] else {
            panic!("expected clipboard event");
        };
        assert_ne!(first.transfer_id, second.transfer_id);
    }

    #[test]
    fn each_encoded_chunk_fits_one_datagram() {
        let text = "x".repeat(MAX_EVENT_SIZE * 10);
        let data = SharedClipboardData::plain_text(text);
        let events = encode_clipboard_events(data).expect("encode clipboard");

        for event in events {
            let (_buf, len) = event.into();
            assert!(len <= MAX_EVENT_SIZE);
        }
    }

    #[test]
    fn rejects_mime_that_leaves_no_payload_room() {
        let data = SharedClipboardData {
            mime: "x".repeat(MAX_EVENT_SIZE),
            payload: b"hello".to_vec(),
        };

        assert!(matches!(
            encode_clipboard_events(data),
            Err(ClipboardError::TooLarge)
        ));
    }

    #[test]
    fn rejects_transfers_over_size_limit() {
        let data = SharedClipboardData {
            mime: CLIPBOARD_MIME_TEXT.to_owned(),
            payload: vec![b'x'; MAX_CLIPBOARD_TRANSFER_SIZE + 1],
        };

        assert!(matches!(
            encode_clipboard_events(data),
            Err(ClipboardError::TooLarge)
        ));
    }

    #[test]
    fn receiver_rejects_unbounded_chunk_count() {
        let mut receiver = ClipboardTransferReceiver::default();
        let chunk = ClipboardData {
            transfer_id: 1,
            chunk_index: 0,
            chunk_count: u16::MAX,
            mime: CLIPBOARD_MIME_TEXT.to_owned(),
            payload: b"x".to_vec(),
        };

        assert!(matches!(
            receiver.push(chunk),
            Err(ClipboardError::TooLarge)
        ));
    }
}
