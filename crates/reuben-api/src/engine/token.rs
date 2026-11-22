//! Who may use the engine channel: a loopback peer always, any other peer only with the
//! server's token.
//!
//! A connection may open with one credential line, `{"token":"…"}`, before its first request. The
//! line draws no reply. [`admit`] decides each connection's first line; a server calls it once per
//! connection and never again, so a credential anywhere later is an unreadable request. The token
//! rides in clear: the channel has no TLS, and an encrypted network (Tailscale) is what keeps it
//! private.

use alloc::string::String;
use core::fmt;
use core::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::wire::Response;

/// The environment variable a server and every client read the engine channel's token from
/// when no `--token` names one.
pub const ENGINE_TOKEN_ENV: &str = "REUBEN_ENGINE_TOKEN";

/// The refusal a non-loopback peer gets when its first line carries no token.
pub const TOKEN_REQUIRED: &str = "this engine takes connections from other machines only with \
                                  its token: pass --token or set REUBEN_ENGINE_TOKEN";

/// The refusal a peer gets for a token that is not the server's.
pub const REFUSED_TOKEN: &str = "the engine refused this token";

/// The engine channel's shared secret. Never printed: its `Debug` redacts it, and it is
/// compared only through [`matches`](Self::matches).
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// A token, refusing an empty or all-whitespace one.
    pub fn new(token: impl Into<String>) -> Result<Self, EmptyToken> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(EmptyToken);
        }
        Ok(Self(token))
    }

    /// Whether `other` is this token. Constant time over the bytes of equal-length inputs, so a
    /// peer cannot find the token a byte at a time by timing refusals.
    pub fn matches(&self, other: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), other.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
    }
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.matches(&other.0)
    }
}

impl Eq for Token {}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// [`Token::new`] was given nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyToken;

impl fmt::Display for EmptyToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a engine channel token must not be empty")
    }
}

impl core::error::Error for EmptyToken {}

/// The credential line's shape. It has no `verb`, so it never parses as a request.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    token: String,
}

/// The newline-terminated credential line a client sends before its first request.
pub fn credential_line(token: &Token) -> String {
    let mut line = serde_json::to_string(&Credential {
        token: token.0.clone(),
    })
    .expect("a credential always serializes");
    line.push('\n');
    line
}

/// What a connection's first line is.
#[derive(Debug, Clone, PartialEq)]
pub enum Admit {
    /// A credential the server accepts: read on, with no reply.
    Credential,
    /// A request the peer may make: serve it.
    Request,
    /// Answer this, then close.
    Refused(Response),
}

/// Decide a connection's first line from a peer at `peer`, on a server expecting `expected`.
///
/// A server with no token admits everyone and ignores a credential. A server with one refuses a
/// wrong token from any peer, loopback included, so a misconfigured client hears about it at once;
/// it serves a loopback peer without a token, and refuses any other peer that sent none.
pub fn admit(expected: Option<&Token>, peer: IpAddr, first: &str) -> Admit {
    if let Ok(Credential { token }) = serde_json::from_str::<Credential>(first) {
        return match expected {
            Some(expected) if !expected.matches(&token) => Admit::Refused(refused(REFUSED_TOKEN)),
            _ => Admit::Credential,
        };
    }
    if expected.is_none() || peer.to_canonical().is_loopback() {
        Admit::Request
    } else {
        Admit::Refused(refused(TOKEN_REQUIRED))
    }
}

fn refused(message: &str) -> Response {
    Response::Error {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::ToString;

    use super::*;
    use crate::engine::wire::Request;

    fn token(t: &str) -> Token {
        Token::new(t).expect("a token")
    }

    #[test]
    fn a_token_is_never_empty() {
        assert_eq!(Token::new("").err(), Some(EmptyToken));
        assert_eq!(Token::new("  \t").err(), Some(EmptyToken));
        assert!(Token::new("s3cret").is_ok());
    }

    #[test]
    fn a_token_does_not_print() {
        let printed = format!("{:?}", token("s3cret"));
        assert!(!printed.contains("s3cret"), "{printed}");
    }

    #[test]
    fn a_token_matches_only_itself() {
        let t = token("s3cret");
        assert!(t.matches("s3cret"));
        assert!(!t.matches("s3creT"));
        assert!(!t.matches("s3cre"));
        assert!(!t.matches("s3crets"));
    }

    #[test]
    fn a_credential_line_is_not_a_request() {
        let line = credential_line(&token("s3cret"));
        assert!(line.ends_with('\n'));
        assert!(Request::from_ndjson(&line).is_err(), "{line}");
    }

    #[test]
    fn admission_table() {
        let good = token("good");
        let loopback = ["127.0.0.1", "::1", "::ffff:127.0.0.1"];
        let remote = ["192.168.1.4", "100.72.0.1", "::ffff:192.168.1.4"];
        let good_line = credential_line(&good);
        let bad_line = credential_line(&token("bad"));
        let ping = Request::Ping.to_ndjson();
        let required = Admit::Refused(refused(TOKEN_REQUIRED));
        let wrong = Admit::Refused(refused(REFUSED_TOKEN));

        for peer in loopback.iter().chain(&remote) {
            let ip: IpAddr = peer.parse().expect("an ip");
            let is_loopback = loopback.contains(peer);
            // No token on the server: everyone is served, a credential is ignored.
            assert_eq!(admit(None, ip, &good_line), Admit::Credential, "{peer}");
            assert_eq!(admit(None, ip, &bad_line), Admit::Credential, "{peer}");
            assert_eq!(admit(None, ip, &ping), Admit::Request, "{peer}");
            // A token on the server.
            assert_eq!(
                admit(Some(&good), ip, &good_line),
                Admit::Credential,
                "{peer}"
            );
            assert_eq!(admit(Some(&good), ip, &bad_line), wrong, "{peer}");
            let expected = if is_loopback {
                Admit::Request
            } else {
                required.clone()
            };
            assert_eq!(admit(Some(&good), ip, &ping), expected, "{peer}");
        }
    }

    #[test]
    fn a_refusal_never_carries_the_token() {
        let good = token("good");
        let ip: IpAddr = "192.168.1.4".parse().expect("an ip");
        let Admit::Refused(response) = admit(Some(&good), ip, &credential_line(&token("guess")))
        else {
            panic!("refused");
        };
        let line = response.to_ndjson();
        assert!(!line.contains("good") && !line.contains("guess"), "{line}");
        assert!(line.contains(&REFUSED_TOKEN.to_string()), "{line}");
    }
}
