//! Karma economy primitives, Ed25519 work-receipt signing/verification, and priority tiers.

use std::net::{IpAddr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::NodeIdentity;

/// Tokens served per 1 Karma point earned (minimum 1 Karma for any non-empty response).
pub const TOKENS_PER_KARMA: u64 = 10;

/// Minimum Karma balance to reach the `CONTRIBUTOR` tier.
pub const TIER_CONTRIBUTOR_MIN: i64 = 1;

/// Minimum Karma balance to reach the `VIP_ALPHA` priority tier.
pub const TIER_VIP_ALPHA_MIN: i64 = 2_000;

/// Maximum tokens accepted in a single receipt to prevent single-shot abuse.
pub const MAX_TOKENS_PER_RECEIPT: u64 = 100_000;

/// Maximum Karma a single `(client, worker)` pair can earn per rolling hour (anti-collusion).
pub const MAX_PAIR_KARMA_PER_HOUR: i64 = 500;

/// Gentle cloud inference sink: 1 Karma spent per 20 tokens on non-LAN cloud inference
/// (only deducted from positive balances; community users with 0 Karma are never blocked).
pub const CLOUD_SPEND_TOKENS_PER_KARMA: u64 = 20;

/// Soft weekly inactivity decay percentage applied to balances above [`WEEKLY_DECAY_FLOOR`].
pub const WEEKLY_DECAY_PERCENT: i64 = 10;

/// Balances at or below this floor are exempt from weekly decay.
pub const WEEKLY_DECAY_FLOOR: i64 = 100;

/// Maximum allowed age (in seconds) for a work receipt timestamp (10 minutes).
pub const RECEIPT_MAX_AGE_SECS: u64 = 600;

/// Priority tier derived from a node's Karma balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum KarmaTier {
    Community = 1,
    Contributor = 2,
    VipAlpha = 3,
}

impl KarmaTier {
    /// Derive tier from a Karma balance.
    pub fn from_balance(balance: i64) -> Self {
        if balance >= TIER_VIP_ALPHA_MIN {
            Self::VipAlpha
        } else if balance >= TIER_CONTRIBUTOR_MIN {
            Self::Contributor
        } else {
            Self::Community
        }
    }

    /// Canonical wire/display name for the tier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VipAlpha => "VIP_ALPHA",
            Self::Contributor => "CONTRIBUTOR",
            Self::Community => "COMMUNITY",
        }
    }

    /// Numeric priority rank for worker scheduling (`3` = highest).
    pub fn priority_rank(&self) -> u8 {
        *self as u8
    }

    /// Karma threshold required to reach the next tier, or `None` if already `VIP_ALPHA`.
    pub fn next_tier_threshold(balance: i64) -> Option<i64> {
        if balance >= TIER_VIP_ALPHA_MIN {
            None
        } else if balance >= TIER_CONTRIBUTOR_MIN {
            Some(TIER_VIP_ALPHA_MIN)
        } else {
            Some(TIER_CONTRIBUTOR_MIN)
        }
    }
}

/// Cryptographically signed proof-of-work receipt emitted by a client after
/// receiving inference tokens from a remote worker node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkReceipt {
    pub client_node_id: String,
    pub worker_node_id: String,
    pub tokens_processed: u64,
    pub timestamp: u64,
    pub nonce: String,
    #[serde(default)]
    pub is_lan: bool,
    pub signature_hash: String,
}

impl WorkReceipt {
    /// Canonical byte payload signed by the client's Ed25519 private key.
    pub fn signing_payload(&self) -> String {
        format!(
            "hivebear-karma-v1:{}:{}:{}:{}:{}:{}",
            self.client_node_id,
            self.worker_node_id,
            self.tokens_processed,
            self.timestamp,
            self.nonce,
            if self.is_lan { 1 } else { 0 }
        )
    }

    /// Create and sign a new [`WorkReceipt`] using the client's [`NodeIdentity`].
    pub fn sign(
        client_identity: &NodeIdentity,
        worker_node_id: impl Into<String>,
        tokens_processed: u64,
        is_lan: bool,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut receipt = Self {
            client_node_id: client_identity.node_id.to_hex(),
            worker_node_id: worker_node_id.into(),
            tokens_processed,
            timestamp,
            nonce: Uuid::new_v4().to_string(),
            is_lan,
            signature_hash: String::new(),
        };
        let payload = receipt.signing_payload();
        let sig = client_identity.signing_key.sign(payload.as_bytes());
        receipt.signature_hash = hex::encode(sig.to_bytes());
        receipt
    }

    /// Verify the receipt's Ed25519 signature against `client_node_id`.
    pub fn verify_signature(&self) -> std::result::Result<(), String> {
        let pubkey_bytes = hex::decode(&self.client_node_id)
            .map_err(|_| "Invalid client_node_id hex encoding".to_string())?;
        let pubkey_arr: [u8; 32] = pubkey_bytes
            .try_into()
            .map_err(|_| "client_node_id must be a 32-byte Ed25519 public key".to_string())?;
        let verifying_key = VerifyingKey::from_bytes(&pubkey_arr)
            .map_err(|_| "Invalid Ed25519 public key in client_node_id".to_string())?;

        let sig_bytes = hex::decode(&self.signature_hash)
            .map_err(|_| "Invalid signature_hash hex encoding".to_string())?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| "signature_hash must be a 64-byte Ed25519 signature".to_string())?;
        let signature = Signature::from_bytes(&sig_arr);

        let payload = self.signing_payload();
        verifying_key
            .verify(payload.as_bytes(), &signature)
            .map_err(|_| "Invalid Ed25519 signature on work receipt".to_string())
    }
}

