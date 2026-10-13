use crate::engine::HybridTimestamp;
use crate::{ClipContent, DeviceId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u16 = 2;

/// Supported content capabilities advertised during the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Text,
}

/// Reasons why a peer is closing the network connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoodbyeReason {
    Shutdown,
    Unpaired,
    ProtocolError,
    Duplicate,
}

/// Top-level wire protocol messages exchanged between peers over the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        protocol_version: u16,
        device_id: DeviceId,
        device_name: String,
        capabilities: Vec<Capability>,
    },
    ClipUpdate {
        update_id: Uuid,
        origin: DeviceId,
        timestamp: HybridTimestamp,
        content: ClipContent,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Goodbye {
        reason: GoodbyeReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_serializes_with_internal_type_tag() {
        let msg = Message::Ping { nonce: 12345 };
        let json = serde_json::to_string(&msg).expect("failed to serialize");

        // Verify the exact wire format: {"type":"ping","nonce":12345}
        assert_eq!(json, r#"{"type":"ping","nonce":12345}"#);

        // Round-trip back from JSON to Rust struct
        let parsed: Message = serde_json::from_str(&json).expect("failed to deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn goodbye_serializes_with_reason() {
        let msg = Message::Goodbye {
            reason: GoodbyeReason::Shutdown,
        };
        let json = serde_json::to_string(&msg).expect("failed to serialize");

        assert_eq!(json, r#"{"type":"goodbye","reason":"shutdown"}"#);

        let parsed: Message = serde_json::from_str(&json).expect("failed to deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn hello_serializes_with_capabilities() {
        let dev_id: DeviceId = "11111111-1111-1111-1111-111111111111"
            .parse()
            .expect("valid uuid");

        let msg = Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: dev_id,
            device_name: "Howie's Mac".to_string(),
            capabilities: vec![Capability::Text],
        };

        let json = serde_json::to_string(&msg).expect("failed to serialize");

        assert_eq!(
            json,
            r#"{"type":"hello","protocol_version":2,"device_id":"11111111-1111-1111-1111-111111111111","device_name":"Howie's Mac","capabilities":["text"]}"#
        );

        let parsed: Message = serde_json::from_str(&json).expect("failed to deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn clip_update_serializes_cleanly() {
        let update = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let origin_id: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let update_msg = Message::ClipUpdate {
            update_id: update,
            origin: origin_id,
            timestamp: HybridTimestamp::new(1700000000000, 42),
            content: ClipContent::Text {
                text: "sync me".to_string(),
            },
        };

        let json = serde_json::to_string(&update_msg).expect("failed to serialize");

        assert_eq!(
            json,
            r#"{"type":"clip_update","update_id":"22222222-2222-2222-2222-222222222222","origin":"11111111-1111-1111-1111-111111111111","timestamp":{"wall_ms":1700000000000,"counter":42},"content":{"type":"text","text":"sync me"}}"#
        );

        let parsed: Message = serde_json::from_str(&json).expect("failed to deserialize");
        assert_eq!(update_msg, parsed);
    }
}
