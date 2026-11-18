//! Identifiers in the lines of a log that people post: disk and device
//! serial numbers, MAC addresses, Apple device identifiers (ECID, UDID, a
//! Mac's serial number), and computer, network and account names. A line
//! keeps a short tag in their place, the same for the same value in every
//! line, so two lines about one disk still match:
//!
//! - [`Tail`]: a serial number's last four characters, as the screen names
//!   a disk; too little to identify the hardware.
//! - [`Mac`]: a MAC address's vendor half, then its last byte.
//! - [`Name`]: a name's short hash, `#` and four hex digits.
//!
//! [`mask_line`] does the same to text Stela relays without formatting it,
//! as the guest's console and its restore ledger: MAC addresses anywhere,
//! and the value after a name that says it is an identifier.
use alloc::{borrow::Cow, string::String};
use core::fmt::{self, Write as _};

/// Characters of an identifier a [`Tail`] keeps.
const KEPT: usize = 4;

/// An identifier's last four characters after `..` (all of a shorter
/// one), or `none` for an empty one.
pub struct Tail<'a>(pub &'a str);

impl fmt::Display for Tail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let identifier = self.0.trim();
        if identifier.is_empty() {
            return f.write_str("none");
        }
        let start = identifier
            .char_indices()
            .rev()
            .nth(KEPT - 1)
            .map_or(0, |(start, _)| start);
        write!(f, "..{}", &identifier[start..])
    }
}

impl fmt::Debug for Tail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A MAC address as `aa:bb:cc:..:..:ff`: the vendor's half, which says
/// whose adapter it is, and the last byte, which tells a PC's adapters
/// apart.
pub struct Mac<'a>(pub &'a [u8; 6]);

impl fmt::Display for Mac<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, _, _, last] = *self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:..:..:{last:02x}")
    }
}

/// A name (a computer's, a network's, an account's) as `#` and the four
/// hex digits of a hash of it, or `none` for an empty one.
pub struct Name<'a>(pub &'a str);

impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        // FNV-1a, folded to 16 bits.
        let hash = self.0.bytes().fold(0x811c_9dc5u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        write!(f, "#{:04x}", (hash >> 16) ^ (hash & 0xffff))
    }
}

/// Names after which a number is a device identifier, matched where they
/// end a word, in any case: `ApECID`, `ECID unique chip identifier :`,
/// `IOPlatformSerialNumber`, `Cryptex1,UDID`.
const NUMBERED: [&str; 12] = [
    "ecid",
    "udid",
    "uniquechipid",
    "unique chip identifier",
    "universal device identifier",
    "uniquedeviceid",
    "serialnumber",
    "serial number",
    "serial-number",
    "serial_number",
    "imei",
    "meid",
];
/// Names after which `=` or `:` gives a name.
const NAMED: [&str; 9] = [
    "computer-name",
    "computername",
    "localhostname",
    "hostname",
    "host name",
    "ssid",
    "network name",
    "username",
    "user name",
];
/// Folders whose next part is an account's name.
const ACCOUNT_FOLDERS: [&str; 4] = ["/users/", "/home/", "/media/", "/run/media/"];
/// How far after its name a number is looked for.
const NUMBER_WITHIN: usize = 40;
/// The fewest characters of a number taken for an identifier.
const NUMBER_LEAST: usize = 6;

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Where `name` ends when it starts at `at` of `bytes`, any case, and ends
/// a word there.
fn name_at(bytes: &[u8], at: usize, name: &str) -> Option<usize> {
    let end = at + name.len();
    (bytes.get(at..end)?.eq_ignore_ascii_case(name.as_bytes())
        && !bytes.get(end).copied().is_some_and(is_word))
    .then_some(end)
}

/// The number that follows a [`NUMBERED`] name ending at `from`: the first
/// run of letters, digits, `-` and `_` within [`NUMBER_WITHIN`] bytes that
/// holds a digit and is at least [`NUMBER_LEAST`] long, or a list of
/// bytes in brackets, `[0, 0, 129, …]`, that comes first; whether it is
/// such a list.
fn number_after(bytes: &[u8], from: usize) -> Option<(usize, usize, bool)> {
    let limit = bytes.len().min(from + NUMBER_WITHIN);
    let mut at = from;
    while at < limit {
        if bytes[at] == b'[' {
            let end = at + 1 + bytes[at + 1..].iter().position(|&byte| byte == b']')?;
            if bytes[at + 1..end].iter().any(u8::is_ascii_digit) {
                return Some((at + 1, end, true));
            }
        }
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let end = at
            + bytes[at..]
                .iter()
                .position(|&byte| !(is_word(byte) || byte == b'-'))
                .unwrap_or(bytes.len() - at);
        if end - at >= NUMBER_LEAST && bytes[at..end].iter().any(u8::is_ascii_digit) {
            return Some((at, end, false));
        }
        at = end;
    }
    None
}

