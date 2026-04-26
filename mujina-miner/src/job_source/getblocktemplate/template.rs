//! Parsing for `getblocktemplate` JSON responses.
//!
//! [`GbtResponse`] is the wire-shape struct (serde-deriving).
//! [`GbtResponse::parse`] converts it into a [`ParsedTemplate`]
//! carrying decoded `bitcoin` types ready for coinbase building and
//! merkle computation.

use std::str::FromStr;

use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, CompactTarget, Target, Txid, Wtxid};
use serde::Deserialize;
use thiserror::Error;

/// Raw JSON shape of a `getblocktemplate` response.
#[derive(Debug, Deserialize)]
pub struct GbtResponse {
    pub version: i32,
    pub previousblockhash: String,
    #[serde(default)]
    pub transactions: Vec<RawTemplateTx>,
    pub coinbasevalue: u64,
    #[serde(default)]
    pub longpollid: Option<String>,
    pub target: String,
    pub mintime: u32,
    pub curtime: u32,
    pub bits: String,
    pub height: u32,
    #[serde(default)]
    pub default_witness_commitment: Option<String>,
    #[serde(default)]
    pub signet_challenge: Option<String>,
    #[serde(default)]
    pub sigoplimit: Option<u32>,
    #[serde(default)]
    pub sizelimit: Option<u32>,
    #[serde(default)]
    pub weightlimit: Option<u32>,
}

/// Raw JSON shape of a single transaction entry in the template.
#[derive(Debug, Deserialize)]
pub struct RawTemplateTx {
    pub data: String,
    pub txid: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub depends: Vec<u32>,
    pub fee: u64,
    #[serde(default)]
    pub sigops: Option<u32>,
    pub weight: u32,
}

/// Parsed template ready for coinbase building.
#[derive(Debug, Clone)]
pub struct ParsedTemplate {
    pub height: u32,
    pub version: i32,
    pub prev_blockhash: BlockHash,
    pub bits: CompactTarget,
    pub target: Target,
    pub curtime: u32,
    pub mintime: u32,
    pub coinbase_value: u64,
    pub default_witness_commitment: Option<Vec<u8>>,
    pub signet_challenge: Option<Vec<u8>>,
    pub longpollid: Option<String>,
    pub transactions: Vec<TemplateTx>,
}

/// One transaction from the template, parsed.
#[derive(Debug, Clone)]
pub struct TemplateTx {
    pub txid: Txid,
    pub wtxid: Wtxid,
    pub raw: Vec<u8>,
    pub fee_sats: u64,
    pub weight: u32,
}

/// Errors raised while parsing a [`GbtResponse`].
#[derive(Error, Debug)]
pub enum ParseError {
    #[error("hex decode of {field}: {source}")]
    Hex {
        field: &'static str,
        #[source]
        source: hex::FromHexError,
    },

    #[error("invalid {field}: {message}")]
    BadField {
        field: &'static str,
        message: String,
    },
}

impl GbtResponse {
    /// Decode hex-encoded fields and produce a [`ParsedTemplate`].
    pub fn parse(self) -> Result<ParsedTemplate, ParseError> {
        let prev_blockhash =
            BlockHash::from_str(&self.previousblockhash).map_err(|e| ParseError::BadField {
                field: "previousblockhash",
                message: e.to_string(),
            })?;

        let bits = parse_bits(&self.bits)?;
        let target = parse_target(&self.target)?;
        let default_witness_commitment = decode_optional_hex(
            "default_witness_commitment",
            self.default_witness_commitment,
        )?;
        let signet_challenge = decode_optional_hex("signet_challenge", self.signet_challenge)?;

        let transactions = self
            .transactions
            .into_iter()
            .map(RawTemplateTx::parse)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ParsedTemplate {
            height: self.height,
            version: self.version,
            prev_blockhash,
            bits,
            target,
            curtime: self.curtime,
            mintime: self.mintime,
            coinbase_value: self.coinbasevalue,
            default_witness_commitment,
            signet_challenge,
            longpollid: self.longpollid,
            transactions,
        })
    }
}

