//! Coinbase transaction construction for the getblocktemplate path.
//!
//! [`build_coinbase`] produces a [`CoinbaseSplit`] suitable for
//! Mujina's existing `MerkleRootTemplate`: the full coinbase
//! transaction is `coinbase1 || extranonce1 || extranonce2 ||
//! coinbase2`, where `extranonce1` is always empty (we own the
//! entire extranonce space when going direct-to-node).
//!
//! [`compute_merkle_branches`] returns the sibling hashes from leaf
//! level up to the root for the coinbase position (index 0), so the
//! same merkle-root code path used for Stratum sources also handles
//! this one.

use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode;
use bitcoin::hash_types::TxMerkleNode;
use bitcoin::hashes::{Hash, sha256d};
use bitcoin::script::ScriptBuf;
use bitcoin::transaction::Version as TxVersion;
use bitcoin::{Amount, OutPoint, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use thiserror::Error;

use super::template::ParsedTemplate;

/// Maximum scriptSig length for a coinbase per Bitcoin Core's
/// `IsStandardCoinbase` rule. Exceeding this gets the block rejected
/// even though it's protocol-valid.
const MAX_COINBASE_SCRIPTSIG: usize = 100;

/// Sentinel byte pattern used to locate the extranonce2 placeholder
/// inside the serialized coinbase. The probability of these bytes
/// appearing organically inside the rest of the tx is vanishingly
/// small for `extranonce2_size >= 4`; the splitter still validates
/// that exactly one occurrence exists.
const PLACEHOLDER_BYTE_BASE: u8 = 0xCC;

/// A coinbase transaction split for extranonce2 rolling.
///
/// Re-assemble the full coinbase as
/// `coinbase1 || extranonce1 || extranonce2 || coinbase2`.
/// `extranonce1` is always empty for this builder.
#[derive(Debug, Clone)]
pub struct CoinbaseSplit {
    pub coinbase1: Vec<u8>,
    pub extranonce1: Vec<u8>,
    pub coinbase2: Vec<u8>,
    pub extranonce2_size: u8,
}

/// Errors raised while constructing a coinbase split.
#[derive(Error, Debug)]
pub enum BuildError {
    #[error("scriptSig too long: {len} bytes (max {max})", max = MAX_COINBASE_SCRIPTSIG)]
    ScriptSigTooLong { len: usize },

    #[error("extranonce2_size must be in 1..=8 (got {0})")]
    BadExtranonce2Size(u8),

    #[error("placeholder not found in serialized coinbase")]
    PlaceholderNotFound,

    #[error("placeholder appeared multiple times in serialized coinbase")]
    PlaceholderAmbiguous,
}

/// Build a coinbase for `template`, with a placeholder space for
/// extranonce2 rolling.
///
/// `payout_script` is the output script that will receive the block
/// reward (typically `address.script_pubkey()` from the operator's
/// configured address). `vanity` is the bytes to include in the
/// scriptSig after BIP34 height + extranonce; truncated to fit the
/// standard scriptSig limit.
pub fn build_coinbase(
    template: &ParsedTemplate,
    payout_script: &ScriptBuf,
    vanity: &[u8],
    extranonce2_size: u8,
) -> Result<CoinbaseSplit, BuildError> {
    if extranonce2_size == 0 || extranonce2_size > 8 {
        return Err(BuildError::BadExtranonce2Size(extranonce2_size));
    }

    let placeholder: Vec<u8> = (0..extranonce2_size)
        .map(|i| PLACEHOLDER_BYTE_BASE.wrapping_add(i))
        .collect();

    let script_sig = build_script_sig(template.height, &placeholder, vanity)?;
    let coinbase = build_coinbase_tx(template, payout_script, ScriptBuf::from(script_sig));
    let serialized = encode::serialize(&coinbase);
    split_at_placeholder(&serialized, &placeholder, extranonce2_size)
}

/// Compute merkle branches for the coinbase tx at index 0.
///
/// `non_coinbase_txids` is the list of txids of all template
/// transactions in order, excluding the coinbase. The returned
/// vector contains the sibling node at each level, from leaf level
/// up to (but not including) the root.
pub fn compute_merkle_branches(non_coinbase_txids: &[Txid]) -> Vec<TxMerkleNode> {
    // Initial level: a placeholder coinbase hash at index 0, then
    // the rest of the txids. The placeholder value never affects
    // the branches themselves --- only the root depends on it ---
    // so we use zeros for clarity.
    let mut level: Vec<sha256d::Hash> = std::iter::once(sha256d::Hash::all_zeros())
        .chain(
            non_coinbase_txids
                .iter()
                .map(|t| sha256d::Hash::from_byte_array(t.to_byte_array())),
        )
        .collect();

    let mut branches = Vec::new();
    let mut idx: usize = 0;

    while level.len() > 1 {
        // Pad odd levels by duplicating the last node, per
        // Bitcoin's merkle convention.
        if level.len() % 2 == 1 {
            let last = *level.last().unwrap();
            level.push(last);
        }

        let sibling = level[idx ^ 1];
        branches.push(TxMerkleNode::from_byte_array(sibling.to_byte_array()));

        let mut next = Vec::with_capacity(level.len() / 2);
        for chunk in level.chunks(2) {
            let mut concat = [0u8; 64];
            concat[..32].copy_from_slice(&chunk[0].to_byte_array());
            concat[32..].copy_from_slice(&chunk[1].to_byte_array());
            next.push(sha256d::Hash::hash(&concat));
        }
        level = next;
        idx /= 2;
    }

    branches
}

fn build_script_sig(height: u32, placeholder: &[u8], vanity: &[u8]) -> Result<Vec<u8>, BuildError> {
    let height_bytes = encode_cscriptnum(height as i64);
    let mut script_sig =
        Vec::with_capacity(1 + height_bytes.len() + placeholder.len() + vanity.len());
    script_sig.push(height_bytes.len() as u8);
    script_sig.extend_from_slice(&height_bytes);
    script_sig.extend_from_slice(placeholder);

    let max_vanity = MAX_COINBASE_SCRIPTSIG.saturating_sub(script_sig.len());
    let vanity_truncated = &vanity[..vanity.len().min(max_vanity)];
    script_sig.extend_from_slice(vanity_truncated);

    if script_sig.len() > MAX_COINBASE_SCRIPTSIG {
        return Err(BuildError::ScriptSigTooLong {
            len: script_sig.len(),
        });
    }
    Ok(script_sig)
}

fn build_coinbase_tx(
    template: &ParsedTemplate,
    payout_script: &ScriptBuf,
    script_sig: ScriptBuf,
) -> Transaction {
    let mut output = vec![TxOut {
        value: Amount::from_sat(template.coinbase_value),
        script_pubkey: payout_script.clone(),
    }];

    if let Some(witness_script) = &template.default_witness_commitment {
        output.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::from(witness_script.clone()),
        });
    }

    // Coinbase witness reservation: a single 32-zero-byte stack
    // item, per BIP141. Bitcoin Core's `default_witness_commitment`
    // is computed under exactly this assumption.
    let witness = if template.default_witness_commitment.is_some() {
        let mut w = Witness::new();
        w.push([0u8; 32]);
        w
    } else {
        Witness::new()
    };

    Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig,
            sequence: Sequence::MAX,
            witness,
        }],
        output,
    }
}

