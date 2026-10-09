//! The commit graph of the log (part E of stage 6.3), as JetBrains draws it: every commit is a node
//! on a lane, its first parent continues on the same lane (the same color), merges and branch points
//! are diagonals between neighbouring rows, and lanes that end close up — the graph stays narrow.
//!
//! The layout goes row by row over commits in `--date-order` (a commit before its parents), so a page
//! of the log continues from the lanes the previous page left open: `extend(a); extend(b)` lays out
//! the same as `extend(a + b)`. Each row keeps its segments in two halves: from the row's top edge to
//! its node's level (lanes move in here — they close up after a lane ended) and from the node down to
//! the bottom edge (to the parents). The bottom edge of a row and the top edge of the next one have
//! the same lanes.

use flux_git::LogCommit;
use gpui::{
    AnyElement, BorderStyle, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Styled, canvas, fill,
    point, px, quad, size,
};

use crate::theme::UiColors;

/// Width of a lane.
pub const LANE_WIDTH: f32 = 14.;
/// A cell is at most this many lanes wide: a history with dozens of parallel branches doesn't push
/// the messages off the screen (the lanes beyond are cut off).
const MAX_LANES: usize = 16;
const NODE_SIZE: f32 = 7.;
const LINE_WIDTH: f32 = 1.5;

/// A line in half a row: from lane `from` to lane `to`, in the color of lane number `color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub from: u32,
    pub to: u32,
    pub color: u32,
}

/// One row of the graph: the commit's node and the lines that pass through the row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphRow {
    /// The lane of the commit's node.
    pub lane: usize,
    /// How many lanes the row occupies (its width in lanes).
    pub lanes: usize,
    /// The node's color.
    pub color: u32,
    /// From the top edge (lane `from`) to the node's level (lane `to`).
    pub top: Vec<Segment>,
    /// From the node's level (lane `from`) to the bottom edge (lane `to`).
    pub bottom: Vec<Segment>,
}

/// A lane open between rows: the commit it waits for and its color.
#[derive(Debug, Clone, PartialEq)]
struct Lane {
    oid: String,
    color: u32,
}

/// The graph of a log: rows in the log's order.
#[derive(Debug, Clone, Default)]
pub struct GraphLayout {
    rows: Vec<GraphRow>,
    /// The lanes at the bottom edge of the last row.
    open: Vec<Lane>,
    /// The next new lane's color.
    next_color: u32,
}

impl GraphLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the next page of commits (newest first, a commit before its parents).
    pub fn extend(&mut self, commits: &[LogCommit]) {
        self.rows.reserve(commits.len());
        for commit in commits {
            let row = self.place(&commit.oid, &commit.parents);
            self.rows.push(row);
        }
    }

    /// Lays out one commit's row and leaves the lanes open below it.
    fn place(&mut self, oid: &str, parents: &[String]) -> GraphRow {
        let open = std::mem::take(&mut self.open);
        let width_top = open.len();
        // At the node's level: the lanes that go on, in order; the first lane waiting for this
        // commit becomes its node, the others end in it.
        let mut middle: Vec<Lane> = Vec::with_capacity(open.len() + 1);
        let mut top = Vec::with_capacity(open.len());
        let mut node: Option<(usize, u32)> = None;
        let mut ending = Vec::new();
        for (index, lane) in open.into_iter().enumerate() {
            if lane.oid == oid {
                match node {
                    None => {
                        node = Some((middle.len(), lane.color));
                        top.push(segment(index, middle.len(), lane.color));
                        middle.push(lane);
                    }
                    Some(_) => ending.push((index, lane.color)),
                }
            } else {
                top.push(segment(index, middle.len(), lane.color));
                middle.push(lane);
            }
        }
        // A branch head nobody waited for: a new lane at the right.
        let (lane, color) = node.unwrap_or_else(|| {
            let color = self.new_color();
            middle.push(Lane {
                oid: oid.to_string(),
                color,
            });
            (middle.len() - 1, color)
        });
        for (index, color) in ending {
            top.push(segment(index, lane, color));
        }
        let width_middle = middle.len();

        // Below the node: the first parent stays on the node's lane (even if another lane waits for
        // it too — the two meet at the parent, as in JetBrains); a merge's other parents join the
        // lanes that wait for them or get new ones at the right, with a diagonal in that lane's
        // color.
        let mut joins: Vec<&String> = Vec::new();
        let mut bottom_lanes: Vec<Option<Lane>> = middle.into_iter().map(Some).collect();
        bottom_lanes[lane] = parents.first().map(|first| Lane {
            oid: first.clone(),
            color,
        });
        for parent in parents.iter().skip(1) {
            let waiting = bottom_lanes
                .iter()
                .flatten()
                .any(|other| other.oid == *parent);
            if !waiting {
                let color = self.new_color();
                bottom_lanes.push(Some(Lane {
                    oid: parent.clone(),
                    color,
                }));
            }
            joins.push(parent);
        }
        // Lanes at the bottom edge: the open ones closed up; where each lane of the node's level
        // goes.
        let mut bottom = Vec::with_capacity(bottom_lanes.len() + joins.len());
        let mut open = Vec::with_capacity(bottom_lanes.len());
        let mut position = vec![usize::MAX; bottom_lanes.len()];
        for (index, entry) in bottom_lanes.into_iter().enumerate() {
            if let Some(entry) = entry {
                position[index] = open.len();
                open.push(entry);
            }
        }
        for (index, &to) in position.iter().enumerate().take(width_middle) {
            if to != usize::MAX {
                let from = index;
                bottom.push(segment(from, to, open[to].color));
            }
        }
        for parent in joins {
            if let Some(to) = open.iter().position(|other| other.oid == *parent) {
                let segment = segment(lane, to, open[to].color);
                if !bottom.contains(&segment) {
                    bottom.push(segment);
                }
            }
        }
        let lanes = width_top.max(width_middle).max(open.len()).max(1);
        self.open = open;
        GraphRow {
            lane,
            lanes,
            color,
            top,
            bottom,
        }
    }

    fn new_color(&mut self) -> u32 {
        let color = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        color
    }

    pub fn row(&self, index: usize) -> Option<&GraphRow> {
        self.rows.get(index)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

fn segment(from: usize, to: usize, color: u32) -> Segment {
    Segment {
        from: from as u32,
        to: to as u32,
        color,
    }
}

/// The width of a row's graph cell, in pixels.
pub fn cell_width(row: &GraphRow) -> f32 {
    row.lanes.clamp(1, MAX_LANES) as f32 * LANE_WIDTH
}

/// Paints a row of the graph: `row_height` tall, [`cell_width`] wide. `head` — the commit is HEAD
/// (its node is drawn hollow, as in JetBrains).
pub fn render_cell(row: &GraphRow, row_height: f32, head: bool, ui: &UiColors) -> AnyElement {
    let width = cell_width(row);
    let row = row.clone();
    let palette = ui.graph_lanes;
    let background = ui.island;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            paint_row(&row, bounds, head, &palette, background, window)
        },
    )
    .w(px(width))
    .h(px(row_height))
    .into_any_element()
}

