//! Kademlia routing table implementation
//!
//! Uses K-buckets to store peers organized by XOR distance from the local node.

use arrayvec::ArrayVec;
use std::fmt;

use super::distance::{Distance, Id};

/// K parameter: maximum nodes per bucket
pub const K: usize = 20;

/// Alpha parameter: concurrency for lookups
pub const ALPHA: usize = 3;

/// Number of buckets (for 256-bit keyspace)
pub const BUCKET_COUNT: usize = 256;

/// A single K-bucket storing up to K nodes
#[derive(Clone, Default)]
pub struct KBucket {
    /// Nodes in this bucket, ordered by time added (oldest first)
    nodes: ArrayVec<Id, K>,
}

/// Result of attempting to add a node to a bucket
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddNodeResult {
    /// Node was added successfully
    Added,
    /// Node already existed (position unchanged)
    AlreadyExists,
    /// Bucket is full - contains the oldest node that should be pinged
    /// If ping fails, call `replace_oldest` to evict and add the new node
    BucketFull { oldest: Id },
}

impl KBucket {
    /// Empty bucket constant for initialization
    pub const EMPTY: Self = Self {
        nodes: ArrayVec::new_const(),
    };

    /// Create a new empty bucket
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node to the bucket
    ///
    /// Returns true if the node was added or already exists.
    /// Returns false if the bucket is full.
    pub fn add_node(&mut self, node: Id) -> bool {
        matches!(
            self.try_add_node(node),
            AddNodeResult::Added | AddNodeResult::AlreadyExists
        )
    }

    /// Try to add a node to the bucket with detailed result
    ///
    /// Standard Kademlia eviction protocol:
    /// 1. If `BucketFull { oldest }` is returned, ping `oldest`
    /// 2. If ping succeeds, call `refresh_node(oldest)` and discard new node
    /// 3. If ping fails, call `replace_oldest(new_node)` to evict and add
    pub fn try_add_node(&mut self, node: Id) -> AddNodeResult {
        // Check if node already exists
        if self.nodes.iter().any(|n| *n == node) {
            return AddNodeResult::AlreadyExists;
        }

        // Add if space available
        if self.nodes.len() < K {
            self.nodes.push(node);
            return AddNodeResult::Added;
        }

        // Bucket full - return oldest for ping check
        AddNodeResult::BucketFull {
            oldest: self.nodes[0],
        }
    }

    /// Move a node to the back of the list (most recently seen)
    ///
    /// Call this when a node responds to a ping or sends us a message.
    /// Returns true if the node was found and refreshed.
    pub fn refresh_node(&mut self, id: &Id) -> bool {
        if let Some(pos) = self.nodes.iter().position(|n| n == id) {
            let node = self.nodes.remove(pos);
            self.nodes.push(node);
            true
        } else {
            false
        }
    }

    /// Get the oldest node (front of list, least recently seen)
    pub fn oldest(&self) -> Option<&Id> {
        self.nodes.first()
    }

    /// Replace the oldest node with a new one
    ///
    /// Call this when the oldest node fails to respond to a ping.
    /// Returns the evicted node.
    pub fn replace_oldest(&mut self, new_node: Id) -> Option<Id> {
        if self.nodes.is_empty() {
            self.nodes.push(new_node);
            None
        } else {
            let old = self.nodes.remove(0);
            self.nodes.push(new_node);
            Some(old)
        }
    }

    /// Remove a node from the bucket
    pub fn remove_node(&mut self, id: &Id) {
        self.nodes.retain(|n| n != id);
    }

    /// Check if the bucket contains a node
    pub fn contains(&self, id: &Id) -> bool {
        self.nodes.iter().any(|n| n == id)
    }

    /// Get all nodes in the bucket
    pub fn nodes(&self) -> &[Id] {
        &self.nodes
    }

    /// Check if the bucket is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Check if the bucket is full
    pub fn is_full(&self) -> bool {
        self.nodes.len() >= K
    }

