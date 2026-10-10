//! An environment's architecture in 3D, in the universe's window: the
//! private network a translucent box, the cluster a platform inside it,
//! each namespace a slab on it with its workloads standing as pillars as
//! tall as their replicas and its jobs as cubes; the databases cylinders
//! under the cluster; the way in in front (load balancer, firewall, names,
//! the internet); the cloud's buckets, secrets, roles and users beside the
//! network, and the services outside farther away.
//!
//! Looked at like a map: the camera never turns by itself; a drag moves
//! the view along the ground, a drag with Ctrl, Cmd or the secondary
//! button turns it, the wheel comes closer. The scene is built on a
//! thread, and the window draws only while the camera moves.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_egui::egui::{self, Color32, Id, RichText, Stroke, Ui};
use ironquill_codemap::{Deployment, ElementKind, LinkKind, Place};

use crate::theme::{self, DIM, EDGE, PANEL, TEXT};
use crate::universe::UniverseCamera;

/// Frames drawn after the camera stops: the labels follow it there.
const SETTLE_FRAMES: u32 = 4;

/// What a piece of the scene is shaped as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Cuboid,
    Cylinder,
    Sphere,
}

/// A piece of the scene: an element's, or a frame (the network, a
/// namespace).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Piece {
    /// The element it stands for.
    pub(crate) element: Option<usize>,
    pub(crate) shape: Shape,
    /// Its centre.
    pub(crate) at: Vec3,
    pub(crate) size: Vec3,
    pub(crate) colour: [f32; 3],
    /// Below 1, see-through.
    pub(crate) alpha: f32,
}

/// The scene: its pieces, its links, where each element's name goes, and
/// what frames it all.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scene {
    pub(crate) pieces: Vec<Piece>,
    pub(crate) links: Vec<(Vec3, Vec3, [f32; 3])>,
    /// Each element's top, where its name goes.
    pub(crate) tops: Vec<Option<Vec3>>,
    /// Names of frames: a namespace's, the network's.
    pub(crate) captions: Vec<(String, Vec3)>,
    pub(crate) centre: Vec3,
    pub(crate) radius: f32,
}

fn rgb(colour: Color32) -> [f32; 3] {
    [
        f32::from(colour.r()) / 255.0,
        f32::from(colour.g()) / 255.0,
        f32::from(colour.b()) / 255.0,
    ]
}

