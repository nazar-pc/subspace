//! Consensus node interface to the clock master network.

use crate::gossip::{topic, PotGossip};
use crate::state_manager::PotProtocolState;
use crate::utils::{get_consensus_tip_proofs, LOG_TARGET};
use crate::PotComponents;
use futures::{FutureExt, StreamExt};
use parity_scale_codec::Decode;
use sc_network::PeerId;
use sp_blockchain::{HeaderBackend, Info};
use sp_consensus::SyncOracle;
use sp_runtime::traits::Block as BlockT;
use std::sync::Arc;
use std::time::Instant;
use subspace_core_primitives::PotProof;
use tracing::{error, trace};

/// The PoT client implementation
pub struct PotClient<Block: BlockT, Client, SO> {
    gossip: PotGossip<Block>,
    pot_state: Arc<dyn PotProtocolState>,
    client: Arc<Client>,
    sync_oracle: Arc<SO>,
    chain_info_fn: Arc<dyn Fn() -> Info<Block> + Send + Sync>,
}

impl<Block, Client, SO> PotClient<Block, Client, SO>
where
    Block: BlockT,
    Client: HeaderBackend<Block>,
    SO: SyncOracle + Send + Sync + Clone + 'static,
{
    /// Creates the PoT client instance.
    pub fn new(
        components: PotComponents,
        gossip: PotGossip<Block>,
        client: Arc<Client>,
        sync_oracle: Arc<SO>,
        chain_info_fn: Arc<dyn Fn() -> Info<Block> + Send + Sync>,
    ) -> Self {
        Self {
            gossip,
            pot_state: components.protocol_state,
            client,
            sync_oracle,
            chain_info_fn,
        }
    }

    /// Starts the workers.
    pub async fn run(self) {
        // Wait for sync to complete, get the proof from the tip.
        let proofs = match get_consensus_tip_proofs(
            self.client.clone(),
            self.sync_oracle.clone(),
            self.chain_info_fn.clone(),
        )
        .await
        {
            Ok(proofs) => proofs,
            Err(err) => {
                error!("PoT client: Failed to get initial proofs: {err:?}");
                return;
            }
        };
        self.pot_state.reset(proofs);

        let mut incoming_messages = Box::pin(
            self.gossip
                .engine
                .lock()
                .messages_for(topic::<Block>())
                .filter_map(|notification| async move {
                    notification
                        .sender
                        .map(|sender| (sender, notification.message))
                })
                .filter_map(|(sender, message)| async move {
                    PotProof::decode(&mut &message[..])
                        .ok()
                        .map(|msg| (sender, msg))
                }),
        );

        loop {
            let engine = self.gossip.engine.clone();
            let gossip_engine = futures::future::poll_fn(|cx| engine.lock().poll_unpin(cx));

            futures::select! {
                gossiped = incoming_messages.next().fuse() => {
                    if let Some((sender, proof)) = gossiped {
                        trace!(target: LOG_TARGET, "pot_client: got gossiped proof: {sender} => {proof}");
                        self.on_gossip_message(self.pot_state.as_ref(), sender, proof);
                    }
                },
                _ = gossip_engine.fuse() => {
                    error!(target: LOG_TARGET, "pot_client: gossip engine has terminated.");
                    return;
                }
            }
        }
    }

    /// Handles the incoming gossip message.
    fn on_gossip_message(&self, state: &dyn PotProtocolState, sender: PeerId, proof: PotProof) {
        let start_ts = Instant::now();
        let ret = state.on_proof_from_peer(sender, &proof);
        let elapsed = start_ts.elapsed();

        if let Err(err) = ret {
            trace!(target: LOG_TARGET, "pot_client::on gossip: {err:?}, {sender}");
        } else {
            trace!(target: LOG_TARGET, "pot_client::on gossip: {proof}, time=[{elapsed:?}], {sender}");
        }
    }
}
