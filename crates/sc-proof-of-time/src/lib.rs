//! Subspace proof of time implementation.

mod clock_master;
mod gossip;
mod node_client;
mod state_manager;
mod utils;

use crate::state_manager::{init_pot_state, PotProtocolState};
use sp_runtime::traits::Block as BlockT;
use std::sync::Arc;
use subspace_core_primitives::{BlockNumber, SlotNumber};
use subspace_proof_of_time::ProofOfTime;

pub use clock_master::{BootstrapParams, ClockMaster};
pub use gossip::{pot_gossip_peers_set_config, PotGossip};
pub use node_client::PotClient;
pub use state_manager::{PotConsensusState, PotStateSummary};

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
        // TODO: fill proper values. These are set to produce
        // approximately 1 proof/sec during testing.
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

/// Components initialized during the new_partial() phase of set up.
pub struct PotComponents<Block> {
    /// Proof of time implementation.
    proof_of_time: Arc<ProofOfTime>,

    /// Protocol state.
    protocol_state: Arc<dyn PotProtocolState>,

    /// Consensus state.
    consensus_state: Arc<dyn PotConsensusState<Block>>,
}

impl<Block: BlockT> PotComponents<Block> {
    /// Sets up the partial components.
    pub fn new() -> Self {
        let config = PotConfig::default();
        let proof_of_time = Arc::new(ProofOfTime::new(
            config.num_checkpoints,
            config.checkpoint_iterations,
        ));
        let (protocol_state, consensus_state) =
            init_pot_state(config, proof_of_time.clone(), vec![]);

        Self {
            proof_of_time,
            protocol_state,
            consensus_state,
        }
    }

    /// Returns the consensus interface.
    pub fn consensus_state(&self) -> Arc<dyn PotConsensusState<Block>> {
        self.consensus_state.clone()
    }
}

impl<Block: BlockT> Default for PotComponents<Block> {
    fn default() -> Self {
        Self::new()
    }
}

/// The role assigned to subspace-node.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PotRole {
    /// Clock master role of producing proofs + initial bootstrapping.
    ClockMasterBootStrap,

    /// Clock master role of producing proofs.
    ClockMaster,

    /// Consensus PoT client, listens for proofs from clock masters.
    Client,
}

impl PotRole {
    /// Checks if the role is clock master.
    pub fn is_clock_master(&self) -> bool {
        *self == Self::ClockMasterBootStrap || *self == Self::ClockMaster
    }

    /// Checks if the role is clock master bootstrap.
    pub fn is_clock_master_bootstrap(&self) -> bool {
        *self == Self::ClockMasterBootStrap
    }
}