/// Where everything stands.
#[allow(clippy::too_many_lines)]
pub(crate) fn scene(deployment: &Deployment) -> Scene {
    let mut scene = Scene {
        tops: vec![None; deployment.elements.len()],
        ..Scene::default()
    };
    let colour = |kind| rgb(crate::arch_view::colour(kind));
    let mut centres: Vec<Option<Vec3>> = vec![None; deployment.elements.len()];

    // Namespaces, each a slab, in a row along x.
    let mut namespaces: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, e) in deployment.elements.iter().enumerate() {
        if matches!(e.kind, ElementKind::Workload | ElementKind::Job) {
            let ns = e.namespace.clone().unwrap_or_else(|| "default".to_owned());
            match namespaces.iter_mut().find(|(n, _)| *n == ns) {
                Some((_, members)) => members.push(i),
                None => namespaces.push((ns, vec![i])),
            }
        }
    }
    const PITCH: f32 = 2.6;
    let mut x = 0.0_f32;
    let mut slabs: Vec<(f32, f32, f32)> = Vec::new();
    for (ns, members) in &namespaces {
        let per_row = (members.len() as f32).sqrt().ceil().max(1.0) as usize;
        let rows = members.len().div_ceil(per_row);
        let width = per_row as f32 * PITCH + 1.2;
        let depth = rows as f32 * PITCH + 1.2;
        let middle = x + width / 2.0;
        scene.pieces.push(Piece {
            element: None,
            shape: Shape::Cuboid,
            at: Vec3::new(middle, 0.55, 0.0),
            size: Vec3::new(width, 0.3, depth),
            colour: [0.22, 0.26, 0.36],
            alpha: 1.0,
        });
        scene
            .captions
            .push((ns.clone(), Vec3::new(middle, 0.8, depth / 2.0 + 0.4)));
        for (k, &i) in members.iter().enumerate() {
            let (row, col) = (k / per_row, k % per_row);
            let px = x + 0.6 + PITCH * (col as f32 + 0.5);
            let pz = -depth / 2.0 + 0.6 + PITCH * (row as f32 + 0.5);
            let e = &deployment.elements[i];
            let (shape, size) = if e.kind == ElementKind::Job {
                (Shape::Cuboid, Vec3::splat(1.1))
            } else {
                let most = e.replicas.map_or(1, |(_, max)| max).min(16);
                (Shape::Cuboid, Vec3::new(1.1, 1.0 + most as f32 * 0.9, 1.1))
            };
            let at = Vec3::new(px, 0.7 + size.y / 2.0, pz);
            scene.pieces.push(Piece {
                element: Some(i),
                shape,
                at,
                size,
                colour: colour(e.kind),
                alpha: 1.0,
            });
            centres[i] = Some(at);
            scene.tops[i] = Some(at + Vec3::Y * (size.y / 2.0 + 0.3));
        }
        slabs.push((x, width, depth));
        x += width + 1.6;
    }
    let cluster_width = (x - 1.6).max(6.0);
    let cluster_depth = slabs.iter().map(|s| s.2).fold(6.0_f32, f32::max) + 2.0;
    let cluster_centre = Vec3::new(cluster_width / 2.0, 0.0, 0.0);

    // The cluster, a platform under them.
    for (i, e) in deployment.elements.iter().enumerate() {
        if e.kind == ElementKind::Cluster {
            scene.pieces.push(Piece {
                element: Some(i),
                shape: Shape::Cuboid,
                at: cluster_centre + Vec3::Y * 0.2,
                size: Vec3::new(cluster_width + 2.0, 0.4, cluster_depth),
                colour: [0.16, 0.19, 0.27],
                alpha: 1.0,
            });
            centres[i] = Some(cluster_centre);
            scene.tops[i] =
                Some(cluster_centre + Vec3::new(-cluster_width / 2.0, 0.6, cluster_depth / 2.0));
        }
    }

    // Databases, cylinders under the cluster.
    let databases: Vec<usize> = deployment
        .of(ElementKind::Database)
        .map(|(i, _)| i)
        .collect();
    for (k, &i) in databases.iter().enumerate() {
        let spread = (databases.len() as f32 - 1.0) * 3.2;
        let at = Vec3::new(cluster_centre.x - spread / 2.0 + k as f32 * 3.2, -3.2, 0.0);
        scene.pieces.push(Piece {
            element: Some(i),
            shape: Shape::Cylinder,
            at,
            size: Vec3::new(2.2, 2.0, 2.2),
            colour: colour(ElementKind::Database),
            alpha: 1.0,
        });
        centres[i] = Some(at);
        scene.tops[i] = Some(at + Vec3::new(0.0, -1.4, 1.4));
    }

    // The network, a translucent box around it all.
    let network_size = Vec3::new(
        cluster_width + 6.0,
        9.0 + 16.0_f32.min(4.0),
        cluster_depth + 4.0,
    );
    let network_centre = Vec3::new(cluster_centre.x, 1.0, 0.0);
    for (i, e) in deployment.elements.iter().enumerate() {
        if e.kind == ElementKind::Network {
            scene.pieces.push(Piece {
                element: Some(i),
                shape: Shape::Cuboid,
                at: network_centre,
                size: network_size,
                colour: [0.35, 0.45, 0.8],
                alpha: 0.05,
            });
            centres[i] = Some(network_centre);
            scene.tops[i] = Some(
                network_centre
                    + Vec3::new(
                        -network_size.x / 2.0,
                        network_size.y / 2.0,
                        network_size.z / 2.0,
                    ),
            );
        }
    }
    let front = network_size.z / 2.0;

    // The way in, in front, from the load balancers out to the internet.
    for (row, kind) in [
        (0, ElementKind::LoadBalancer),
        (1, ElementKind::Firewall),
        (2, ElementKind::Dns),
        (3, ElementKind::Internet),
    ] {
        let members: Vec<usize> = deployment.of(kind).map(|(i, _)| i).collect();
        for (k, &i) in members.iter().enumerate() {
            let spread = (members.len() as f32 - 1.0) * 3.5;
            let at = Vec3::new(
                cluster_centre.x - spread / 2.0 + k as f32 * 3.5,
                1.4,
                front + 4.0 + row as f32 * 4.5,
            );
            let (shape, size) = match kind {
                ElementKind::Internet => (Shape::Sphere, Vec3::splat(2.4)),
                ElementKind::Firewall => (Shape::Cuboid, Vec3::new(3.0, 2.4, 0.4)),
                ElementKind::Dns => (Shape::Sphere, Vec3::splat(1.2)),
                _ => (Shape::Cuboid, Vec3::new(2.4, 0.8, 1.4)),
            };
            scene.pieces.push(Piece {
                element: Some(i),
                shape,
                at,
                size,
                colour: colour(kind),
                alpha: if kind == ElementKind::Firewall {
                    0.6
                } else {
                    1.0
                },
            });
            centres[i] = Some(at);
            scene.tops[i] = Some(at + Vec3::Y * (size.y / 2.0 + 0.3));
        }
    }

    // The cloud's resources beside the network, a column of each kind.
    let beside = network_centre.x + network_size.x / 2.0 + 4.0;
    for (col, kind) in [
        ElementKind::Bucket,
        ElementKind::Secret,
        ElementKind::Role,
        ElementKind::Users,
    ]
    .into_iter()
    .enumerate()
    {
        let members: Vec<usize> = deployment.of(kind).map(|(i, _)| i).collect();
        for (k, &i) in members.iter().enumerate() {
            let at = Vec3::new(
                beside + col as f32 * 4.5,
                0.8,
                -network_size.z / 2.0 + 1.5 + (k as f32 + col as f32 * 0.5) * 3.6,
            );
            let (shape, size) = match kind {
                ElementKind::Bucket => (Shape::Cylinder, Vec3::new(1.6, 1.4, 1.6)),
                ElementKind::Role | ElementKind::Users => (Shape::Sphere, Vec3::splat(1.2)),
                _ => (Shape::Cuboid, Vec3::splat(1.0)),
            };
            scene.pieces.push(Piece {
                element: Some(i),
                shape,
                at,
                size,
                colour: colour(kind),
                alpha: 1.0,
            });
            centres[i] = Some(at);
            scene.tops[i] = Some(at + Vec3::Y * (size.y / 2.0 + 0.3));
        }
    }

    // The services outside, farther away.
    let outside: Vec<usize> = deployment
        .of(ElementKind::External)
        .map(|(i, _)| i)
        .collect();
    for (k, &i) in outside.iter().enumerate() {
        let at = Vec3::new(beside + 18.0, 2.0, -network_size.z / 2.0 + k as f32 * 3.4);
        scene.pieces.push(Piece {
            element: Some(i),
            shape: Shape::Sphere,
            at,
            size: Vec3::splat(1.6),
            colour: colour(ElementKind::External),
            alpha: 1.0,
        });
        centres[i] = Some(at);
        scene.tops[i] = Some(at + Vec3::Y * 1.2);
    }

    for link in &deployment.links {
        if link.kind == LinkKind::Contains {
            continue;
        }
        if let (Some(a), Some(b)) = (centres[link.from], centres[link.to]) {
            let colour = match link.kind {
                LinkKind::Routes => [1.0, 0.8, 0.3],
                LinkKind::Calls => [0.5, 0.67, 1.0],
                LinkKind::Uses => [0.34, 0.85, 0.9],
                LinkKind::Assumes => [0.85, 0.5, 0.95],
                LinkKind::Contains => [0.4, 0.4, 0.4],
            };
            scene.links.push((a, b, colour));
        }
    }

    let points: Vec<Vec3> = scene
        .pieces
        .iter()
        .flat_map(|p| [p.at - p.size / 2.0, p.at + p.size / 2.0])
        .collect();
    if let (Some(min), Some(max)) = (
        points.iter().copied().reduce(Vec3::min),
        points.iter().copied().reduce(Vec3::max),
    ) {
        scene.centre = (min + max) / 2.0;
        scene.radius = ((max - min).length() / 2.0).max(4.0);
    }
    scene
}

