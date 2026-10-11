//! Every environment at once, in the universe's window ("Deployments",
//! beside "Group by"): each environment a galaxy spread over a sphere, in
//! each a cluster of stars per namespace, each service a star, the objects
//! it deploys small planets on slow orbits around it. The camera drifts,
//! turns on a drag and comes closer with the wheel, as over the code.
//!
//! A click on an environment's name goes into it; a click on a service
//! lays it out flat, on its board. The environments the settings name are
//! rendered one after the other, and the sky fills as they are.

use std::collections::HashSet;
use std::f32::consts::TAU;
use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::egui::{self, Align, Color32, Id, Layout, RichText, Stroke, Ui, UiBuilder};
use universe_codemap::{Fleet, LayerKind};
use universe_ui::App;

use crate::infra::Infra;
use crate::plan::Plan;
use crate::service_board::{self, Board};
use crate::theme::{self, DIM, EDGE, RAISED, TEXT, YELLOW};
use crate::universe::{Assets3d, CodeStar, Universe, UniverseCamera};

/// The room each star takes in its cluster: wide enough for its planets'
/// orbits never to cross a neighbour's.
const SPACING: f32 = 5.0;
/// The farthest a planet orbits from its star's centre.
const REACH: f32 = 2.3;
/// The room between two namespaces' clusters, and between two galaxies.
const CLUSTER_GAP: f32 = 4.0;
const GALAXY_GAP: f32 = 14.0;

/// How bright a star is, lit and faded, in HDR units: above 1, it blooms.
const LIT: f32 = 3.2;
const FADED: f32 = 0.14;

/// The nearest the camera comes, and the farthest it frames from.
const NEAREST: f32 = 4.0;
const FARTHEST: f32 = 700.0;

/// The colours of the first environments; past them, hues spread by the
/// golden angle.
const PALETTE: [[f32; 3]; 8] = [
    [0.3, 0.6, 1.0],
    [1.0, 0.45, 0.2],
    [0.3, 1.0, 0.5],
    [1.0, 0.3, 0.7],
    [1.0, 0.85, 0.25],
    [0.2, 0.95, 0.9],
    [0.65, 0.4, 1.0],
    [0.75, 1.0, 0.25],
];

/// What colours the stars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Colouring {
    /// Stars by environment, planets by kind.
    #[default]
    Kinds,
    /// The same image the same colour in every environment.
    Images,
    /// The deepest layer that set a star's values.
    Values,
}

/// A namespace's cluster in its galaxy.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cluster {
    pub(crate) namespace: String,
    pub(crate) centre: Vec3,
    pub(crate) radius: f32,
}

/// Where a galaxy and its stars are.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    pub(crate) centre: Vec3,
    pub(crate) radius: f32,
    pub(crate) clusters: Vec<Cluster>,
    /// Each star's centre and size, in the galaxy's order.
    pub(crate) stars: Vec<(Vec3, f32)>,
}

/// Where everything is: a place for each galaxy shown, `None` for those
/// hidden.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Scene {
    pub(crate) galaxies: Vec<Option<Placed>>,
    /// How far from the middle the farthest star is.
    pub(crate) extent: f32,
}

/// `n` directions spread evenly over a sphere.
fn sphere(n: usize) -> Vec<Vec3> {
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = i as f32 * 2.399_963;
            Vec3::new(a.cos() * r, y, a.sin() * r)
        })
        .collect()
}

/// The place of the `i`th of `n` points filling a ball of `radius`
/// evenly: as many at each depth as its shell holds, their directions
/// spread apart by the golden ratio.
fn in_ball(i: usize, n: usize, radius: f32) -> Vec3 {
    let depth = radius * ((i as f32 + 0.5) / n as f32).cbrt();
    let y = 1.0 - 2.0 * (i as f32 * 0.618_034 + 0.5).fract();
    let r = (1.0 - y * y).max(0.0).sqrt();
    let a = i as f32 * 2.399_963;
    Vec3::new(a.cos() * r, y, a.sin() * r) * depth
}

/// How large a star is, by how many objects it deploys.
fn star_size(pieces: usize) -> f32 {
    0.35 + 0.08 * (pieces as f32).sqrt().min(6.0)
}

