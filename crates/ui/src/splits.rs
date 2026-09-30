//! Splits within a terminal tab: a tree whose leaves are terminals and whose
//! nodes place them side by side or one above the other.
//! Data only, so it can be saved and tested without a UI.

use proto::TermId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Side by side.
    Row,
    /// One above the other.
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Tree {
    Leaf(TermId),
    Split { axis: Axis, children: Vec<Tree> },
}

impl Tree {
    pub fn leaves(&self) -> Vec<TermId> {
        match self {
            Tree::Leaf(term) => vec![*term],
            Tree::Split { children, .. } => children.iter().flat_map(Tree::leaves).collect(),
        }
    }

    /// Places `new` next to `target`, along `axis` (after it).
    pub fn split(&mut self, target: TermId, new: TermId, axis: Axis) -> bool {
        match self {
            Tree::Leaf(term) if *term == target => {
                *self = Tree::Split {
                    axis,
                    children: vec![Tree::Leaf(target), Tree::Leaf(new)],
                };
                true
            }
            Tree::Leaf(_) => false,
            Tree::Split { axis: own, children } => {
                // If the parent already goes that way, the new one is just another sibling.
                if *own == axis
                    && let Some(ix) = children.iter().position(|child| *child == Tree::Leaf(target))
                {
                    children.insert(ix + 1, Tree::Leaf(new));
                    return true;
                }
                children.iter_mut().any(|child| child.split(target, new, axis))
            }
        }
    }

    /// Removes `target`; a split left with a single branch is replaced by that
    /// branch. Returns `None` if the tree ends up empty.
    pub fn remove(self, target: TermId) -> Option<Tree> {
        self.retain(&|term| term != target)
    }

    /// Keeps the leaves that satisfy `keep`.
    pub fn retain(self, keep: &dyn Fn(TermId) -> bool) -> Option<Tree> {
        match self {
            Tree::Leaf(term) => keep(term).then_some(Tree::Leaf(term)),
            Tree::Split { axis, children } => {
                let mut children: Vec<Tree> = children.into_iter().filter_map(|child| child.retain(keep)).collect();
                match children.len() {
                    0 => None,
                    1 => children.pop(),
                    _ => Some(Tree::Split { axis, children }),
                }
            }
        }
    }

    /// The terminal closest to `from` in `direction`, assuming the branches of
    /// each split share the space equally.
    pub fn neighbor(&self, from: TermId, direction: Direction) -> Option<TermId> {
        let mut rects = Vec::new();
        self.layout(Rect { x: 0., y: 0., w: 1., h: 1. }, &mut rects);
        let (_, origin) = rects.iter().find(|(term, _)| *term == from)?;
        let (cx, cy) = origin.center();
        rects
            .iter()
            .filter(|(term, rect)| {
                *term != from
                    && match direction {
                        Direction::Left => rect.x + rect.w <= origin.x + 1e-6 && overlaps(rect.y, rect.h, origin.y, origin.h),
                        Direction::Right => rect.x >= origin.x + origin.w - 1e-6 && overlaps(rect.y, rect.h, origin.y, origin.h),
                        Direction::Up => rect.y + rect.h <= origin.y + 1e-6 && overlaps(rect.x, rect.w, origin.x, origin.w),
                        Direction::Down => rect.y >= origin.y + origin.h - 1e-6 && overlaps(rect.x, rect.w, origin.x, origin.w),
                    }
            })
            .min_by(|(_, a), (_, b)| {
                let distance = |rect: &Rect| {
                    let (x, y) = rect.center();
                    (x - cx).powi(2) + (y - cy).powi(2)
                };
                distance(a).total_cmp(&distance(b))
            })
            .map(|(term, _)| *term)
    }

    fn layout(&self, rect: Rect, out: &mut Vec<(TermId, Rect)>) {
        match self {
            Tree::Leaf(term) => out.push((*term, rect)),
            Tree::Split { axis, children } => {
                let n = children.len() as f32;
                for (ix, child) in children.iter().enumerate() {
                    let ix = ix as f32;
                    let part = match axis {
                        Axis::Row => Rect {
                            x: rect.x + rect.w * ix / n,
                            w: rect.w / n,
                            ..rect
                        },
                        Axis::Column => Rect {
                            y: rect.y + rect.h * ix / n,
                            h: rect.h / n,
                            ..rect
                        },
                    };
                    child.layout(part, out);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }
}

fn overlaps(a: f32, a_len: f32, b: f32, b_len: f32) -> bool {
    a < b + b_len - 1e-6 && b < a + a_len - 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(term: TermId) -> Tree {
        Tree::Leaf(term)
    }

    #[test]
    fn splits_nest_and_collapse() {
        let mut tree = leaf(1);
        assert!(tree.split(1, 2, Axis::Row));
        // Another one right of 2: a sibling in the same row, not a new level.
        assert!(tree.split(2, 3, Axis::Row));
        assert_eq!(
            tree,
            Tree::Split {
                axis: Axis::Row,
                children: vec![leaf(1), leaf(2), leaf(3)]
            }
        );
        // Below 3: that leaf becomes a column.
        assert!(tree.split(3, 4, Axis::Column));
        assert_eq!(tree.leaves(), vec![1, 2, 3, 4]);

        let tree = tree.remove(4).unwrap();
        assert_eq!(
            tree,
            Tree::Split {
                axis: Axis::Row,
                children: vec![leaf(1), leaf(2), leaf(3)]
            }
        );
        let tree = tree.remove(1).unwrap().remove(2).unwrap();
        assert_eq!(tree, leaf(3));
        assert_eq!(tree.remove(3), None);
    }

    #[test]
    fn prunes_dead_terminals() {
        let mut tree = leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        let alive = [1, 3];
        let tree = tree.retain(&|term| alive.contains(&term)).unwrap();
        assert_eq!(
            tree,
            Tree::Split {
                axis: Axis::Row,
                children: vec![leaf(1), leaf(3)]
            }
        );
    }

    #[test]
    fn moves_between_neighbors() {
        // 1 | 2
        //   | 3
        let mut tree = leaf(1);
        tree.split(1, 2, Axis::Row);
        tree.split(2, 3, Axis::Column);
        assert_eq!(tree.neighbor(1, Direction::Right), Some(2));
        assert_eq!(tree.neighbor(3, Direction::Left), Some(1));
        assert_eq!(tree.neighbor(2, Direction::Down), Some(3));
        assert_eq!(tree.neighbor(3, Direction::Up), Some(2));
        assert_eq!(tree.neighbor(1, Direction::Up), None);
        assert_eq!(tree.neighbor(2, Direction::Right), None);
    }
}
