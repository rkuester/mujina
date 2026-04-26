//! Block-in-progress statistics.
//!
//! Pure function from a [`ParsedTemplate`] (plus a small amount of
//! source-side context: payout address, vanity bytes, network) to a
//! [`BlockInProgress`] suitable for the on-device display via the
//! API.

use bitcoin::{Network, Transaction};

use super::template::{ParsedTemplate, TemplateTx};
use crate::api_client::types::{BlockInProgress, TxSummary};

/// Bitcoin's block weight limit (BIP141).
const WEIGHT_LIMIT: u64 = 4_000_000;
/// Halving period.
const HALVING_INTERVAL: u32 = 210_000;
/// Difficulty retarget window.
const RETARGET_INTERVAL: u32 = 2016;
/// Initial subsidy in sats (50 BTC).
const INITIAL_SUBSIDY_SATS: u64 = 50 * 100_000_000;

/// Assemble a [`BlockInProgress`] snapshot from a parsed template.
///
/// `payout_address` is the operator's display address (whatever
/// `MUJINA_PAYOUT_ADDRESS` was set to). `vanity_bytes` is the bytes
/// inserted into the coinbase scriptSig after the BIP34 height
/// push and the extranonce2 placeholder.
pub fn block_in_progress(
    template: &ParsedTemplate,
    network: Network,
    payout_address: &str,
    vanity_bytes: &[u8],
) -> BlockInProgress {
    let subsidy_sats = subsidy_for_height(template.height);
    let fees_sats = template.coinbase_value.saturating_sub(subsidy_sats);

    let weight: u64 = template.transactions.iter().map(|t| t.weight as u64).sum();
    let weight_pct = (weight as f32 / WEIGHT_LIMIT as f32) * 100.0;

    let summaries: Vec<(usize, TxSummary)> = template
        .transactions
        .iter()
        .enumerate()
        .map(|(i, t)| (i, summarize(t)))
        .collect();

    let top_fee_tx = summaries
        .iter()
        .max_by(|a, b| {
            a.1.sat_per_vb
                .partial_cmp(&b.1.sat_per_vb)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, s)| s.clone());

    let biggest_tx = summaries
        .iter()
        .max_by_key(|(i, _)| template.transactions[*i].weight)
        .map(|(_, s)| s.clone());

    let fee_floor_sat_per_vb = summaries
        .iter()
        .map(|(_, s)| s.sat_per_vb)
        .filter(|v| v.is_finite())
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let ln_shaped_count = summaries
        .iter()
        .filter(|(_, s)| matches!(s.shape_hint.as_deref(), Some(h) if h.starts_with("ln-")))
        .count() as u32;

    let halving_era = template.height / HALVING_INTERVAL;
    let blocks_until_halving = HALVING_INTERVAL - (template.height % HALVING_INTERVAL);
    let retarget_position = template.height % RETARGET_INTERVAL;

    BlockInProgress {
        height: template.height,
        prev_blockhash: template.prev_blockhash.to_string(),
        network: network_str(network).to_string(),
        coinbase_value_sats: template.coinbase_value,
        subsidy_sats,
        fees_sats,
        payout_address: payout_address.to_string(),
        coinbase_message: render_ascii(vanity_bytes),
        tx_count: template.transactions.len(),
        weight,
        weight_pct,
        top_fee_tx,
        biggest_tx,
        fee_floor_sat_per_vb,
        ln_shaped_count,
        halving_era,
        blocks_until_halving,
        retarget_position,
        network_target_hex: format!("{:x}", template.target),
        signet_challenge_hex: template.signet_challenge.as_ref().map(hex::encode),
    }
}

/// Block subsidy at `height` per BIP34's halving schedule.
///
/// The subsidy goes to zero after the 33rd halving (height
/// `33 * 210_000 = 6_930_000`), beyond which the saturating shift
/// would otherwise wrap unhelpfully.
pub fn subsidy_for_height(height: u32) -> u64 {
    let halvings = height / HALVING_INTERVAL;
    if halvings >= 64 {
        0
    } else {
        INITIAL_SUBSIDY_SATS >> halvings
    }
}

fn summarize(tx: &TemplateTx) -> TxSummary {
    let vbytes = tx.weight.div_ceil(4);
    let sat_per_vb = if vbytes == 0 {
        0.0
    } else {
        tx.fee_sats as f64 / vbytes as f64
    };
    let shape_hint = detect_shape(&tx.raw);
    TxSummary {
        txid: tx.txid.to_string(),
        fee_sats: tx.fee_sats,
        vbytes,
        sat_per_vb,
        shape_hint: shape_hint.map(|s| s.to_string()),
    }
}