/// Lays out `fleet`, the galaxies `shown` only: those hidden take no room.
pub(crate) fn layout(fleet: &Fleet, shown: &[bool]) -> Scene {
    // Each galaxy laid out around its own centre first.
    let mut galaxies: Vec<Option<Placed>> = fleet
        .galaxies
        .iter()
        .enumerate()
        .map(|(g, galaxy)| {
            if !shown.get(g).copied().unwrap_or(false) {
                return None;
            }
            let mut namespaces: Vec<(String, Vec<usize>)> = Vec::new();
            for (s, star) in galaxy.stars.iter().enumerate() {
                let ns = star
                    .namespace
                    .clone()
                    .unwrap_or_else(|| "default".to_owned());
                match namespaces.iter_mut().find(|(n, _)| *n == ns) {
                    Some((_, members)) => members.push(s),
                    None => namespaces.push((ns, vec![s])),
                }
            }
            // A ball as large as its stars need, filled evenly.
            let balls: Vec<f32> = namespaces
                .iter()
                .map(|(_, members)| {
                    let n = members.len() as f32;
                    SPACING * (3.0 * n / (4.0 * std::f32::consts::PI)).cbrt()
                })
                .collect();
            let radii: Vec<f32> = balls.iter().map(|b| b + REACH).collect();
            let widest = radii.iter().copied().fold(0.0_f32, f32::max);
            // The clusters on a ring around the centre, each given an arc
            // as wide as it is.
            let ring = if namespaces.len() <= 1 {
                0.0
            } else {
                let around: f32 = radii.iter().map(|r| 2.0 * r + CLUSTER_GAP).sum();
                (around / TAU).max(widest + CLUSTER_GAP)
            };
            let around: f32 = radii.iter().map(|r| 2.0 * r + CLUSTER_GAP).sum();
            let mut angle = 0.0_f32;
            let mut stars = vec![(Vec3::ZERO, 0.0); galaxy.stars.len()];
            let mut clusters = Vec::new();
            for (k, (ns, members)) in namespaces.iter().enumerate() {
                let share = (2.0 * radii[k] + CLUSTER_GAP) / around.max(1.0) * TAU;
                let middle = angle + share / 2.0;
                angle += share;
                // A slight tilt, so that a galaxy reads as one in space.
                let centre = Vec3::new(
                    middle.cos() * ring,
                    (middle * 2.0).sin() * ring * 0.12,
                    middle.sin() * ring,
                );
                let n = members.len();
                for (i, &s) in members.iter().enumerate() {
                    let at = if n == 1 {
                        centre
                    } else {
                        centre + in_ball(i, n, balls[k])
                    };
                    stars[s] = (at, star_size(galaxy.stars[s].pieces.len()));
                }
                clusters.push(Cluster {
                    namespace: ns.clone(),
                    centre,
                    radius: radii[k],
                });
            }
            let radius = clusters
                .iter()
                .map(|c: &Cluster| c.centre.length() + c.radius)
                .fold(0.0_f32, f32::max);
            Some(Placed {
                centre: Vec3::ZERO,
                radius,
                clusters,
                stars,
            })
        })
        .collect();
    // Then the galaxies spread over a sphere, wide enough that the two
    // nearest never touch; one alone in the middle.
    let visible: Vec<usize> = (0..galaxies.len())
        .filter(|g| galaxies[*g].is_some())
        .collect();
    let widest = galaxies
        .iter()
        .flatten()
        .map(|p| p.radius)
        .fold(0.0_f32, f32::max);
    let directions = sphere(visible.len());
    let nearest = directions
        .iter()
        .enumerate()
        .flat_map(|(i, a)| directions[i + 1..].iter().map(move |b| a.distance(*b)))
        .fold(f32::INFINITY, f32::min);
    let spread = if visible.len() <= 1 {
        0.0
    } else {
        (2.0 * widest + GALAXY_GAP) / nearest.max(0.05)
    };
    let mut extent = 0.0_f32;
    for (k, &g) in visible.iter().enumerate() {
        let Some(placed) = &mut galaxies[g] else {
            continue;
        };
        let centre = if visible.len() == 1 {
            Vec3::ZERO
        } else {
            directions[k] * spread
        };
        placed.centre = centre;
        for cluster in &mut placed.clusters {
            cluster.centre += centre;
        }
        for (at, _) in &mut placed.stars {
            *at += centre;
        }
        extent = extent.max(centre.length() + placed.radius);
    }
    Scene { galaxies, extent }
}

/// A hue, at full saturation, as linear RGB.
fn hue(degrees: f32) -> [f32; 3] {
    let c = Color::hsv(degrees.rem_euclid(360.0), 0.72, 1.0).to_linear();
    [c.red, c.green, c.blue]
}

/// The colour of the galaxy at `index`.
pub(crate) fn galaxy_colour(index: usize) -> [f32; 3] {
    PALETTE
        .get(index)
        .copied()
        .unwrap_or_else(|| hue(20.0 + (index - PALETTE.len()) as f32 * 137.507_76))
}

/// FNV-1a: the same colour for the same image, from one run to the next.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The colour of an image, tag and all: the same image the same colour in
/// every environment, another version another colour.
pub(crate) fn image_colour(image: &str) -> [f32; 3] {
    let hash = fnv(image);
    // The golden angle over the hash spreads near hashes far apart.
    let degrees = (hash % 3600) as f32 * 137.507_76;
    let [r, g, b] = hue(degrees);
    // Two images of the same hue told apart by their brightness.
    let light = 0.7 + 0.3 * ((hash >> 20) % 4) as f32 / 3.0;
    [r * light, g * light, b * light]
}

/// The colour of a layer's kind.
pub(crate) fn layer_colour(kind: LayerKind) -> [f32; 3] {
    match kind {
        LayerKind::Chart => [0.45, 0.5, 0.65],
        LayerKind::Base => [0.2, 0.85, 0.95],
        LayerKind::Environment => [0.35, 1.0, 0.45],
        LayerKind::Patch => [1.0, 0.5, 0.15],
    }
}

/// The colour of a Kubernetes kind, for planets and cards.
pub(crate) fn kind_colour(kind: &str) -> [f32; 3] {
    match kind {
        "Ingress" | "HTTPRoute" | "Gateway" | "IngressRoute" => [1.0, 0.8, 0.25],
        "Service" => [0.35, 0.6, 1.0],
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Pod" => [1.0, 0.5, 0.2],
        "Job" | "CronJob" => [1.0, 0.65, 0.4],
        "HorizontalPodAutoscaler" | "PodDisruptionBudget" | "NetworkPolicy" => [0.2, 0.9, 0.85],
        "ConfigMap" => [0.4, 0.95, 0.5],
        "Secret" | "SealedSecret" | "ExternalSecret" => [0.95, 0.35, 0.75],
        "ServiceAccount" | "Role" | "RoleBinding" | "ClusterRole" | "ClusterRoleBinding" => {
            [0.65, 0.45, 1.0]
        }
        "PersistentVolumeClaim" => [0.75, 0.75, 0.4],
        _ => [0.6, 0.65, 0.75],
    }
}

