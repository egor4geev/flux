//! Изменения текста.
//!
//! [`ChangeSet`] описывает правку всего документа последовательностью операций
//! `Retain` / `Delete` / `Insert`, покрывающей его целиком. Такое представление
//! легко применить, инвертировать (для undo) и использовать для пересчёта позиций.

use ropey::Rope;

use crate::selection::Selection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Пропустить `n` символов без изменений.
    Retain(usize),
    /// Удалить `n` символов.
    Delete(usize),
    /// Вставить строку.
    Insert(String),
}

/// К какой стороне вставки прилипает позиция, если вставка ровно в ней.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    Before,
    After,
}

/// Одна правка: заменить символы `from..to` на `text` (`None` — просто удалить).
pub type Change = (usize, usize, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeSet {
    ops: Vec<Operation>,
    /// Длина документа до применения, в символах.
    len: usize,
    /// Длина документа после применения, в символах.
    len_after: usize,
}

impl ChangeSet {
    /// Тождественное изменение документа длины `len`.
    pub fn identity(len: usize) -> Self {
        let mut cs = Self::default();
        cs.retain(len);
        cs
    }

    /// Строит изменение из списка правок, отсортированных по `from`.
    /// Пересекающиеся правки обрезаются, чтобы не залезать на предыдущие.
    pub fn from_changes(len: usize, changes: impl IntoIterator<Item = Change>) -> Self {
        let mut cs = Self::default();
        let mut last = 0;
        for (from, to, text) in changes {
            let from = from.clamp(last, len);
            let to = to.clamp(from, len);
            cs.retain(from - last);
            if let Some(text) = text {
                cs.insert(text);
            }
            cs.delete(to - from);
            last = to;
        }
        cs.retain(len - last);
        cs
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn len_after(&self) -> usize {
        self.len_after
    }

    pub fn ops(&self) -> &[Operation] {
        &self.ops
    }

    /// Ничего не меняет.
    pub fn is_empty(&self) -> bool {
        self.ops.iter().all(|op| matches!(op, Operation::Retain(_)))
    }

    fn retain(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.len += n;
        self.len_after += n;
        if let Some(Operation::Retain(last)) = self.ops.last_mut() {
            *last += n;
        } else {
            self.ops.push(Operation::Retain(n));
        }
    }

    fn delete(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.len += n;
        if let Some(Operation::Delete(last)) = self.ops.last_mut() {
            *last += n;
        } else {
            self.ops.push(Operation::Delete(n));
        }
    }

    /// Вставка всегда идёт перед соседним удалением: так у одного и того же
    /// изменения ровно одно представление.
    fn insert(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.len_after += text.chars().count();
        let at = match self.ops.last() {
            Some(Operation::Delete(_)) => self.ops.len() - 1,
            _ => self.ops.len(),
        };
        if let Some(Operation::Insert(prev)) = at.checked_sub(1).map(|i| &mut self.ops[i]) {
            prev.push_str(&text);
        } else {
            self.ops.insert(at, Operation::Insert(text));
        }
    }

    pub fn apply(&self, text: &mut Rope) {
        debug_assert_eq!(text.len_chars(), self.len, "changeset applied to wrong document");
        let mut pos = 0;
        for op in &self.ops {
            match op {
                Operation::Retain(n) => pos += n,
                Operation::Delete(n) => text.remove(pos..pos + n),
                Operation::Insert(s) => {
                    text.insert(pos, s);
                    pos += s.chars().count();
                }
            }
        }
    }

    /// Изменение, отменяющее это. `original` — документ до применения.
    pub fn invert(&self, original: &Rope) -> ChangeSet {
        let mut inverted = Self::default();
        let mut pos = 0;
        for op in &self.ops {
            match op {
                Operation::Retain(n) => {
                    inverted.retain(*n);
                    pos += n;
                }
                Operation::Delete(n) => {
                    inverted.insert(original.slice(pos..pos + n).to_string());
                    pos += n;
                }
                Operation::Insert(s) => inverted.delete(s.chars().count()),
            }
        }
        inverted
    }

    /// Где окажется позиция `pos` после применения изменения.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        let mut old = 0;
        let mut new = 0;
        for op in &self.ops {
            match op {
                Operation::Retain(n) => {
                    if pos < old + n {
                        return new + (pos - old);
                    }
                    old += n;
                    new += n;
                }
                Operation::Delete(n) => {
                    if pos < old + n {
                        return new;
                    }
                    old += n;
                }
                Operation::Insert(s) => {
                    if pos == old && assoc == Assoc::Before {
                        return new;
                    }
                    new += s.chars().count();
                }
            }
        }
        new + pos.saturating_sub(old)
    }

    /// То же, что [`map_pos`](Self::map_pos) для каждой позиции, но за один проход по
    /// изменению: O(операций + позиций). Позиции должны идти по неубыванию, а равные — сначала
    /// с `Assoc::Before` (так идут концы и начала соседних диапазонов: конец — `Before`,
    /// следующее начало — `After`).
    pub fn map_sorted(&self, positions: impl IntoIterator<Item = (usize, Assoc)>) -> Vec<usize> {
        let mut mapped = Vec::new();
        let (mut i, mut old, mut new) = (0, 0, 0);
        'positions: for (pos, assoc) in positions {
            while let Some(op) = self.ops.get(i) {
                match op {
                    Operation::Retain(n) => {
                        if pos < old + n {
                            mapped.push(new + (pos - old));
                            continue 'positions;
                        }
                        old += n;
                        new += n;
                    }
                    Operation::Delete(n) => {
                        if pos < old + n {
                            mapped.push(new);
                            continue 'positions;
                        }
                        old += n;
                    }
                    Operation::Insert(s) => {
                        if pos == old && assoc == Assoc::Before {
                            mapped.push(new);
                            continue 'positions;
                        }
                        new += s.chars().count();
                    }
                }
                i += 1;
            }
            mapped.push(new + pos.saturating_sub(old));
        }
        mapped
    }
}

