//! Shared wire framing helpers for protocol messages.

/// Frame header size in bytes: 1 byte type + 4 byte length.
pub const FRAME_HEADER_LEN: usize = 5;

/// Parsed frame view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Message type byte.
    pub msg_type: u8,
    /// Frame payload.
    pub payload: &'a [u8],
    /// Total frame size (header + payload).
    pub total_size: usize,
}

/// Errors when decoding a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Input does not contain a full frame.
    TooShort,
    /// Trailing bytes after a complete frame.
    TrailingBytes,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort => write!(f, "frame too short"),
            FrameError::TrailingBytes => write!(f, "trailing bytes after frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode a framed message as `[type][len][payload]`.
pub fn encode_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    bytes.push(msg_type);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// Decode a frame from bytes. Returns the frame and total bytes consumed.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Err(FrameError::TooShort);
    }

    let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    let total_size = FRAME_HEADER_LEN + len;

    if bytes.len() < total_size {
        return Err(FrameError::TooShort);
    }

    Ok(Frame {
        msg_type: bytes[0],
        payload: &bytes[FRAME_HEADER_LEN..total_size],
        total_size,
    })
}

/// Decode a frame and require that it consumes the full buffer.
pub fn decode_frame_exact(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    let frame = decode_frame(bytes)?;
    if frame.total_size != bytes.len() {
        return Err(FrameError::TrailingBytes);
    }
    Ok(frame)
}
