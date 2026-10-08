//! Edit history for undo/redo.
//!
//! Edits are collected into groups: characters typed in a row are undone with a single Cmd+Z.

use crate::transaction::Transaction;

#[derive(Debug, Clone)]
pub struct Revision {
    /// An edit together with the selection after it.
    pub transaction: Transaction,
    /// The inverse edit together with the selection before it.
    pub inversion: Transaction,
}

#[derive(Debug)]
struct Group {
    /// A unique id of the document state after the group. Needed to tell whether the current state
    /// matches the saved one.
    id: u64,
    revisions: Vec<Revision>,
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_id: u64,
}

impl History {
    pub fn commit(&mut self, revision: Revision, coalesce: bool) {
        self.redo.clear();
        self.next_id += 1;
        match self.undo.last_mut() {
            Some(group) if coalesce => {
                group.revisions.push(revision);
                group.id = self.next_id;
            }
            _ => self.undo.push(Group {
                id: self.next_id,
                revisions: vec![revision],
            }),
        }
    }

    /// Transactions to apply in sequence to undo the last group.
    pub fn undo(&mut self) -> Option<Vec<Transaction>> {
        let group = self.undo.pop()?;
        let txs = group
            .revisions
            .iter()
            .rev()
            .map(|rev| rev.inversion.clone())
            .collect();
        self.redo.push(group);
        Some(txs)
    }

    pub fn redo(&mut self) -> Option<Vec<Transaction>> {
        let group = self.redo.pop()?;
        let txs = group
            .revisions
            .iter()
            .map(|rev| rev.transaction.clone())
            .collect();
        self.undo.push(group);
        Some(txs)
    }

    /// The id of the current state; `0` is the original document.
    pub fn state_id(&self) -> u64 {
        self.undo.last().map_or(0, |group| group.id)
    }
}