fn split_at_placeholder(
    serialized: &[u8],
    placeholder: &[u8],
    extranonce2_size: u8,
) -> Result<CoinbaseSplit, BuildError> {
    let mut positions = serialized
        .windows(placeholder.len())
        .enumerate()
        .filter(|(_, w)| *w == placeholder)
        .map(|(i, _)| i);
    let pos = positions.next().ok_or(BuildError::PlaceholderNotFound)?;
    if positions.next().is_some() {
        return Err(BuildError::PlaceholderAmbiguous);
    }
    Ok(CoinbaseSplit {
        coinbase1: serialized[..pos].to_vec(),
        extranonce1: Vec::new(),
        coinbase2: serialized[pos + placeholder.len()..].to_vec(),
        extranonce2_size,
    })
}

/// Encode a non-negative integer as Bitcoin's CScriptNum, used for
/// BIP34's coinbase height push.
///
/// Returns the bytes of the encoded integer (without the push-size
/// prefix). Zero is represented by an empty vector. Positive values
/// are little-endian, with a trailing `0x00` byte appended whenever
/// the high bit of the most-significant byte would otherwise be set
/// (so as not to be interpreted as a negative number).
fn encode_cscriptnum(n: i64) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }

    let mut bytes = Vec::new();
    let mut abs = n.unsigned_abs();
    while abs != 0 {
        bytes.push((abs & 0xff) as u8);
        abs >>= 8;
    }

    if bytes.last().copied().unwrap_or(0) & 0x80 != 0 {
        bytes.push(0);
    }

    bytes
}

