//! The catalog (docs/ARCADECHAIN.md §2.4): which segments of the listed chains exist, as
//! every arcade agrees on them.
//!
//! For each family of each listed chain the catalog holds a row of **spans**, from height 0
//! on, each starting where the one before ended. A span holds one confirmed segment, or
//! nothing where the segment it held was proved wrong and removed.
//!
//! - An arcade with draw weight **offers** an entry for a span that is open: the one after
//!   the last, or a removed entry's (same first and last height).
//! - Arcades with draw weight **attest** to it. Offering an entry is attesting to it. A key
//!   stands behind one entry for a first height at a time.
//! - An entry is **confirmed** in the block that brings the weight behind it over half of all
//!   draw weight, counted as the weights are in that block. The other entries offered for
//!   its span are dropped with their attestations.
//! - A removed entry is never taken again.
//!
//! What this can't check is whether the root is the right one: that is what the attesters
//! say, and what reveal demands and fraud proofs settle when they lie.

use crate::block::{self, Attest, BlockError, CatalogEntry, Item, Key};
use crate::merkle::Hash;
use crate::params::Net;
use crate::segment;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The draw weights the catalog counts with.
pub trait Weights {
    fn weight(&self, key: &Key) -> u64;
    /// All draw weight.
    fn total(&self) -> u128;
}

/// A confirmed segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Its segment id.
    pub id: Hash,
    pub entry: CatalogEntry,
    /// The height of the block that confirmed it.
    pub height: u64,
}

/// An entry that waits for attestations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub entry: CatalogEntry,
    /// The keys behind it: its poster, then the attesters in the order they came.
    pub behind: Vec<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Span {
    first: u64,
    last: u64,
    listed: Option<Listed>,
}

/// One family of one chain.
type Line = (String, u8);

fn line_of(e: &CatalogEntry) -> Line {
    (e.chain.clone(), e.family)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Offered(Hash),
    Attested(Hash),
    /// `id` took its span; `dropped` is every entry that was offered for it, `id`'s too.
    Confirmed { id: Hash, dropped: Vec<(Hash, Candidate)>, new_span: bool },
    Removed(Listed),
}

/// What one block did, for taking it back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Undo(Vec<Op>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    net: Net,
    /// Bytes of blocks a segment must hold (`params::SEGMENT_BYTES`; less on local test chains).
    full: u64,
    lines: BTreeMap<Line, Vec<Span>>,
    waiting: BTreeMap<Hash, Candidate>,
    /// Where each confirmed segment is: its line and its span's place in it.
    listed: HashMap<Hash, (Line, usize)>,
    removed: HashSet<Hash>,
}

fn no(what: &'static str) -> BlockError {
    BlockError::Invalid(what)
}

impl Catalog {
    pub fn new(net: Net, full: u64) -> Self {
        Catalog { net, full, lines: BTreeMap::new(), waiting: BTreeMap::new(), listed: HashMap::new(), removed: HashSet::new() }
    }

    pub fn net(&self) -> Net {
        self.net
    }

    pub fn segment_bytes(&self) -> u64 {
        self.full
    }

    /// A confirmed segment by its id.
    pub fn get(&self, id: &Hash) -> Option<&Listed> {
        let (line, at) = self.listed.get(id)?;
        self.lines.get(line)?.get(*at)?.listed.as_ref()
    }

    /// An entry that waits, by its segment id.
    pub fn waiting(&self, id: &Hash) -> Option<&Candidate> {
        self.waiting.get(id)
    }

    /// Every entry that waits, in id order.
    pub fn all_waiting(&self) -> impl Iterator<Item = (&Hash, &Candidate)> {
        self.waiting.iter()
    }

    /// The spans of one family of one chain, in height order: (first, last, its segment).
    pub fn spans(&self, chain: &str, family: u8) -> impl Iterator<Item = (u64, u64, Option<&Listed>)> {
        self.lines.get(&(chain.to_string(), family)).into_iter().flatten().map(|s| (s.first, s.last, s.listed.as_ref()))
    }

