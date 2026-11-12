//! A segment's bytes (docs/ARCADECHAIN.md §2.1, §2.4): where a source chain is cut into
//! segments, what a segment of blocks holds byte for byte, and its root.
//!
//! Everything here follows from the source chain alone, so every arcade that keeps the chain
//! makes the same segments with the same roots, and exactly one root is right for each.
//!
//! A segment of the **blocks** family, for heights `first..=last`:
//!
//! ```text
//! blocks   each block as its chain sends it (header, then transactions), in height order,
//!          the first at byte 0
//! table    one u64 (little-endian) per height: where that block starts, counted in bytes
//!          from the start of the segment
//! ```
//!
//! The blocks come first (Martin 2026-11-12) so a segment is sealed block by block as its blocks arrive: a block's
//! bytes are where they'll stay before the segment is cut, and only the table, known once it is, goes on the end.
//!
//! The root is the Merkle root (merkle.rs) over these bytes cut into 127-byte leaves, the last
//! leaf filled up with zeros.

use crate::merkle::{self, Hash, MerkleError, RootBuilder};
use crate::params;
use crate::sloth::DATA_LEAF;

/// The blocks family (a catalog entry's `family`).
pub const FAMILY_BLOCKS: u8 = 0;
/// The ord index's journals (§2.2, §14.17): the same layout, with each height's journal record
/// (arcade_chaindb::journal) in place of its block.
pub const FAMILY_ORD: u8 = 1;
/// The families (2 to 8: the other indexes in the same layout, arcade_chaindb::journal: ledger,
/// profiles, messages, names, tokens, maps, tags) the catalog lists.
pub const FAMILIES: &[u8] = &[FAMILY_BLOCKS, FAMILY_ORD, 2, 3, 4, 5, 6, 7, 8, FAMILY_COINS];
/// The coins each block spends (Martin 2026-11-12): with the blocks, everything the coin tables and the address index
/// are rebuilt from. Per height: u32 count, then per input of every transaction but the coinbase, in block order: the
/// coin's txid (32), vout (u32), value (u64), the height it was made at (u32), 1 if a coinbase made it else 0, its
/// script (u32 length + bytes). All little-endian.
pub const FAMILY_COINS: u8 = 9;
/// Bytes of one table row.
pub const ROW: u64 = 8;

/// A run of whole ranges that makes one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first: u64,
    pub last: u64,
    /// Bytes of its blocks (without the table).
    pub block_bytes: u64,
}

impl Span {
    pub fn heights(&self) -> u64 {
        self.last - self.first + 1
    }

    /// Bytes of the whole segment: the table, then the blocks.
    pub fn bytes(&self) -> u64 {
        table_bytes(self.heights()) + self.block_bytes
    }
}

pub fn table_bytes(heights: u64) -> u64 {
    heights * ROW
}

/// Is `first..=last` made of whole ranges?
pub fn whole_ranges(first: u64, last: u64) -> bool {
    first % params::RANGE == 0 && last >= first && last % params::RANGE == params::RANGE - 1
}

/// Cuts a chain into segments. `range_bytes[i]` is the bytes of the blocks in range `i`
/// (heights `i * RANGE ..= i * RANGE + RANGE - 1`), for every range that is complete, from
/// range 0 on. A segment starts where the one before ended and takes ranges until its blocks
/// hold at least `full` bytes; ranges left over at the end wait for more.
///
/// A segment never changes once it is cut: more ranges at the end only add segments.
pub fn cut(range_bytes: &[u64], full: u64) -> Vec<Span> {
    let mut out = Vec::new();
    let (mut first, mut sum) = (0u64, 0u64);
    for (i, &b) in range_bytes.iter().enumerate() {
        sum = sum.saturating_add(b);
        if sum >= full {
            let last = (i as u64 + 1) * params::RANGE - 1;
            out.push(Span { first, last, block_bytes: sum });
            first = last + 1;
            sum = 0;
        }
    }
    out
}

/// The table of a segment whose blocks have these sizes, in height order (it follows the blocks).
pub fn table(block_sizes: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(block_sizes.len() * ROW as usize);
    let mut at = 0u64;
    for &size in block_sizes {
        out.extend_from_slice(&at.to_le_bytes());
        at += size;
    }
    out
}

/// Where the table row of `height` lies in a segment that starts at height `first` and whose blocks hold
/// `block_bytes` (the table starts there).
pub fn row_at(block_bytes: u64, first: u64, height: u64) -> u64 {
    block_bytes + (height - first) * ROW
}

/// The leaves that hold bytes `at .. at + len` (`len > 0`): the first leaf and how many.
pub fn leaves_of(at: u64, len: u64) -> (u64, u64) {
    let first = at / DATA_LEAF as u64;
    let last = (at + len - 1) / DATA_LEAF as u64;
    (first, last - first + 1)
}

