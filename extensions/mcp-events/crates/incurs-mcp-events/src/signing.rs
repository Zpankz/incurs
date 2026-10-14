//! Standard Webhooks signing: `v1,` + base64 HMAC-SHA256 over `id.timestamp.body`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

const PREFIX: &str = "whsec_";
const MIN_KEY_BYTES: usize = 24;
const MAX_KEY_BYTES: usize = 64;
const TOLERANCE_SECONDS: i64 = 300;

/// A parsed `whsec_` signing secret.
#[derive(Clone)]
pub struct Secret {
    text: String,
    key: Vec<u8>,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    MissingPrefix,
    NotBase64,
    Length(usize),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPrefix => write!(formatter, "secret must start with {PREFIX}"),
            Self::NotBase64 => write!(formatter, "secret must be base64 after {PREFIX}"),
            Self::Length(length) => write!(
                formatter,
                "secret must decode to {MIN_KEY_BYTES}-{MAX_KEY_BYTES} bytes, got {length}"
            ),
        }
    }
}

impl Secret {
    pub fn parse(text: &str) -> Result<Self, SecretError> {
        let encoded = text
            .strip_prefix(PREFIX)
            .ok_or(SecretError::MissingPrefix)?;
        let key = STANDARD
            .decode(encoded)
            .map_err(|_| SecretError::NotBase64)?;
        if !(MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&key.len()) {
            return Err(SecretError::Length(key.len()));
        }
        Ok(Self {
            text: text.to_string(),
            key,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn mac(&self, message_id: &str, timestamp: &str, body: &[u8]) -> Vec<u8> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(message_id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.finalize().into_bytes().to_vec()
    }
}

pub fn sign(secret: &Secret, message_id: &str, timestamp: i64, body: &[u8]) -> String {
    format!(
        "v1,{}",
        STANDARD.encode(secret.mac(message_id, &timestamp.to_string(), body))
    )
}

/// The four headers every outbound webhook carries.
pub fn signed_headers(
    secret: &Secret,
    message_id: &str,
    timestamp: i64,
    body: &[u8],
    subscription_id: &str,
) -> Vec<(String, String)> {
    vec![
        ("content-type".to_string(), "application/json".to_string()),
        ("webhook-id".to_string(), message_id.to_string()),
        ("webhook-timestamp".to_string(), timestamp.to_string()),
        (
            "webhook-signature".to_string(),
            sign(secret, message_id, timestamp, body),
        ),
        (
            "X-MCP-Subscription-Id".to_string(),
            subscription_id.to_string(),
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Timestamp,
    Stale,
    Signature,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Timestamp => "webhook-timestamp is not a Unix-second integer",
            Self::Stale => "webhook-timestamp is outside the tolerance window",
            Self::Signature => "no webhook-signature entry matches",
        })
    }
}

/// Verify a received request over its raw body bytes. Any space-separated `v1`
/// entry may match, which allows key rotation.
pub fn verify(
    secret: &Secret,
    message_id: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
    now_s: i64,
) -> Result<(), VerifyError> {
    let sent: i64 = timestamp
        .trim()
        .parse()
        .map_err(|_| VerifyError::Timestamp)?;
    if (now_s - sent).abs() > TOLERANCE_SECONDS {
        return Err(VerifyError::Stale);
    }
    let expected = secret.mac(message_id, timestamp.trim(), body);
    let matched = signature
        .split(' ')
        .filter_map(|entry| entry.strip_prefix("v1,"))
        .filter_map(|encoded| STANDARD.decode(encoded).ok())
        .any(|candidate| bool::from(candidate.ct_eq(&expected)));
    if matched {
        Ok(())
    } else {
        Err(VerifyError::Signature)
    }
}