    /// The first height of the next segment of this family of this chain.
    pub fn next(&self, chain: &str, family: u8) -> u64 {
        self.lines.get(&(chain.to_string(), family)).and_then(|spans| spans.last()).map_or(0, |s| s.last + 1)
    }

    /// Confirmed segments, and the bytes in them.
    pub fn totals(&self) -> (usize, u64) {
        let all = self.lines.values().flatten().filter_map(|s| s.listed.as_ref());
        all.fold((0, 0), |(n, bytes), l| (n + 1, bytes + l.entry.bytes))
    }

    /// Was this segment id confirmed once and then proved wrong?
    pub fn was_removed(&self, id: &Hash) -> bool {
        self.removed.contains(id)
    }

    /// Take in a block's catalog entries and attestations, in the block's order. Other items
    /// are someone else's. On an error nothing has changed.
    pub fn apply(&mut self, items: &[Item], height: u64, weights: &impl Weights) -> Result<Undo, BlockError> {
        let mut undo = Undo::default();
        for item in items {
            let done = match item {
                Item::CatalogEntry(e) => self.offer(e, height, weights, &mut undo),
                Item::Attest(a) => self.attest(a, height, weights, &mut undo),
                _ => Ok(()),
            };
            if let Err(e) = done {
                self.undo(undo);
                return Err(e);
            }
        }
        Ok(undo)
    }

    /// May an entry be offered for `first..=last` of this line?
    fn open(&self, line: &Line, first: u64, last: u64) -> bool {
        let spans = self.lines.get(line).map(Vec::as_slice).unwrap_or(&[]);
        if first == spans.last().map_or(0, |s| s.last + 1) {
            return true;
        }
        match spans.binary_search_by_key(&first, |s| s.first) {
            Ok(at) => spans[at].listed.is_none() && spans[at].last == last,
            Err(_) => false,
        }
    }

    /// Does `key` stand behind an entry that waits for the span starting at `first`?
    fn stands(&self, line: &Line, first: u64, key: &Key) -> bool {
        self.waiting.values().any(|c| c.entry.first == first && c.entry.family == line.1 && c.entry.chain == line.0 && c.behind.contains(key))
    }

    fn offer(&mut self, e: &CatalogEntry, height: u64, weights: &impl Weights, undo: &mut Undo) -> Result<(), BlockError> {
        if !self.net.listed.contains(&e.chain.as_str()) {
            return Err(no("catalog entry: not a listed chain"));
        }
        if !segment::FAMILIES.contains(&e.family) {
            return Err(no("catalog entry: no such family"));
        }
        if !segment::whole_ranges(e.first, e.last) {
            return Err(no("catalog entry: not whole ranges"));
        }
        // The table, then at least a full segment of blocks.
        let least = (e.last - e.first + 1).checked_mul(segment::ROW).and_then(|table| table.checked_add(self.full));
        if least.map_or(true, |least| e.bytes < least) {
            return Err(no("catalog entry: not a full segment"));
        }
        if !block::verify(&e.poster, &e.message(&self.net), &e.sig) {
            return Err(BlockError::BadSignature("catalog entry"));
        }
        if weights.weight(&e.poster) == 0 {
            return Err(no("catalog entry: its poster has no draw weight"));
        }
        let id = e.segment(&self.net);
        if self.removed.contains(&id) {
            return Err(no("catalog entry: proved wrong before"));
        }
        if self.waiting.contains_key(&id) {
            return Err(no("catalog entry: offered already"));
        }
        let line = line_of(e);
        if !self.open(&line, e.first, e.last) {
            return Err(no("catalog entry: not the next segment of its chain"));
        }
        if self.stands(&line, e.first, &e.poster) {
            return Err(no("catalog entry: its poster stands behind another for these heights"));
        }
        self.waiting.insert(id, Candidate { entry: e.clone(), behind: vec![e.poster] });
        undo.0.push(Op::Offered(id));
        self.settle(&id, height, weights, undo);
        Ok(())
    }