/// Builds a segment's root from its bytes, fed in order in pieces of any size.
#[derive(Default)]
pub struct Rooter {
    tree: RootBuilder,
    leaf: Vec<u8>,
    bytes: u64,
}

impl Rooter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, mut data: &[u8]) {
        self.bytes += data.len() as u64;
        while !data.is_empty() {
            let take = (DATA_LEAF - self.leaf.len()).min(data.len());
            self.leaf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.leaf.len() == DATA_LEAF {
                self.tree.push_leaf(&self.leaf);
                self.leaf.clear();
            }
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The root and the number of bytes it covers.
    pub fn finish(mut self) -> Result<(Hash, u64), MerkleError> {
        if !self.leaf.is_empty() {
            self.leaf.resize(DATA_LEAF, 0);
            self.tree.push_leaf(&self.leaf);
        }
        Ok((self.tree.finish()?, self.bytes))
    }
}

/// Leaves of a segment in a row, with the hashes that tie them to its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// Index of the first leaf.
    pub first: u64,
    pub leaves: Vec<[u8; DATA_LEAF]>,
    pub path: Vec<Hash>,
}

impl Run {
    /// Cut out of a whole segment held in memory (tests, and segments small enough).
    pub fn of(segment: &[u8], first: u64, count: u64) -> Result<Run, MerkleError> {
        let all: Vec<[u8; DATA_LEAF]> = segment
            .chunks(DATA_LEAF)
            .map(|c| {
                let mut leaf = [0u8; DATA_LEAF];
                leaf[..c.len()].copy_from_slice(c);
                leaf
            })
            .collect();
        let hashes: Vec<Hash> = all.iter().map(|l| merkle::leaf_hash(l)).collect();
        let path = merkle::run_path(&hashes, first, count)?;
        Ok(Run { first, leaves: all[first as usize..(first + count) as usize].to_vec(), path })
    }

    /// Are these the leaves at this place of the segment with this root and size?
    pub fn proves(&self, root: &Hash, segment_bytes: u64) -> bool {
        let hashes: Vec<Hash> = self.leaves.iter().map(|l| merkle::leaf_hash(l)).collect();
        merkle::verify_run(root, segment_bytes.div_ceil(DATA_LEAF as u64), self.first, &hashes, &self.path)
    }

    /// Bytes `at .. at + len` of the segment, when the run holds all of them.
    pub fn bytes(&self, at: u64, len: u64) -> Option<Vec<u8>> {
        let start = self.first.checked_mul(DATA_LEAF as u64)?;
        let from = at.checked_sub(start)?;
        let to = from.checked_add(len)?;
        if to > (self.leaves.len() * DATA_LEAF) as u64 {
            return None;
        }
        Some(self.leaves.iter().flatten().copied().skip(from as usize).take(len as usize).collect())
    }

    /// The table row at `at`: where a block starts.
    pub fn row(&self, at: u64) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(at, ROW)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_is_cut_the_same_whatever_comes_after() {
        let r = params::RANGE;
        // Ranges of 300, 300, 500, 1200, 100, 100 bytes; a segment is full at 1000.
        let sizes = [300, 300, 500, 1200, 100, 100];
        let spans = cut(&sizes, 1000);
        assert_eq!(
            spans,
            vec![
                Span { first: 0, last: 3 * r - 1, block_bytes: 1100 },
                Span { first: 3 * r, last: 4 * r - 1, block_bytes: 1200 },
            ]
        );
        assert!(spans.iter().all(|s| whole_ranges(s.first, s.last)));
        assert_eq!(spans[0].bytes(), 3 * r * ROW + 1100);
        // Fewer or more ranges at the end never move a cut already made.
        for n in 0..=sizes.len() {
            let part = cut(&sizes[..n], 1000);
            assert_eq!(part[..], spans[..part.len()], "{n} ranges");
        }
        let mut more = sizes.to_vec();
        more.extend([900, 5]);
        assert_eq!(cut(&more, 1000)[..2], spans[..]);
        assert_eq!(cut(&more, 1000)[2], Span { first: 4 * r, last: 7 * r - 1, block_bytes: 1100 });
        assert!(cut(&[], 1000).is_empty());
        assert!(cut(&[999], 1000).is_empty());
        // Exactly full counts as full.
        assert_eq!(cut(&[1000], 1000).len(), 1);
    }

    #[test]
    fn whole_ranges_only() {
        let r = params::RANGE;
        assert!(whole_ranges(0, r - 1));
        assert!(whole_ranges(2 * r, 5 * r - 1));
        assert!(!whole_ranges(1, r - 1));
        assert!(!whole_ranges(0, r));
        assert!(!whole_ranges(r, r - 1));
    }

