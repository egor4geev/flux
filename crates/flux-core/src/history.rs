//! История правок для undo/redo.
//!
//! Правки собираются в группы: подряд набранные символы отменяются одним Cmd+Z.

use crate::transaction::Transaction;

#[derive(Debug, Clone)]
pub struct Revision {
    /// Правка вместе с выделением после неё.
    pub transaction: Transaction,
    /// Обратная правка вместе с выделением до неё.
    pub inversion: Transaction,
}

#[derive(Debug)]
struct Group {
    /// Уникальный id состояния документа после группы. Нужен, чтобы понять,
    /// совпадает ли текущее состояние с сохранённым.
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

    /// Транзакции, которые надо применить по порядку, чтобы отменить последнюю группу.
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

    /// Id текущего состояния; `0` — исходный документ.
    pub fn state_id(&self) -> u64 {
        self.undo.last().map_or(0, |group| group.id)
    }
}