    fn attest(&mut self, a: &Attest, height: u64, weights: &impl Weights, undo: &mut Undo) -> Result<(), BlockError> {
        let Some(c) = self.waiting.get(&a.segment) else {
            return Err(no("attestation: no such entry waits"));
        };
        if !block::verify(&a.attester, &a.message(&self.net), &a.sig) {
            return Err(BlockError::BadSignature("attestation"));
        }
        if weights.weight(&a.attester) == 0 {
            return Err(no("attestation: its signer has no draw weight"));
        }
        if self.stands(&line_of(&c.entry), c.entry.first, &a.attester) {
            return Err(no("attestation: its signer stands behind an entry for these heights already"));
        }
        self.waiting.get_mut(&a.segment).expect("found above").behind.push(a.attester);
        undo.0.push(Op::Attested(a.segment));
        self.settle(&a.segment, height, weights, undo);
        Ok(())
    }

    /// Confirm the waiting entry `id` if more than half of all draw weight is behind it.
    fn settle(&mut self, id: &Hash, height: u64, weights: &impl Weights, undo: &mut Undo) {
        let c = &self.waiting[id];
        let behind: u128 = c.behind.iter().map(|key| weights.weight(key) as u128).sum();
        if behind * 2 <= weights.total() {
            return;
        }
        let (line, first, last) = (line_of(&c.entry), c.entry.first, c.entry.last);
        let rivals: Vec<Hash> = self.waiting.iter().filter(|(_, o)| o.entry.first == first && line_of(&o.entry) == line).map(|(id, _)| *id).collect();
        let dropped: Vec<(Hash, Candidate)> = rivals.into_iter().map(|id| (id, self.waiting.remove(&id).expect("just listed"))).collect();
        let entry = dropped.iter().find(|(other, _)| other == id).expect("its own").1.entry.clone();
        let listed = Some(Listed { id: *id, entry, height });
        let spans = self.lines.entry(line.clone()).or_default();
        let (at, new_span) = match spans.binary_search_by_key(&first, |s| s.first) {
            Ok(at) => {
                spans[at].listed = listed;
                (at, false)
            }
            Err(_) => {
                spans.push(Span { first, last, listed });
                (spans.len() - 1, true)
            }
        };
        self.listed.insert(*id, (line, at));
        undo.0.push(Op::Confirmed { id: *id, dropped, new_span });
    }

    /// Take a confirmed segment out: it was proved wrong, or its holders didn't answer. Its
    /// span stays where it is and is open for another entry. `false` when `id` isn't listed.
    pub fn remove(&mut self, id: &Hash, undo: &mut Undo) -> bool {
        let Some((line, at)) = self.listed.remove(id) else { return false };
        let listed = self.lines.get_mut(&line).expect("listed")[at].listed.take().expect("listed");
        self.removed.insert(*id);
        undo.0.push(Op::Removed(listed));
        true
    }