/// Cheap heuristics on output scripts. False positives are
/// acceptable; the goal is "what kind of activity is this?" not a
/// precise classification.
fn detect_shape(raw: &[u8]) -> Option<&'static str> {
    let tx: Transaction = bitcoin::consensus::encode::deserialize(raw).ok()?;
    if tx.output.is_empty() {
        return None;
    }

    let mut has_p2wsh = false;
    let mut has_anchor = false;
    let mut single_program_output = tx.output.len() == 1;

    for out in &tx.output {
        let script = out.script_pubkey.as_bytes();
        // P2WSH: OP_0 OP_PUSHBYTES_32 ...
        if script.len() == 34 && script[0] == 0x00 && script[1] == 0x20 {
            has_p2wsh = true;
        }
        // Lightning anchor outputs are 330-sat P2WSH.
        if out.value == bitcoin::Amount::from_sat(330)
            && script.len() == 34
            && script[0] == 0x00
            && script[1] == 0x20
        {
            has_anchor = true;
        }
        // Anything other than a single P2WPKH or P2TR output disqualifies
        // the wallet-self-send heuristic.
        if !is_p2wpkh(script) && !is_p2tr(script) {
            single_program_output = false;
        }
    }

    if has_anchor {
        Some("ln-anchor")
    } else if has_p2wsh && tx.output.len() <= 2 {
        Some("ln-funding-2of2")
    } else if single_program_output && tx.input.len() == 1 {
        Some("wallet-self-send")
    } else {
        None
    }
}

fn is_p2wpkh(script: &[u8]) -> bool {
    script.len() == 22 && script[0] == 0x00 && script[1] == 0x14
}

fn is_p2tr(script: &[u8]) -> bool {
    script.len() == 34 && script[0] == 0x51 && script[1] == 0x20
}

/// Render bytes as ASCII for display. Non-printable bytes become
/// `?` so the on-device UI doesn't mangle the terminal.
fn render_ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

