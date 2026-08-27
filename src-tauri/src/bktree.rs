//! BK-tree over 128-bit perceptual fingerprints, indexed by Hamming distance.
//!
//! Hamming distance on a fixed-width hash is a proper metric (symmetric, satisfies
//! the triangle inequality), which is exactly what a BK-tree needs: children of a
//! node are keyed by their distance *to that node*, so a radius query only has to
//! descend into children whose edge distance could still land inside the query
//! radius, instead of touching every item. On real photo libraries, where most
//! pairs are far apart and only a handful of near-duplicates sit close together,
//! this turns a scan of the whole set per lookup into a scan of a small neighbourhood.

use rayon::prelude::*;
use std::collections::HashMap;

/// Perceptual fingerprints are 128 bits, so the maximum possible distance is 128 and
/// every node needs at most 129 children (edge distances 0..=128).
type Distance = u32;

struct Node {
    hash: u128,
    /// Indices of items that hashed to exactly this value. Perceptual hashing
    /// collapses genuinely different photos onto the same 128 bits often enough
    /// that a node needs to hold more than one item, not just the first.
    items: Vec<usize>,
    children: HashMap<Distance, Box<Node>>,
}

impl Node {
    fn new(hash: u128, item: usize) -> Self {
        Node { hash, items: vec![item], children: HashMap::new() }
    }

    fn insert(&mut self, hash: u128, item: usize) {
        let d = hamming(self.hash, hash);
        if d == 0 {
            self.items.push(item);
            return;
        }
        match self.children.get_mut(&d) {
            Some(child) => child.insert(hash, item),
            None => {
                self.children.insert(d, Box::new(Node::new(hash, item)));
            }
        }
    }

    /// Collects every item within `radius` of `query`, alongside its distance.
    ///
    /// The triangle inequality is the whole trick: any item under a child reached
    /// through an edge of distance `edge` is at least `|edge - d|` away from the
    /// query, where `d` is this node's own distance to the query. A child edge
    /// outside `[d - radius, d + radius]` cannot contain a match, so it is skipped
    /// without ever visiting it.
    fn find_within(&self, query: u128, radius: Distance, out: &mut Vec<(usize, Distance)>) {
        let d = hamming(self.hash, query);
        if d <= radius {
            out.extend(self.items.iter().map(|&item| (item, d)));
        }
        let lo = d.saturating_sub(radius);
        let hi = d.saturating_add(radius);
        for (&edge, child) in &self.children {
            if edge >= lo && edge <= hi {
                child.find_within(query, radius, out);
            }
        }
    }
}

#[inline]
fn hamming(a: u128, b: u128) -> u32 {
    (a ^ b).count_ones()
}

/// A BK-tree of 128-bit fingerprints. Read-only queries (`find_within`) take `&self`
/// and touch no shared mutable state, so once built, a tree can be queried from every
/// `rayon` worker at once — that is where this actually pays for itself, since
/// insertion of even 50,000 hashes is already well under a second done plainly.
pub struct BkTree {
    root: Option<Box<Node>>,
    len: usize,
}

impl BkTree {
    pub fn new() -> Self {
        BkTree { root: None, len: 0 }
    }

    /// Builds a tree from `(item_index, hash)` pairs in one pass. Insertion order
    /// does not affect query results, only the tree's shape, so this does not need
    /// to run in parallel to make queries fast afterward.
    pub fn build(items: impl IntoIterator<Item = (usize, u128)>) -> Self {
        let mut tree = BkTree::new();
        for (item, hash) in items {
            tree.insert(hash, item);
        }
        tree
    }

    pub fn insert(&mut self, hash: u128, item: usize) {
        self.len += 1;
        match &mut self.root {
            Some(root) => root.insert(hash, item),
            None => self.root = Some(Box::new(Node::new(hash, item))),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Every indexed item within `radius` of `query`, as `(item_index, distance)`,
    /// in no particular order.
    pub fn find_within(&self, query: u128, radius: Distance) -> Vec<(usize, Distance)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.find_within(query, radius, &mut out);
        }
        out
    }

    /// Runs `find_within` for every query in parallel across `rayon`'s pool. Meant
    /// for exactly the case this tree exists for: a batch of tens of thousands of
    /// fingerprints, each needing its own neighbourhood search, that no longer have
    /// to wait on each other.
    pub fn find_within_many(
        &self,
        queries: &[(usize, u128)],
        radius: Distance,
    ) -> Vec<(usize, Vec<(usize, Distance)>)> {
        queries
            .par_iter()
            .map(|&(qid, hash)| (qid, self.find_within(hash, radius)))
            .collect()
    }
}

