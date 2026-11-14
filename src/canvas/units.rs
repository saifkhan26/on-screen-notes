//! Objects taken together: a group as one, and the objects picked by
//! their tags — a group whole only when every member is picked, else
//! each picked member alone. Find (Ctrl+F) lists and the media filter
//! highlights them this way.

use crate::canvas::canvas::Layer;
use std::collections::HashMap;

/// Objects of one layer taken as one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unit {
    pub strokes: Vec<usize>,
    pub shapes: Vec<usize>,
    /// Every member's tags, each once, in the order first met.
    pub tags: Vec<String>,
    /// The group they make up, or 0 for objects taken on their own.
    pub group: u64,
}

impl Unit {
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.strokes.len() + self.shapes.len()
    }

    fn push(&mut self, is_shape: bool, i: usize, tags: &[String]) {
        if is_shape { self.shapes.push(i) } else { self.strokes.push(i) }
        merge(&mut self.tags, tags);
    }
}

/// Tags are the same tag whatever their case.
pub fn same_tag(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn merge(into: &mut Vec<String>, tags: &[String]) {
    for t in tags {
        if !into.iter().any(|have| same_tag(have, t)) {
            into.push(t.clone());
        }
    }
}

/// Every object of `l`: (is a shape, index, group, tags).
fn objects(l: &Layer) -> impl Iterator<Item = (bool, usize, u64, &[String])> {
    l.strokes
        .iter()
        .enumerate()
        .map(|(i, s)| (false, i, s.group, s.tags.as_slice()))
        .chain(l.shapes.iter().enumerate().map(|(i, s)| (true, i, s.group(), s.tags())))
}

/// The objects of `l` whose tags `pick` takes (untagged ones never). A
/// group comes whole when `pick` takes every member — the group was
/// tagged as one — and otherwise only its picked members, each alone. In
/// the order the objects come, a group where its first member does.
pub fn tagged_by(l: &Layer, pick: impl Fn(&[String]) -> bool) -> Vec<Unit> {
    // A group's members, and the picked ones among them.
    struct Group {
        at: usize,
        whole: Unit,
        picked: Vec<Unit>,
    }
    let mut singles: Vec<(usize, Unit)> = Vec::new();
    let mut groups: HashMap<u64, Group> = HashMap::new();
    for (n, (is_shape, i, group, tags)) in objects(l).enumerate() {
        let picked = !tags.is_empty() && pick(tags);
        let mut alone = Unit::default();
        alone.push(is_shape, i, tags);
        if group == 0 {
            if picked {
                singles.push((n, alone));
            }
            continue;
        }
        let g = groups.entry(group).or_insert_with(|| Group { at: n, whole: Unit { group, ..Default::default() }, picked: Vec::new() });
        g.whole.push(is_shape, i, tags);
        if picked {
            g.picked.push(alone);
        }
    }
    let mut out: Vec<(usize, Unit)> = singles;
    for g in groups.into_values() {
        if g.picked.len() == g.whole.len() {
            out.push((g.at, g.whole));
        } else {
            out.extend(g.picked.into_iter().map(|u| (g.at, u)));
        }
    }
    out.sort_by_key(|(at, u)| (*at, u.strokes.first().copied(), u.shapes.first().copied()));
    out.into_iter().map(|(_, u)| u).collect()
}

/// The layer's tagged objects (any tag), as `tagged_by` takes them.
pub fn tagged(l: &Layer) -> Vec<Unit> {
    tagged_by(l, |_| true)
}

/// The layer's groups, each whole, with its members' tags.
pub fn groups(l: &Layer) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    let mut at: HashMap<u64, usize> = HashMap::new();
    for (is_shape, i, group, tags) in objects(l) {
        if group == 0 {
            continue;
        }
        let k = *at.entry(group).or_insert_with(|| {
            out.push(Unit { group, ..Default::default() });
            out.len() - 1
        });
        out[k].push(is_shape, i, tags);
    }
    out
}

/// The canvas box round these objects of `l`; `None` if none is there.
pub fn bounds_of(l: &Layer, strokes: &[usize], shapes: &[usize]) -> Option<[f32; 4]> {
    let strokes = strokes.iter().filter_map(|&i| l.strokes.get(i)?.bounds());
    let shapes = shapes.iter().filter_map(|&i| Some(l.shapes.get(i)?.obb().aabb()));
    strokes.chain(shapes).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::shape::tests_support::rect;

    fn shape(group: u64, tags: &[&str]) -> crate::canvas::shape::Shape {
        let mut s = rect([0.0, 0.0], [10.0, 10.0]);
        s.set_group(group);
        s.set_tags(&tags.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        s
    }

    #[test]
    fn a_group_comes_whole_only_when_every_member_is_picked() {
        let l = Layer {
            shapes: vec![
                shape(0, &["pose"]),        // 0: alone
                shape(5, &[]),              // 1 ┐ group 5: one member tagged
                shape(5, &["Pose", "ref"]), // 2 ┘
                shape(9, &["pose"]),        // 3 ┐ group 9: tagged as one
                shape(9, &["POSE"]),        // 4 ┘
                shape(0, &[]),              // 5: untagged
            ],
            ..Layer::default()
        };
        let t = tagged(&l);
        let got: Vec<(Vec<usize>, u64)> = t.iter().map(|u| (u.shapes.clone(), u.group)).collect();
        // Group 5 gives only its tagged member; group 9 comes whole.
        assert_eq!(got, vec![(vec![0], 0), (vec![2], 0), (vec![3, 4], 9)]);
        assert_eq!(t[1].tags, vec!["Pose".to_string(), "ref".to_string()]);
        assert_eq!(t[2].tags, vec!["pose".to_string()], "one tag, case aside");
        // Picking one tag: group 9 has it throughout; in group 5 only one does.
        let refs = tagged_by(&l, |tags| tags.iter().any(|t| same_tag(t, "ref")));
        assert_eq!(refs.iter().map(|u| u.shapes.clone()).collect::<Vec<_>>(), vec![vec![2]]);
        let g = groups(&l);
        assert_eq!(g.iter().map(|u| (u.group, u.shapes.clone())).collect::<Vec<_>>(), vec![(5, vec![1, 2]), (9, vec![3, 4])]);
        assert!(same_tag("Walk", "wALK"));
    }
}