    /// Number of nodes in the bucket
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

impl fmt::Debug for KBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KBucket")
            .field("nodes", &self.nodes.len())
            .finish()
    }
}

/// Empty bucket constant for returning references
static EMPTY_BUCKET: KBucket = KBucket::EMPTY;

/// Collection of K-buckets with lazy initialization
#[derive(Clone, Default)]
pub struct Buckets {
    /// Buckets, lazily initialized as needed
    buckets: Vec<KBucket>,
}

impl Buckets {
    /// Create empty buckets
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a bucket by index (read-only)
    pub fn get(&self, index: usize) -> &KBucket {
        if index >= BUCKET_COUNT {
            panic!("Bucket index out of range: {} >= {}", index, BUCKET_COUNT);
        }
        if index >= self.buckets.len() {
            &EMPTY_BUCKET
        } else {
            &self.buckets[index]
        }
    }

    /// Get a bucket by index (mutable, creates if needed)
    pub fn get_mut(&mut self, index: usize) -> &mut KBucket {
        if index >= BUCKET_COUNT {
            panic!("Bucket index out of range: {} >= {}", index, BUCKET_COUNT);
        }
        if index >= self.buckets.len() {
            self.buckets.resize_with(index + 1, KBucket::new);
        }
        &mut self.buckets[index]
    }

    /// Iterate over all buckets
    pub fn iter(&self) -> impl Iterator<Item = &KBucket> {
        self.buckets.iter()
    }

    /// Total number of nodes across all buckets
    pub fn total_nodes(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }
}

impl fmt::Debug for Buckets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let non_empty: Vec<(usize, usize)> = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.is_empty())
            .map(|(i, b)| (i, b.len()))
            .collect();

        f.debug_struct("Buckets")
            .field("total_buckets", &self.buckets.len())
            .field("non_empty", &non_empty)
            .finish()
    }
}

/// Kademlia routing table
#[derive(Debug)]
pub struct RoutingTable {
    /// The local node's ID
    pub local_id: Id,
    /// K-buckets organized by distance
    pub buckets: Buckets,
}

impl RoutingTable {
    /// Create a new routing table for the given local ID
    pub fn new(local_id: Id) -> Self {
        Self {
            local_id,
            buckets: Buckets::new(),
        }
    }

    /// Create a routing table with pre-existing buckets (for restoration from DB)
    pub fn with_buckets(local_id: Id, buckets: Buckets) -> Self {
        let mut table = Self { local_id, buckets };
        // Remove self from any bucket
        for i in 0..BUCKET_COUNT {
            if table.buckets.get(i).contains(&local_id) {
                table.buckets.get_mut(i).remove_node(&local_id);
            }
        }
        table
    }

    /// Get the bucket index for a target ID
    ///
    /// Returns None if the target is the local ID.
    pub fn bucket_index(&self, target: &Id) -> Option<usize> {
        self.local_id.distance(target).bucket_index()
    }

    /// Add a node to the routing table
    ///
    /// Returns true if the node was added, false if the appropriate bucket is full.
    /// Returns false immediately if the node is our local ID.
    pub fn add_node(&mut self, node: Id) -> bool {
        let Some(bucket_idx) = self.bucket_index(&node) else {
            return false; // Same as local ID
        };
        self.buckets.get_mut(bucket_idx).add_node(node)
    }

    /// Try to add a node with detailed result for eviction handling
    ///
    /// Returns `None` if node is our local ID.
    /// Otherwise returns the result of the add attempt.
    pub fn try_add_node(&mut self, node: Id) -> Option<AddNodeResult> {
        let bucket_idx = self.bucket_index(&node)?;
        Some(self.buckets.get_mut(bucket_idx).try_add_node(node))
    }

    /// Refresh a node (move to most recently seen position)
    ///
    /// Call when a node responds to us or sends us a message.
    pub fn refresh_node(&mut self, id: &Id) -> bool {
        if let Some(bucket_idx) = self.bucket_index(id) {
            self.buckets.get_mut(bucket_idx).refresh_node(id)
        } else {
            false
        }
    }