/// The name that follows a [`NAMED`] name ending at `from`, after `=` or
/// `:` and spaces or quotes: what a quote closes, or up to a space, comma,
/// semicolon or bracket.
fn name_after(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut at = from;
    let mut said = false;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'=' | b':' => said = true,
            b' ' | b'\t' | b'>' => {}
            _ => break,
        }
        at += 1;
    }
    if !said || at >= bytes.len() {
        return None;
    }
    if let quote @ (b'"' | b'\'') = bytes[at] {
        let end = bytes[at + 1..].iter().position(|&byte| byte == quote)?;
        return Some((at + 1, at + 1 + end));
    }
    let end = bytes[at..]
        .iter()
        .position(|&byte| matches!(byte, b' ' | b'\t' | b',' | b';' | b')' | b']' | b'}'))
        .map_or(bytes.len(), |end| at + end);
    (end > at).then_some((at, end))
}

/// A MAC address at `at`: six pairs of hex digits joined by one `:` or
/// `-`, with no hex digit or joiner on either side.
fn mac_at(bytes: &[u8], at: usize) -> Option<[u8; 6]> {
    let text = bytes.get(at..at + 17)?;
    let joiner = text[2];
    if !matches!(joiner, b':' | b'-') {
        return None;
    }
    let edge = |byte: Option<&u8>| {
        byte.is_none_or(|&byte| !byte.is_ascii_hexdigit() && byte != b':' && byte != b'-')
    };
    if !edge(at.checked_sub(1).and_then(|before| bytes.get(before))) || !edge(bytes.get(at + 17)) {
        return None;
    }
    let mut mac = [0; 6];
    for (index, octet) in mac.iter_mut().enumerate() {
        let pair = &text[index * 3..index * 3 + 2];
        if index < 5 && text[index * 3 + 2] != joiner {
            return None;
        }
        let pair = core::str::from_utf8(pair).ok()?;
        *octet = u8::from_str_radix(pair, 16).ok()?;
        if !pair.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
    }
    Some(mac)
}

