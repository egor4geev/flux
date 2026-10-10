//! Percent-encoding of URL components and forms (RFC 3986): the unreserved characters stay, every
//! other byte of the UTF-8 becomes `%XX`. Decoding takes `+` for a space, as forms and queries
//! write it.

/// `text` as a URL component: a query value, a form field.
pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A decoded URL component: `%XX` bytes (as UTF-8, lossily), `+` as a space; a broken escape
/// stays as it is.
pub(crate) fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = match bytes[index..] {
            [b'%', high, low, ..] => hex(high).zip(hex(low)).map(|(high, low)| high << 4 | low),
            _ => None,
        };
        match (bytes[index], escaped) {
            (_, Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                index += 1;
            }
            (byte, None) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|value| value as u8)
}

/// `name=value` pairs joined with `&`, each encoded: a query, a form's body.
pub(crate) fn pairs(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The decoded value of `name` in a query (`a=1&b=two`); the first one if repeated.
pub(crate) fn find(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key) == name).then(|| decode(value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes() {
        assert_eq!(encode("a b&c=д"), "a%20b%26c%3D%D0%B4");
        assert_eq!(encode("safe-._~09AZ"), "safe-._~09AZ");
        assert_eq!(decode("a%20b%26c%3D%D0%B4"), "a b&c=д");
        assert_eq!(decode("one+two"), "one two");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%4"), "%zz%4");
    }

    #[test]
    fn queries() {
        assert_eq!(
            pairs(&[("client_id", "abc"), ("redirect_uri", "http://127.0.0.1:8123/cb")]),
            "client_id=abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A8123%2Fcb"
        );
        let query = "code=x%2By&state=s+1&empty&code=second";
        assert_eq!(find(query, "code").as_deref(), Some("x+y"));
        assert_eq!(find(query, "state").as_deref(), Some("s 1"));
        assert_eq!(find(query, "empty").as_deref(), Some(""));
        assert_eq!(find(query, "missing"), None);
    }
}
