//! Where each node of a map stands in space: a force-directed layout, run a
//! step at a time so that a view can show the nodes finding their places.
//!
//! Every node pushes every other away, each edge pulls its ends to a length
//! of its own, shorter for a folder and what it holds than for an import,
//! and a weak pull to the centre keeps apart pieces from drifting off. The
//! largest move a step allows shrinks as the layout cools, so that it
//! settles rather than shakes.

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

/// A place for every node of a map, being found.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    positions: Vec<[f32; 3]>,
    velocities: Vec<[f32; 3]>,
    heat: f32,
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

    /// Moves every node one step along the forces on it.
    pub fn step(&mut self, map: &CodeMap) {
        let n = self.positions.len();
        let mut forces = vec![[0.0_f32; 3]; n];
        for i in 0..n {
            for j in (i + 1)..n {
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
            if a >= n || b >= n {
                continue;
            }
            let d = sub(self.positions[b], self.positions[a]);
            let len = length(d).max(NEAREST);
            let f = scale(d, pull * (len - rest) / len);
            forces[a] = add(forces[a], f);
            forces[b] = sub(forces[b], f);
        }
        for (i, force) in forces.iter().enumerate() {
            let pulled = sub(*force, scale(self.positions[i], GRAVITY));
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
