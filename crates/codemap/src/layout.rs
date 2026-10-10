//! Where each node of a map stands in space: a force-directed layout, run a
//! step at a time so that a view can show the nodes finding their places.
//!
//! Every node pushes every other away, each edge pulls its ends to a length
//! of its own, shorter for a folder and what it holds than for an import,
//! and a weak pull to the centre keeps apart pieces from drifting off. The
//! largest move a step allows shrinks as the layout cools, so that it
//! settles rather than shakes.
//!
//! Grouped, each group is a galaxy: its nodes are pulled to a point of
//! their own, the points far enough apart for the galaxies not to meet, and
//! only the edges within a galaxy pull. A group within a group is a cluster
//! around its galaxy's point. Narrowed to some nodes, the others stay where
//! they are, and those looked at spread out from where they were.

use crate::grouping::Grouping;
use crate::map::{CodeMap, EdgeKind};

/// How hard two nodes push each other apart.
const REPULSION: f32 = 3.2;
/// Below this distance two nodes push as if this far: no infinite force.
const NEAREST: f32 = 0.05;
/// The length a folder keeps from what it holds.
const HOLD_LENGTH: f32 = 1.4;
/// The length an import keeps between two files.
const IMPORT_LENGTH: f32 = 3.6;
/// How hard an edge pulls toward its length, per kind.
const HOLD_PULL: f32 = 0.12;
const IMPORT_PULL: f32 = 0.012;
/// The pull to the centre, per unit of distance.
const GRAVITY: f32 = 0.012;
/// What is left of a node's speed after each step.
const DAMPING: f32 = 0.82;
/// The largest move of the first step, and of the last ones.
const HOT: f32 = 1.5;
const COLD: f32 = 0.004;
/// How much of the heat each step keeps.
const COOLING: f32 = 0.985;
/// The pull of a node to its group's point, per unit of distance: stronger
/// than the pull to the centre, so that a galaxy holds together.
const ANCHOR_PULL: f32 = 0.04;
/// About how far from its point the farthest node of a galaxy of `n`
/// nodes settles: `SPREAD` times the cube root of `n`, less `SHRINK`, as
/// the edges within it pull it tighter. Where the pull to its point
/// balances its nodes pushing each other, measured on real projects.
const SPREAD: f32 = 4.4;
const SHRINK: f32 = 2.5;
/// How much room is left between galaxies, and between the clusters of a
/// galaxy, over what they would need to touch.
const GAP: f32 = 1.2;
/// The heat a regrouping starts from: enough for the nodes to cross over
/// to their new galaxy, less than a new layout's.
const REGROUP_HEAT: f32 = 1.0;

/// A place for every node of a map, being found.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    positions: Vec<[f32; 3]>,
    velocities: Vec<[f32; 3]>,
    heat: f32,
    /// Whether each node moves: those not looked at stay put.
    moving: Vec<bool>,
    /// The point each node is pulled to, when grouped.
    anchors: Vec<Option<[f32; 3]>>,
    /// The group of each node, so that only the edges within one pull.
    groups: Vec<Option<(usize, Option<usize>)>>,
}

impl Layout {
    /// Every node of `map` packed near the centre, on a small sphere, ready
    /// to spread out: the same map always starts, and ends, the same.
    #[must_use]
    pub fn new(map: &CodeMap) -> Self {
        let n = map.nodes.len().max(1) as f32;
        // A Fibonacci sphere: points spread evenly, with no randomness.
        let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
        let positions = (0..map.nodes.len())
            .map(|i| {
                let y = 1.0 - 2.0 * (i as f32 + 0.5) / n;
                let r = (1.0 - y * y).max(0.0).sqrt();
                let a = golden * i as f32;
                let scale = 0.3;
                [a.cos() * r * scale, y * scale, a.sin() * r * scale]
            })
            .collect();
        Self {
            velocities: vec![[0.0; 3]; map.nodes.len()],
            positions,
            heat: HOT,
            moving: vec![true; map.nodes.len()],
            anchors: vec![None; map.nodes.len()],
            groups: vec![None; map.nodes.len()],
        }
    }