/// Marks what belongs to the architecture's scene.
#[derive(Component)]
pub(crate) struct InArch;

/// The 3D architecture, as the panels and the systems share it.
#[derive(Resource, Default)]
pub(crate) struct Arch3d {
    /// Whether the universe window shows it, instead of the code.
    pub(crate) active: bool,
    pub(crate) deployment: Option<Arc<Deployment>>,
    /// The scene being built: held in a lock, as a resource is shared.
    building: Option<Mutex<Receiver<Scene>>>,
    pub(crate) scene: Option<Scene>,
    /// The scene must be spawned anew.
    respawn: bool,
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
    /// What the panels report of the mouse, for the next frame.
    pub(crate) pan: Vec2,
    pub(crate) turn: Vec2,
    pub(crate) zoom: f32,
    pub(crate) frame_all: bool,
    /// Frames left to draw: while the camera moves, and a few after.
    pub(crate) moving: u32,
    /// Where each element's name goes on screen.
    pub(crate) on_screen: Vec<Option<Vec2>>,
    pub(crate) captions_on_screen: Vec<Option<Vec2>>,
    pub(crate) hovered: Option<usize>,
    pub(crate) chosen: Option<usize>,
    /// How much of the window the panels cover on the right.
    pub(crate) covered: f32,
    /// The element last clicked, and when.
    last_click: Option<(usize, f64)>,
}

