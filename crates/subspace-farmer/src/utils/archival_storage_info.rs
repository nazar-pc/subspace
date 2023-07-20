use cuckoofilter::{CuckooFilter, ExportedCuckooFilter};
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use subspace_core_primitives::PieceIndex;
use subspace_networking::libp2p::PeerId;
use subspace_networking::CuckooFilterDTO;

const CONNECTED_PEERS_NUMBER_LIMIT: usize = 50;

struct PeerFilter {
    cuckoo_filter: CuckooFilter<DefaultHasher>,
    // defines whether the peer owning this filter is currently connected
    peer_connected: bool,
}

#[derive(Clone, Default)]
pub struct ArchivalStorageInfo {
    peers: Arc<Mutex<HashMap<PeerId, PeerFilter>>>,
}

impl Debug for ArchivalStorageInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchivalStorageInfo")
            .field("peers (len)", &self.peers.lock().len())
            .finish()
    }
}

impl ArchivalStorageInfo {
    pub fn update_cuckoo_filter(
        &self,
        peer_id: PeerId,
        cuckoo_filter_dto: Arc<CuckooFilterDTO>,
        currently_connected_peers: &[PeerId],
    ) {
        let currently_connected_peers = currently_connected_peers
            .iter()
            .cloned()
            .collect::<HashSet<_>>();

        let exported_filter = ExportedCuckooFilter {
            values: cuckoo_filter_dto.values.clone(),
            length: cuckoo_filter_dto.length as usize,
        };

        let cuckoo_filter = CuckooFilter::from(exported_filter);

        let mut peer_filters = self.peers.lock();

        peer_filters.insert(
            peer_id,
            PeerFilter {
                cuckoo_filter,
                peer_connected: true,
            },
        );

        peer_filters.iter_mut().for_each(|(peer_id, peer_filter)| {
            peer_filter.peer_connected = currently_connected_peers.contains(peer_id);
        });

        peer_filters.retain(|_, peer_filter| peer_filter.peer_connected);

        // Truncate current peer set by limits.
        let mut connected_peers = peer_filters
            .iter()
            .map(|(peer_id, _)| *peer_id)
            .collect::<Vec<_>>();

        let exceeding_number_of_connected_peers = connected_peers
            .len()
            .saturating_div(CONNECTED_PEERS_NUMBER_LIMIT);

        let mut rng = StdRng::seed_from_u64({
            // Hash of PeerID
            let mut s = DefaultHasher::new();
            peer_id.hash(&mut s);
            s.finish()
        });

        // Remove random peers when we exceed the limit of storing peers (and their cuckoo-filters).
        for _ in 0..exceeding_number_of_connected_peers {
            let random_index = rng.gen_range(0..connected_peers.len());

            let removing_peer_id = *connected_peers
                .get(random_index)
                .expect("Index is checked to be present.");

            connected_peers.swap_remove(random_index);
            peer_filters.remove(&removing_peer_id);
        }
    }

    pub fn peers_contain_piece(&self, piece_index: &PieceIndex) -> Vec<PeerId> {
        let mut result = Vec::new();
        for (peer_id, peer_filter) in self.peers.lock().iter() {
            if peer_filter.cuckoo_filter.contains(piece_index) {
                result.push(*peer_id)
            }
        }

        result
    }
}