impl RawTemplateTx {
    fn parse(self) -> Result<TemplateTx, ParseError> {
        let txid = Txid::from_str(&self.txid).map_err(|e| ParseError::BadField {
            field: "transactions[].txid",
            message: e.to_string(),
        })?;
        let wtxid = match self.hash {
            Some(h) => Wtxid::from_str(&h).map_err(|e| ParseError::BadField {
                field: "transactions[].hash",
                message: e.to_string(),
            })?,
            None => Wtxid::from_byte_array(txid.to_byte_array()),
        };
        let raw = hex::decode(&self.data).map_err(|source| ParseError::Hex {
            field: "transactions[].data",
            source,
        })?;
        Ok(TemplateTx {
            txid,
            wtxid,
            raw,
            fee_sats: self.fee,
            weight: self.weight,
        })
    }
}

fn parse_bits(hex_str: &str) -> Result<CompactTarget, ParseError> {
    let bytes = hex::decode(hex_str).map_err(|source| ParseError::Hex {
        field: "bits",
        source,
    })?;
    if bytes.len() != 4 {
        return Err(ParseError::BadField {
            field: "bits",
            message: format!("expected 4 bytes, got {}", bytes.len()),
        });
    }
    let raw = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    Ok(CompactTarget::from_consensus(raw))
}

fn parse_target(hex_str: &str) -> Result<Target, ParseError> {
    // Bitcoin Core renders the network target as a 32-byte big-endian
    // hex string. `Target::from_be_bytes` matches that ordering.
    let bytes = hex::decode(hex_str).map_err(|source| ParseError::Hex {
        field: "target",
        source,
    })?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| ParseError::BadField {
            field: "target",
            message: format!("expected 32 bytes, got {}", v.len()),
        })?;
    Ok(Target::from_be_bytes(array))
}