impl Arch3d {
    /// Shows `deployment`, its scene built on a thread.
    pub(crate) fn open(&mut self, deployment: Deployment) {
        let deployment = Arc::new(deployment);
        let (send, receive) = mpsc::channel();
        let building = Arc::clone(&deployment);
        std::thread::spawn(move || {
            let _ = send.send(scene(&building));
        });
        self.deployment = Some(deployment);
        self.building = Some(Mutex::new(receive));
        self.scene = None;
        self.chosen = None;
        self.hovered = None;
        self.active = true;
        self.moving = SETTLE_FRAMES;
    }

    /// Whether it is waiting for its scene.
    pub(crate) fn building(&self) -> bool {
        self.building.is_some()
    }

    /// Takes the scene, once built.
    fn take_built(&mut self) {
        let Some(received) = self
            .building
            .as_ref()
            .and_then(|b| b.lock().ok().map(|r| r.try_recv()))
        else {
            return;
        };
        match received {
            Ok(scene) => {
                self.building = None;
                self.scene = Some(scene);
                self.respawn = true;
                self.frame_all = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.building = None,
        }
    }

    /// Whether the window must draw this frame.
    pub(crate) fn needs_frames(&self) -> bool {
        self.moving > 0 || self.building.is_some() || self.respawn
    }
}

/// Spawns the scene once built, and takes it away when the architecture
/// is left; hides the code universe's stars meanwhile.
pub(crate) fn spawn(
    mut commands: Commands,
    mut arch: ResMut<Arch3d>,
    shown: Res<crate::universe::Universe>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    existing: Query<Entity, With<InArch>>,
    mut stars: Query<&mut Visibility, (With<crate::universe::InUniverse>, Without<InArch>)>,
) {
    arch.take_built();
    let wanted = arch.active && shown.shown;
    // The code's stars hide behind the architecture.
    let stars_visible = shown.shown && !arch.active;
    for mut visibility in &mut stars {
        let want = if stars_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != want {
            *visibility = want;
        }
    }
    if !wanted || arch.respawn {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
    }
    if !wanted || !arch.respawn {
        return;
    }
    arch.respawn = false;
    let Some(scene) = arch.scene.clone() else {
        return;
    };
    let cuboid = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let cylinder = meshes.add(Cylinder::new(0.5, 1.0));
    let sphere = meshes.add(Sphere::new(0.5).mesh().uv(24, 16));
    for piece in &scene.pieces {
        let [r, g, b] = piece.colour;
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba(r, g, b, piece.alpha),
            // See-through, it only tints: lit, it would glow over the rest.
            emissive: if piece.alpha < 1.0 {
                LinearRgba::BLACK
            } else {
                LinearRgba::rgb(r * 0.25, g * 0.25, b * 0.25)
            },
            perceptual_roughness: 0.7,
            alpha_mode: if piece.alpha < 1.0 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            double_sided: piece.alpha < 1.0,
            cull_mode: if piece.alpha < 1.0 {
                None
            } else {
                Some(bevy::render::render_resource::Face::Back)
            },
            ..default()
        });
        let mesh = match piece.shape {
            Shape::Cuboid => cuboid.clone(),
            Shape::Cylinder => cylinder.clone(),
            Shape::Sphere => sphere.clone(),
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_translation(piece.at).with_scale(piece.size),
            InArch,
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 9_000.0,
            ..default()
        },
        Transform::from_xyz(-20.0, 40.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
        InArch,
    ));
    commands.spawn((
        PointLight {
            intensity: 4_000_000.0,
            range: 400.0,
            ..default()
        },
        Transform::from_translation(scene.centre + Vec3::new(10.0, 30.0, 40.0)),
        InArch,
    ));
}

