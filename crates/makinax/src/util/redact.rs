//! Outbound credential redaction.
//!
//! RPC endpoints routinely carry their key IN THE URL — Tenderly gateways,
//! Alchemy, Infura, QuickNode all do. Errors, tool payloads and logs that
//! embed one of these URLs must not leak the key, while still keeping
//! enough of the URL that a reader can tell which endpoint failed.
//!
//! Deliberately conservative: it redacts the SECRET-LOOKING part and keeps
//! the host and shape, because "fork backend failed" is useless without
//! knowing which endpoint failed. Redaction that destroys diagnosis gets
//! removed by the next person who needs to debug at 2am.

/// The replacement marker. Distinctive so it is obvious in a transcript that
/// redaction happened rather than the value being empty.
pub const MARK: &str = "<redacted>";

/// Redact credentials in a URL-bearing string, preserving scheme, host and
/// path shape. Handles the three shapes that actually occur:
///   - `https://user:pass@host/…`        → `https://user:<redacted>@host/…`
///   - `https://host/<long-opaque-key>`  → `https://host/<redacted>`
///   - `?apikey=…` / `?key=…` / `?token=…` → value replaced
pub fn url(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (i, word) in raw.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&redact_word(word));
    }
    out
}

/// [`url`] for a whole message: the same redaction, with the message's own
/// whitespace kept as it is (line breaks and indentation included), so a
/// text with no credential in it comes back byte for byte.
pub fn text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while !rest.is_empty() {
        let gap = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..gap]);
        rest = &rest[gap..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        out.push_str(&redact_word(&rest[..word]));
        rest = &rest[word..];
    }
    out
}

/// Characters that end a URL's host and path when it is embedded in prose
/// or in a JSON body. A URL in an error comes wrapped
/// (`(fork source: https://…):`, `{"url":"https://…"}`), and a wrapper read
/// as part of the key segment makes the segment look non-opaque, so the
/// secret passes in exactly the case that matters.
const URL_END: &[char] = &[
    '"', '\'', '`', '<', '>', '(', ')', '[', ']', '{', '}', ',', ';', '\\',
];

/// The schemes whose URLs are redacted.
const SCHEMES: [&str; 4] = ["https", "http", "wss", "ws"];

/// The next network URL in `s`: where its scheme starts, and where what
/// follows `://` starts. The scheme is matched at the END of the letters
/// before `://`, so a URL glued to the end of something else (another URL's
/// key, a `name=`) is still found.
fn next_url(s: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(i) = s[from..].find("://") {
        let sep = from + i;
        let letters = s[..sep]
            .bytes()
            .rev()
            .take_while(u8::is_ascii_alphabetic)
            .count();
        let run = s[sep - letters..sep].to_ascii_lowercase();
        if let Some(scheme) = SCHEMES.iter().find(|scheme| run.ends_with(**scheme)) {
            return Some((sep - scheme.len(), sep + 3));
        }
        from = sep + 3;
    }
    None
}

/// Redact every URL in one whitespace-free word, keeping whatever surrounds
/// each URL.
fn redact_word(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some((_, after)) = next_url(rest) {
        out.push_str(&rest[..after]);
        rest = redact_one(&rest[after..], &mut out);
    }
    out.push_str(rest);
    out
}

/// Where the authority (`user:password@host:port`) of the URL that starts
/// `tail` ends: at the first `/`, `?` or `#`, not counting the slashes of a
/// `://`, which a password may hold.
fn authority_end(tail: &str) -> usize {
    let mut from = 0;
    while let Some(i) = tail[from..].find(['/', '?', '#']) {
        let at = from + i;
        if tail[..at].ends_with(':') && tail[at..].starts_with("//") {
            from = at + 2;
        } else {
            return at;
        }
    }
    tail.len()
}