fn decode_optional_hex(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<Vec<u8>>, ParseError> {
    value
        .map(|h| hex::decode(&h).map_err(|source| ParseError::Hex { field, source }))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic template fixture --- not from a real node, but
    /// shaped exactly like what `bitcoind` emits. Real fixtures get
    /// captured into `tests/fixtures/` once a live node is on hand.
    fn synthetic_template_json() -> &'static str {
        r#"{
            "version": 536870912,
            "previousblockhash": "00000000000000000001543bc4d2b5e1434cb64af8995e21fe15ca708a6ac9e3",
            "transactions": [
                {
                    "data": "0200000001abcdef0000000000000000000000000000000000000000000000000000000000000000ffffffff00ffffffff0100e1f50500000000160014000102030405060708090a0b0c0d0e0f1011121300000000",
                    "txid": "1111111111111111111111111111111111111111111111111111111111111111",
                    "hash": "2222222222222222222222222222222222222222222222222222222222222222",
                    "depends": [],
                    "fee": 1234,
                    "sigops": 4,
                    "weight": 568
                }
            ],
            "coinbasevalue": 312500000,
            "longpollid": "abc123",
            "target": "0000000000000000000302a90000000000000000000000000000000000000000",
            "mintime": 1738182000,
            "mutable": ["time", "transactions", "prevblock"],
            "noncerange": "00000000ffffffff",
            "sigoplimit": 80000,
            "sizelimit": 4000000,
            "weightlimit": 4000000,
            "curtime": 1738182030,
            "bits": "17029a8a",
            "height": 881423,
            "default_witness_commitment": "6a24aa21a9edb395560ab72068c2afb1d7e6c7db26b81348208 9d2c4d2471b9036c5e826140601",
            "signet_challenge": null
        }"#
    }

    #[test]
    fn parses_synthetic_response_with_all_fields() {
        // The synthetic JSON is hand-built so we can target the
        // values; the parse logic still has to do the real work
        // (hex decoding, byte ordering, type construction).
        let json = synthetic_template_json().replace(' ', "");
        let raw: GbtResponse = serde_json::from_str(&json).unwrap();
        let parsed = raw.parse().unwrap();

        assert_eq!(parsed.height, 881423);
        assert_eq!(parsed.version, 536870912);
        assert_eq!(parsed.coinbase_value, 312500000);
        assert_eq!(parsed.curtime, 1738182030);
        assert_eq!(parsed.mintime, 1738182000);
        assert_eq!(parsed.longpollid.as_deref(), Some("abc123"));
        assert_eq!(parsed.signet_challenge, None);
        assert!(parsed.default_witness_commitment.is_some());
        assert_eq!(parsed.transactions.len(), 1);
        assert_eq!(parsed.transactions[0].fee_sats, 1234);
        assert_eq!(parsed.transactions[0].weight, 568);
    }

    #[test]
    fn parses_template_without_witness_commitment() {
        let json = r#"{
            "version": 1,
            "previousblockhash": "0000000000000000000000000000000000000000000000000000000000000000",
            "transactions": [],
            "coinbasevalue": 5000000000,
            "target": "00000000ffff0000000000000000000000000000000000000000000000000000",
            "mintime": 0,
            "curtime": 1,
            "bits": "1d00ffff",
            "height": 0
        }"#;
        let raw: GbtResponse = serde_json::from_str(json).unwrap();
        let parsed = raw.parse().unwrap();
        assert!(parsed.default_witness_commitment.is_none());
        assert!(parsed.signet_challenge.is_none());
    }

    #[test]
    fn parses_signet_challenge_when_present() {
        let json = r#"{
            "version": 1,
            "previousblockhash": "0000000000000000000000000000000000000000000000000000000000000000",
            "transactions": [],
            "coinbasevalue": 0,
            "target": "00000000ffff0000000000000000000000000000000000000000000000000000",
            "mintime": 0,
            "curtime": 1,
            "bits": "1d00ffff",
            "height": 0,
            "signet_challenge": "512103ad5e0edad18cb1f0fc0d28a3d4f1f3e445640337489abb10404f2d1e086be430210359ef5021964fe22d6f8e05b2463c9540ce96883fe3b278760f048f5189f2e6c452ae"
        }"#;
        let raw: GbtResponse = serde_json::from_str(json).unwrap();
        let parsed = raw.parse().unwrap();
        let challenge = parsed.signet_challenge.unwrap();
        assert_eq!(challenge.len(), 71);
        assert_eq!(challenge[0], 0x51); // OP_1 (start of 1-of-2 multisig)
    }

    #[test]
    fn rejects_bad_blockhash() {
        let json = r#"{
            "version": 1,
            "previousblockhash": "not-a-hash",
            "transactions": [],
            "coinbasevalue": 0,
            "target": "00000000ffff0000000000000000000000000000000000000000000000000000",
            "mintime": 0,
            "curtime": 1,
            "bits": "1d00ffff",
            "height": 0
        }"#;
        let raw: GbtResponse = serde_json::from_str(json).unwrap();
        let err = raw.parse().unwrap_err();
        assert!(
            matches!(
                err,
                ParseError::BadField {
                    field: "previousblockhash",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_short_target() {
        let json = r#"{
            "version": 1,
            "previousblockhash": "0000000000000000000000000000000000000000000000000000000000000000",
            "transactions": [],
            "coinbasevalue": 0,
            "target": "deadbeef",
            "mintime": 0,
            "curtime": 1,
            "bits": "1d00ffff",
            "height": 0
        }"#;
        let raw: GbtResponse = serde_json::from_str(json).unwrap();
        let err = raw.parse().unwrap_err();
        assert!(
            matches!(
                err,
                ParseError::BadField {
                    field: "target",
                    ..
                }
            ),
            "got {err:?}"
        );
    }
}