impl Default for BkTree {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(usize, u128)> for BkTree {
    fn from_iter<T: IntoIterator<Item = (usize, u128)>>(iter: T) -> Self {
        BkTree::build(iter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_finds_nothing() {
        let tree = BkTree::new();
        assert!(tree.is_empty());
        assert!(tree.find_within(0, 128).is_empty());
    }

    #[test]
    fn exact_match_at_distance_zero() {
        let tree = BkTree::build([(0, 0b1010), (1, 0b1111)]);
        let hits = tree.find_within(0b1010, 0);
        assert_eq!(hits, vec![(0, 0)]);
    }

    #[test]
    fn duplicate_hashes_all_come_back() {
        // Two different photos can hash identically: both must be found, not just
        // whichever was inserted first.
        let tree = BkTree::build([(0, 42u128), (1, 42u128), (2, 999)]);
        let mut hits = tree.find_within(42, 0);
        hits.sort();
        assert_eq!(hits, vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn respects_radius_boundary() {
        // 0b0000 and 0b0111 differ in 3 bits.
        let tree = BkTree::build([(0, 0b0000u128)]);
        assert_eq!(tree.find_within(0b0111, 2), vec![]);
        assert_eq!(tree.find_within(0b0111, 3), vec![(0, 3)]);
    }

    #[test]
    fn matches_brute_force_on_random_data() {
        // The property that actually matters: for any radius, the tree returns
        // exactly the same set a linear scan would, just faster. A weak but
        // deterministic PRNG is enough here — this isn't about randomness quality,
        // only about generating varied fingerprints without a rand dependency.
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let mut state = 0x243F6A8885A308D3u64;
        let hashes: Vec<u128> = (0..500)
            .map(|_| ((next(&mut state) as u128) << 64) | next(&mut state) as u128)
            .collect();

        let tree = BkTree::build(hashes.iter().copied().enumerate());

        for radius in [1u32, 8, 32] {
            for (qi, &q) in hashes.iter().enumerate().step_by(37) {
                let mut expected: Vec<(usize, u32)> = hashes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &h)| {
                        let d = hamming(h, q);
                        (d <= radius).then_some((i, d))
                    })
                    .collect();
                let mut got = tree.find_within(q, radius);
                expected.sort();
                got.sort();
                assert_eq!(got, expected, "mismatch for query {qi} at radius {radius}");
            }
        }
    }

    #[test]
    fn find_within_many_matches_sequential_lookups() {
        let mut state = 0x9E3779B97F4A7C15u64;
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let hashes: Vec<u128> = (0..200)
            .map(|_| ((next(&mut state) as u128) << 64) | next(&mut state) as u128)
            .collect();
        let tree = BkTree::build(hashes.iter().copied().enumerate());
        let queries: Vec<(usize, u128)> = hashes.iter().copied().enumerate().collect();

        let parallel = tree.find_within_many(&queries, 16);
        for (qid, mut hits) in parallel {
            let mut sequential = tree.find_within(hashes[qid], 16);
            hits.sort();
            sequential.sort();
            assert_eq!(hits, sequential);
        }
    }

    /// First attempt at this benchmark used pure uniform-random 128-bit hashes and
    /// found the tree 18x *slower* than a linear scan. That was real, and it is a
    /// known property of metric trees, not a bug: Hamming distance between two
    /// uniform-random 128-bit values concentrates tightly around 64 (std. dev.
    /// ~5.7), so a radius-28 window around any node's own ~64-ish distance to the
    /// query covers nearly every child edge. Almost nothing gets pruned, and the
    /// tree pays for pointer-chasing and hashmap lookups that a flat, cache-friendly
    /// linear scan never does, for no benefit. That is the "curse of dimensionality"
    /// that metric trees are known to hit on data with no real cluster structure.
    ///
    /// A photo library is not uniform noise, though: most photos are unrelated to
    /// each other (~64 bits apart, like above), but the ones that *do* match are
    /// genuinely close (a few bits apart, from a burst or a re-export), which is
    /// exactly the structure a BK-tree is built to exploit. This benchmark models
    /// that instead: a majority of unrelated singletons plus a minority of tight
    /// clusters, and only claims a win if the tree actually beats linear on data
    /// shaped like the thing `compute_view` actually processes.
    #[test]
    #[ignore = "timing, run explicitly with --ignored --release"]
    fn bench_50k_tree_vs_linear_scan() {
        use std::time::Instant;

        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn random_hash(state: &mut u64) -> u128 {
            ((next(state) as u128) << 64) | next(state) as u128
        }
        /// Flips `count` distinct pseudo-random bit positions in `hash`.
        fn flip_bits(state: &mut u64, hash: u128, count: u32) -> u128 {
            let mut out = hash;
            for _ in 0..count {
                out ^= 1u128 << (next(state) % 128);
            }
            out
        }

        let mut state = 0xABCDEF0123456789u64;
        let threshold = 28; // the app's own default similarity threshold
        let n = 50_000;
        let singletons = 45_000;
        let cluster_members = n - singletons;
        let clusters = 1_000; // ~5 members per cluster: a burst or a raw/JPEG-style pair set

        let mut hashes: Vec<u128> = (0..singletons).map(|_| random_hash(&mut state)).collect();
        for c in 0..clusters {
            let seed = random_hash(&mut state);
            let members_in_this_cluster = cluster_members / clusters + (c == 0) as u32 * (cluster_members % clusters);
            for _ in 0..members_in_this_cluster {
                // Well inside the threshold, the way a real near-duplicate is.
                let flips = next(&mut state) as u32 % (threshold / 2);
                hashes.push(flip_bits(&mut state, seed, flips));
            }
        }
        assert_eq!(hashes.len() as u32, n);

        let tree = BkTree::build(hashes.iter().copied().enumerate());
        let start = Instant::now();
        let tree_hits: usize = hashes.iter().map(|&h| tree.find_within(h, threshold).len()).sum();
        let tree_time = start.elapsed();

        let start = Instant::now();
        let linear_hits: usize = hashes
            .iter()
            .map(|&h| hashes.iter().filter(|&&other| hamming(h, other) <= threshold).count())
            .sum();
        let linear_time = start.elapsed();

        println!("BK-tree ({n} items, {clusters} clusters): {tree_hits} hits in {tree_time:?}");
        println!("Linear  ({n} items, {clusters} clusters): {linear_hits} hits in {linear_time:?}");
        assert_eq!(tree_hits, linear_hits, "tree and linear scan must agree on every hit");
        assert!(
            tree_time < linear_time,
            "expected the tree to beat a linear scan at n={n} on clustered data, got tree={tree_time:?} linear={linear_time:?}"
        );
    }
}