/// Redact one URL, given what follows its `scheme://`, into `out`; returns
/// what is left of the word after it.
///
/// The host and path end at a wrapper character or where another URL
/// starts. A query or fragment is read parameter by parameter: a
/// secret-named value is masked whole, up to the next `&` or `#` or the end
/// of the word, whatever characters it holds; any other value is kept, up
/// to a URL nested in it, which is then redacted as a URL of its own.
fn redact_one<'a>(tail: &'a str, out: &mut String) -> &'a str {
    // A password may hold any character, so the URL's end is looked for
    // only after its userinfo. `&` separates parameters and never ends the
    // authority, and neither does the `//` of a `://` inside a password.
    let userinfo = tail[..authority_end(tail)]
        .rfind('@')
        .map_or(0, |at| at + 1);
    let nested = next_url(&tail[userinfo..]).map_or(tail.len(), |(start, _)| userinfo + start);
    let stop = tail[userinfo..]
        .find(|c: char| URL_END.contains(&c) || matches!(c, '?' | '#' | '&'))
        .map_or(tail.len(), |i| userinfo + i)
        .min(nested);
    let params_follow = matches!(tail[stop..].chars().next(), Some('?' | '#' | '&'));
    let path_end = if params_follow {
        stop
    } else {
        // Sentence punctuation after a URL is not part of it.
        tail[..stop]
            .trim_end_matches(['.', ':'])
            .len()
            .max(userinfo)
    };

    if userinfo > 0 {
        let name = tail[..userinfo - 1].split(':').next().unwrap_or("");
        out.push_str(name);
        out.push(':');
        out.push_str(MARK);
        out.push('@');
    }
    for (i, seg) in tail[userinfo..path_end].split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        // segment 0 is the host — never redacted, it is the diagnosis.
        out.push_str(if i > 0 && looks_secret(seg) {
            MARK
        } else {
            seg
        });
    }
    if !params_follow {
        return &tail[path_end..];
    }

    out.push_str(&tail[stop..stop + 1]);
    let mut rest = &tail[stop + 1..];
    loop {
        let end = rest.find(['&', '#']).unwrap_or(rest.len());
        let param = &rest[..end];
        match param.split_once('=') {
            Some((name, value))
                if !name.contains("://")
                    && secret_param(name)
                    && !(address_param(name) && is_address(value)) =>
            {
                out.push_str(name);
                out.push('=');
                out.push_str(MARK);
            }
            _ => {
                if let Some((start, _)) = next_url(param) {
                    out.push_str(&param[..start]);
                    return &rest[start..];
                }
                out.push_str(param);
            }
        }
        if end == rest.len() {
            return "";
        }
        out.push_str(&rest[end..end + 1]);
        rest = &rest[end + 1..];
    }
}

/// The names (lowercase) whose value is masked when found anywhere in a
/// parameter's name.
const SECRET_NAMES: [&str; 8] = [
    "key",
    "apikey",
    "api_key",
    "token",
    "access_token",
    "secret",
    "auth",
    "password",
];

fn secret_param(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_NAMES.iter().any(|needle| n.contains(needle))
}

/// Parameter names (lowercase, matched whole) that carry a token's contract
/// address. They read as secret because they contain `token`; an
/// address-shaped value under exactly one of these names is public chain
/// data and stays readable. Under any other secret-reading name a value is
/// masked whatever its shape: a 40-hex API key is still a key.
const ADDRESS_NAMES: [&str; 10] = [
    "tokenin",
    "tokenout",
    "inputtoken",
    "outputtoken",
    "fromtoken",
    "totoken",
    "selltoken",
    "buytoken",
    "srctoken",
    "dsttoken",
];

fn address_param(name: &str) -> bool {
    ADDRESS_NAMES.contains(&name.to_ascii_lowercase().as_str())
}