/// `line` with each identifier in a tag's place: MAC addresses as
/// [`Mac`], the number after a device identifier's name as [`Tail`], and
/// a computer's, network's or account's name as [`Name`]. Borrowed when
/// it holds none.
pub fn mask_line(line: &str) -> Cow<'_, str> {
    let bytes = line.as_bytes();
    let mut masked = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let mut replace = |start: usize, end: usize, tag: &dyn fmt::Display| {
            masked.push_str(&line[copied..start]);
            let _ = write!(masked, "{tag}");
            copied = end;
            end
        };
        let word_start = at == 0 || !is_word(bytes[at - 1]);
        if let Some(mac) = mac_at(bytes, at) {
            at = replace(at, at + 17, &Mac(&mac));
            continue;
        }
        if let Some((start, end, list)) = NUMBERED
            .iter()
            .find_map(|name| name_at(bytes, at, name))
            .and_then(|end| number_after(bytes, end))
        {
            let value = &line[start..end];
            at = match list {
                true => replace(start, end, &Name(value)),
                false => replace(start, end, &Tail(value)),
            };
            continue;
        }
        if let Some((start, end)) = word_start
            .then(|| NAMED.iter().find_map(|name| name_at(bytes, at, name)))
            .flatten()
            .and_then(|end| name_after(bytes, end))
        {
            at = replace(start, end, &Name(&line[start..end]));
            continue;
        }
        if let Some(folder) = ACCOUNT_FOLDERS.iter().find(|folder| {
            bytes
                .get(at..at + folder.len())
                .is_some_and(|text| text.eq_ignore_ascii_case(folder.as_bytes()))
        }) {
            let start = at + folder.len();
            let end = bytes[start..]
                .iter()
                .position(|&byte| matches!(byte, b'/' | b' ' | b'"' | b'\'' | b')' | b','))
                .map_or(bytes.len(), |end| start + end);
            at = match end > start {
                true => replace(start, end, &Name(&line[start..end])),
                false => start,
            };
            continue;
        }
        // The next character, whole.
        at += line[at..].chars().next().map_or(1, char::len_utf8);
    }
    if copied == 0 {
        return Cow::Borrowed(line);
    }
    masked.push_str(&line[copied..]);
    Cow::Owned(masked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, string::ToString};

    #[test]
    fn a_tail_keeps_the_last_four_characters_of_a_serial() {
        assert_eq!(Tail("S4EWNX0R123456Z").to_string(), "..456Z");
        assert_eq!(Tail(" WD-WX12A3456789  ").to_string(), "..6789");
        // A serial whose end is a dash keeps it, as the disk shows it.
        assert_eq!(Tail("1234-5678-AB-C").to_string(), "..AB-C");
        assert_eq!(Tail("disk").to_string(), "..disk");
        assert_eq!(Tail("XY").to_string(), "..XY");
        assert_eq!(Tail("  ").to_string(), "none");
        assert_eq!(format!("{:?}", Tail("ABCDEFGH")), "..EFGH");
        // Characters, not bytes.
        assert_eq!(Tail("serie-éàüö").to_string(), "..éàüö");
    }

    #[test]
    fn a_mac_keeps_its_vendor_and_last_byte() {
        let mac = [0x00, 0xe0, 0x4c, 0x12, 0x34, 0x5e];
        assert_eq!(Mac(&mac).to_string(), "00:e0:4c:..:..:5e");
    }

    #[test]
    fn a_name_is_a_short_hash_the_same_for_the_same_name() {
        let tag = Name("Someones-MacBook").to_string();
        assert!(tag.starts_with('#') && tag.len() == 5, "{tag}");
        assert_eq!(Name("Someones-MacBook").to_string(), tag);
        assert_ne!(Name("Other-MacBook").to_string(), tag);
        assert_eq!(Name("").to_string(), "none");
    }

    #[test]
    fn a_line_without_identifiers_is_borrowed() {
        for line in [
            "HOST PCI 0000:00:1f.3 8086:a348 audio",
            "configuring debugging over serial",
            "UDID support enabled: 1",
            "GPU REFUSED buffer of connection 3 made at serial 1234567",
            "time 12:34:56",
        ] {
            assert!(matches!(mask_line(line), Cow::Borrowed(_)), "{line}");
        }
    }

    #[test]
    fn identifiers_in_relayed_text_become_tags() {
        let cases = [
            (
                "RTC en0 mac=a4:83:e7:12:34:56 (1)",
                "RTC en0 mac=a4:83:e7:..:..:56 (1)",
            ),
            ("link AA-BB-CC-DD-EE-FF up", "link aa:bb:cc:..:..:ff up"),
            (
                "restored_external: queried property: ECID => 0x001A2B3C4D5E6F70",
                "restored_external: queried property: ECID => ..6F70",
            ),
            (
                "restored_external: ECID unique chip identifier : 7354123456789012",
                "restored_external: ECID unique chip identifier : ..9012",
            ),
            (
                "libignition: ecid : 0x1234567890abcdef",
                "libignition: ecid : ..cdef",
            ),
            (
                "UDID universal device identifier : 00008103-001A2B3C4D5E6F70",
                "UDID universal device identifier : ..6F70",
            ),
            (
                r#"Dict({"ApECID": Integer(7354123456789012), "ApChipId": Integer(33024)})"#,
                r#"Dict({"ApECID": Integer(..9012), "ApChipId": Integer(33024)})"#,
            ),
            (
                r#"{"SerialNumber": String("C02ABC123DEF"), "Cryptex1,UDID": Data(00008103-ABCD1234)}"#,
                r#"{"SerialNumber": String("..3DEF"), "Cryptex1,UDID": Data(..1234)}"#,
            ),
            (
                r#"{"Cryptex1,UDID": Data([0, 0, 129, 3, 0, 26, 43, 60]), "Next": 1}"#,
                &format!(
                    r#"{{"Cryptex1,UDID": Data([{}]), "Next": 1}}"#,
                    Name("0, 0, 129, 3, 0, 26, 43, 60")
                ),
            ),
            (r#"ApECID = "-7354123456789012";"#, r#"ApECID = "-..9012";"#),
            (
                r#"set computer-name = "Someones MacBook""#,
                &format!(r#"set computer-name = "{}""#, Name("Someones MacBook")),
            ),
            (
                "joined ssid: HomeNet5G, channel 36",
                &format!("joined ssid: {}, channel 36", Name("HomeNet5G")),
            ),
            (
                "read /media/someone/STELA/log.txt",
                &format!("read /media/{}/STELA/log.txt", Name("someone")),
            ),
            (
                "home /Users/someone",
                &format!("home /Users/{}", Name("someone")),
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(mask_line(line), expected, "{line}");
        }
    }
}