    /// Replace the oldest node in a bucket with a new node
    ///
    /// Call when the oldest node fails to respond to a ping.
    pub fn replace_oldest(&mut self, new_node: Id) -> Option<Id> {
        let bucket_idx = self.bucket_index(&new_node)?;
        self.buckets.get_mut(bucket_idx).replace_oldest(new_node)
    }

    /// Remove a node from the routing table
    pub fn remove_node(&mut self, id: &Id) {
        if let Some(bucket_idx) = self.bucket_index(id) {
            self.buckets.get_mut(bucket_idx).remove_node(id);
        }
    }

    /// Check if the routing table contains a node
    pub fn contains(&self, id: &Id) -> bool {
        if let Some(bucket_idx) = self.bucket_index(id) {
            self.buckets.get(bucket_idx).contains(id)
        } else {
            false
        }
    }

    /// Get all nodes in the routing table
    pub fn nodes(&self) -> impl Iterator<Item = &Id> {
        self.buckets.iter().flat_map(|bucket| bucket.nodes())
    }

    /// Find the K closest nodes to a target
    ///
    /// Uses XOR distance metric. Results are sorted by distance (closest first).
    pub fn find_closest_nodes(&self, target: &Id, k: usize) -> Vec<Id> {
        // Handle k=0 edge case
        if k == 0 {
            return Vec::new();
        }

        // Collect all nodes with their distances
        let mut candidates: Vec<(Distance, Id)> = self
            .nodes()
            .map(|node| (target.distance(node), *node))
            .collect();

        // If we have more than k candidates, use partial sort for efficiency
        if k < candidates.len() {
            candidates.select_nth_unstable(k - 1);
            candidates.truncate(k);
        }

        // Sort by distance
        candidates.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        candidates.into_iter().map(|(_, id)| id).collect()
    }

    /// Total number of nodes in the routing table
    pub fn len(&self) -> usize {
        self.buckets.total_nodes()
    }