    /// Take back what one block did; blocks are taken back newest first.
    pub fn undo(&mut self, undo: Undo) {
        for op in undo.0.into_iter().rev() {
            match op {
                Op::Offered(id) => {
                    self.waiting.remove(&id);
                }
                Op::Attested(id) => {
                    self.waiting.get_mut(&id).expect("it waited").behind.pop();
                }
                Op::Confirmed { id, dropped, new_span } => {
                    let (line, at) = self.listed.remove(&id).expect("it was listed");
                    let spans = self.lines.get_mut(&line).expect("its line");
                    if new_span {
                        spans.pop();
                        if spans.is_empty() {
                            self.lines.remove(&line);
                        }
                    } else {
                        spans[at].listed = None;
                    }
                    self.waiting.extend(dropped);
                }
                Op::Removed(listed) => {
                    let line = line_of(&listed.entry);
                    let spans = self.lines.get_mut(&line).expect("its line");
                    let at = spans.binary_search_by_key(&listed.entry.first, |s| s.first).expect("its span");
                    self.removed.remove(&listed.id);
                    self.listed.insert(listed.id, (line, at));
                    spans[at].listed = Some(listed);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::params::{MAINNET, RANGE, TESTNET};
    use ed25519_dalek::SigningKey;

    const NET: Net = TESTNET;
    const FULL: u64 = 1_000;

    struct Held(Vec<(Key, u64)>);

    impl Weights for Held {
        fn weight(&self, key: &Key) -> u64 {
            self.0.iter().find(|(k, _)| k == key).map_or(0, |(_, w)| *w)
        }
        fn total(&self) -> u128 {
            self.0.iter().map(|(_, w)| *w as u128).sum()
        }
    }

    fn secret(who: u8) -> SigningKey {
        SigningKey::from_bytes(&[who; 32])
    }

    fn key(who: u8) -> Key {
        secret(who).verifying_key().to_bytes()
    }

    /// Arcades 1 to 4 hold 10, 20, 30 and 40; arcade 9 holds nothing.
    fn held() -> Held {
        Held((1..=4).map(|who| (key(who), who as u64 * 10)).collect())
    }

    /// `who`'s entry for ranges `from..=to` of `chain`; `root` tells entries apart.
    pub(crate) fn entry_on(net: &Net, who: &SigningKey, chain: &str, from: u64, to: u64, bytes: u64, root: u8) -> CatalogEntry {
        let mut e = CatalogEntry {
            chain: chain.into(),
            family: segment::FAMILY_BLOCKS,
            first: from * RANGE,
            last: (to + 1) * RANGE - 1,
            bytes,
            manifest: [root; 32],
            root: [root; 32],
            poster: who.verifying_key().to_bytes(),
            sig: [0; 64],
        };
        e.sig = block::sign(who, &e.message(net));
        e
    }

    pub(crate) fn attest_on(net: &Net, who: &SigningKey, segment: Hash) -> Attest {
        let mut a = Attest { segment, attester: who.verifying_key().to_bytes(), sig: [0; 64] };
        a.sig = block::sign(who, &a.message(net));
        a
    }

    /// One range holds this many bytes when it is just full.
    fn just_full(ranges: u64) -> u64 {
        ranges * RANGE * segment::ROW + FULL
    }

    fn entry(who: u8, from: u64, to: u64, root: u8) -> CatalogEntry {
        entry_on(&NET, &secret(who), "doge", from, to, just_full(to - from + 1), root)
    }

    fn attest(who: u8, segment: Hash) -> Attest {
        attest_on(&NET, &secret(who), segment)
    }

    fn refused(c: &mut Catalog, item: Item) -> BlockError {
        let before = c.clone();
        let e = c.apply(&[item], 7, &held()).unwrap_err();
        assert_eq!(*c, before);
        e
    }

    #[test]
    fn an_entry_is_confirmed_by_more_than_half_the_weight() {
        let mut c = Catalog::new(NET, FULL);
        let empty = c.clone();
        let e = entry(1, 0, 0, 1);
        let id = e.segment(&NET);

        // Offered by 10 of 100: it waits.
        let u1 = c.apply(&[Item::CatalogEntry(e.clone())], 5, &held()).unwrap();
        assert_eq!(c.waiting(&id).unwrap().behind, vec![key(1)]);
        assert_eq!((c.get(&id), c.next("doge", 0)), (None, 0));
        let offered = c.clone();

        // 10 + 40 is exactly half: not enough. With 20 more it is.
        let u2 = c.apply(&[Item::Attest(attest(4, id))], 6, &held()).unwrap();
        assert!(c.get(&id).is_none());
        let half = c.clone();
        let u3 = c.apply(&[Item::Attest(attest(2, id))], 7, &held()).unwrap();
        assert_eq!(c.get(&id), Some(&Listed { id, entry: e.clone(), height: 7 }));
        assert!(c.waiting(&id).is_none());
        assert_eq!(c.next("doge", 0), RANGE);
        assert_eq!(c.spans("doge", 0).map(|(first, last, l)| (first, last, l.is_some())).collect::<Vec<_>>(), vec![(0, RANGE - 1, true)]);
        assert_eq!(c.totals(), (1, e.bytes));
        // Another chain's row is its own.
        assert_eq!(c.next("pep", 0), 0);

        // Block by block, back to nothing.
        c.undo(u3);
        assert_eq!(c, half);
        c.undo(u2);
        assert_eq!(c, offered);
        c.undo(u1);
        assert_eq!(c, empty);

        // All in one block is the same as one by one; a holder of more than half needs nobody.
        let u = c.apply(&[Item::CatalogEntry(e.clone()), Item::Attest(attest(4, id)), Item::Attest(attest(2, id))], 7, &held()).unwrap();
        assert!(c.get(&id).is_some());
        c.undo(u);
        assert_eq!(c, empty);
        let alone = Held(vec![(key(1), 51), (key(2), 49)]);
        c.apply(&[Item::CatalogEntry(e)], 5, &alone).unwrap();
        assert!(c.get(&id).is_some());
    }

    #[test]
    fn the_winner_drops_its_rivals_and_the_next_span_opens() {
        let mut c = Catalog::new(NET, FULL);
        let (right, wrong, longer) = (entry(1, 0, 0, 1), entry(2, 0, 0, 2), entry(3, 0, 1, 3));
        let ids = [right.segment(&NET), wrong.segment(&NET), longer.segment(&NET)];
        // Three entries for the heights from 0: two roots, and one that takes two ranges.
        c.apply(&[Item::CatalogEntry(right.clone()), Item::CatalogEntry(wrong), Item::CatalogEntry(longer)], 5, &held()).unwrap();
        assert_eq!(c.all_waiting().count(), 3);
        // The second range can't be offered before the first is settled.
        assert_eq!(refused(&mut c, Item::CatalogEntry(entry(4, 1, 1, 4))), no("catalog entry: not the next segment of its chain"));
        let before = c.clone();
        // 4 joins 1: 50 of 100, not enough. A key stands behind one entry for a first height.
        c.apply(&[Item::Attest(attest(4, ids[0]))], 6, &held()).unwrap();
        assert_eq!(refused(&mut c, Item::Attest(attest(4, ids[1]))), no("attestation: its signer stands behind an entry for these heights already"));
        assert_eq!(refused(&mut c, Item::Attest(attest(4, ids[0]))), no("attestation: its signer stands behind an entry for these heights already"));
        assert_eq!(refused(&mut c, Item::Attest(attest(2, ids[0]))), no("attestation: its signer stands behind an entry for these heights already"));
        assert_eq!(refused(&mut c, Item::CatalogEntry(entry(1, 0, 0, 9))), no("catalog entry: its poster stands behind another for these heights"));
        c.undo(Undo(vec![Op::Attested(ids[0])]));
        assert_eq!(c, before);

        // 3 and 4 hold 70: 4's attestation to 3's entry confirms it, and the others are gone.
        let u = c.apply(&[Item::Attest(attest(4, ids[2]))], 6, &held()).unwrap();
        assert_eq!(c.get(&ids[2]).unwrap().entry.last, 2 * RANGE - 1);
        assert_eq!(c.all_waiting().count(), 0);
        assert_eq!(c.next("doge", 0), 2 * RANGE);
        assert_eq!(refused(&mut c, Item::Attest(attest(4, ids[0]))), no("attestation: no such entry waits"));
        assert_eq!(refused(&mut c, Item::CatalogEntry(right)), no("catalog entry: not the next segment of its chain"));
        // Everybody is free again for the next span.
        c.apply(&[Item::CatalogEntry(entry(1, 2, 2, 5)), Item::CatalogEntry(entry(2, 2, 2, 6))], 8, &held()).unwrap();
        assert_eq!(c.all_waiting().count(), 2);
        // Taking the confirming block back brings the three back as they were.
        let mut back = Catalog::new(NET, FULL);
        back.apply(&[Item::CatalogEntry(entry(1, 0, 0, 1)), Item::CatalogEntry(entry(2, 0, 0, 2)), Item::CatalogEntry(entry(3, 0, 1, 3))], 5, &held()).unwrap();
        let mut again = back.clone();
        let u2 = again.apply(&[Item::Attest(attest(4, ids[2]))], 6, &held()).unwrap();
        assert_eq!(u, u2);
        again.undo(u2);
        assert_eq!(again, back);
    }

    #[test]
    fn what_an_entry_must_be() {
        let mut c = Catalog::new(NET, FULL);
        let good = entry(1, 0, 0, 1);
        let resigned = |change: &dyn Fn(&mut CatalogEntry)| {
            let mut e = good.clone();
            change(&mut e);
            e.sig = block::sign(&secret(1), &e.message(&NET));
            Item::CatalogEntry(e)
        };
        assert_eq!(refused(&mut c, resigned(&|e| e.chain = "mine".into())), no("catalog entry: not a listed chain"));
        assert_eq!(refused(&mut c, resigned(&|e| e.family = 10)), no("catalog entry: no such family"));
        for change in [(|e| e.first = 1) as fn(&mut CatalogEntry), |e| e.last -= 1, |e| e.last = u64::MAX, |e| (e.first, e.last) = (RANGE, RANGE - 1)] {
            assert_eq!(refused(&mut c, resigned(&change)), no("catalog entry: not whole ranges"));
        }
        assert_eq!(refused(&mut c, resigned(&|e| e.bytes -= 1)), no("catalog entry: not a full segment"));
        assert_eq!(refused(&mut c, resigned(&|e| e.bytes = 0)), no("catalog entry: not a full segment"));
        // A second range with only one range's bytes isn't full either.
        assert_eq!(refused(&mut c, resigned(&|e| e.last += RANGE)), no("catalog entry: not a full segment"));
        assert_eq!(refused(&mut c, Item::CatalogEntry(entry(1, 1, 1, 1))), no("catalog entry: not the next segment of its chain"));
        assert_eq!(refused(&mut c, Item::CatalogEntry(entry(9, 0, 0, 1))), no("catalog entry: its poster has no draw weight"));
        // Signed for the other network, by another key, or changed after signing.
        assert_eq!(refused(&mut c, Item::CatalogEntry(entry_on(&MAINNET, &secret(1), "doge", 0, 0, just_full(1), 1))), BlockError::BadSignature("catalog entry"));
        let mut stolen = good.clone();
        stolen.poster = key(2);
        assert_eq!(refused(&mut c, Item::CatalogEntry(stolen)), BlockError::BadSignature("catalog entry"));
        let mut pointer = good.clone();
        pointer.manifest[0] ^= 1;
        assert_eq!(refused(&mut c, Item::CatalogEntry(pointer)), BlockError::BadSignature("catalog entry"));

        // The good one waits; the same segment can't be offered again, by anyone.
        c.apply(&[Item::CatalogEntry(good.clone())], 5, &held()).unwrap();
        assert_eq!(refused(&mut c, Item::CatalogEntry(good.clone())), no("catalog entry: offered already"));
        let mut copy = good.clone();
        copy.poster = key(2);
        copy.sig = block::sign(&secret(2), &copy.message(&NET));
        assert_eq!(refused(&mut c, Item::CatalogEntry(copy)), no("catalog entry: offered already"));

        // Attestations: for an entry that waits, signed, by a key with weight.
        let id = good.segment(&NET);
        assert_eq!(refused(&mut c, Item::Attest(attest(2, [7; 32]))), no("attestation: no such entry waits"));
        assert_eq!(refused(&mut c, Item::Attest(attest(9, id))), no("attestation: its signer has no draw weight"));
        assert_eq!(refused(&mut c, Item::Attest(attest_on(&MAINNET, &secret(2), id))), BlockError::BadSignature("attestation"));
        let mut other = attest(2, id);
        other.attester = key(3);
        assert_eq!(refused(&mut c, Item::Attest(other)), BlockError::BadSignature("attestation"));

        // One bad item and the whole block's items are as if never seen.
        let before = c.clone();
        assert!(c.apply(&[Item::Attest(attest(2, id)), Item::Attest(attest(4, id)), Item::CatalogEntry(entry(9, 1, 1, 2))], 6, &held()).is_err());
        assert_eq!(c, before);
    }

    #[test]
    fn a_removed_entry_leaves_its_span_open() {
        let mut c = Catalog::new(NET, FULL);
        let w = Held(vec![(key(1), 60), (key(2), 40)]);
        // Three segments: ranges 0, 1 to 2, and 3.
        let entries = [entry(1, 0, 0, 1), entry(1, 1, 2, 2), entry(1, 3, 3, 3)];
        let ids: Vec<Hash> = entries.iter().map(|e| e.segment(&NET)).collect();
        for e in &entries {
            c.apply(&[Item::CatalogEntry(e.clone())], 5, &w).unwrap();
        }
        assert_eq!(c.totals().0, 3);
        let whole = c.clone();

        // The middle one is proved wrong.
        let mut u = Undo::default();
        assert!(c.remove(&ids[1], &mut u));
        assert!(!c.remove(&ids[1], &mut u));
        assert!(!c.remove(&[9; 32], &mut u));
        assert!(c.get(&ids[1]).is_none() && c.was_removed(&ids[1]));
        assert!(c.get(&ids[0]).is_some() && c.get(&ids[2]).is_some());
        assert_eq!(c.spans("doge", 0).map(|(first, _, l)| (first / RANGE, l.is_some())).collect::<Vec<_>>(), vec![(0, true), (1, false), (3, true)]);
        assert_eq!(c.next("doge", 0), 4 * RANGE);
        let holed = c.clone();

        // It is never taken again. Its span takes another root for the same heights only.
        let again = |c: &mut Catalog, e: CatalogEntry| c.apply(&[Item::CatalogEntry(e)], 9, &w).map(|_| ());
        assert_eq!(again(&mut c, entries[1].clone()), Err(no("catalog entry: proved wrong before")));
        assert_eq!(again(&mut c, entry(1, 1, 1, 4)), Err(no("catalog entry: not the next segment of its chain")));
        assert_eq!(again(&mut c, entry(1, 1, 3, 4)), Err(no("catalog entry: not the next segment of its chain")));
        assert_eq!(again(&mut c, entry(1, 2, 2, 4)), Err(no("catalog entry: not the next segment of its chain")));
        assert_eq!(c, holed);
        // 2 offers the right one (it waits), and the row goes on at its end meanwhile.
        let right = entry(2, 1, 2, 5);
        let u2 = c.apply(&[Item::CatalogEntry(right.clone()), Item::CatalogEntry(entry(1, 4, 4, 6))], 9, &w).unwrap();
        assert_eq!(c.next("doge", 0), 5 * RANGE);
        let u3 = c.apply(&[Item::Attest(attest(1, right.segment(&NET)))], 10, &w).unwrap();
        assert_eq!(c.get(&right.segment(&NET)).unwrap().height, 10);
        assert_eq!(c.spans("doge", 0).filter(|(_, _, l)| l.is_some()).count(), 4);
        assert_eq!(c.spans("doge", 0).map(|(first, _, _)| first / RANGE).collect::<Vec<_>>(), vec![0, 1, 3, 4]);

        // And all of it back.
        c.undo(u3);
        c.undo(u2);
        assert_eq!(c, holed);
        c.undo(u);
        assert_eq!(c, whole);
    }
}