/// Validate that a hex node ID is a well-formed 32-byte Ed25519 public key.
pub fn validate_ed25519_node_id(node_id_hex: &str) -> std::result::Result<VerifyingKey, String> {
    let pubkey_bytes =
        hex::decode(node_id_hex).map_err(|_| "Invalid node_id hex encoding".to_string())?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| "node_id must be a 32-byte Ed25519 public key".to_string())?;
    VerifyingKey::from_bytes(&pubkey_arr)
        .map_err(|_| "Invalid Ed25519 public key in node_id".to_string())
}

/// Sign a coordinator control action (`register`, `heartbeat`, `deregister`, `signals`)
/// with the node's Ed25519 private key. Returns `(timestamp_secs, signature_hex)`.
pub fn sign_coordinator_action(identity: &NodeIdentity, action: &str) -> (u64, String) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let msg = format!(
        "hivebear-coord-v1:{}:{}:{}",
        identity.node_id.to_hex(),
        action,
        ts
    );
    let sig = identity.signing_key.sign(msg.as_bytes());
    (ts, hex::encode(sig.to_bytes()))
}

/// Verify an Ed25519 signature on a coordinator control action (`register`, `heartbeat`,
/// `deregister`, `signals`) within [`RECEIPT_MAX_AGE_SECS`].
pub fn verify_coordinator_action(
    node_id_hex: &str,
    action: &str,
    timestamp: u64,
    sig_hex: &str,
) -> std::result::Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.abs_diff(timestamp) > RECEIPT_MAX_AGE_SECS {
        return Err("Coordinator signature timestamp expired".to_string());
    }
    let verifying_key = validate_ed25519_node_id(node_id_hex)?;
    let sig_bytes =
        hex::decode(sig_hex).map_err(|_| "Invalid signature hex encoding".to_string())?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "Signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);
    let msg = format!("hivebear-coord-v1:{}:{}:{}", node_id_hex, action, timestamp);
    verifying_key
        .verify(msg.as_bytes(), &signature)
        .map_err(|_| "Invalid Ed25519 signature for coordinator action".to_string())
}

/// Calculate Karma earned by a worker for serving `tokens_processed` tokens.
pub fn karma_earned_for_tokens(tokens_processed: u64) -> i64 {
    if tokens_processed == 0 {
        return 0;
    }
    ((tokens_processed / TOKENS_PER_KARMA).max(1)) as i64
}

/// Calculate gentle Karma sink for a client consuming remote inference.
/// Local WiFi LAN (`is_lan == true`) is always 100% free (`0`).
pub fn cloud_karma_cost_for_tokens(tokens_processed: u64, is_lan: bool) -> i64 {
    if is_lan || tokens_processed == 0 {
        return 0;
    }
    (tokens_processed / CLOUD_SPEND_TOKENS_PER_KARMA) as i64
}

/// Apply weekly soft decay to a balance above [`WEEKLY_DECAY_FLOOR`].
pub fn apply_weekly_decay(balance: i64) -> i64 {
    if balance <= WEEKLY_DECAY_FLOOR {
        return balance;
    }
    let decay = (balance * WEEKLY_DECAY_PERCENT) / 100;
    (balance - decay.max(1)).max(WEEKLY_DECAY_FLOOR)
}

/// Returns `true` if `addr` is a local/private LAN address (RFC1918, loopback, link-local).
pub fn is_lan_addr(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_sign_and_verify_roundtrip() {
        let client = NodeIdentity::generate();
        let worker = NodeIdentity::generate();
        let receipt = WorkReceipt::sign(&client, worker.node_id.to_hex(), 250, false);
        assert!(receipt.verify_signature().is_ok());
    }

    #[test]
    fn receipt_tampering_fails_verification() {
        let client = NodeIdentity::generate();
        let worker = NodeIdentity::generate();
        let mut receipt = WorkReceipt::sign(&client, worker.node_id.to_hex(), 250, false);
        receipt.tokens_processed = 99_999;
        assert!(receipt.verify_signature().is_err());
    }

    #[test]
    fn receipt_wrong_signer_fails_verification() {
        let client = NodeIdentity::generate();
        let imposter = NodeIdentity::generate();
        let worker = NodeIdentity::generate();
        let mut receipt = WorkReceipt::sign(&imposter, worker.node_id.to_hex(), 100, false);
        receipt.client_node_id = client.node_id.to_hex();
        assert!(receipt.verify_signature().is_err());
    }

    #[test]
    fn economy_math_and_tiers() {
        assert_eq!(karma_earned_for_tokens(0), 0);
        assert_eq!(karma_earned_for_tokens(5), 1);
        assert_eq!(karma_earned_for_tokens(100), 10);
        assert_eq!(cloud_karma_cost_for_tokens(100, true), 0);
        assert_eq!(cloud_karma_cost_for_tokens(100, false), 5);

        assert_eq!(KarmaTier::from_balance(0), KarmaTier::Community);
        assert_eq!(KarmaTier::from_balance(1), KarmaTier::Contributor);
        assert_eq!(KarmaTier::from_balance(1999), KarmaTier::Contributor);
        assert_eq!(KarmaTier::from_balance(2000), KarmaTier::VipAlpha);

        assert_eq!(apply_weekly_decay(50), 50);
        assert_eq!(apply_weekly_decay(1000), 900);
    }

    #[test]
    fn lan_address_detection() {
        assert!(is_lan_addr("192.168.0.4:7878".parse().unwrap()));
        assert!(is_lan_addr("10.0.0.5:7878".parse().unwrap()));
        assert!(is_lan_addr("127.0.0.1:7878".parse().unwrap()));
        assert!(!is_lan_addr("34.63.209.220:7878".parse().unwrap()));
    }
}
