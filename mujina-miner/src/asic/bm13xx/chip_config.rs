//! Chip configuration for BM13xx ASIC chips.
//!
//! All BM13xx chip models share the same configurable fields---only the
//! default values differ. Use [`bm1362()`] or [`bm1370()`] to get appropriate
//! defaults, then modify fields as needed.
//!
//! # PLL Calculation
//!
//! PLL configuration is handled by [`ChipConfig::calculate_pll()`]. Empirical
//! analysis of S19 J Pro (BM1362) and S21 Pro (BM1370) serial captures confirms
//! these chips use identical PLL parameters:
//!
//! - FBDIV range: 0xA0--0xEF (160--239)
//! - Post-divider constraint: `postdiv1 >= postdiv2`
//! - VCO threshold: flag=0x50 when VCO >= 2400 MHz, else 0x40
//!
//! BM1366 and BM1368 use a different FBDIV range (0x90--0xEB = 144--235) per
//! esp-miner, and BM1366 additionally requires strict `postdiv1 > postdiv2`.
//! Support for these chips would require chip-specific `PllParams`.

use super::protocol::{Frequency, IoDriverStrength, PllConfig};

/// Configuration for a BM13xx ASIC chip.
///
/// All chip models share the same fields. Use [`bm1362()`] or [`bm1370()`]
/// to get appropriate defaults, then modify fields as needed.
#[derive(Debug, Clone)]
pub struct ChipConfig {
    /// Chip model identifier (e.g., 0x1362, 0x1370).
    /// Verified during enumeration to ensure correct chip type.
    pub chip_id: u16,

    /// Minimum supported frequency for this chip model.
    pub min_freq: Frequency,

    /// Maximum supported frequency for this chip model.
    pub max_freq: Frequency,

    /// IO driver strength for signal integrity.
    pub io_driver: IoDriverStrength,

    /// Nonce range configuration value (chip-family-specific).
    ///
    /// Controls how chips divide the 32-bit nonce search space. Empirically
    /// determined values differ by chip family rather than chain length.
    pub nonce_range: u32,
}

/// Crystal oscillator frequency for BM13xx chips (25 MHz).
const CRYSTAL_MHZ: f32 = 25.0;

impl ChipConfig {
    /// Check if a chip ID matches this configuration.
    pub fn verify_chip_id(&self, id: u16) -> bool {
        self.chip_id == id
    }

    /// Calculate optimal PLL configuration for target frequency.
    ///
    /// Searches for PLL divider values that produce the closest match to
    /// the target frequency. Validated against S19 J Pro (BM1362) and S21 Pro
    /// (BM1370) serial captures---both chips use identical PLL parameters.
    pub fn calculate_pll(&self, freq: Frequency) -> PllConfig {
        let target_freq = freq.mhz();
        let mut best_config = PllConfig::new(0xa0, 2, 0x55); // Default
        let mut min_error = f32::MAX;

        // Search for optimal PLL settings
        // ref_divider: 1 or 2
        // post_divider1: 1-7, must be >= post_divider2
        // post_divider2: 1-7
        // fb_divider: 0xa0-0xef (160-239)

        for ref_div in [2, 1] {
            for post_div1 in (1..=7).rev() {
                for post_div2 in (1..=7).rev() {
                    if post_div1 >= post_div2 {
                        // Calculate required feedback divider
                        let fb_div_f =
                            (post_div1 * post_div2) as f32 * target_freq * ref_div as f32
                                / CRYSTAL_MHZ;
                        let fb_div = fb_div_f.round() as u8;

                        if (0xa0..=0xef).contains(&fb_div) {
                            // Calculate actual frequency with these settings
                            let actual_freq = CRYSTAL_MHZ * fb_div as f32
                                / (ref_div as f32 * post_div1 as f32 * post_div2 as f32);
                            let error = (target_freq - actual_freq).abs();

                            if error < min_error && error < 1.0 {
                                min_error = error;
                                // Encode post dividers as per hardware format
                                let post_div = ((post_div1 - 1) << 4) | (post_div2 - 1);
                                best_config = PllConfig::new(fb_div, ref_div, post_div);
                            }
                        }
                    }
                }
            }
        }

        best_config
    }
}

/// BM1362 defaults (EmberOne, S19 J Pro).
///
/// Sources:
/// - S19 J Pro serial captures
/// - skot/bm1397-docs
pub fn bm1362() -> ChipConfig {
    ChipConfig {
        chip_id: 0x1362,
        min_freq: Frequency::from_mhz(50.0),
        max_freq: Frequency::from_mhz(525.0),
        io_driver: IoDriverStrength::normal(),
        nonce_range: 0x8118_0000, // From emberone-miner (12 chips)
    }
}