    /// Where each node stands, in the order of the map's nodes.
    #[must_use]
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Whether the layout has cooled down: further steps barely move it.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.heat <= COLD
    }

    /// The middle of the nodes that move, and how far the farthest of them
    /// is from it: what a view frames.
    #[must_use]
    pub fn extent(&self) -> ([f32; 3], f32) {
        let moving: Vec<[f32; 3]> = self
            .positions
            .iter()
            .zip(&self.moving)
            .filter(|(_, m)| **m)
            .map(|(p, _)| *p)
            .collect();
        if moving.is_empty() {
            return ([0.0; 3], 0.0);
        }
        let middle = scale(
            moving.iter().fold([0.0; 3], |a, p| add(a, *p)),
            1.0 / moving.len() as f32,
        );
        let far = moving
            .iter()
            .map(|p| length(sub(*p, middle)))
            .fold(0.0, f32::max);
        (middle, far)
    }

    /// Gathers the nodes as `grouping` says, from where they stand: the
    /// nodes it looks at move to their galaxies around the middle of where
    /// they are now, or around the centre when it looks at them all, and
    /// the others stay put.
    pub fn regroup(&mut self, grouping: &Grouping) {
        let n = self.positions.len();
        self.moving = (0..n).map(|i| grouping.inside(i)).collect();
        self.groups = (0..n).map(|i| grouping.of(i)).collect();
        let all = self.moving.iter().all(|m| *m);
        let origin = if all { [0.0; 3] } else { self.extent().0 };
        // How many nodes each galaxy, and each cluster, holds.
        let mut sizes: Vec<(usize, Vec<usize>)> = grouping
            .groups
            .iter()
            .enumerate()
            .map(|(g, _)| (0, vec![0; grouping.subgroups.get(g).map_or(0, Vec::len)]))
            .collect();
        for (top, sub) in self.groups.iter().flatten() {
            if let Some(size) = sizes.get_mut(*top) {
                size.0 += 1;
                if let Some(s) = sub.and_then(|s| size.1.get_mut(s)) {
                    *s += 1;
                }
            }
        }
        let width = |nodes: usize| (SPREAD * (nodes.max(1) as f32).cbrt() - SHRINK).max(1.2);
        // Each cluster's point around its galaxy's, and how wide the
        // galaxy is with its clusters around.
        let galaxies: Vec<(f32, Vec<[f32; 3]>)> = sizes
            .iter()
            .map(|(nodes, clusters)| {
                let widths: Vec<f32> = clusters.iter().map(|c| width(*c)).collect();
                let ring = spacing(&widths);
                let points = sphere(clusters.len(), ring);
                let wide = widths.iter().fold(0.0, |a: f32, w| a.max(*w)) + ring;
                (wide.max(width(*nodes)), points)
            })
            .collect();
        let widths: Vec<f32> = galaxies.iter().map(|g| g.0).collect();
        let centres = sphere(galaxies.len(), spacing(&widths));
        self.anchors = self
            .groups
            .iter()
            .zip(&self.moving)
            .map(|(group, moving)| {
                let (top, sub) = group.filter(|_| *moving)?;
                let centre = add(origin, *centres.get(top)?);
                let around = sub
                    .and_then(|s| galaxies.get(top)?.1.get(s).copied())
                    .unwrap_or([0.0; 3]);
                Some(add(centre, around))
            })
            .collect();
        for (v, moving) in self.velocities.iter_mut().zip(&self.moving) {
            if !moving {
                *v = [0.0; 3];
            }
        }
        self.heat = self.heat.max(REGROUP_HEAT);
    }

    /// Moves every node one step along the forces on it.
    pub fn step(&mut self, map: &CodeMap) {
        let n = self.positions.len();
        let mut forces = vec![[0.0_f32; 3]; n];
        let moving: Vec<usize> = (0..n).filter(|&i| self.moving[i]).collect();
        for (k, &i) in moving.iter().enumerate() {
            for &j in &moving[k + 1..] {
                let d = sub(self.positions[i], self.positions[j]);
                let len = length(d).max(NEAREST);
                let push = REPULSION / (len * len);
                let f = scale(d, push / len);
                forces[i] = add(forces[i], f);
                forces[j] = sub(forces[j], f);
            }
        }
        for edge in &map.edges {
            let (rest, pull) = match edge.kind {
                EdgeKind::Contains => (HOLD_LENGTH, HOLD_PULL),
                EdgeKind::Imports | EdgeKind::Calls => (IMPORT_LENGTH, IMPORT_PULL),
            };
            let (a, b) = (edge.from, edge.to);
            if a >= n || b >= n || !self.moving[a] || !self.moving[b] {
                continue;
            }
            // Between galaxies, an edge is drawn but does not pull: it
            // would drag them into each other, or a folder above them all
            // into one of them.
            let anchored = self.anchors[a].is_some() || self.anchors[b].is_some();
            if self.groups[a] != self.groups[b] && anchored {
                continue;
            }
            let d = sub(self.positions[b], self.positions[a]);
            let len = length(d).max(NEAREST);
            let f = scale(d, pull * (len - rest) / len);
            forces[a] = add(forces[a], f);
            forces[b] = sub(forces[b], f);
        }
        for &i in &moving {
            let pulled = match self.anchors[i] {
                Some(anchor) => add(
                    forces[i],
                    scale(sub(anchor, self.positions[i]), ANCHOR_PULL),
                ),
                None => sub(forces[i], scale(self.positions[i], GRAVITY)),
            };
            let mut v = scale(add(self.velocities[i], pulled), DAMPING);
            let speed = length(v);
            if speed > self.heat {
                v = scale(v, self.heat / speed);
            }
            self.velocities[i] = v;
            self.positions[i] = add(self.positions[i], v);
        }
        self.heat = (self.heat * COOLING).max(COLD);
    }
}