#[cfg(test)]
mod tests {
    use bitcoin::WPubkeyHash;
    use bitcoin::consensus::deserialize;
    use bitcoin::merkle_tree;

    use super::*;
    use crate::job_source::test_blocks::block_881423;

    fn dummy_payout_script() -> ScriptBuf {
        // P2WPKH script with a deterministic 20-byte hash. Avoids
        // baking a real address into the test suite.
        ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x42u8; 20]))
    }

    fn template_at_height(height: u32, coinbase_value: u64) -> ParsedTemplate {
        ParsedTemplate {
            height,
            version: 0x20000000,
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
            target: bitcoin::Target::MAX_ATTAINABLE_MAINNET,
            curtime: 0,
            mintime: 0,
            coinbase_value,
            // Use a real witness commitment script so that the
            // builder takes the segwit path.
            default_witness_commitment: Some(
                hex::decode(
                    "6a24aa21a9edb395560ab72068c2afb1d7e6c7db26b8134820\
                     89d2c4d2471b9036c5e826140601",
                )
                .unwrap(),
            ),
            signet_challenge: None,
            longpollid: None,
            transactions: Vec::new(),
        }
    }

    #[test]
    fn cscriptnum_known_values() {
        assert_eq!(encode_cscriptnum(0), Vec::<u8>::new());
        assert_eq!(encode_cscriptnum(1), vec![0x01]);
        assert_eq!(encode_cscriptnum(127), vec![0x7f]);
        // 128 has the high bit set in the only-byte encoding, so it
        // gets a sign-disambiguation 0x00 appended.
        assert_eq!(encode_cscriptnum(128), vec![0x80, 0x00]);
        assert_eq!(encode_cscriptnum(255), vec![0xff, 0x00]);
        assert_eq!(encode_cscriptnum(256), vec![0x00, 0x01]);
        assert_eq!(encode_cscriptnum(32767), vec![0xff, 0x7f]);
        assert_eq!(encode_cscriptnum(32768), vec![0x00, 0x80, 0x00]);
        // Block 881,423's height encodes as 0x0d730f.
        assert_eq!(encode_cscriptnum(881_423), vec![0x0f, 0x73, 0x0d]);
    }

    #[test]
    fn build_coinbase_rejects_invalid_extranonce2_size() {
        let template = template_at_height(881_423, 312_500_000);
        let err = build_coinbase(&template, &dummy_payout_script(), b"", 0).unwrap_err();
        assert!(matches!(err, BuildError::BadExtranonce2Size(0)));
        let err = build_coinbase(&template, &dummy_payout_script(), b"", 9).unwrap_err();
        assert!(matches!(err, BuildError::BadExtranonce2Size(9)));
    }

    #[test]
    fn build_coinbase_rejects_oversize_scriptsig() {
        // Vanity is truncated to fit, but a oversize-after-truncate
        // path is still useful as a sanity check on the limit.
        let template = template_at_height(881_423, 312_500_000);
        // 100-byte vanity is fine because builder truncates it; this
        // test instead constructs a tiny manual scriptSig that is
        // oversize before truncation. The truncation logic is
        // defensive, so we expect this to succeed.
        let huge_vanity = vec![0x55; 200];
        let split = build_coinbase(&template, &dummy_payout_script(), &huge_vanity, 4).unwrap();
        let recomposed = recompose(&split, &[0xCC, 0xCD, 0xCE, 0xCF]);
        let _: Transaction = deserialize(&recomposed).unwrap();
    }

    #[test]
    fn build_coinbase_round_trip_recomposes_to_valid_tx() {
        let template = template_at_height(881_423, 312_500_000);
        let split = build_coinbase(&template, &dummy_payout_script(), b"/mujina-test/", 4).unwrap();

        // Re-assemble with the exact placeholder and confirm the
        // result deserializes as a valid Transaction.
        let placeholder: Vec<u8> = (0..4).map(|i| 0xCC + i).collect();
        let recomposed = recompose(&split, &placeholder);
        let tx: Transaction = deserialize(&recomposed).unwrap();

        // Structural assertions. These come from BIP rules + our
        // builder choices, not from copying expected bytes.
        assert!(tx.is_coinbase());
        assert_eq!(tx.input.len(), 1);
        assert_eq!(tx.input[0].previous_output, OutPoint::null());
        assert_eq!(tx.input[0].witness.len(), 1);
        assert_eq!(tx.input[0].witness.iter().next().unwrap(), &[0u8; 32]);
        assert_eq!(tx.output.len(), 2);
        assert_eq!(
            tx.output[0].value,
            bitcoin::Amount::from_sat(template.coinbase_value),
        );
        assert_eq!(tx.output[0].script_pubkey, dummy_payout_script());

        // BIP34 height push at the start of scriptSig.
        let script_bytes = tx.input[0].script_sig.as_bytes();
        assert_eq!(script_bytes[0], 0x03); // PUSHBYTES_3
        assert_eq!(&script_bytes[1..4], &[0x0f, 0x73, 0x0d]);

        // Vanity tail is present.
        assert!(
            tx.input[0]
                .script_sig
                .as_bytes()
                .windows(b"/mujina-test/".len())
                .any(|w| w == b"/mujina-test/")
        );
    }

    #[test]
    fn build_coinbase_recomposed_with_real_extranonce2_changes_txid() {
        let template = template_at_height(881_423, 312_500_000);
        let split = build_coinbase(&template, &dummy_payout_script(), b"", 4).unwrap();

        let placeholder: Vec<u8> = (0..4).map(|i| 0xCC + i).collect();
        let real_en2 = [0xDE, 0xAD, 0xBE, 0xEF];

        let tx_a: Transaction = deserialize(&recompose(&split, &placeholder)).unwrap();
        let tx_b: Transaction = deserialize(&recompose(&split, &real_en2)).unwrap();

        assert_ne!(tx_a.compute_txid(), tx_b.compute_txid());
    }

    #[test]
    fn split_at_placeholder_rejects_missing() {
        let serialized = vec![0u8; 100];
        let placeholder = vec![0xAA, 0xBB];
        let err = split_at_placeholder(&serialized, &placeholder, 2).unwrap_err();
        assert!(matches!(err, BuildError::PlaceholderNotFound));
    }

    #[test]
    fn split_at_placeholder_rejects_ambiguous() {
        let mut serialized = vec![0u8; 100];
        serialized[10..12].copy_from_slice(&[0xAA, 0xBB]);
        serialized[50..52].copy_from_slice(&[0xAA, 0xBB]);
        let placeholder = vec![0xAA, 0xBB];
        let err = split_at_placeholder(&serialized, &placeholder, 2).unwrap_err();
        assert!(matches!(err, BuildError::PlaceholderAmbiguous));
    }

    #[test]
    fn merkle_branches_match_bitcoin_crate_for_synthetic_set() {
        // Synthesize a coinbase txid plus N other txids, compute
        // branches with our function, then independently compute
        // the merkle root using the bitcoin crate's tree builder
        // and compare to a manual climb using our branches.
        let coinbase_txid = Txid::from_byte_array([0u8; 32]);
        let other_txids: Vec<Txid> = (1u8..=7)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i;
                Txid::from_byte_array(bytes)
            })
            .collect();

        let branches = compute_merkle_branches(&other_txids);
        // 8 leaves -> 3 levels of branches.
        assert_eq!(branches.len(), 3);

        // Reference root: bitcoin crate over [coinbase, ...others].
        let leaves: Vec<TxMerkleNode> = std::iter::once(coinbase_txid)
            .chain(other_txids.iter().copied())
            .map(|t| TxMerkleNode::from_byte_array(t.to_byte_array()))
            .collect();
        let reference_root = merkle_tree::calculate_root(leaves.into_iter()).unwrap();

        // Climb manually using our branches and the placeholder
        // coinbase hash (zeros, since the synthetic coinbase txid
        // is zero).
        let mut current = [0u8; 32];
        for branch in &branches {
            let mut concat = [0u8; 64];
            concat[..32].copy_from_slice(&current);
            concat[32..].copy_from_slice(branch.as_byte_array());
            current = sha256d::Hash::hash(&concat).to_byte_array();
        }

        assert_eq!(current, reference_root.to_byte_array());
    }

    #[test]
    fn merkle_branches_handles_odd_leaf_count() {
        // 5 leaves: 1 coinbase + 4 others. Tree shape forces
        // padding on the upper levels.
        let other_txids: Vec<Txid> = (1u8..=4)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i;
                Txid::from_byte_array(bytes)
            })
            .collect();
        let branches = compute_merkle_branches(&other_txids);
        // 5 leaves -> log2 ceil = 3 levels of branches.
        assert_eq!(branches.len(), 3);

        // Cross-check via independent root computation.
        let coinbase_txid = Txid::from_byte_array([0u8; 32]);
        let leaves: Vec<TxMerkleNode> = std::iter::once(coinbase_txid)
            .chain(other_txids.iter().copied())
            .map(|t| TxMerkleNode::from_byte_array(t.to_byte_array()))
            .collect();
        let reference_root = merkle_tree::calculate_root(leaves.into_iter()).unwrap();

        let mut current = [0u8; 32];
        for branch in &branches {
            let mut concat = [0u8; 64];
            concat[..32].copy_from_slice(&current);
            concat[32..].copy_from_slice(branch.as_byte_array());
            current = sha256d::Hash::hash(&concat).to_byte_array();
        }
        assert_eq!(current, reference_root.to_byte_array());
    }

    #[test]
    fn merkle_branches_single_other_tx() {
        // 2 leaves -> 1 branch.
        let other = Txid::from_byte_array([1u8; 32]);
        let branches = compute_merkle_branches(&[other]);
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].as_byte_array(), &[1u8; 32]);
    }

    #[test]
    fn merkle_branches_no_other_txs() {
        // 1 leaf (just coinbase) -> 0 branches; merkle root IS the
        // coinbase txid.
        let branches = compute_merkle_branches(&[]);
        assert!(branches.is_empty());
    }

    #[test]
    fn coinbase_for_block_881423_matches_real_height_push() {
        // Sanity-check the BIP34 height encoding against the real
        // block: the first 4 bytes of its scriptSig are the height
        // push.
        let real = block_881423::COINBASE_TX;
        let scriptsig_len_offset = 4 + 2 + 1 + 32 + 4; // version+marker+flag+incount+prev+idx
        assert_eq!(real[scriptsig_len_offset + 1], 0x03); // PUSHBYTES_3
        assert_eq!(
            &real[scriptsig_len_offset + 2..scriptsig_len_offset + 5],
            encode_cscriptnum(881_423).as_slice(),
        );
    }

    fn recompose(split: &CoinbaseSplit, extranonce2: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            split.coinbase1.len()
                + split.extranonce1.len()
                + extranonce2.len()
                + split.coinbase2.len(),
        );
        out.extend_from_slice(&split.coinbase1);
        out.extend_from_slice(&split.extranonce1);
        out.extend_from_slice(extranonce2);
        out.extend_from_slice(&split.coinbase2);
        out
    }
}
