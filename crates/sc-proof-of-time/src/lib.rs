//! Subspace proof of time implementation.

mod state_manager;

use subspace_core_primitives::{BlockNumber, SlotNumber};

#[derive(Debug, Clone)]
pub struct PotConfig {
    /// Frequency of entropy injection from consensus.
    pub randomness_update_interval_blocks: BlockNumber,

    /// Starting point for entropy injection from consensus.
    pub injection_depth_blocks: BlockNumber,

    /// Number of slots it takes for updated global randomness to
    /// take effect.
    pub global_randomness_reveal_lag_slots: SlotNumber,

    /// Number of slots it takes for injected randomness to
    /// take effect.
    pub pot_injection_lag_slots: SlotNumber,

    /// If the received proof is more than max_future_slots into the
    /// future from the current tip's slot, reject it.
    pub max_future_slots: SlotNumber,

    /// Number of checkpoints per proof.
    pub num_checkpoints: u8,

    /// Number of EAS iterations per checkpoints.
    /// Total iterations per proof = num_checkpoints * checkpoint_iterations.
    /// TODO: config should specify pot_iterations instead, checkpoint_iterations
    /// can be computed as pot_iterations / num_checkpoints.
    pub checkpoint_iterations: u32,
}

impl Default for PotConfig {
    fn default() -> Self {
        // TODO: fill proper values
        Self {
            randomness_update_interval_blocks: 18,
            injection_depth_blocks: 90,
            global_randomness_reveal_lag_slots: 6,
            pot_injection_lag_slots: 6,
            max_future_slots: 10,
            num_checkpoints: 16,
            checkpoint_iterations: 200_000,
        }
    }
}
