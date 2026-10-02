use base64::{engine::general_purpose::STANDARD, Engine as _};

/// Encode bytes as an OSC 52 host-clipboard update.
///
/// The caller is responsible for enforcing an appropriate clipboard size limit.
pub fn osc52(data: &[u8]) -> Vec<u8> {
    let encoded = STANDARD.encode(data);
    let mut command = Vec::with_capacity(encoded.len() + 8);
    command.extend_from_slice(b"\x1b]52;c;");
    command.extend_from_slice(encoded.as_bytes());
    command.push(0x07);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_clipboard_without_raw_payload() {
        assert_eq!(osc52(b"hello"), b"\x1b]52;c;aGVsbG8=\x07");
    }
}