fn lane_x(bounds: &Bounds<Pixels>, lane: u32) -> Pixels {
    bounds.left() + px(lane as f32 * LANE_WIDTH + LANE_WIDTH / 2.)
}

fn paint_row(
    row: &GraphRow,
    bounds: Bounds<Pixels>,
    head: bool,
    palette: &[Hsla; 8],
    background: Hsla,
    window: &mut gpui::Window,
) {
    let color = |index: u32| palette[index as usize % palette.len()];
    let max = MAX_LANES as u32;
    let (top, middle, bottom) = (
        bounds.top(),
        bounds.top() + bounds.size.height / 2.,
        bounds.bottom(),
    );
    let mut line = |from: u32, to: u32, y0: Pixels, y1: Pixels, color: Hsla| {
        if from >= max && to >= max {
            return;
        }
        let mut path = PathBuilder::stroke(px(LINE_WIDTH));
        path.move_to(point(lane_x(&bounds, from.min(max)), y0));
        path.line_to(point(lane_x(&bounds, to.min(max)), y1));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    for segment in &row.top {
        line(segment.from, segment.to, top, middle, color(segment.color));
    }
    for segment in &row.bottom {
        line(
            segment.from,
            segment.to,
            middle,
            bottom,
            color(segment.color),
        );
    }
    let lane = (row.lane as u32).min(max);
    let center = point(lane_x(&bounds, lane), middle);
    let node = Bounds::new(
        point(center.x - px(NODE_SIZE / 2.), center.y - px(NODE_SIZE / 2.)),
        size(px(NODE_SIZE), px(NODE_SIZE)),
    );
    let node_color = color(row.color);
    if head {
        window.paint_quad(quad(
            node,
            px(NODE_SIZE / 2.),
            background,
            px(1.5),
            node_color,
            BorderStyle::Solid,
        ));
    } else {
        window.paint_quad(fill(node, node_color).corner_radii(px(NODE_SIZE / 2.)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(oid: &str, parents: &[&str]) -> LogCommit {
        LogCommit {
            oid: oid.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            summary: String::new(),
            author: String::new(),
            author_email: String::new(),
            author_time: 0,
            committer: String::new(),
            commit_time: 0,
        }
    }

    fn layout(commits: &[LogCommit]) -> GraphLayout {
        let mut graph = GraphLayout::new();
        graph.extend(commits);
        graph
    }

    fn lanes(graph: &GraphLayout) -> Vec<usize> {
        (0..graph.len())
            .map(|i| graph.row(i).unwrap().lane)
            .collect()
    }

    #[test]
    fn a_linear_history_is_one_lane() {
        let graph = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
        assert_eq!(lanes(&graph), [0, 0, 0]);
        let first = graph.row(0).unwrap();
        assert_eq!(first.lanes, 1);
        assert!(first.top.is_empty(), "nothing above the newest commit");
        assert_eq!(first.bottom, [segment(0, 0, 0)]);
        let root = graph.row(2).unwrap();
        assert_eq!(root.top, [segment(0, 0, 0)]);
        assert!(root.bottom.is_empty());
        // One color all along.
        assert!((0..3).all(|i| graph.row(i).unwrap().color == 0));
    }

    #[test]
    fn a_branch_and_its_merge() {
        // m merges f into the main line: m(a2, f), f(a1), a2(a1), a1.
        let graph = layout(&[
            commit("m", &["a2", "f"]),
            commit("f", &["a1"]),
            commit("a2", &["a1"]),
            commit("a1", &[]),
        ]);
        assert_eq!(lanes(&graph), [0, 1, 0, 0]);
        let merge = graph.row(0).unwrap();
        // The first parent goes straight down, the second gets a new lane and a diagonal.
        assert_eq!(merge.bottom, [segment(0, 0, 0), segment(0, 1, 1)]);
        let branch = graph.row(1).unwrap();
        assert_eq!(branch.color, 1);
        assert_eq!(branch.lanes, 2);
        // f's lane waits for a1, as does the main line: at a1 the branch's lane ends in the node.
        let fork = graph.row(3).unwrap();
        assert!(fork.top.contains(&segment(1, 0, 1)), "{:?}", fork.top);
        assert_eq!(fork.lanes, 2);
        assert!(fork.bottom.is_empty());
    }

    #[test]
    fn several_branch_heads_get_their_own_lanes_and_colors() {
        let graph = layout(&[
            commit("x", &["base"]),
            commit("y", &["base"]),
            commit("z", &["base"]),
            commit("base", &[]),
        ]);
        assert_eq!(lanes(&graph), [0, 1, 2, 0]);
        let colors: Vec<u32> = (0..3).map(|i| graph.row(i).unwrap().color).collect();
        assert_eq!(colors, [0, 1, 2]);
        // y and z: their first parent is already waited for on lane 0 — they keep their own lanes
        // waiting for it until base.
        let base = graph.row(3).unwrap();
        assert_eq!(base.top.len(), 3);
        assert!(base.top.iter().all(|segment| segment.to == 0));
    }

    #[test]
    fn an_octopus_merge_opens_a_lane_per_parent() {
        let graph = layout(&[
            commit("o", &["a", "b", "c"]),
            commit("c", &["r"]),
            commit("b", &["r"]),
            commit("a", &["r"]),
            commit("r", &[]),
        ]);
        let octopus = graph.row(0).unwrap();
        assert_eq!(octopus.bottom.len(), 3);
        assert_eq!(octopus.lanes, 3);
        assert_eq!(lanes(&graph), [0, 2, 1, 0, 0]);
    }

    #[test]
    fn lanes_close_up_after_one_ends() {
        // Two branch heads; the left one ends at its root, the right one moves left.
        let graph = layout(&[
            commit("l", &["l0"]),
            commit("r", &["r0"]),
            commit("l0", &[]),
            commit("r0", &[]),
        ]);
        assert_eq!(lanes(&graph), [0, 1, 0, 0]);
        let after = graph.row(3).unwrap();
        assert_eq!(after.top, [segment(0, 0, 1)]);
        let ended = graph.row(2).unwrap();
        assert_eq!(
            ended.bottom,
            [segment(1, 0, 1)],
            "r's lane moves into the gap"
        );
    }

    #[test]
    fn pages_lay_out_as_one() {
        let commits = [
            commit("m", &["a2", "f"]),
            commit("x", &["m"]),
            commit("f", &["a1"]),
            commit("a2", &["a1"]),
            commit("o", &["a1", "f2", "f3"]),
            commit("f2", &["a1"]),
            commit("f3", &["a0"]),
            commit("a1", &["a0"]),
            commit("a0", &[]),
        ];
        let whole = layout(&commits);
        for split in 0..commits.len() {
            let mut paged = GraphLayout::new();
            paged.extend(&commits[..split]);
            paged.extend(&commits[split..]);
            assert_eq!(paged.rows, whole.rows, "split at {split}");
        }
    }

    #[test]
    fn a_large_history_lays_out_quickly() {
        // 100k commits: a main line with a short branch merged every 10 commits.
        let mut commits = Vec::new();
        let count = 100_000;
        for i in (0..count).rev() {
            let oid = format!("c{i}");
            let parent = (i > 0).then(|| format!("c{}", i - 1));
            if i % 10 == 0 && i > 0 {
                commits.push(LogCommit {
                    parents: vec![parent.clone().unwrap(), format!("b{i}")],
                    ..commit(&oid, &[])
                });
                commits.push(commit(&format!("b{i}"), &[&format!("c{}", i - 1)]));
            } else {
                commits.push(LogCommit {
                    parents: parent.into_iter().collect(),
                    ..commit(&oid, &[])
                });
            }
        }
        let started = std::time::Instant::now();
        let graph = layout(&commits);
        let took = started.elapsed();
        assert_eq!(graph.len(), commits.len());
        assert!((0..graph.len()).all(|i| graph.row(i).unwrap().lanes <= 2));
        // Debug builds are slow; a release build takes a few tens of milliseconds.
        assert!(took.as_secs() < 5, "{took:?}");
    }
}
