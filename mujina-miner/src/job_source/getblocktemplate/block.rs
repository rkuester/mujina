//! Block reconstruction and submission for the getblocktemplate path.
//!
//! Given a stored [`super::TemplateState`] and a [`Share`] from the
//! scheduler, [`assemble_block`] recomposes the full coinbase, climbs
//! the merkle tree, builds the header, and serializes the entire
//! block to hex ready for `submitblock`.

use bitcoin::block::Header as BlockHeader;
use bitcoin::consensus::encode;
use bitcoin::hash_types::TxMerkleNode;
use bitcoin::hashes::{Hash, sha256d};
use bitcoin::{Transaction, VarInt};
use thiserror::Error;

use super::coinbase::CoinbaseSplit;
use super::template::ParsedTemplate;
use crate::job_source::Share;

/// Errors raised while assembling a block for submission.
#[derive(Error, Debug)]
pub enum AssembleError {
    #[error("share is missing extranonce2")]
    MissingExtranonce2,

    #[error("coinbase failed to deserialize: {0}")]
    BadCoinbase(#[from] encode::Error),
}

/// Build a hex-encoded block from a template, its coinbase split,
/// pre-computed merkle branches, and a share.
///
/// Returns the same hex string `submitblock` expects: full block
/// (header + tx count varint + coinbase + remaining template txs).
pub fn assemble_block(
    template: &ParsedTemplate,
    coinbase: &CoinbaseSplit,
    merkle_branches: &[TxMerkleNode],
    share: &Share,
) -> Result<String, AssembleError> {
    let coinbase_bytes = recompose_coinbase(coinbase, share)?;
    let coinbase_tx: Transaction = encode::deserialize(&coinbase_bytes)?;
    let merkle_root = climb_merkle(coinbase_tx.compute_txid(), merkle_branches);

    let header = BlockHeader {
        version: share.version,
        prev_blockhash: template.prev_blockhash,
        merkle_root,
        time: share.time,
        bits: template.bits,
        nonce: share.nonce,
    };

    let mut bytes = encode::serialize(&header);
    let tx_count = 1 + template.transactions.len();
    encode_varint(&mut bytes, tx_count as u64);
    bytes.extend_from_slice(&coinbase_bytes);
    for tx in &template.transactions {
        bytes.extend_from_slice(&tx.raw);
    }

    Ok(hex::encode(&bytes))
}

fn recompose_coinbase(coinbase: &CoinbaseSplit, share: &Share) -> Result<Vec<u8>, AssembleError> {
    let extranonce2 = share
        .extranonce2
        .as_ref()
        .ok_or(AssembleError::MissingExtranonce2)?;
    let mut out = Vec::with_capacity(
        coinbase.coinbase1.len()
            + coinbase.extranonce1.len()
            + extranonce2.size() as usize
            + coinbase.coinbase2.len(),
    );
    out.extend_from_slice(&coinbase.coinbase1);
    out.extend_from_slice(&coinbase.extranonce1);
    extranonce2.extend_vec(&mut out);
    out.extend_from_slice(&coinbase.coinbase2);
    Ok(out)
}

fn climb_merkle(coinbase_txid: bitcoin::Txid, branches: &[TxMerkleNode]) -> TxMerkleNode {
    let mut current = coinbase_txid.to_byte_array();
    for branch in branches {
        let mut concat = [0u8; 64];
        concat[..32].copy_from_slice(&current);
        concat[32..].copy_from_slice(branch.as_byte_array());
        current = sha256d::Hash::hash(&concat).to_byte_array();
    }
    TxMerkleNode::from_byte_array(current)
}

fn encode_varint(out: &mut Vec<u8>, n: u64) {
    let varint = VarInt(n);
    encode::Encodable::consensus_encode(&varint, out).expect("Vec write is infallible");
}

#[cfg(test)]
mod tests {
    use bitcoin::block::Version as BlockVersion;

    use super::super::coinbase::{build_coinbase, compute_merkle_branches};
    use super::*;
    use crate::job_source::Extranonce2;
    use crate::job_source::test_blocks::block_881423;

    /// Build the inputs needed by `assemble_block` from
    /// `block_881423`'s real fixture data.
    fn block_881423_state() -> (ParsedTemplate, CoinbaseSplit, Vec<TxMerkleNode>, Share) {
        // The fixture's coinbase1 ends just before extranonce1, and
        // coinbase2 starts after extranonce2. We construct a
        // `CoinbaseSplit` that mirrors the real block's split.
        let split = CoinbaseSplit {
            coinbase1: block_881423::coinbase1_bytes().to_vec(),
            extranonce1: block_881423::extranonce1_bytes().to_vec(),
            coinbase2: block_881423::coinbase2_bytes().to_vec(),
            extranonce2_size: 4,
        };

        // We don't have the txs of block 881_423, only the merkle
        // branches. So `template.transactions` stays empty; tests
        // that need the full block body assert only the header
        // bytes (which depend on merkle root, not on the body).
        let template = ParsedTemplate {
            height: 881_423,
            version: 0x2e596000_u32 as i32,
            prev_blockhash: *block_881423::PREV_BLOCKHASH,
            bits: *block_881423::BITS,
            target: bitcoin::Target::MAX_ATTAINABLE_MAINNET,
            curtime: block_881423::TIME,
            mintime: 0,
            coinbase_value: 0,
            default_witness_commitment: None,
            signet_challenge: None,
            longpollid: None,
            transactions: Vec::new(),
        };

        let share = Share {
            job_id: "test".into(),
            nonce: block_881423::NONCE,
            time: block_881423::TIME,
            version: BlockVersion::from_consensus(0x2e596000_u32 as i32),
            extranonce2: Some(*block_881423::EXTRANONCE2),
        };

        (
            template,
            split,
            block_881423::MERKLE_BRANCHES.clone(),
            share,
        )
    }

