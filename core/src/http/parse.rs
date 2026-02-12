//! HTTP parsing utilities

/// Find the end of HTTP headers (position after \r\n\r\n or \n\n)
pub fn find_header_end(data: &[u8]) -> Option<usize> {
    // Look for \r\n\r\n
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
    }
    // Look for \n\n (curl sometimes uses this)
    for i in 0..data.len().saturating_sub(1) {
        if &data[i..i + 2] == b"\n\n" {
            return Some(i + 2);
        }
    }
    None
}

/// Parse Content-Length header from HTTP headers string
pub fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("content-length:") {
            if let Some(value) = line.split(':').nth(1) {
                if let Ok(len) = value.trim().parse::<usize>() {
                    return len;
                }
            }
        }
    }
    0 // No Content-Length header means no body
}

/// Simple JSON string extraction (avoids adding serde_json dependency)
pub fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let start = json.find(&pattern)?;
    let after_key = &json[start + pattern.len()..];

    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];

    let quote_start = after_colon.find('"')?;
    let after_quote = &after_colon[quote_start + 1..];

    let mut end = 0;
    let chars: Vec<char> = after_quote.chars().collect();
    while end < chars.len() {
        if chars[end] == '"' && (end == 0 || chars[end - 1] != '\\') {
            break;
        }
        end += 1;
    }

    if end < chars.len() {
        Some(after_quote[..end].to_string())
    } else {
        None
    }
}

pub fn http_response(status: u16, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body.len(),
        body
    )
}

pub fn http_json_response(status: u16, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body.len(),
        body
    )
}
