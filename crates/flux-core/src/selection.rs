//! Курсоры и выделения.

use crate::transaction::{Assoc, ChangeSet};

/// Одно выделение: от `anchor` (где начали) до `head` (где курсор).
/// Пустое, когда `anchor == head` — тогда это просто курсор.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
    /// Колонка, к которой курсор стремится при движении вверх/вниз:
    /// проходя через короткую строку, он не забывает исходную колонку.
    pub goal_column: Option<usize>,
}

impl Range {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal_column: None,
        }
    }

    pub fn point(pos: usize) -> Self {
        Self::new(pos, pos)
    }

    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Переносит курсор в `pos`. С `extend` — растягивает выделение, иначе схлопывает.
    pub fn put_cursor(&self, pos: usize, extend: bool) -> Self {
        Self::new(if extend { self.anchor } else { pos }, pos)
    }

    pub fn with_goal_column(mut self, column: Option<usize>) -> Self {
        self.goal_column = column;
        self
    }

    pub fn map(&self, changes: &ChangeSet) -> Self {
        Self::new(
            changes.map_pos(self.anchor, Assoc::After),
            changes.map_pos(self.head, Assoc::After),
        )
    }

    fn overlaps(&self, other: &Range) -> bool {
        self.from() < other.to() && other.from() < self.to()
            || self.from() == other.from()
            || self.to() == other.to()
    }

    fn merge(&self, other: &Range) -> Self {
        let from = self.from().min(other.from());
        let to = self.to().max(other.to());
        if self.anchor <= self.head {
            Self::new(from, to)
        } else {
            Self::new(to, from)
        }
    }
}

/// Набор выделений (мультикурсор). Всегда непустой, отсортирован
/// и без пересечений. `primary` — главное выделение, за ним следит скролл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range>,
    primary: usize,
}

impl Selection {
    pub fn new(ranges: Vec<Range>, primary: usize) -> Self {
        assert!(!ranges.is_empty(), "selection must contain at least one range");
        let primary = primary.min(ranges.len() - 1);
        Self { ranges, primary }.normalize()
    }

    pub fn single(anchor: usize, head: usize) -> Self {
        Self::from_range(Range::new(anchor, head))
    }

    pub fn point(pos: usize) -> Self {
        Self::from_range(Range::point(pos))
    }

    pub fn from_range(range: Range) -> Self {
        Self {
            ranges: vec![range],
            primary: 0,
        }
    }

    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// Число выделений (курсоров).
    #[allow(
        clippy::len_without_is_empty,
        reason = "выделение никогда не пусто: is_empty всегда был бы false"
    )]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Range> {
        self.ranges.iter()
    }

    /// Применяет `f` к каждому выделению и снова нормализует результат.
    pub fn transform(&self, f: impl FnMut(&Range) -> Range) -> Self {
        Self {
            ranges: self.ranges.iter().map(f).collect(),
            primary: self.primary,
        }
        .normalize()
    }

    pub fn map(&self, changes: &ChangeSet) -> Self {
        self.transform(|range| range.map(changes))
    }

    fn normalize(mut self) -> Self {
        let primary_range = self.ranges[self.primary];
        self.ranges.sort_by_key(|range| range.from());

        let mut merged: Vec<Range> = Vec::with_capacity(self.ranges.len());
        let mut primary = 0;
        for range in self.ranges {
            match merged.last_mut() {
                Some(last) if last.overlaps(&range) => *last = last.merge(&range),
                _ => merged.push(range),
            }
            if range == primary_range {
                primary = merged.len() - 1;
            }
        }
        Self {
            ranges: merged,
            primary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_sorts_and_merges() {
        let sel = Selection::new(
            vec![
                Range::new(10, 12),
                Range::point(3),
                Range::new(11, 15),
                Range::point(3),
            ],
            2,
        );
        assert_eq!(sel.ranges(), &[Range::point(3), Range::new(10, 15)]);
        assert_eq!(sel.primary(), Range::new(10, 15));
    }

    #[test]
    fn adjacent_ranges_stay_separate() {
        let sel = Selection::new(vec![Range::new(0, 2), Range::new(2, 4)], 0);
        assert_eq!(sel.len(), 2);
    }

    #[test]
    fn merge_keeps_direction() {
        let sel = Selection::new(vec![Range::new(5, 2), Range::new(4, 1)], 0);
        assert_eq!(sel.ranges(), &[Range::new(5, 1)]);
    }
}
