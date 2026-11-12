//! Undo logging: every index change a block makes is recorded so the block can be
//! disconnected exactly, in one transaction, on a reorg. Tables are recorded by name, so
//! add-on indexes (tags, profiles, ord, ...) are undone the same way as core tables.

use arcade_store::{Result, StoreError, WriteTxn};
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOp {
    /// Plain table key: restore `old` (None = the key did not exist).
    Restore { table: String, key: Vec<u8>, old: Option<Vec<u8>> },
    /// Dup table: this value was added; undo removes it.
    DupAdded { table: String, key: Vec<u8>, val: Vec<u8> },
}

/// A write transaction view that records the inverse of every change.
pub struct Logged<'a, 'db> {
    pub w: &'a WriteTxn<'db>,
    ops: RefCell<Vec<UndoOp>>,
}

impl<'a, 'db> Logged<'a, 'db> {
    pub fn new(w: &'a WriteTxn<'db>) -> Self {
        Logged { w, ops: RefCell::new(Vec::new()) }
    }

    pub fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.w.get(table, key)
    }

    pub fn scan(&self, table: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.w.scan_prefix(table, prefix)
    }

    pub fn put(&self, table: &str, key: &[u8], val: &[u8]) -> Result<()> {
        let old = self.w.get(table, key)?;
        if old.as_deref() == Some(val) {
            return Ok(());
        }
        self.w.put(table, key, val)?;
        self.ops.borrow_mut().push(UndoOp::Restore { table: table.into(), key: key.to_vec(), old });
        Ok(())
    }

    pub fn del(&self, table: &str, key: &[u8]) -> Result<()> {
        if let Some(old) = self.w.get(table, key)? {
            self.w.del(table, key)?;
            self.ops.borrow_mut().push(UndoOp::Restore { table: table.into(), key: key.to_vec(), old: Some(old) });
        }
        Ok(())
    }

    /// Add a value to a dup table. The caller guarantees it was not already present.
    pub fn dup_add(&self, table: &str, key: &[u8], val: &[u8]) -> Result<()> {
        self.w.put(table, key, val)?;
        self.ops.borrow_mut().push(UndoOp::DupAdded { table: table.into(), key: key.to_vec(), val: val.to_vec() });
        Ok(())
    }

    /// Changes recorded so far.
    pub fn len(&self) -> usize {
        self.ops.borrow().len()
    }

    /// The (table, key) pairs of the changes recorded since `from` (a [`Logged::len`]).
    pub fn touched_since(&self, from: usize) -> Vec<(String, Vec<u8>)> {
        self.ops
            .borrow()
            .get(from..)
            .unwrap_or(&[])
            .iter()
            .map(|op| match op {
                UndoOp::Restore { table, key, .. } | UndoOp::DupAdded { table, key, .. } => (table.clone(), key.clone()),
            })
            .collect()
    }

    pub fn into_ops(self) -> Vec<UndoOp> {
        self.ops.into_inner()
    }
}

/// Replay a block's undo record backwards.
pub fn apply_undo(w: &WriteTxn, ops: &[UndoOp]) -> Result<()> {
    for op in ops.iter().rev() {
        match op {
            UndoOp::Restore { table, key, old } => match old {
                Some(v) => w.put(table, key, v)?,
                None => {
                    w.del(table, key)?;
                }
            },
            UndoOp::DupAdded { table, key, val } => {
                w.del_dup(table, key, val)?;
            }
        }
    }
    Ok(())
}

fn field(o: &mut Vec<u8>, b: &[u8]) {
    o.extend_from_slice(&(b.len() as u32).to_le_bytes());
    o.extend_from_slice(b);
}

pub fn encode(ops: &[UndoOp]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    for op in ops {
        match op {
            UndoOp::Restore { table, key, old } => {
                o.push(if old.is_some() { 1 } else { 0 });
                o.push(table.len() as u8);
                o.extend_from_slice(table.as_bytes());
                field(&mut o, key);
                if let Some(v) = old {
                    field(&mut o, v);
                }
            }
            UndoOp::DupAdded { table, key, val } => {
                o.push(2);
                o.push(table.len() as u8);
                o.extend_from_slice(table.as_bytes());
                field(&mut o, key);
                field(&mut o, val);
            }
        }
    }
    o
}

pub fn decode(b: &[u8]) -> Result<Vec<UndoOp>> {
    let bad = || StoreError::Corrupt { table: "core.v1.undo", why: "truncated undo record" };
    let mut pos = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = b.get(pos..pos + n).ok_or_else(bad)?;
        pos += n;
        Ok(s)
    };
    let n = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = take(1)?[0];
        let tl = take(1)?[0] as usize;
        let table = String::from_utf8(take(tl)?.to_vec()).map_err(|_| bad())?;
        let klen = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let key = take(klen)?.to_vec();
        let mut val = || -> Result<Vec<u8>> {
            let l = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
            Ok(take(l)?.to_vec())
        };
        ops.push(match kind {
            0 => UndoOp::Restore { table, key, old: None },
            1 => UndoOp::Restore { table, key, old: Some(val()?) },
            2 => UndoOp::DupAdded { table, key, val: val()? },
            _ => return Err(bad()),
        });
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let ops = vec![
            UndoOp::Restore { table: "core.v2.outputs".into(), key: vec![1, 2], old: None },
            UndoOp::Restore { table: "ledger.v1.tag".into(), key: vec![9], old: Some(vec![7, 7, 7]) },
            UndoOp::DupAdded { table: "core.v1.txs".into(), key: vec![5], val: vec![6] },
        ];
        assert_eq!(decode(&encode(&ops)).unwrap(), ops);
        assert!(decode(&encode(&ops)[..10]).is_err());
    }
}
