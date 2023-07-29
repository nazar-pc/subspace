//! PoT gossip functionality.

use parity_scale_codec::Decode;
use parking_lot::{Mutex, RwLock};
use sc_network::config::NonDefaultSetConfig;
use sc_network::PeerId;
use sc_network_gossip::{
    GossipEngine, MessageIntent, Syncing as GossipSyncing, ValidationResult, Validator,
    ValidatorContext,
};
use sp_runtime::traits::{Block as BlockT, Hash as HashT, Header as HeaderT};
use std::collections::HashSet;
use std::sync::Arc;
use subspace_core_primitives::crypto::blake2b_256_hash;
use subspace_core_primitives::PotProof;

pub(crate) const GOSSIP_PROTOCOL: &str = "/subspace/subspace-proof-of-time";

type MessageHash = [u8; 32];

/// PoT message topic.
pub(crate) fn topic<Block: BlockT>() -> Block::Hash {
    <<Block::Header as HeaderT>::Hashing as HashT>::hash(b"subspace-proof-of-time-gossip")
}

/// Validator for gossiped messages
#[derive(Debug)]
pub(crate) struct PotGossipValidator {
    pending: RwLock<HashSet<MessageHash>>,
}

impl PotGossipValidator {
    /// Creates the validator.
    pub(crate) fn new() -> Self {
        Self {
            pending: RwLock::new(HashSet::new()),
        }
    }

    /// Called when the message is broadcast.
    pub(crate) fn on_broadcast(&self, msg: &[u8]) {
        let hash = blake2b_256_hash(msg);
        let mut pending = self.pending.write();
        pending.insert(hash);
    }
}

impl<Block: BlockT> Validator<Block> for PotGossipValidator {
    fn validate(
        &self,
        _context: &mut dyn ValidatorContext<Block>,
        _sender: &PeerId,
        mut data: &[u8],
    ) -> ValidationResult<Block::Hash> {
        match PotProof::decode(&mut data) {
            Ok(_) => ValidationResult::ProcessAndKeep(topic::<Block>()),
            Err(_) => ValidationResult::Discard,
        }
    }

    fn message_expired<'a>(&'a self) -> Box<dyn FnMut(Block::Hash, &[u8]) -> bool + 'a> {
        Box::new(move |_topic, data| {
            let hash = blake2b_256_hash(data);
            let pending = self.pending.read();
            !pending.contains(&hash)
        })
    }

    fn message_allowed<'a>(
        &'a self,
    ) -> Box<dyn FnMut(&PeerId, MessageIntent, &Block::Hash, &[u8]) -> bool + 'a> {
        Box::new(move |_who, _intent, _topic, data| {
            let hash = blake2b_256_hash(data);
            let mut pending = self.pending.write();
            if pending.contains(&hash) {
                pending.remove(&hash);
                true
            } else {
                false
            }
        })
    }
}

/// PoT gossip components.
#[derive(Clone)]
pub struct PotGossip<Block: BlockT> {
    pub(crate) engine: Arc<Mutex<GossipEngine<Block>>>,
    pub(crate) validator: Arc<PotGossipValidator>,
}

impl<Block: BlockT> PotGossip<Block> {
    pub fn new<Network, GossipSync>(network: Network, sync: Arc<GossipSync>) -> Self
    where
        Network: sc_network_gossip::Network<Block> + Send + Sync + Clone + 'static,
        GossipSync: GossipSyncing<Block> + 'static,
    {
        let validator = Arc::new(PotGossipValidator::new());
        let engine = Arc::new(Mutex::new(GossipEngine::new(
            network,
            sync,
            GOSSIP_PROTOCOL,
            validator.clone(),
            None,
        )));
        Self { engine, validator }
    }
}

/// Returns the network configuration for PoT gossip.
pub fn pot_gossip_peers_set_config() -> NonDefaultSetConfig {
    let mut cfg = NonDefaultSetConfig::new(GOSSIP_PROTOCOL.into(), 5 * 1024 * 1024);
    cfg.allow_non_reserved(25, 25);
    cfg
}