/// How far from their middle to put things `widths` wide, for them not to
/// meet: none when there is only one, which sits in the middle.
fn spacing(widths: &[f32]) -> f32 {
    if widths.len() < 2 {
        return 0.0;
    }
    let squares: f32 = widths.iter().map(|w| w * w).sum();
    let widest = widths.iter().fold(0.0, |a: f32, w| a.max(*w));
    // Two must not meet when on either side; many spread over a sphere
    // whose surface holds them all.
    GAP * (0.65 * squares.sqrt()).max(1.05 * widest)
}

/// `count` points spread evenly over a sphere `radius` wide, the same each
/// time.
fn sphere(count: usize, radius: f32) -> Vec<[f32; 3]> {
    if count == 1 {
        return vec![[0.0; 3]];
    }
    let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    (0..count)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / count as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = golden * i as f32;
            [a.cos() * r * radius, y * radius, a.sin() * r * radius]
        })
        .collect()
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{Edge, Node, NodeKind};

    /// A root holding two folders, each holding three files; one import
    /// across.
    fn tree() -> CodeMap {
        let mut map = CodeMap::default();
        let mut push = |path: &str, depth| {
            map.nodes.push(Node {
                path: path.into(),
                kind: NodeKind::Folder,
                depth,
            });
            map.nodes.len() - 1
        };
        let root = push("", 0);
        let mut edges = Vec::new();
        for folder in ["a", "b"] {
            let f = push(folder, 1);
            edges.push((root, f));
            for file in 0..3 {
                let x = push(&format!("{folder}/{file}"), 2);
                edges.push((f, x));
            }
        }
        map.edges = edges
            .into_iter()
            .map(|(from, to)| Edge {
                from,
                to,
                kind: EdgeKind::Contains,
            })
            .collect();
        map.edges.push(Edge {
            from: 2,
            to: 6,
            kind: EdgeKind::Imports,
        });
        map
    }

    fn settle(map: &CodeMap) -> Layout {
        let mut layout = Layout::new(map);
        for _ in 0..2_000 {
            layout.step(map);
        }
        layout
    }

    fn distance(layout: &Layout, a: usize, b: usize) -> f32 {
        length(sub(layout.positions()[a], layout.positions()[b]))
    }

    #[test]
    fn the_same_map_settles_the_same_and_stays_finite() {
        let map = tree();
        let one = settle(&map);
        assert_eq!(one, settle(&map));
        assert!(one.settled());
        assert!(one.positions().iter().flatten().all(|v| v.is_finite()));
    }

    /// Two packages of twelve files each, and a document at the root.
    fn packages() -> (tempfile::TempDir, CodeMap) {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        };
        write("README.md");
        for package in ["one", "two"] {
            write(&format!("{package}/pyproject.toml"));
            for file in 0..11 {
                write(&format!("{package}/{package}/m{file}.py"));
            }
        }
        let map = crate::map(dir.path(), 100);
        (dir, map)
    }

    fn middle(layout: &Layout, nodes: &[usize]) -> [f32; 3] {
        let sum = nodes
            .iter()
            .fold([0.0; 3], |a, n| add(a, layout.positions()[*n]));
        scale(sum, 1.0 / nodes.len() as f32)
    }

    #[test]
    fn grouped_each_galaxy_keeps_its_files_apart_from_the_others() {
        let (_dir, map) = packages();
        let grouping = crate::group(&map, &[], &[crate::Criterion::Module]);
        let mut layout = Layout::new(&map);
        layout.regroup(&grouping);
        for _ in 0..2_000 {
            layout.step(&map);
        }
        assert!(layout.settled());
        let members = |g: usize| -> Vec<usize> {
            (0..map.nodes.len())
                .filter(|&n| {
                    grouping.of(n).map(|o| o.0) == Some(g)
                        && matches!(map.nodes[n].kind, crate::NodeKind::File { .. })
                })
                .collect()
        };
        let (one, two) = (members(0), members(1));
        let (m1, m2) = (middle(&layout, &one), middle(&layout, &two));
        let apart = length(sub(m1, m2));
        let widest = one
            .iter()
            .map(|n| length(sub(layout.positions()[*n], m1)))
            .chain(two.iter().map(|n| length(sub(layout.positions()[*n], m2))))
            .fold(0.0, f32::max);
        // Each galaxy's farthest file is nearer its own middle than half
        // the way to the other's: they do not meet.
        assert!(widest < apart / 2.0, "widest {widest}, apart {apart}");
    }

    #[test]
    fn narrowed_what_is_not_looked_at_stays_put() {
        let (_dir, map) = packages();
        let mut layout = Layout::new(&map);
        layout.regroup(&crate::group(&map, &[], &[crate::Criterion::Module]));
        for _ in 0..500 {
            layout.step(&map);
        }
        let before = layout.positions().to_vec();
        let focus = [crate::Key::Module("one".into())];
        let grouping = crate::group(&map, &focus, &[crate::Criterion::Language]);
        layout.regroup(&grouping);
        for _ in 0..200 {
            layout.step(&map);
        }
        for (node, was) in before.iter().enumerate() {
            if !grouping.inside(node) {
                assert_eq!(layout.positions()[node], *was, "{}", map.nodes[node].path);
            }
        }
        let file = map.find("one/one/m0.py").unwrap();
        assert!(grouping.inside(file));
        assert_ne!(layout.positions()[file], before[file]);
    }

    #[test]
    fn a_folder_keeps_its_files_closer_than_the_other_folders_files() {
        let map = tree();
        let layout = settle(&map);
        // Folder a is node 1, its files 2..5; folder b's files are 6..9.
        let own = (2..5).map(|f| distance(&layout, 1, f)).fold(0.0, f32::max);
        let other = (7..9)
            .map(|f| distance(&layout, 1, f))
            .fold(f32::MAX, f32::min);
        assert!(own < other, "own {own}, other {other}");
        // Nothing sits on anything else.
        for i in 0..map.nodes.len() {
            for j in (i + 1)..map.nodes.len() {
                assert!(distance(&layout, i, j) > 0.3);
            }
        }
    }
}
