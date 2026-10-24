use base64::Engine;
use serde::{Deserialize, Serialize};

/// Engine-owned continuation saved independently of the model-facing execution record.
///
/// A checkpoint contains trusted bytecode. An executor must verify `identity`
/// before loading it and reject incompatible data instead of replaying source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCheckpoint {
    /// Executor build and complete program fingerprint.
    pub identity: String,
    /// Opaque engine bytes, encoded as base64 on JSON transports.
    #[serde(with = "bytes")]
    pub bytes: Vec<u8>,
}

mod bytes {
    use super::*;

    pub fn serialize<S: serde::Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_transport_uses_literal_base64_and_rejects_invalid_bytes() {
        let value = ExecutionCheckpoint {
            identity: "fixture".into(),
            bytes: vec![0, 1, 255],
        };
        let wire = serde_json::json!({"identity":"fixture","bytes":"AAH/"});
        assert_eq!(serde_json::to_value(&value).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ExecutionCheckpoint>(wire).unwrap(),
            value
        );
        assert!(
            serde_json::from_str::<ExecutionCheckpoint>(r#"{"identity":"fixture","bytes":"!"}"#)
                .is_err()
        );
    }
}