/// Moves the camera as the panels asked, draws the links, and says where
/// each element's name goes.
pub(crate) fn look(
    mut arch: ResMut<Arch3d>,
    universe: Res<crate::universe::Universe>,
    mut camera: Query<(&mut Transform, &Camera, &GlobalTransform), With<UniverseCamera>>,
    mut gizmos: Gizmos,
) {
    if !(arch.active && universe.shown) {
        return;
    }
    let Some(scene) = arch.scene.clone() else {
        return;
    };
    let pan = std::mem::take(&mut arch.pan);
    let turn = std::mem::take(&mut arch.turn);
    let zoom = std::mem::take(&mut arch.zoom);
    let mut moved = pan != Vec2::ZERO || turn != Vec2::ZERO || zoom != 0.0;
    if std::mem::take(&mut arch.frame_all) {
        // From the front, a little above.
        arch.focus = scene.centre;
        arch.yaw = 0.0;
        arch.pitch = 0.8;
        arch.distance = scene.radius * 2.1;
        moved = true;
    }
    arch.yaw -= turn.x * 0.006;
    arch.pitch = (arch.pitch + turn.y * 0.005).clamp(0.12, 1.45);
    if zoom != 0.0 {
        arch.distance *= (-zoom * 0.0015).clamp(-1.0, 1.0).exp();
    }
    arch.distance = arch.distance.clamp(3.0, scene.radius * 6.0);
    // A drag moves the ground under the pointer, as far as it seems to.
    let right = Vec3::new(arch.yaw.cos(), 0.0, -arch.yaw.sin());
    let forward = Vec3::new(-arch.yaw.sin(), 0.0, -arch.yaw.cos());
    let speed = arch.distance * 0.0016;
    let focus = arch.focus - right * pan.x * speed + forward * pan.y * speed;
    arch.focus = focus;
    if moved {
        arch.moving = SETTLE_FRAMES;
    } else {
        arch.moving = arch.moving.saturating_sub(1);
    }
    let Ok((mut transform, camera, global)) = camera.single_mut() else {
        return;
    };
    let offset = Vec3::new(
        arch.yaw.sin() * arch.pitch.cos(),
        arch.pitch.sin(),
        arch.yaw.cos() * arch.pitch.cos(),
    ) * arch.distance;
    let looking = Transform::from_translation(arch.focus + offset).looking_at(arch.focus, Vec3::Y);
    let aspect = camera
        .logical_viewport_size()
        .map_or(1.6, |size| size.x / size.y.max(1.0));
    let half_width = arch.distance * std::f32::consts::FRAC_PI_8.tan() * aspect;
    let shift = looking.right() * (arch.covered * half_width);
    let wanted = looking.with_translation(looking.translation + shift);
    if *transform != wanted {
        *transform = wanted;
    }
    for (a, b, [r, g, bl]) in &scene.links {
        // Up from each end, across, and down: links read over the scene.
        let lift = Vec3::Y * 1.5;
        let colour = Color::linear_rgba(*r * 1.6, *g * 1.6, *bl * 1.6, 0.85);
        gizmos.line(*a, *a + lift, colour);
        gizmos.line(*a + lift, *b + lift, colour);
        gizmos.line(*b + lift, *b, colour);
    }
    arch.on_screen = scene
        .tops
        .iter()
        .map(|p| p.and_then(|p| camera.world_to_viewport(global, p).ok()))
        .collect();
    arch.captions_on_screen = scene
        .captions
        .iter()
        .map(|(_, p)| camera.world_to_viewport(global, *p).ok())
        .collect();
}