/// BM1370 defaults (Bitaxe Gamma, S21 Pro).
///
/// Sources:
/// - S21 Pro serial captures
/// - Bitaxe Gamma logic analyzer captures
pub fn bm1370() -> ChipConfig {
    ChipConfig {
        chip_id: 0x1370,
        min_freq: Frequency::from_mhz(50.0),
        max_freq: Frequency::from_mhz(600.0),
        io_driver: IoDriverStrength::normal(),
        nonce_range: 0xB51E_0000, // From S21 Pro captures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_chip_id_matches() {
        let config = bm1362();
        assert!(config.verify_chip_id(0x1362));
        assert!(!config.verify_chip_id(0x1370));
    }

    /// Test cases from serial captures showing PLL values sent by esp-miner.
    ///
    /// Sources:
    /// - Bitaxe Gamma logic analyzer captures
    /// - bitaxeorg/esp-miner
    #[test]
    fn pll_calculation_produces_valid_frequencies() {
        // Note: esp-miner uses first-found algorithm while we find optimal settings
        // Format: (target_mhz, [fb_div, ref_div, post_div] from esp-miner)
        let test_cases = vec![
            (62.5, [0xd2, 0x02, 0x65]),  // 62.50MHz
            (75.0, [0xd2, 0x02, 0x64]),  // 75.00MHz
            (100.0, [0xe0, 0x02, 0x63]), // 100.00MHz
            (400.0, [0xe0, 0x02, 0x60]), // 400.00MHz
            (500.0, [0xa2, 0x02, 0x30]), // 500.00MHz -> esp-miner gives 506.25MHz
        ];

        let config = bm1370();

        for (target_mhz, esp_miner_raw) in test_cases {
            let freq = Frequency::from_mhz(target_mhz);
            let pll = config.calculate_pll(freq);

            // Calculate actual frequencies for both esp-miner and our values
            let esp_post_div1 = ((esp_miner_raw[2] >> 4) & 0xf) + 1;
            let esp_post_div2 = (esp_miner_raw[2] & 0xf) + 1;
            let esp_actual_mhz = 25.0 * esp_miner_raw[0] as f32
                / (esp_miner_raw[1] as f32 * esp_post_div1 as f32 * esp_post_div2 as f32);

            let our_post_div1 = ((pll.post_div >> 4) & 0xf) + 1;
            let our_post_div2 = (pll.post_div & 0xf) + 1;
            let our_actual_mhz = 25.0 * pll.fb_div as f32
                / (pll.ref_div as f32 * our_post_div1 as f32 * our_post_div2 as f32);

            // Calculate errors
            let esp_error = (target_mhz - esp_actual_mhz).abs();
            let our_error = (target_mhz - our_actual_mhz).abs();

            println!("Target: {:.2}MHz", target_mhz);
            println!(
                "  esp-miner: fb={:#04x} ref={} post={:#04x} -> {:.2}MHz (error: {:.4}MHz)",
                esp_miner_raw[0], esp_miner_raw[1], esp_miner_raw[2], esp_actual_mhz, esp_error
            );
            println!(
                "  Our calc:  fb={:#04x} ref={} post={:#04x} -> {:.2}MHz (error: {:.4}MHz)",
                pll.fb_div, pll.ref_div, pll.post_div, our_actual_mhz, our_error
            );

            // Verify our calculation produces valid PLL parameters
            assert!(
                pll.fb_div >= 0xa0 && pll.fb_div <= 0xef,
                "fb_div out of range: {:#04x}",
                pll.fb_div
            );
            assert!(
                pll.ref_div == 1 || pll.ref_div == 2,
                "ref_div invalid: {}",
                pll.ref_div
            );

            // Verify our error is reasonable (within 1MHz)
            assert!(
                our_error < 1.0,
                "Frequency error too large: {:.2}MHz for target {}MHz",
                our_error,
                target_mhz
            );

            // Our algorithm should produce equal or better results
            // Allow small tolerance for floating point comparison
            assert!(
                our_error <= esp_error + 0.01,
                "Our algorithm produced worse result than esp-miner for {}MHz",
                target_mhz
            );
        }
    }

    /// Validate VCO flag is set correctly based on VCO frequency.
    ///
    /// The S19 J Pro capture shows flag=0x40 for VCO < 2400 MHz and flag=0x50
    /// for VCO >= 2400 MHz. VCO = fb_div * 25 / ref_div.
    ///
    /// Source: S19 J Pro capture analysis
    #[test]
    fn pll_vco_flag_set_correctly() {
        let config = bm1362();

        // Test across the frequency range and verify flag matches VCO threshold
        for freq_mhz in [100.0, 200.0, 300.0, 400.0, 500.0, 525.0] {
            let pll = config.calculate_pll(Frequency::from_mhz(freq_mhz));
            let vco = pll.fb_div as f32 * 25.0 / pll.ref_div as f32;
            let expected_flag = if vco >= 2400.0 { 0x50 } else { 0x40 };

            assert_eq!(
                pll.flag, expected_flag,
                "{}MHz: VCO={:.1} should use flag=0x{:02X}, got 0x{:02X}",
                freq_mhz, vco, expected_flag, pll.flag
            );
        }
    }

    /// Verify BM1362 and BM1370 produce identical PLL for same frequency.
    ///
    /// Empirically confirmed from S19 J Pro (BM1362) and S21 Pro (BM1370)
    /// captures which show identical frequency ramp sequences.
    #[test]
    fn bm1362_and_bm1370_pll_identical() {
        let bm1362_config = bm1362();
        let bm1370_config = bm1370();

        // Test frequencies from the capture ramp sequence
        for freq_mhz in [100.0, 200.0, 300.0, 400.0, 500.0] {
            let freq = Frequency::from_mhz(freq_mhz);
            let pll_1362 = bm1362_config.calculate_pll(freq);
            let pll_1370 = bm1370_config.calculate_pll(freq);

            assert_eq!(
                pll_1362, pll_1370,
                "BM1362 and BM1370 should produce identical PLL for {}MHz",
                freq_mhz
            );
        }
    }
}
