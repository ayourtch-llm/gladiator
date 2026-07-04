pub fn decode_sse_chunk(chunk: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(chunk);
    let mut payloads = Vec::new();

    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            if !data.is_empty() {
                payloads.push(data.to_string());
            }
        }
    }

    payloads
}