    fn segment(sizes: &[u64]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (i, &s) in sizes.iter().enumerate() {
            bytes.extend(std::iter::repeat_n(i as u8 + 1, s as usize));
        }
        bytes.extend(table(sizes));
        bytes
    }

    #[test]
    fn the_table_says_where_each_block_starts() {
        let sizes = [81, 300, 5, 1000];
        let bytes = segment(&sizes);
        assert_eq!(bytes.len() as u64, table_bytes(4) + 1386);
        let mut at = 0;
        for (i, &s) in sizes.iter().enumerate() {
            let r = row_at(1386, 0, i as u64) as usize;
            let row = u64::from_le_bytes(bytes[r..r + 8].try_into().unwrap());
            assert_eq!(row, at);
            assert!(bytes[row as usize..(row + s) as usize].iter().all(|&b| b == i as u8 + 1));
            at += s;
        }
        assert_eq!(row_at(1386, 70_000, 70_003), 1386 + 24);
    }

    #[test]
    fn the_root_is_the_same_however_the_bytes_are_fed() {
        for len in [1usize, 126, 127, 128, 254, 1000, 12_345] {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
            let leaves: Vec<Hash> = bytes
                .chunks(DATA_LEAF)
                .map(|c| {
                    let mut l = [0u8; DATA_LEAF];
                    l[..c.len()].copy_from_slice(c);
                    merkle::leaf_hash(&l)
                })
                .collect();
            let want = merkle::root(&leaves).unwrap();
            for step in [1usize, 5, 127, 128, 1000, len] {
                let mut r = Rooter::new();
                for part in bytes.chunks(step) {
                    r.push(part);
                }
                assert_eq!(r.bytes(), len as u64);
                assert_eq!(r.finish().unwrap(), (want, len as u64), "{len} bytes fed {step} at a time");
            }
        }
        assert_eq!(Rooter::new().finish(), Err(MerkleError::Empty));
        // Bytes of zeros at the end are not the same segment as a shorter one: the size is
        // part of the segment's id (block.rs), and the leaf count follows from the size.
        let mut a = Rooter::new();
        a.push(&[1; 127]);
        let mut b = Rooter::new();
        b.push(&[1; 127]);
        b.push(&[0; 1]);
        assert_ne!(a.finish().unwrap(), b.finish().unwrap());
    }

    #[test]
    fn a_run_shows_bytes_of_the_segment_and_nothing_else() {
        let sizes = [81, 300, 5, 1000];
        let bytes = segment(&sizes);
        let mut r = Rooter::new();
        r.push(&bytes);
        let (root, len) = r.finish().unwrap();

        // The row of the third block, then that block's bytes.
        let at = row_at(1386, 0, 2);
        let (first, count) = leaves_of(at, ROW);
        let run = Run::of(&bytes, first, count).unwrap();
        assert!(run.proves(&root, len));
        let start = run.row(at).unwrap();
        assert_eq!(start, 81 + 300);
        let (first, count) = leaves_of(start, 5);
        let block = Run::of(&bytes, first, count).unwrap();
        assert!(block.proves(&root, len));
        assert_eq!(block.bytes(start, 5).unwrap(), vec![3u8; 5]);
        // Bytes the run doesn't hold aren't given.
        assert_eq!(block.bytes(start, 5 + 127), None);
        assert_eq!(block.bytes(0, 1).filter(|_| block.first > 0), None);
        assert_eq!(block.bytes(u64::MAX, 2), None);

        // A run that spans leaves.
        let (first, count) = leaves_of(100, 400);
        assert_eq!((first, count), (0, 4));
        let wide = Run::of(&bytes, first, count).unwrap();
        assert!(wide.proves(&root, len));
        assert_eq!(wide.bytes(100, 400).unwrap(), bytes[100..500]);

        // A changed byte or another place: no proof.
        let mut bent = wide.clone();
        bent.leaves[1][3] ^= 1;
        assert!(!bent.proves(&root, len));
        let mut moved = wide.clone();
        moved.first += 1;
        assert!(!moved.proves(&root, len));
        // A size that gives the tree another shape around the run: no proof. (A root alone
        // doesn't fix the size: a run can prove under two sizes whose trees are alike around
        // it. The size is fixed by the catalog entry, which the segment's id is made from, and
        // an entry's size is right or wrong together with its root.)
        assert!(!wide.proves(&root, 5 * 127));
        assert!(!wide.proves(&root, 3 * 127));
        assert!(wide.proves(&root, len + 127));
        // The last leaf is filled with zeros, and its run still proves.
        let (first, count) = leaves_of(len - 1, 1);
        let end = Run::of(&bytes, first, count).unwrap();
        assert!(end.proves(&root, len));
        assert_eq!(leaves_of(0, 127), (0, 1));
        assert_eq!(leaves_of(126, 2), (0, 2));
        assert_eq!(leaves_of(127, 1), (1, 1));
    }
}