/// A colour of the scene as the panels draw it.
pub(crate) fn egui_colour([r, g, b]: [f32; 3]) -> Color32 {
    let c = Color::linear_rgb(r, g, b).to_srgba();
    Color32::from_rgb(
        (c.red.clamp(0.0, 1.0) * 255.0) as u8,
        (c.green.clamp(0.0, 1.0) * 255.0) as u8,
        (c.blue.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// Marks what belongs to the fleet's scene.
#[derive(Component)]
pub(crate) struct InFleet;

/// A planet: its star, and its orbit.
#[derive(Component)]
pub(crate) struct Planet {
    galaxy: usize,
    star: usize,
    radius: f32,
    phase: f32,
    speed: f32,
    tilt: Quat,
}

/// The fleet as the panels and the systems share it.
#[derive(Resource)]
pub(crate) struct FleetView {
    /// Whether the universe's window shows it, rather than the code.
    pub(crate) active: bool,
    fleet: Option<Arc<Fleet>>,
    scene: Scene,
    /// The scene must be spawned anew.
    respawn: bool,
    /// The stars must take their colours again.
    recolour: bool,
    pub(crate) colouring: Colouring,
    /// The galaxy gone into.
    entered: Option<usize>,
    /// The environments the settings do not name that are shown, by name.
    others: HashSet<String>,
    /// What the stars were last searched for, and whether each is found:
    /// `None` until searched, to search again.
    query: Option<String>,
    found: Vec<Vec<bool>>,
    /// The service laid out flat, instead of the space.
    pub(crate) board: Option<Board>,
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
    target_distance: f32,
    /// Whether the wheel set the distance since the view last framed.
    zoomed: bool,
    /// What the panels report of the mouse, for the next frame.
    pub(crate) drag: Vec2,
    pub(crate) zoom: f32,
    /// How much of the window the panels cover on the right.
    pub(crate) covered: f32,
    /// Where each star, galaxy name and namespace name is on screen.
    /// Each star's centre on screen, and its radius there.
    stars_on_screen: Vec<Vec<Option<(Vec2, f32)>>>,
    galaxies_on_screen: Vec<Option<Vec2>>,
    clusters_on_screen: Vec<Option<Vec2>>,
    hovered: Option<(usize, usize)>,
    /// Each star's entity and material.
    stars: Vec<(usize, usize, Handle<StandardMaterial>)>,
    spawned: bool,
}

impl Default for FleetView {
    fn default() -> Self {
        Self {
            active: false,
            fleet: None,
            scene: Scene::default(),
            respawn: false,
            recolour: false,
            colouring: Colouring::default(),
            entered: None,
            others: HashSet::new(),
            query: None,
            found: Vec::new(),
            board: None,
            focus: Vec3::ZERO,
            yaw: 0.6,
            pitch: 0.4,
            distance: 200.0,
            target_distance: 200.0,
            zoomed: false,
            drag: Vec2::ZERO,
            zoom: 0.0,
            covered: 0.0,
            stars_on_screen: Vec::new(),
            galaxies_on_screen: Vec::new(),
            clusters_on_screen: Vec::new(),
            hovered: None,
            stars: Vec::new(),
            spawned: false,
        }
    }
}

impl FleetView {
    /// Takes `fleet`, when another was assembled, and lays it out again.
    fn update(&mut self, fleet: Arc<Fleet>) {
        if self.fleet.as_ref().is_some_and(|f| Arc::ptr_eq(f, &fleet)) {
            return;
        }
        // The galaxy gone into is found again by its name.
        let entered = self
            .entered
            .and_then(|g| self.fleet.as_ref()?.galaxies.get(g).map(|g| g.name.clone()));
        self.entered = entered.and_then(|name| fleet.galaxies.iter().position(|g| g.name == name));
        if let Some(board) = &mut self.board
            && !board.find_again(self.fleet.as_deref(), &fleet)
        {
            self.board = None;
        }
        self.fleet = Some(fleet);
        self.query = None;
        self.relayout();
    }

    /// Whether the galaxy at `g` is shown.
    fn shown(&self, g: usize) -> bool {
        self.fleet.as_ref().is_some_and(|f| {
            f.galaxies
                .get(g)
                .is_some_and(|galaxy| galaxy.named || self.others.contains(&galaxy.name))
        })
    }

    fn relayout(&mut self) {
        let Some(fleet) = &self.fleet else {
            return;
        };
        let shown: Vec<bool> = (0..fleet.galaxies.len()).map(|g| self.shown(g)).collect();
        self.scene = layout(fleet, &shown);
        if self.entered.is_some_and(|g| !shown[g]) {
            self.entered = None;
        }
        self.respawn = true;
        self.recolour = true;
        self.zoomed = false;
        self.hovered = None;
    }

    /// Searches the stars' names for `query`, unless they already are.
    fn search(&mut self, query: &str) {
        if self.query.as_deref() == Some(query) {
            return;
        }
        self.query = Some(query.to_owned());
        self.found = self.fleet.as_ref().map_or_else(Vec::new, |fleet| {
            fleet
                .galaxies
                .iter()
                .map(|g| {
                    g.stars
                        .iter()
                        .map(|s| {
                            query.is_empty()
                                || universe_ui::search_matches(query, [s.name.as_str()])
                        })
                        .collect()
                })
                .collect()
        });
        self.recolour = true;
    }

    /// Whether a star is lit: in the galaxy gone into, if any, and found.
    fn lit(&self, g: usize, s: usize) -> bool {
        self.entered.is_none_or(|e| e == g)
            && self
                .found
                .get(g)
                .and_then(|f| f.get(s))
                .copied()
                .unwrap_or(true)
    }

    /// A star's colour, as the colouring says.
    fn star_colour(&self, g: usize, s: usize) -> [f32; 3] {
        let Some(star) = self
            .fleet
            .as_ref()
            .and_then(|f| f.galaxies.get(g)?.stars.get(s))
        else {
            return [0.5, 0.5, 0.5];
        };
        match self.colouring {
            Colouring::Kinds => galaxy_colour(g),
            Colouring::Images => star
                .images
                .first()
                .map_or([0.4, 0.42, 0.5], |i| image_colour(i)),
            Colouring::Values => star.deepest().map_or([0.4, 0.42, 0.5], layer_colour),
        }
    }

    /// Whether the window must draw continuously.
    pub(crate) fn moving(&self) -> bool {
        self.active && self.board.is_none()
    }
}

/// Spawns the scene when laid out, takes it away when the fleet is left,
/// and hides the code's stars meanwhile.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn(
    mut commands: Commands,
    mut view: ResMut<FleetView>,
    universe: Res<Universe>,
    assets: Option<Res<Assets3d>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, With<InFleet>>,
    mut code: Query<&mut Visibility, (With<CodeStar>, Without<InFleet>)>,
) {
    let wanted = view.active && universe.shown;
    let code_visible = universe.shown && !view.active;
    for mut visibility in &mut code {
        let want = if code_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != want {
            *visibility = want;
        }
    }
    if (!wanted || view.respawn) && view.spawned {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
        view.stars.clear();
        view.spawned = false;
    }
    if !wanted || view.spawned {
        return;
    }
    let (Some(assets), Some(fleet)) = (assets, view.fleet.clone()) else {
        return;
    };
    view.respawn = false;
    view.spawned = true;
    view.recolour = true;
    let mut kinds: Vec<(String, Handle<StandardMaterial>)> = Vec::new();
    let scene = view.scene.clone();
    let mut stars = Vec::new();
    for (g, placed) in scene.galaxies.iter().enumerate() {
        let Some(placed) = placed else { continue };
        for (s, (at, size)) in placed.stars.iter().enumerate() {
            let material = materials.add(StandardMaterial {
                base_color: Color::BLACK,
                emissive: LinearRgba::rgb(LIT, LIT, LIT),
                ..default()
            });
            commands.spawn((
                Mesh3d(assets.sphere.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(*at).with_scale(Vec3::splat(*size)),
                InFleet,
            ));
            stars.push((g, s, material));
            let pieces = &fleet.galaxies[g].stars[s].pieces;
            for (j, piece) in pieces.iter().enumerate() {
                let material = match kinds.iter().find(|(k, _)| *k == piece.kind) {
                    Some((_, m)) => m.clone(),
                    None => {
                        let [r, gr, b] = kind_colour(&piece.kind);
                        let m = materials.add(StandardMaterial {
                            base_color: Color::BLACK,
                            emissive: LinearRgba::rgb(r * 1.4, gr * 1.4, b * 1.4),
                            ..default()
                        });
                        kinds.push((piece.kind.clone(), m.clone()));
                        m
                    }
                };
                // Rings outwards, each planet at its own angle and tilt.
                let radius = size * 1.6 + 0.18 * (j % 6) as f32 + 0.05 * (j / 6) as f32;
                let tilt = Quat::from_rotation_y(j as f32 * 1.1)
                    * Quat::from_rotation_x(0.25 + 0.35 * ((j * 7) % 5) as f32 / 4.0);
                commands.spawn((
                    Mesh3d(assets.sphere.clone()),
                    MeshMaterial3d(material),
                    Transform::from_translation(*at).with_scale(Vec3::splat(0.12)),
                    Planet {
                        galaxy: g,
                        star: s,
                        radius: radius.min(REACH),
                        phase: j as f32 * 2.399_963,
                        speed: 0.35 / radius.max(0.5),
                        tilt,
                    },
                    InFleet,
                ));
            }
        }
    }
    view.stars = stars;
}

/// What the camera's query reads and moves.
type CameraParts = (
    &'static mut Transform,
    &'static Camera,
    &'static GlobalTransform,
);

/// Each frame while shown: the planets on their orbits, the colours, the
/// camera, and where the names go.
pub(crate) fn animate(
    mut view: ResMut<FleetView>,
    universe: Res<Universe>,
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut planets: Query<(&mut Transform, &mut Visibility, &Planet), Without<UniverseCamera>>,
    mut camera: Query<CameraParts, (With<UniverseCamera>, Without<Planet>)>,
) {
    if !(view.active && universe.shown) || view.board.is_some() || !view.spawned {
        return;
    }
    let dt = time.delta_secs().min(0.1);
    let t = time.elapsed_secs_wrapped();
    let scene = view.scene.clone();
    if std::mem::take(&mut view.recolour) {
        let tints: Vec<(Handle<StandardMaterial>, LinearRgba)> = view
            .stars
            .iter()
            .map(|(g, s, m)| {
                let [r, gr, b] = view.star_colour(*g, *s);
                let k = if view.lit(*g, *s) { LIT } else { FADED };
                (m.clone(), LinearRgba::rgb(r * k, gr * k, b * k))
            })
            .collect();
        for (handle, emissive) in tints {
            if let Some(mut material) = materials.get_mut(&handle) {
                material.emissive = emissive;
            }
        }
    }
    for (mut transform, mut visibility, planet) in &mut planets {
        let lit = view.lit(planet.galaxy, planet.star);
        let want = if lit {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != want {
            *visibility = want;
        }
        if !lit {
            continue;
        }
        let Some((centre, _)) = scene
            .galaxies
            .get(planet.galaxy)
            .and_then(Option::as_ref)
            .and_then(|p| p.stars.get(planet.star))
        else {
            continue;
        };
        let angle = planet.phase + t * planet.speed;
        let orbit = planet.tilt * Vec3::new(angle.cos(), 0.0, angle.sin()) * planet.radius;
        transform.translation = *centre + orbit;
    }

    // The camera: drifting, turned by a drag, gliding to frame the whole
    // or the galaxy gone into.
    let drag = std::mem::take(&mut view.drag);
    let zoom = std::mem::take(&mut view.zoom);
    // Still under the pointer: a star drifting away is hard to click.
    if drag == Vec2::ZERO && view.hovered.is_none() {
        view.yaw += 0.05 * dt;
    }
    view.yaw -= drag.x * 0.006;
    view.pitch = (view.pitch + drag.y * 0.006).clamp(-1.4, 1.4);
    let goal = match view
        .entered
        .and_then(|g| scene.galaxies.get(g).and_then(Option::as_ref))
    {
        Some(placed) => (placed.centre, (placed.radius * 2.6).max(12.0)),
        None => (Vec3::ZERO, (scene.extent * 2.3).clamp(12.0, FARTHEST)),
    };
    if zoom != 0.0 {
        view.zoomed = true;
        view.target_distance *= (-zoom * 0.0015).clamp(-1.5, 1.5).exp();
    } else if !view.zoomed {
        view.target_distance = goal.1;
    }
    view.target_distance = view.target_distance.clamp(NEAREST, FARTHEST * 1.4);
    let ease = 1.0 - (-3.0 * dt).exp();
    view.focus = view.focus.lerp(goal.0, ease);
    let target = view.target_distance;
    view.distance += (target - view.distance) * ease;
    let Ok((mut transform, camera, global)) = camera.single_mut() else {
        return;
    };
    let offset = Vec3::new(
        view.yaw.cos() * view.pitch.cos(),
        view.pitch.sin(),
        view.yaw.sin() * view.pitch.cos(),
    ) * view.distance;
    let looking = Transform::from_translation(view.focus + offset).looking_at(view.focus, Vec3::Y);
    let aspect = camera
        .logical_viewport_size()
        .map_or(1.6, |size| size.x / size.y.max(1.0));
    let half_width = view.distance * std::f32::consts::FRAC_PI_8.tan() * aspect;
    let shift = looking.right() * (view.covered * half_width);
    *transform = looking.with_translation(looking.translation + shift);
    let (up, right) = (*looking.up(), *looking.right());
    let on_screen = |p: Vec3| camera.world_to_viewport(global, p).ok();
    view.stars_on_screen = scene
        .galaxies
        .iter()
        .map(|placed| {
            placed.as_ref().map_or_else(Vec::new, |p| {
                p.stars
                    .iter()
                    .map(|(at, size)| {
                        let centre = on_screen(*at)?;
                        let edge = on_screen(*at + right * *size)?;
                        Some((centre, centre.distance(edge)))
                    })
                    .collect()
            })
        })
        .collect();
    // A name just above the highest of its stars, as the camera sees them.
    let above = |centre: Vec3, stars: &mut dyn Iterator<Item = &(Vec3, f32)>| {
        let top = stars
            .map(|(at, size)| (*at - centre).dot(up) + size)
            .fold(0.0_f32, f32::max);
        on_screen(centre + up * (top + 0.8))
    };
    view.galaxies_on_screen = scene
        .galaxies
        .iter()
        .map(|p| {
            p.as_ref()
                .and_then(|p| above(p.centre, &mut p.stars.iter()))
        })
        .collect();
    view.clusters_on_screen = view
        .entered
        .and_then(|g| scene.galaxies.get(g).and_then(Option::as_ref))
        .map_or_else(Vec::new, |p| {
            p.clusters
                .iter()
                .map(|c| {
                    let mut members = p
                        .stars
                        .iter()
                        .filter(|(at, _)| at.distance(c.centre) <= c.radius);
                    above(c.centre, &mut members)
                })
                .collect()
        });
}

/// The fleet's panel: the mouse for the camera, the names, the bar; the
/// board instead when a service is laid out. Returns whether to go back
/// to the code.
pub(crate) fn show(
    ui: &mut Ui,
    view: &mut FleetView,
    infra: &mut Infra,
    plan: &Plan,
    app: &mut App,
) -> bool {
    infra.want();
    infra.poll(plan.map().map(|m| &m.services), app);
    infra.render_all();
    if let Some(fleet) = infra.fleet() {
        view.update(fleet);
    }
    view.search(app.map_search());
    let rect = ui.max_rect();
    if infra.busy() {
        ui.ctx().request_repaint();
    }
    if let (Some(board), Some(fleet)) = (&mut view.board, view.fleet.clone()) {
        let opacity = ui
            .ctx()
            .animate_value_with_time(Id::new("opacity"), app.opacity(), 0.25);
        ui.painter()
            .rect_filled(rect, 0.0, theme::see(theme::BACKGROUND, opacity.max(0.9)));
        if service_board::show(ui, rect, &fleet, board, infra, app) {
            view.board = None;
        }
        crate::plan::switch(ui, rect, app);
        return false;
    }
    let response = ui.allocate_rect(
        rect.with_max_x(rect.max.x - crate::plan::EDGE_GRIP),
        egui::Sense::click_and_drag(),
    );
    if response.dragged() {
        let d = response.drag_delta();
        view.drag += Vec2::new(d.x, d.y);
    }
    if response.hovered() {
        view.zoom += ui.input(|i| i.smooth_scroll_delta.y);
    }
    let pointer = response.hover_pos();
    view.hovered = pointer.and_then(|at| {
        view.stars_on_screen
            .iter()
            .enumerate()
            .flat_map(|(g, stars)| {
                stars
                    .iter()
                    .enumerate()
                    .filter_map(move |(s, p)| p.map(|p| ((g, s), p)))
            })
            .filter(|((g, s), _)| view.lit(*g, *s))
            .map(|(gs, (p, radius))| (gs, egui::pos2(p.x, p.y).distance(at) - radius.max(10.0)))
            .filter(|(_, d)| *d < 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(gs, _)| gs)
    });
    if response.clicked()
        && let Some((g, s)) = view.hovered
    {
        view.board = Some(Board::new(g, s));
    }
    let painter = ui.painter_at(rect);
    let Some(fleet) = view.fleet.clone() else {
        let text = match &infra.error {
            Some(error) => format!("The settings could not be read: {error}"),
            None => "Reading the deployment…".to_owned(),
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(18.0),
            DIM,
        );
        return bar(ui, rect, view, infra, None);
    };
    if fleet.galaxies.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No environment found: no Kustomize tree in the project or in the repositories its settings link",
            egui::FontId::proportional(16.0),
            DIM,
        );
    }

    // The names worth reading: the service under the pointer, those found,
    // and every one of the galaxy gone into while few enough.
    let searching = view.query.as_deref().is_some_and(|q| !q.is_empty());
    for (g, stars) in view.stars_on_screen.iter().enumerate() {
        let named = (view.entered == Some(g) && stars.len() <= 80) || searching;
        for (s, at) in stars.iter().enumerate() {
            let Some((at, radius)) = at else { continue };
            let picked = view.hovered == Some((g, s));
            if !picked && !(named && view.lit(g, s)) {
                continue;
            }
            let star = &fleet.galaxies[g].stars[s];
            let text = if picked {
                let objects = star.pieces.len();
                let mut text = format!("{} · {}", star.name, fleet.galaxies[g].name);
                if objects > 0 {
                    text.push_str(&format!(" · {objects} objects"));
                }
                if let Some(image) = star.images.first() {
                    text.push_str(&format!(" · {image}"));
                }
                text
            } else {
                star.name.clone()
            };
            let colour = egui_colour(view.star_colour(g, s));
            crate::shadowed(
                &painter,
                // Beside the star, up and right, however near it is.
                egui::pos2(at.x + radius * 0.7 + 6.0, at.y - radius * 0.7 - 2.0),
                egui::Align2::LEFT_BOTTOM,
                &text,
                egui::FontId::proportional(if picked { 15.0 } else { 12.5 }),
                if picked { TEXT } else { colour },
            );
        }
    }
    if let Some(g) = view.entered
        && let Some(Some(placed)) = view.scene.galaxies.get(g)
    {
        for (cluster, at) in placed.clusters.iter().zip(&view.clusters_on_screen) {
            if let Some(at) = at {
                crate::shadowed(
                    &painter,
                    egui::pos2(at.x, at.y),
                    egui::Align2::CENTER_BOTTOM,
                    &cluster.namespace,
                    egui::FontId::proportional(13.0),
                    DIM,
                );
            }
        }
    }
    // Each environment's name over it, a click away from going into it.
    let mut enter = None;
    for (g, at) in view.galaxies_on_screen.iter().enumerate() {
        let Some(at) = at else { continue };
        if view.entered.is_some_and(|e| e != g) {
            continue;
        }
        let at = egui::pos2(at.x, at.y);
        if !rect.contains(at) {
            continue;
        }
        let galaxy = &fleet.galaxies[g];
        let colour = egui_colour(galaxy_colour(g));
        let font = egui::FontId::proportional(16.0);
        let galley = painter.layout_no_wrap(galaxy.name.clone(), font.clone(), colour);
        // The name over its count, both above the stars.
        let name_at = at - egui::vec2(0.0, 14.0);
        let area = egui::Rect::from_center_size(
            name_at + egui::vec2(0.0, -galley.size().y / 2.0),
            galley.size() + egui::vec2(12.0, 6.0),
        );
        let label = ui
            .interact(area, Id::new(("fleet-galaxy", g)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if label.hovered() {
            painter.rect_filled(area, 6.0, Color32::from_black_alpha(160));
            painter.rect_stroke(
                area,
                6.0,
                Stroke::new(1.0, colour.gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
        }
        crate::shadowed(
            &painter,
            name_at,
            egui::Align2::CENTER_BOTTOM,
            &galaxy.name,
            font,
            colour,
        );
        let under = if galaxy.rendered {
            format!("{} services", galaxy.stars.len())
        } else if infra.rendering() == Some(galaxy.name.as_str()) {
            format!("{} services · rendering…", galaxy.stars.len())
        } else {
            format!("{} services · not rendered yet", galaxy.stars.len())
        };
        crate::shadowed(
            &painter,
            at,
            egui::Align2::CENTER_BOTTOM,
            &under,
            egui::FontId::proportional(11.0),
            DIM,
        );
        if label.clicked() {
            enter = Some(g);
        }
    }
    if let Some(g) = enter {
        view.entered = Some(g);
        view.zoomed = false;
        view.recolour = true;
    }
    legend(ui, rect, view, &fleet);
    let hint = [
        "drag to turn",
        "scroll to come closer",
        "click an environment's name to go into it",
        "click a service to lay it out",
        "Ctrl-N to leave",
    ]
    .join(" · ");
    painter.text(
        rect.left_bottom() + egui::vec2(24.0, -18.0),
        egui::Align2::LEFT_BOTTOM,
        hint,
        egui::FontId::proportional(12.0),
        DIM,
    );
    let back = bar(ui, rect, view, infra, Some(&fleet));
    crate::plan::switch(ui, rect, app);
    back
}

/// The title and the bar: the way back to the code, the colourings, the
/// environments named as chips, the others in a menu, and what is being
/// rendered. Returns whether to go back to the code.
fn bar(
    ui: &mut Ui,
    rect: egui::Rect,
    view: &mut FleetView,
    infra: &mut Infra,
    fleet: Option<&Fleet>,
) -> bool {
    let corner = rect.left_top() + egui::vec2(24.0, 22.0);
    let painter = ui.painter_at(rect);
    let entered = view
        .entered
        .and_then(|g| fleet?.galaxies.get(g))
        .map(|g| g.name.clone());
    let title = match &entered {
        Some(name) => format!("Deployments › {name}"),
        None => "Deployments".to_owned(),
    };
    painter.text(
        corner,
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(26.0),
        TEXT,
    );
    if let Some(fleet) = fleet {
        let shown: Vec<usize> = (0..fleet.galaxies.len())
            .filter(|g| view.shown(*g))
            .collect();
        let services: usize = shown.iter().map(|g| fleet.galaxies[*g].stars.len()).sum();
        let rendered = shown
            .iter()
            .filter(|g| fleet.galaxies[**g].rendered)
            .count();
        let mut summary = format!(
            "{} environments · {services} services · {rendered} rendered",
            shown.len()
        );
        if let Some(name) = infra.rendering() {
            summary.push_str(&format!(" · rendering {name} with kustomize and helm…"));
        }
        painter.text(
            corner + egui::vec2(0.0, 36.0),
            egui::Align2::LEFT_TOP,
            summary,
            egui::FontId::proportional(13.0),
            DIM,
        );
    }
    let mut back = false;
    let mut relayout = false;
    let mut enter: Option<Option<usize>> = None;
    let mut render = None;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                corner + egui::vec2(0.0, 60.0),
                egui::vec2((rect.width() - 560.0).max(300.0), 26.0),
            ))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let code = egui::Button::new(RichText::new("‹ Code").size(13.0).color(TEXT))
                .fill(theme::see(RAISED, 0.9))
                .stroke(Stroke::new(1.0, EDGE));
            if ui
                .add(code)
                .on_hover_text("Back to the code's universe")
                .clicked()
            {
                back = true;
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Colour by").size(12.0).color(DIM));
            for (name, colouring, hint) in [
                ("Kinds", Colouring::Kinds, "Stars by environment, planets by kind"),
                ("Images", Colouring::Images, "The same image the same colour everywhere: versions that differ stand out"),
                ("Values", Colouring::Values, "The deepest layer that set a service's values: chart default, base, environment, patch"),
            ] {
                let on = view.colouring == colouring;
                let text = RichText::new(name).size(13.0).color(if on { TEXT } else { DIM });
                if ui.add(egui::Button::selectable(on, text)).on_hover_text(hint).clicked() {
                    view.colouring = colouring;
                    view.recolour = true;
                }
            }
            let Some(fleet) = fleet else {
                return;
            };
            ui.add_space(8.0);
            let all = view.entered.is_none();
            if ui
                .add(egui::Button::selectable(
                    all,
                    RichText::new("All").size(13.0).color(if all { TEXT } else { DIM }),
                ))
                .on_hover_text("Every environment")
                .clicked()
            {
                enter = Some(None);
            }
            for (g, galaxy) in fleet.galaxies.iter().enumerate() {
                if !galaxy.named {
                    continue;
                }
                let on = view.entered == Some(g);
                let text = RichText::new(&galaxy.name)
                    .size(13.0)
                    .color(egui_colour(galaxy_colour(g)).gamma_multiply(if on { 1.0 } else { 0.75 }));
                if ui
                    .add(egui::Button::selectable(on, text))
                    .on_hover_text(format!("Go into {}", galaxy.name))
                    .clicked()
                {
                    enter = Some(Some(g));
                }
            }
            let others: Vec<usize> = (0..fleet.galaxies.len())
                .filter(|g| !fleet.galaxies[*g].named)
                .collect();
            if !others.is_empty() {
                ui.menu_button(
                    RichText::new(format!("{} more", others.len())).size(13.0).color(DIM),
                    |ui| {
                        ui.label(
                            RichText::new("Found in the repositories, not named in the settings")
                                .size(11.5)
                                .color(DIM),
                        );
                        for g in others {
                            let name = fleet.galaxies[g].name.clone();
                            let mut on = view.others.contains(&name);
                            if ui.checkbox(&mut on, &name).changed() {
                                if on {
                                    view.others.insert(name);
                                    render = Some(g);
                                } else {
                                    view.others.remove(&name);
                                }
                                relayout = true;
                            }
                        }
                    },
                );
            }
        },
    );
    if let Some(entered) = enter {
        view.entered = entered;
        view.zoomed = false;
        view.recolour = true;
    }
    if let Some(g) = render {
        infra.render_one(g);
    }
    if relayout {
        view.relayout();
    }
    back
}

/// What the colours mean, under the bar.
fn legend(ui: &Ui, rect: egui::Rect, view: &FleetView, fleet: &Fleet) {
    let painter = ui.painter_at(rect);
    let mut at = rect.left_top() + egui::vec2(24.0, 118.0);
    let mut entries: Vec<(String, [f32; 3])> = Vec::new();
    match view.colouring {
        Colouring::Kinds => {
            let mut kinds: Vec<(&str, usize)> = Vec::new();
            for galaxy in &fleet.galaxies {
                for piece in galaxy.stars.iter().flat_map(|s| &s.pieces) {
                    match kinds.iter_mut().find(|(k, _)| *k == piece.kind) {
                        Some(entry) => entry.1 += 1,
                        None => kinds.push((&piece.kind, 1)),
                    }
                }
            }
            kinds.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            entries = kinds
                .into_iter()
                .take(10)
                .map(|(k, n)| (format!("{k}  {n}"), kind_colour(k)))
                .collect();
        }
        Colouring::Images => {
            let mut images: Vec<(&str, usize)> = Vec::new();
            for galaxy in &fleet.galaxies {
                for image in galaxy.stars.iter().filter_map(|s| s.images.first()) {
                    match images.iter_mut().find(|(i, _)| *i == image) {
                        Some(entry) => entry.1 += 1,
                        None => images.push((image, 1)),
                    }
                }
            }
            images.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            let more = images.len().saturating_sub(12);
            entries = images
                .into_iter()
                .take(12)
                .map(|(i, n)| (format!("{i}  {n}"), image_colour(i)))
                .collect();
            if more > 0 {
                entries.push((format!("and {more} more"), [0.3, 0.3, 0.35]));
            }
        }
        Colouring::Values => {
            for kind in LayerKind::ALL {
                entries.push((
                    format!("set last by the {}", kind.label()),
                    layer_colour(kind),
                ));
            }
        }
    }
    for (text, colour) in entries {
        painter.circle_filled(at + egui::vec2(5.0, 7.0), 4.0, egui_colour(colour));
        painter.text(
            at + egui::vec2(16.0, 0.0),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(12.0),
            DIM,
        );
        at.y += 18.0;
    }
    if view.colouring == Colouring::Images {
        painter.text(
            at + egui::vec2(0.0, 6.0),
            egui::Align2::LEFT_TOP,
            "a service's own image, tag and all",
            egui::FontId::proportional(11.0),
            YELLOW.gamma_multiply(0.7),
        );
    }
}

#[cfg(test)]
mod tests {
    use universe_codemap::{Galaxy, Place, Star};

    use super::*;

    fn star(name: &str, namespace: &str) -> Star {
        Star {
            name: name.into(),
            namespace: Some(namespace.into()),
            chart: None,
            images: Vec::new(),
            pieces: Vec::new(),
            settings: Vec::new(),
            place: Place::new("ops", "apps/x.yaml", 1),
        }
    }

    fn galaxy(name: &str, stars: Vec<Star>) -> Galaxy {
        Galaxy {
            name: name.into(),
            named: true,
            rendered: false,
            stars,
        }
    }

    fn fleet() -> Fleet {
        let many = |ns: &str, n: usize| -> Vec<Star> {
            (0..n).map(|i| star(&format!("s{i}"), ns)).collect()
        };
        Fleet {
            galaxies: vec![
                galaxy("prod", [many("shop", 12), many("batch", 4)].concat()),
                galaxy("staging", many("shop", 9)),
                galaxy(
                    "dev",
                    [many("shop", 3), many("tools", 7), many("x", 1)].concat(),
                ),
                galaxy("qa", many("shop", 30)),
            ],
        }
    }

    #[test]
    fn galaxies_stand_apart_and_no_star_reaches_another() {
        let scene = layout(&fleet(), &[true; 4]);
        let placed: Vec<&Placed> = scene.galaxies.iter().flatten().collect();
        assert_eq!(placed.len(), 4);
        for (a, first) in placed.iter().enumerate() {
            for (at, _) in &first.stars {
                assert!(at.distance(first.centre) + REACH <= first.radius + 1e-3);
            }
            for (b, second) in placed.iter().enumerate() {
                if a == b {
                    continue;
                }
                assert!(first.centre.distance(second.centre) > first.radius + second.radius);
                for (at, _) in &first.stars {
                    assert!(at.distance(second.centre) > second.radius + REACH);
                }
            }
            // Within a galaxy, two stars never share their orbits.
            for (i, (p, _)) in first.stars.iter().enumerate() {
                for (q, _) in &first.stars[i + 1..] {
                    assert!(p.distance(*q) > 1.0, "stars too close: {p} {q}");
                }
            }
        }
    }

    #[test]
    fn a_hidden_galaxy_takes_no_room_and_one_alone_sits_in_the_middle() {
        let all = fleet();
        let scene = layout(&all, &[true, false, true, true]);
        assert!(scene.galaxies[1].is_none());
        let mut without = all.clone();
        without.galaxies.remove(1);
        let other = layout(&without, &[true; 3]);
        assert_eq!(scene.galaxies[0], other.galaxies[0]);
        assert_eq!(scene.galaxies[3], other.galaxies[2]);
        let alone = layout(&all, &[false, true, false, false]);
        assert_eq!(alone.galaxies[1].as_ref().unwrap().centre, Vec3::ZERO);
    }

    #[test]
    fn an_image_keeps_its_colour_and_another_version_has_another() {
        assert_eq!(
            image_colour("registry.example.com/api:1.2.0"),
            image_colour("registry.example.com/api:1.2.0")
        );
        let images = [
            "registry.example.com/api:1.2.0",
            "registry.example.com/api:1.2.1",
            "registry.example.com/api:1.3.0",
            "registry.example.com/web:1.2.0",
            "nginx:1.27",
            "redis:7",
        ];
        for (i, a) in images.iter().enumerate() {
            for b in &images[i + 1..] {
                let (x, y) = (image_colour(a), image_colour(b));
                let distance: f32 = x.iter().zip(y).map(|(p, q)| (p - q).abs()).sum();
                assert!(distance > 0.08, "{a} and {b} look alike");
            }
        }
        // Many environments stay told apart.
        let colours: Vec<[f32; 3]> = (0..20).map(galaxy_colour).collect();
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