fn network_str(network: Network) -> &'static str {
    match network {
        Network::Bitcoin => "bitcoin",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::Hash;
    use bitcoin::{Target, Txid, Wtxid};

    use super::*;

    fn empty_template(height: u32, coinbase_value: u64) -> ParsedTemplate {
        ParsedTemplate {
            height,
            version: 0x20000000,
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
            target: Target::MAX_ATTAINABLE_MAINNET,
            curtime: 0,
            mintime: 0,
            coinbase_value,
            default_witness_commitment: None,
            signet_challenge: None,
            longpollid: None,
            transactions: Vec::new(),
        }
    }

    fn fake_tx(label: u8, raw: Vec<u8>, fee_sats: u64, weight: u32) -> TemplateTx {
        TemplateTx {
            txid: Txid::from_byte_array([label; 32]),
            wtxid: Wtxid::from_byte_array([label; 32]),
            raw,
            fee_sats,
            weight,
        }
    }

    #[test]
    fn subsidy_schedule_known_values() {
        assert_eq!(subsidy_for_height(0), 50 * 100_000_000);
        assert_eq!(subsidy_for_height(209_999), 50 * 100_000_000);
        assert_eq!(subsidy_for_height(210_000), 25 * 100_000_000);
        assert_eq!(subsidy_for_height(420_000), 1_250_000_000);
        // Block 881_423 is in era 4: 50 -> 25 -> 12.5 -> 6.25 -> 3.125 BTC.
        assert_eq!(subsidy_for_height(881_423), 3_125_000_00);
        // Beyond the 33rd halving: subsidy is zero.
        assert_eq!(subsidy_for_height(33 * 210_000), 0);
    }

    #[test]
    fn empty_template_has_zero_fees_and_no_summaries() {
        let template = empty_template(881_423, 3_125_000_00);
        let stats = block_in_progress(&template, Network::Bitcoin, "addr", b"/test/");

        assert_eq!(stats.height, 881_423);
        assert_eq!(stats.subsidy_sats, 3_125_000_00);
        assert_eq!(stats.fees_sats, 0);
        assert_eq!(stats.tx_count, 0);
        assert_eq!(stats.weight, 0);
        assert_eq!(stats.weight_pct, 0.0);
        assert!(stats.top_fee_tx.is_none());
        assert!(stats.biggest_tx.is_none());
        assert!(stats.fee_floor_sat_per_vb.is_none());
        assert_eq!(stats.ln_shaped_count, 0);
        assert_eq!(stats.coinbase_message, "/test/");
        assert_eq!(stats.payout_address, "addr");
        assert_eq!(stats.network, "bitcoin");
        assert_eq!(stats.halving_era, 4);
        assert_eq!(
            stats.blocks_until_halving,
            210_000 - (881_423 - 4 * 210_000)
        );
        assert_eq!(stats.retarget_position, 881_423 % 2016);
    }

    #[test]
    fn fee_market_summary_picks_top_and_biggest() {
        let mut template = empty_template(1, 5_000_000_000);
        // Whale: high fee rate.
        template
            .transactions
            .push(fake_tx(1, vec![0u8; 100], 10_000, 400));
        // Big legacy tx with low fee rate.
        template
            .transactions
            .push(fake_tx(2, vec![0u8; 8000], 1_000, 32_000));
        // Average.
        template
            .transactions
            .push(fake_tx(3, vec![0u8; 200], 200, 800));

        let stats = block_in_progress(&template, Network::Bitcoin, "addr", b"");

        let top = stats.top_fee_tx.unwrap();
        assert_eq!(top.fee_sats, 10_000);
        assert_eq!(top.vbytes, 100); // 400 / 4
        assert_eq!(top.sat_per_vb, 100.0);

        let biggest = stats.biggest_tx.unwrap();
        assert_eq!(biggest.fee_sats, 1_000);
        assert_eq!(biggest.vbytes, 8_000);

        let floor = stats.fee_floor_sat_per_vb.unwrap();
        // Lowest sat/vB is 1_000 / 8_000 = 0.125
        assert!((floor - 0.125).abs() < 1e-9, "floor was {floor}");

        assert_eq!(stats.tx_count, 3);
        assert_eq!(stats.weight, 400 + 32_000 + 800);
    }

    #[test]
    fn coinbase_message_strips_non_printable() {
        let template = empty_template(1, 5_000_000_000);
        let stats = block_in_progress(
            &template,
            Network::Bitcoin,
            "addr",
            &[0x2f, b'h', b'i', 0x00, 0x7f, 0x2f],
        );
        // 0x00 and 0x7f are not in 0x20..0x7f.
        assert_eq!(stats.coinbase_message, "/hi??/");
    }

    #[test]
    fn ln_anchor_detection() {
        // Build a transaction with one P2WSH 330-sat output.
        let raw = build_simple_tx_with_anchor();
        let template = ParsedTemplate {
            transactions: vec![fake_tx(1, raw, 100, 600)],
            ..empty_template(1, 5_000_000_000)
        };
        let stats = block_in_progress(&template, Network::Bitcoin, "addr", b"");
        assert_eq!(stats.ln_shaped_count, 1);
        assert_eq!(
            stats.top_fee_tx.unwrap().shape_hint.as_deref(),
            Some("ln-anchor")
        );
    }

    /// Construct a serialized tx with a 330-sat P2WSH output. Tiny
    /// hand-built bytes so we don't pull in test helpers.
    fn build_simple_tx_with_anchor() -> Vec<u8> {
        // version=2, marker/flag (segwit), 1 input, prev=zeros,
        // index=0, scriptSig=empty, sequence=ffffffff, 1 output:
        // value=330, scriptPubKey=00 20 [32 zeros] (P2WSH).
        // Witness: 0 items per input (still need a marker since
        // we wrote one), then locktime.
        //
        // Simplest valid encoding: legacy format (no marker/flag,
        // no witness data). detect_shape just looks at outputs.
        let mut tx = Vec::new();
        tx.extend_from_slice(&2u32.to_le_bytes()); // version
        tx.push(0x01); // input count
        tx.extend_from_slice(&[0u8; 32]); // prev txid
        tx.extend_from_slice(&[0u8; 4]); // prev index
        tx.push(0x00); // scriptSig length = 0
        tx.extend_from_slice(&u32::MAX.to_le_bytes()); // sequence
        tx.push(0x01); // output count
        tx.extend_from_slice(&330u64.to_le_bytes()); // value
        tx.push(34); // scriptPubKey length
        tx.push(0x00); // OP_0
        tx.push(0x20); // PUSHBYTES_32
        tx.extend_from_slice(&[0u8; 32]); // 32-byte program
        tx.extend_from_slice(&[0u8; 4]); // locktime
        tx
    }
}