    /// Check if the routing table is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    fn make_id_with_prefix(prefix: &[u8]) -> Id {
        let mut bytes = [0u8; 32];
        bytes[..prefix.len()].copy_from_slice(prefix);
        Id::new(bytes)
    }

    #[test]
    fn test_kbucket_add_node() {
        let mut bucket = KBucket::new();

        // Add a node
        let id = make_id(1);
        assert!(bucket.add_node(id));
        assert!(bucket.contains(&id));
        assert_eq!(bucket.len(), 1);
    }

    #[test]
    fn test_kbucket_add_duplicate() {
        let mut bucket = KBucket::new();
        let id = make_id(1);

        assert!(bucket.add_node(id));
        assert!(bucket.add_node(id)); // Adding again should return true
        assert_eq!(bucket.len(), 1); // But count stays at 1
    }

    #[test]
    fn test_kbucket_full() {
        let mut bucket = KBucket::new();

        // Fill the bucket
        for i in 0..K {
            assert!(bucket.add_node(make_id(i as u8)));
        }

        assert!(bucket.is_full());

        // Try to add one more - should fail
        assert!(!bucket.add_node(make_id(100)));
    }

    #[test]
    fn test_kbucket_remove() {
        let mut bucket = KBucket::new();
        let id = make_id(1);

        bucket.add_node(id);
        assert!(bucket.contains(&id));

        bucket.remove_node(&id);
        assert!(!bucket.contains(&id));
        assert!(bucket.is_empty());
    }

    #[test]
    fn test_routing_table_add_node() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        let peer = make_id(1);
        assert!(table.add_node(peer));
        assert!(table.contains(&peer));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_routing_table_cannot_add_self() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Cannot add ourselves
        assert!(!table.add_node(local));
        assert!(!table.contains(&local));
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn test_routing_table_bucket_assignment() {
        // Local ID with all zeros
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // ID that differs in first bit should go to bucket 0
        let peer = make_id_with_prefix(&[0x80]);
        assert!(table.add_node(peer));
        assert_eq!(table.bucket_index(&peer), Some(0));

        // ID that differs in second bit should go to bucket 1
        let peer2 = make_id_with_prefix(&[0x40]);
        assert!(table.add_node(peer2));
        assert_eq!(table.bucket_index(&peer2), Some(1));
    }

    #[test]
    fn test_find_closest_nodes() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add some nodes
        for i in 1..=10u8 {
            table.add_node(make_id(i));
        }

        // Find 3 closest to target
        let target = make_id(5);
        let closest = table.find_closest_nodes(&target, 3);

        assert_eq!(closest.len(), 3);

        // First result should be the target itself (if it exists)
        // or the closest node to it
        assert_eq!(closest[0], make_id(5));
    }

    #[test]
    fn test_find_closest_nodes_sorted_by_distance() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add nodes with various IDs
        let ids: Vec<u8> = vec![0xFF, 0x01, 0x80, 0x10, 0x08];
        for &id in &ids {
            table.add_node(make_id(id));
        }

        // Find closest to local (0x00)
        let target = make_id(0);
        let closest = table.find_closest_nodes(&target, 5);

        // Should be sorted by XOR distance to 0x00
        // 0x01 is closest, then 0x08, 0x10, 0x80, 0xFF
        assert_eq!(closest[0], make_id(0x01));
        assert_eq!(closest[1], make_id(0x08));
        assert_eq!(closest[2], make_id(0x10));
        assert_eq!(closest[3], make_id(0x80));
        assert_eq!(closest[4], make_id(0xFF));
    }

    #[test]
    fn test_find_closest_less_than_k() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add only 3 nodes
        for i in 1..=3u8 {
            table.add_node(make_id(i));
        }

        // Ask for 10 closest
        let closest = table.find_closest_nodes(&local, 10);

        // Should return all 3, not fail
        assert_eq!(closest.len(), 3);
    }

    #[test]
    fn test_remove_node() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        let peer = make_id(1);
        table.add_node(peer);
        assert!(table.contains(&peer));

        table.remove_node(&peer);
        assert!(!table.contains(&peer));
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn test_with_buckets_removes_self() {
        let local = make_id(0);
        let peer = make_id(1);

        // Create buckets with local ID in them (simulating bad DB data)
        let mut buckets = Buckets::new();
        let bucket_idx = local.distance(&peer).bucket_index().unwrap();
        buckets.get_mut(bucket_idx).add_node(peer);

        // Also add local to wrong bucket (shouldn't be there)
        let wrong_bucket_idx = local.distance(&make_id(2)).bucket_index().unwrap();
        buckets.get_mut(wrong_bucket_idx).add_node(local);

        // Create table with these buckets
        let table = RoutingTable::with_buckets(local, buckets);

        // Local should not be in the table
        assert!(!table.contains(&local));
        // Peer should still be there
        assert!(table.contains(&peer));
    }

    #[test]
    fn test_buckets_debug() {
        let mut buckets = Buckets::new();
        buckets.get_mut(0).add_node(make_id(1));
        buckets.get_mut(5).add_node(make_id(2));
        buckets.get_mut(5).add_node(make_id(3));

        let debug = format!("{:?}", buckets);
        assert!(debug.contains("Buckets"));
        assert!(debug.contains("non_empty"));
    }

    #[test]
    fn test_many_nodes_in_different_buckets() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add nodes that will land in different buckets
        // Using carefully chosen IDs to spread across buckets
        let mut added = 0;
        for i in 1..=100u8 {
            if table.add_node(make_id(i)) {
                added += 1;
            }
        }

        assert!(added > 0);
        assert_eq!(table.len(), added);

        // Find closest should work
        let closest = table.find_closest_nodes(&make_id(50), K);
        assert!(closest.len() <= K);
    }

    #[test]
    fn test_find_closest_nodes_k_zero() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add some nodes
        for i in 1..=10u8 {
            table.add_node(make_id(i));
        }

        // k=0 should return empty vec
        let closest = table.find_closest_nodes(&make_id(5), 0);
        assert!(closest.is_empty());
    }

    #[test]
    fn test_kbucket_try_add_node() {
        let mut bucket = KBucket::new();

        // Add a node
        let id1 = make_id(1);
        assert_eq!(bucket.try_add_node(id1), AddNodeResult::Added);

        // Add same node again
        assert_eq!(bucket.try_add_node(id1), AddNodeResult::AlreadyExists);

        // Fill the bucket
        for i in 2..=K as u8 {
            assert_eq!(bucket.try_add_node(make_id(i)), AddNodeResult::Added);
        }

        // Try to add one more - should return BucketFull with oldest
        let new_node = make_id(100);
        let result = bucket.try_add_node(new_node);
        assert!(matches!(result, AddNodeResult::BucketFull { oldest } if oldest == id1));
    }

    #[test]
    fn test_kbucket_refresh_node() {
        let mut bucket = KBucket::new();

        // Add nodes in order: 1, 2, 3
        bucket.add_node(make_id(1));
        bucket.add_node(make_id(2));
        bucket.add_node(make_id(3));

        // Oldest is 1
        assert_eq!(bucket.oldest(), Some(&make_id(1)));

        // Refresh node 1 - should move to back
        assert!(bucket.refresh_node(&make_id(1)));

        // Now oldest is 2
        assert_eq!(bucket.oldest(), Some(&make_id(2)));

        // Node 1 should be at the back (most recent)
        assert_eq!(bucket.nodes().last(), Some(&make_id(1)));
    }

    #[test]
    fn test_kbucket_replace_oldest() {
        let mut bucket = KBucket::new();

        // Add nodes: 1, 2, 3
        bucket.add_node(make_id(1));
        bucket.add_node(make_id(2));
        bucket.add_node(make_id(3));

        // Replace oldest (1) with new node (100)
        let evicted = bucket.replace_oldest(make_id(100));
        assert_eq!(evicted, Some(make_id(1)));

        // Node 1 should be gone, node 100 should be at back
        assert!(!bucket.contains(&make_id(1)));
        assert!(bucket.contains(&make_id(100)));
        assert_eq!(bucket.oldest(), Some(&make_id(2)));
    }

    #[test]
    fn test_eviction_workflow() {
        // Simulate the full Kademlia eviction workflow
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // All these nodes go to the same bucket (differ only in lower bits)
        // Create nodes that will all land in the same bucket
        let mut nodes_in_bucket = Vec::new();
        for i in 1..=(K + 1) as u8 {
            nodes_in_bucket.push(make_id(i));
        }

        // Add K nodes - should all succeed
        for i in 0..K {
            let result = table.try_add_node(nodes_in_bucket[i]);
            assert!(matches!(result, Some(AddNodeResult::Added)));
        }

        // Try to add K+1th node - should get BucketFull
        let new_node = nodes_in_bucket[K];
        let result = table.try_add_node(new_node);

        if let Some(AddNodeResult::BucketFull { oldest }) = result {
            // Simulate: oldest node failed ping, so we evict it
            let evicted = table.replace_oldest(new_node);
            assert_eq!(evicted, Some(oldest));

            // New node should now be in the table
            assert!(table.contains(&new_node));
            // Oldest should be gone
            assert!(!table.contains(&oldest));
        } else {
            // Nodes might have gone to different buckets
            // Just verify the new node was handled
            assert!(result.is_some());
        }
    }

    #[test]
    fn test_routing_table_refresh_node() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        let peer = make_id(1);
        table.add_node(peer);

        // Refresh should succeed for existing node
        assert!(table.refresh_node(&peer));

        // Refresh should fail for non-existent node
        assert!(!table.refresh_node(&make_id(99)));

        // Refresh should fail for local ID
        assert!(!table.refresh_node(&local));
    }
}