    #[test]
    fn assemble_block_header_matches_block_881423() {
        let (template, split, branches, share) = block_881423_state();
        let hex = assemble_block(&template, &split, &branches, &share).unwrap();
        let bytes = hex::decode(&hex).unwrap();
        // First 80 bytes of any serialized block are the header.
        assert_eq!(
            &bytes[..80],
            &block_881423::HEADER_BYTES,
            "assembled header bytes don't match block 881423"
        );
    }

    #[test]
    fn assemble_block_includes_template_txs_in_order() {
        // Build a tiny synthetic template with a couple of fake txs
        // and confirm they appear after the coinbase in the output.
        let template = ParsedTemplate {
            height: 1,
            version: 0x20000000,
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
            target: bitcoin::Target::MAX_ATTAINABLE_MAINNET,
            curtime: 0,
            mintime: 0,
            coinbase_value: 5_000_000_000,
            default_witness_commitment: Some(
                hex::decode(
                    "6a24aa21a9edb395560ab72068c2afb1d7e6c7db26b81348208\
                     9d2c4d2471b9036c5e826140601",
                )
                .unwrap(),
            ),
            signet_challenge: None,
            longpollid: None,
            transactions: vec![
                fake_template_tx(b"tx-one"),
                fake_template_tx(b"tx-two-longer"),
            ],
        };
        let payout =
            bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array([0x42u8; 20]));
        let split = build_coinbase(&template, &payout, b"", 4).unwrap();
        let branches = compute_merkle_branches(
            &template
                .transactions
                .iter()
                .map(|t| t.txid)
                .collect::<Vec<_>>(),
        );
        let share = Share {
            job_id: "test".into(),
            nonce: 0,
            time: 0,
            version: BlockVersion::from_consensus(0x20000000),
            extranonce2: Some(Extranonce2::new(0xDEADBEEF, 4).unwrap()),
        };

        let hex_str = assemble_block(&template, &split, &branches, &share).unwrap();
        let bytes = hex::decode(&hex_str).unwrap();

        // Fake txs are not valid Transactions, so we can't
        // deserialize the full block. Search for our marker bytes
        // instead.
        assert!(
            find_subseq(&bytes, b"tx-one").is_some(),
            "first tx body missing"
        );
        assert!(
            find_subseq(&bytes, b"tx-two-longer").is_some(),
            "second tx body missing"
        );
        // First tx (coinbase) precedes both.
        let pos_one = find_subseq(&bytes, b"tx-one").unwrap();
        let pos_two = find_subseq(&bytes, b"tx-two-longer").unwrap();
        assert!(pos_one < pos_two);
    }

    #[test]
    fn assemble_block_propagates_extranonce2_change_through_merkle_root() {
        // Two shares differ only in extranonce2; their resulting
        // headers must differ in the merkle-root region (bytes
        // 36..68) but match elsewhere.
        let (template, split, branches, mut share) = block_881423_state();
        let hex_a = assemble_block(&template, &split, &branches, &share).unwrap();

        // Mutate extranonce2 to a different value of the same size.
        share.extranonce2 = Some(Extranonce2::new(0x12345678, 4).unwrap());
        let hex_b = assemble_block(&template, &split, &branches, &share).unwrap();

        let bytes_a = hex::decode(&hex_a).unwrap();
        let bytes_b = hex::decode(&hex_b).unwrap();

        // Version, prev_blockhash, time, bits, nonce regions match.
        assert_eq!(&bytes_a[0..36], &bytes_b[0..36]); // version + prev
        assert_eq!(&bytes_a[68..80], &bytes_b[68..80]); // time, bits, nonce

        // Merkle root region differs.
        assert_ne!(&bytes_a[36..68], &bytes_b[36..68]);
    }

    #[test]
    fn assemble_block_rejects_share_without_extranonce2() {
        let (template, split, branches, mut share) = block_881423_state();
        share.extranonce2 = None;
        let err = assemble_block(&template, &split, &branches, &share).unwrap_err();
        assert!(matches!(err, AssembleError::MissingExtranonce2));
    }

    fn fake_template_tx(body: &[u8]) -> super::super::template::TemplateTx {
        super::super::template::TemplateTx {
            txid: bitcoin::Txid::from_byte_array(*sha256d::Hash::hash(body).as_byte_array()),
            wtxid: bitcoin::Wtxid::from_byte_array(*sha256d::Hash::hash(body).as_byte_array()),
            raw: body.to_vec(),
            fee_sats: 0,
            weight: body.len() as u32 * 4,
        }
    }

    fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
}