/// A 20-byte hex address.
fn is_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|hex| hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A path segment that looks like a credential: long, opaque, and not a word
/// anyone would put in a URL by hand. Provider key formats (Tenderly, Alchemy,
/// Infura) are 20+ chars of mixed alphanumerics.
fn looks_secret(seg: &str) -> bool {
    if seg.len() < 16 {
        return false;
    }
    if !seg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    let digits = seg.chars().filter(char::is_ascii_digit).count();
    let alpha = seg.chars().filter(char::is_ascii_alphabetic).count();
    // Mixed alphanumeric with no separator words — a hex/base58/uuid-ish blob.
    digits > 0 && alpha > 0 && !seg.contains("-rpc") && !seg.contains("api-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative provider shapes, plus keyless public endpoints that
    /// must pass through untouched. The HOST must survive — a redaction that
    /// makes "which endpoint failed?" unanswerable will be deleted by
    /// whoever debugs next.
    #[test]
    fn credentials_go_but_diagnosis_stays() {
        let cases = [
            (
                "https://mainnet.gateway.tenderly.co/3vK9xQ2mZp7Lw4Nb8Rt1Yc",
                "https://mainnet.gateway.tenderly.co/<redacted>",
            ),
            (
                "https://eth-mainnet.g.alchemy.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://eth-mainnet.g.alchemy.com/v2/<redacted>",
            ),
            (
                "https://user:hunter2@rpc.example.com/x",
                "https://user:<redacted>@rpc.example.com/x",
            ),
            (
                "https://rpc.example.com/path?apikey=abc123&chain=base",
                "https://rpc.example.com/path?apikey=<redacted>&chain=base",
            ),
            // Keyless public endpoints must pass through UNTOUCHED.
            (
                "https://base-rpc.publicnode.com",
                "https://base-rpc.publicnode.com",
            ),
            (
                "https://ethereum-rpc.publicnode.com",
                "https://ethereum-rpc.publicnode.com",
            ),
            ("http://127.0.0.1:8545", "http://127.0.0.1:8545"),
        ];
        for (raw, want) in cases {
            assert_eq!(url(raw), want, "input: {raw}");
        }
    }

    /// The real use: a sentence with a URL embedded in it.
    #[test]
    fn redacts_inside_an_error_sentence() {
        let msg = "fork backend failed for chain `base` during spawn (fork source: \
                   https://base.gateway.tenderly.co/3vK9xQ2mZp7Lw4Nb8Rt1Yc): timed out";
        let out = url(msg);
        assert!(!out.contains("3vK9xQ2mZp7Lw4Nb8Rt1Yc"), "{out}");
        assert!(
            out.contains("base.gateway.tenderly.co"),
            "the host must survive: {out}"
        );
        assert!(
            out.contains("timed out"),
            "the diagnosis must survive: {out}"
        );
    }

    /// A URL wrapped in JSON, after a `key=`, or beside another URL in the
    /// same whitespace-free run: each one is redacted, the wrapping stays.
    #[test]
    fn redacts_urls_wrapped_in_a_body_or_joined_together() {
        let cases = [
            (
                r#"{"error":"https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr"}"#,
                r#"{"error":"https://rpc.example.com/v2/<redacted>"}"#,
            ),
            (
                "url=https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr.",
                "url=https://rpc.example.com/v2/<redacted>.",
            ),
            (
                r#"["https://a.example.com/3vK9xQ2mZp7Lw4Nb8Rt1Yc","wss://b.example.com/9fKd2Lm4Xp7Qw1Nz8Bt3Yr"]"#,
                r#"["https://a.example.com/<redacted>","wss://b.example.com/<redacted>"]"#,
            ),
            // A password holding a character that would otherwise end the URL.
            (
                "(https://user:p(ss,w)rd@rpc.example.com/x)",
                "(https://user:<redacted>@rpc.example.com/x)",
            ),
            // Not a network URL: untouched.
            (
                "file:///tmp/3vK9xQ2mZp7Lw4Nb8Rt1Yc",
                "file:///tmp/3vK9xQ2mZp7Lw4Nb8Rt1Yc",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(url(raw), want, "input: {raw}");
            assert_eq!(text(raw), want, "input: {raw}");
        }
    }

    /// `text` keeps a message's own whitespace; `url` joins words with one
    /// space. A message with no credential comes back from `text` unchanged.
    #[test]
    fn text_keeps_whitespace_and_leaves_plain_messages_alone() {
        let plain = "  server returned an error response:\n\terror code -32000:  nonce too low \n";
        assert_eq!(text(plain), plain);
        let keyed = "error sending request for url\n  (https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr):\n  timed out";
        assert_eq!(
            text(keyed),
            "error sending request for url\n  (https://rpc.example.com/v2/<redacted>):\n  timed out"
        );
        assert_eq!(text(""), "");
    }

    /// An address under a name that is a token's address by its meaning
    /// stays readable. Any other value under such a name goes, and so does
    /// ANY value, address-shaped or not, under a name that says secret.
    #[test]
    fn an_address_stays_only_under_an_address_typed_name() {
        let token = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
        let raw =
            format!("https://api.example.com/v1/routes?tokenIn={token}&amountIn=100&token=abc123");
        assert_eq!(
            text(&raw),
            format!(
                "https://api.example.com/v1/routes?tokenIn={token}&amountIn=100&token=<redacted>"
            )
        );
        // A key that happens to be 0x and forty hex digits is still a key.
        let hex_key = "0x00112233445566778899aabbccddeeff00112233";
        for name in [
            "apikey",
            "api_key",
            "key",
            "token",
            "access_token",
            "secret",
            "auth",
            "password",
            "tokenInKey",
        ] {
            let shown = text(&format!(
                "https://api.example.com/v1?{name}={hex_key}&chain=base"
            ));
            assert_eq!(
                shown,
                format!("https://api.example.com/v1?{name}=<redacted>&chain=base"),
                "{name}"
            );
        }
        // Not an address: masked even under an address-typed name.
        assert_eq!(
            text("https://api.example.com/v1?tokenIn=abc123"),
            "https://api.example.com/v1?tokenIn=<redacted>"
        );
    }

    /// The two lists cannot overlap: no address-typed name is itself one of
    /// the names that say secret.
    #[test]
    fn no_address_typed_name_is_a_secret_name() {
        for name in ADDRESS_NAMES {
            assert!(!SECRET_NAMES.contains(&name), "{name}");
            assert!(
                secret_param(name),
                "{name} is listed because it reads as secret"
            );
        }
    }

    /// A keyed URL carried inside another URL's query, in its fragment, or
    /// glued to the end of another URL is redacted as a URL of its own.
    #[test]
    fn a_url_nested_in_or_joined_to_another_is_redacted_too() {
        let cases = [
            (
                "https://proxy.example.com/fetch?url=https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://proxy.example.com/fetch?url=https://rpc.example.com/v2/<redacted>",
            ),
            // The nested URL's key is followed by the outer URL's next parameter.
            (
                "https://proxy.example.com/fetch?url=https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr&apikey=abc123&chain=base",
                "https://proxy.example.com/fetch?url=https://rpc.example.com/v2/<redacted>&apikey=<redacted>&chain=base",
            ),
            (
                "https://proxy.example.com/fetch?url=wss://user:hunter2@rpc.example.com/x",
                "https://proxy.example.com/fetch?url=wss://user:<redacted>@rpc.example.com/x",
            ),
            (
                "https://app.example.com/cb#next=https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://app.example.com/cb#next=https://rpc.example.com/v2/<redacted>",
            ),
            // Two URLs with nothing between them.
            (
                "https://a.example.com/v2/3vK9xQ2mZp7Lw4Nb8Rt1Ychttps://b.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://a.example.com/v2/<redacted>https://b.example.com/v2/<redacted>",
            ),
            (
                "https://a.example.com/v2/3vK9xQ2mZp7Lw4Nb8Rt10http://b.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://a.example.com/v2/<redacted>http://b.example.com/v2/<redacted>",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(url(raw), want, "input: {raw}");
            assert_eq!(text(raw), want, "input: {raw}");
        }
    }

    /// A secret-named value is masked whole, whatever characters it holds:
    /// to the next parameter, or to the end of the URL.
    #[test]
    fn a_secret_value_is_masked_whole_whatever_it_contains() {
        let cases = [
            (
                "https://rpc.example.com/v1?apikey=ab(cd)ef,gh;ij&chain=base",
                "https://rpc.example.com/v1?apikey=<redacted>&chain=base",
            ),
            (
                r#"https://rpc.example.com/v1?chain=base&token=ab"cd'ef"#,
                "https://rpc.example.com/v1?chain=base&token=<redacted>",
            ),
            (
                "https://rpc.example.com/v1?secret=ab://cd",
                "https://rpc.example.com/v1?secret=<redacted>",
            ),
            (
                "https://app.example.com/cb?state=1#access_token=ab(cd",
                "https://app.example.com/cb?state=1#access_token=<redacted>",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(url(raw), want, "input: {raw}");
            assert_eq!(text(raw), want, "input: {raw}");
        }
    }

    /// A password is masked whole whatever it holds: a `&`, which elsewhere
    /// separates parameters, or text that looks like a URL of its own.
    #[test]
    fn a_password_holding_an_ampersand_or_a_url_is_masked_whole() {
        let cases = [
            (
                "https://user:pa&ss@rpc.example.com/x",
                "https://user:<redacted>@rpc.example.com/x",
            ),
            (
                "(wss://user:pa&key=ss@rpc.example.com/x?chain=base):",
                "(wss://user:<redacted>@rpc.example.com/x?chain=base):",
            ),
            (
                "https://user:https://hunter2@rpc.example.com/x",
                "https://user:<redacted>@rpc.example.com/x",
            ),
            (
                "https://user:pa&ws://hunter2@rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr",
                "https://user:<redacted>@rpc.example.com/v2/<redacted>",
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(url(raw), want, "input: {raw}");
            assert_eq!(text(raw), want, "input: {raw}");
            assert!(!text(raw).contains("ss@"), "input: {raw}");
            assert!(!text(raw).contains("hunter2"), "input: {raw}");
        }
    }
}
