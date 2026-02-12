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

/// Errors when encoding/decoding a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Input does not contain a full frame.
    TooShort,
    /// Trailing bytes after a complete frame.
    TrailingBytes,
    /// Payload is too large for the 32-bit wire length field.
    PayloadTooLarge,
    /// Header + payload size overflowed usize.
    LengthOverflow,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort => write!(f, "frame too short"),
            FrameError::TrailingBytes => write!(f, "trailing bytes after frame"),
            FrameError::PayloadTooLarge => write!(f, "frame payload too large"),
            FrameError::LengthOverflow => write!(f, "frame length overflow"),
        }
    }
}

impl std::error::Error for FrameError {}

fn payload_len_u32(payload_len: usize) -> Result<u32, FrameError> {
    u32::try_from(payload_len).map_err(|_| FrameError::PayloadTooLarge)
}

fn frame_total_size(payload_len: usize) -> Result<usize, FrameError> {
    FRAME_HEADER_LEN
        .checked_add(payload_len)
        .ok_or(FrameError::LengthOverflow)
}

/// Encode a framed message as `[type][len][payload]`.
pub fn encode_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let payload_len =
        payload_len_u32(payload.len()).expect("frame payload exceeds 32-bit length field");
    let total_size = frame_total_size(payload.len()).expect("frame total size overflow");

    let mut bytes = Vec::with_capacity(total_size);
    bytes.push(msg_type);
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// Decode a frame from bytes. Returns the frame and total bytes consumed.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Err(FrameError::TooShort);
    }

    let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    let total_size = frame_total_size(len)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let encoded = encode_frame(0xA1, &[1, 2, 3, 4]);
        let frame = decode_frame_exact(&encoded).expect("decode should succeed");
        assert_eq!(frame.msg_type, 0xA1);
        assert_eq!(frame.payload, &[1, 2, 3, 4]);
        assert_eq!(frame.total_size, FRAME_HEADER_LEN + 4);
    }

    #[test]
    fn test_decode_too_short_header() {
        assert_eq!(decode_frame(&[0x01, 0x00, 0x00]), Err(FrameError::TooShort));
    }

    #[test]
    fn test_decode_too_short_payload() {
        let bytes = [0x01, 0x00, 0x00, 0x00, 0x05, 1, 2, 3];
        assert_eq!(decode_frame(&bytes), Err(FrameError::TooShort));
    }

    #[test]
    fn test_decode_frame_allows_trailing_bytes() {
        let mut bytes = encode_frame(0x11, &[9, 8]);
        bytes.extend_from_slice(&[7, 6, 5]);

        let frame = decode_frame(&bytes).expect("decode should allow trailing bytes");
        assert_eq!(frame.msg_type, 0x11);
        assert_eq!(frame.payload, &[9, 8]);
        assert_eq!(frame.total_size, FRAME_HEADER_LEN + 2);
    }

    #[test]
    fn test_decode_frame_exact_rejects_trailing_bytes() {
        let mut bytes = encode_frame(0x22, &[3, 2, 1]);
        bytes.push(0xFF);

        assert_eq!(decode_frame_exact(&bytes), Err(FrameError::TrailingBytes));
    }

    #[test]
    fn test_frame_total_size_overflow() {
        assert_eq!(
            frame_total_size(usize::MAX),
            Err(FrameError::LengthOverflow)
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_payload_len_u32_rejects_too_large() {
        let too_large = (u32::MAX as usize) + 1;
        assert_eq!(payload_len_u32(too_large), Err(FrameError::PayloadTooLarge));
    }
}