/// The panels' part: the mouse for the camera, the names, the details of
/// the element chosen. Returns the place to open, on a double click or a
/// click in the details.
pub(crate) fn view(
    ui: &mut Ui,
    arch: &mut Arch3d,
    painter_rect: egui::Rect,
) -> (Option<Place>, bool) {
    let rect = painter_rect;
    let response = ui.allocate_rect(
        rect.with_max_x(rect.max.x - crate::plan::EDGE_GRIP),
        egui::Sense::click_and_drag(),
    );
    let mut open = None;
    let mut leave = false;
    let turning = ui.input(|i| i.modifiers.ctrl || i.modifiers.command)
        || response.dragged_by(egui::PointerButton::Secondary);
    if response.dragged() {
        let d = response.drag_delta();
        if turning {
            arch.turn += Vec2::new(d.x, d.y);
        } else {
            arch.pan += Vec2::new(d.x, d.y);
        }
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            arch.zoom += scroll;
        }
    }
    // The camera moves on the next frame: ask for it, the window may be
    // asleep otherwise.
    if arch.pan != Vec2::ZERO || arch.turn != Vec2::ZERO || arch.zoom != 0.0 || arch.frame_all {
        ui.ctx().request_repaint();
    }
    let pointer = response.hover_pos();
    let hovered = pointer.and_then(|at| {
        arch.on_screen
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.map(|p| (i, egui::pos2(p.x, p.y).distance(at))))
            .filter(|(_, d)| *d < 28.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    });
    if hovered != arch.hovered {
        arch.hovered = hovered;
    }
    let deployment = arch.deployment.clone();
    // Two clicks on one element within half a second open it: counted
    // here, as a frame late to come can make egui's own count miss one.
    let now = ui.input(|i| i.time);
    if response.clicked() {
        let again = arch
            .last_click
            .is_some_and(|(e, at)| Some(e) == hovered && now - at < 0.5);
        if again && let (Some(i), Some(d)) = (hovered, &deployment) {
            open = d.elements[i].places.first().cloned();
            arch.last_click = None;
        } else {
            // Chosen without moving the camera.
            arch.chosen = hovered;
            arch.last_click = hovered.map(|h| (h, now));
        }
    }
    let painter = ui.painter_at(rect);
    let Some(deployment) = deployment else {
        return (None, false);
    };
    if arch.building() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Building the scene…",
            egui::FontId::proportional(18.0),
            DIM,
        );
    }
    for (i, at) in arch.on_screen.iter().enumerate() {
        let Some(at) = at else { continue };
        let e = &deployment.elements[i];
        let picked = arch.hovered == Some(i) || arch.chosen == Some(i);
        let colour = crate::arch_view::colour(e.kind);
        let text = if picked {
            format!("{} · {}", e.name, e.kind.label())
        } else {
            e.name.clone()
        };
        crate::shadowed(
            &painter,
            egui::pos2(at.x, at.y),
            egui::Align2::CENTER_BOTTOM,
            &text,
            egui::FontId::proportional(if picked { 15.0 } else { 12.5 }),
            if picked { TEXT } else { colour },
        );
    }
    if let Some(scene) = &arch.scene {
        for ((name, _), at) in scene.captions.iter().zip(&arch.captions_on_screen) {
            if let Some(at) = at {
                crate::shadowed(
                    &painter,
                    egui::pos2(at.x, at.y),
                    egui::Align2::CENTER_TOP,
                    name,
                    egui::FontId::proportional(12.0),
                    DIM,
                );
            }
        }
    }
    let corner = rect.left_top() + egui::vec2(24.0, 22.0);
    painter.text(
        corner,
        egui::Align2::LEFT_TOP,
        format!("Architecture · {}", deployment.environment),
        egui::FontId::proportional(26.0),
        TEXT,
    );
    painter.text(
        corner + egui::vec2(0.0, 36.0),
        egui::Align2::LEFT_TOP,
        "drag to move · Ctrl, Cmd or the right button and drag to turn · the wheel to come closer · click to choose · double-click to open where it is configured",
        egui::FontId::proportional(12.5),
        DIM,
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                corner + egui::vec2(0.0, 60.0),
                egui::vec2(400.0, 30.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            if ui.button(RichText::new("Frame all").size(13.0)).clicked() {
                arch.frame_all = true;
            }
            if ui
                .button(RichText::new("‹ Back to the services").size(13.0))
                .clicked()
            {
                leave = true;
            }
        },
    );
    if let Some(i) = arch.chosen.filter(|i| *i < deployment.elements.len()) {
        let width = 360.0;
        egui::Area::new(Id::new("arch3d-details"))
            .fixed_pos(rect.left_top() + egui::vec2(24.0, 110.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::see(PANEL, 0.92))
                    .stroke(Stroke::new(1.0, EDGE))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.set_max_height(rect.height() - 180.0);
                        egui::ScrollArea::vertical()
                            .id_salt("arch3d-details")
                            .show(ui, |ui| {
                                if let Some(p) =
                                    crate::arch_view::element_details(ui, &deployment.elements[i])
                                {
                                    open = Some(p);
                                }
                            });
                    });
            });
    }
    (open, leave)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironquill_codemap::{ArchLink, Element};

    fn element(
        kind: ElementKind,
        name: &str,
        ns: Option<&str>,
        replicas: Option<(u32, u32)>,
    ) -> Element {
        Element {
            kind,
            name: name.into(),
            summary: String::new(),
            properties: Vec::new(),
            places: Vec::new(),
            namespace: ns.map(str::to_owned),
            service: None,
            replicas,
        }
    }

    #[test]
    fn pillars_as_tall_as_their_replicas_databases_under_the_way_in_in_front() {
        let deployment = Deployment {
            environment: "prod".into(),
            elements: vec![
                element(ElementKind::Network, "vpc", None, None),
                element(ElementKind::Cluster, "eks", None, None),
                element(ElementKind::Workload, "api", Some("shop"), Some((2, 6))),
                element(ElementKind::Workload, "web", Some("shop"), Some((1, 1))),
                element(ElementKind::Job, "report", Some("batch"), None),
                element(ElementKind::Database, "db", None, None),
                element(ElementKind::LoadBalancer, "alb", None, None),
                element(ElementKind::Internet, "Internet", None, None),
                element(ElementKind::Bucket, "files", None, None),
            ],
            links: vec![ArchLink {
                from: 6,
                to: 2,
                kind: LinkKind::Routes,
                label: String::new(),
            }],
            notes: Vec::new(),
        };
        let scene = scene(&deployment);
        let piece = |i: usize| scene.pieces.iter().find(|p| p.element == Some(i)).unwrap();
        assert!(
            piece(2).size.y > piece(3).size.y,
            "six replicas stand taller than one"
        );
        assert!(
            piece(5).at.y < piece(1).at.y,
            "the database is under the cluster"
        );
        assert!(
            piece(6).at.z > piece(1).at.z && piece(7).at.z > piece(6).at.z,
            "the way in is in front"
        );
        assert!(
            piece(8).at.x > piece(0).at.x,
            "the cloud's resources beside the network"
        );
        assert!(piece(0).alpha < 1.0, "the network is see-through");
        assert_eq!(scene.links.len(), 1);
        assert_eq!(scene.captions.len(), 2, "a caption per namespace");
        assert!(scene.radius > 4.0);
    }
}