/// Изменение текста вместе с выделением, которое должно получиться после него.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub changes: ChangeSet,
    /// `None` — выделение пересчитывается через [`Selection::map`].
    pub selection: Option<Selection>,
}

impl Transaction {
    pub fn new(changes: ChangeSet) -> Self {
        Self {
            changes,
            selection: None,
        }
    }

    pub fn change(text: &Rope, changes: impl IntoIterator<Item = Change>) -> Self {
        Self::new(ChangeSet::from_changes(text.len_chars(), changes))
    }

    pub fn with_selection(mut self, selection: Selection) -> Self {
        self.selection = Some(selection);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, changes: Vec<Change>) -> (Rope, ChangeSet) {
        let mut rope = Rope::from_str(text);
        let cs = ChangeSet::from_changes(rope.len_chars(), changes);
        cs.apply(&mut rope);
        (rope, cs)
    }

    #[test]
    fn apply_insert_delete_replace() {
        let (rope, cs) = apply(
            "hello world",
            vec![
                (0, 0, Some(">".into())),
                (5, 6, None),
                (6, 11, Some("rust".into())),
            ],
        );
        assert_eq!(rope, ">hellorust");
        assert_eq!(cs.len(), 11);
        assert_eq!(cs.len_after(), 10);
    }

    #[test]
    fn invert_restores_original() {
        let original = Rope::from_str("один два три\nчетыре");
        let mut rope = original.clone();
        let cs = ChangeSet::from_changes(
            rope.len_chars(),
            vec![(0, 4, Some("1".into())), (9, 12, None), (13, 13, Some("→".into()))],
        );
        let inverted = cs.invert(&original);
        cs.apply(&mut rope);
        assert_ne!(rope, original);
        inverted.apply(&mut rope);
        assert_eq!(rope, original);
    }

    #[test]
    fn insert_goes_before_delete() {
        let mut cs = ChangeSet::default();
        cs.delete(2);
        cs.insert("ab".into());
        assert_eq!(
            cs.ops(),
            &[Operation::Insert("ab".into()), Operation::Delete(2)]
        );
    }

    #[test]
    fn map_pos_through_insert() {
        let cs = ChangeSet::from_changes(5, vec![(2, 2, Some("xyz".into()))]);
        assert_eq!(cs.map_pos(1, Assoc::After), 1);
        assert_eq!(cs.map_pos(2, Assoc::Before), 2);
        assert_eq!(cs.map_pos(2, Assoc::After), 5);
        assert_eq!(cs.map_pos(4, Assoc::After), 7);
        assert_eq!(cs.map_pos(5, Assoc::After), 8);
    }

    #[test]
    fn map_pos_through_delete() {
        let cs = ChangeSet::from_changes(10, vec![(2, 6, None)]);
        assert_eq!(cs.map_pos(1, Assoc::After), 1);
        assert_eq!(cs.map_pos(4, Assoc::After), 2);
        assert_eq!(cs.map_pos(6, Assoc::After), 2);
        assert_eq!(cs.map_pos(8, Assoc::After), 4);
    }

    #[test]
    fn map_pos_after_replace_lands_after_new_text() {
        let cs = ChangeSet::from_changes(10, vec![(2, 6, Some("ab".into()))]);
        assert_eq!(cs.map_pos(6, Assoc::After), 4);
        assert_eq!(cs.map_pos(2, Assoc::Before), 2);
    }

    #[test]
    fn map_sorted_agrees_with_map_pos() {
        // Детерминированный «случайный» набор правок и позиций.
        let mut seed = 0x2545_f491_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..200 {
            let len = 1 + next(40);
            let mut changes = Vec::new();
            let mut at = 0;
            while at < len && changes.len() < 6 {
                let from = at + next(len - at + 1);
                let to = (from + next(4)).min(len);
                let text = (next(3) > 0).then(|| "x".repeat(next(4)));
                changes.push((from, to, text));
                at = to + 1;
            }
            let cs = ChangeSet::from_changes(len, changes);
            // Диапазоны по возрастанию: начало — After, конец — Before.
            let mut positions = Vec::new();
            let mut pos = 0;
            while pos < len {
                let start = pos + next(3);
                let end = (start + 1 + next(5)).min(len + 1);
                if start >= end || end > len {
                    break;
                }
                positions.push((start, Assoc::After));
                positions.push((end, Assoc::Before));
                pos = end;
            }
            let expected: Vec<usize> = positions
                .iter()
                .map(|&(pos, assoc)| cs.map_pos(pos, assoc))
                .collect();
            assert_eq!(cs.map_sorted(positions.iter().copied()), expected, "{cs:?}");
        }
    }

    #[test]
    fn overlapping_changes_are_clipped() {
        let (rope, _) = apply("abcdef", vec![(1, 4, None), (2, 5, None)]);
        assert_eq!(rope, "af");
    }
}
