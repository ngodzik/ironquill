//! The codebase as a universe (Ctrl-N, twice): each file a star coloured by its
//! language and sized by its length, each folder a dimmer one, the links
//! between them what holds what and what imports what. The stars spread out
//! from the centre as the layout finds their places, and light up as the
//! agent reads them, or flare with sparks as it edits them.
//!
//! The scene is drawn by a camera of its own, in HDR with bloom, under the
//! panels. It is only drawn while shown: hidden, the camera sleeps and the
//! window goes back to drawing only when something changes.

use std::path::PathBuf;

use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use ironquill_codemap::{CodeMap, EdgeKind, Language, Layout, NodeKind};
use ironquill_ui::Entry;

/// The most files mapped: beyond, the layout would take too long to settle
/// and the sky too busy to read.
const MAX_FILES: usize = 1_200;

/// Layout steps run each frame while it settles: fast enough to watch the
/// stars find their places, slow enough to see them do it.
const STEPS_PER_FRAME: usize = 2;

/// How bright a star is at rest, in HDR units: above 1, it blooms.
const REST: f32 = 3.6;

/// How much brighter a star a file the agent just read or edited is, and
/// how fast that fades, per second.
const READ_FLASH: f32 = 10.0;
const EDIT_FLASH: f32 = 28.0;
const FADE: f32 = 1.6;

/// Sparks thrown by an edit, how fast at most, and how long they live.
const SPARKS: usize = 36;
const SPARK_SPEED: f32 = 3.2;
const SPARK_LIFE: f32 = 1.4;

/// The camera's nearest to what it looks at: closer, a star would pass
/// through it and its glow fill the window.
const NEAREST: f32 = 3.5;

/// The far stars of the background.
const SKY_STARS: usize = 1_400;

/// The layer the links are drawn on: seen by the universe's camera only, so
/// that the panels' camera does not draw them flattened.
const LINKS_LAYER: usize = 1;

/// How the universe is shown, read and written by the panels and by the
/// systems that draw it.
#[derive(Resource)]
pub(crate) struct Universe {
    /// Whether it shows, as the state says.
    pub(crate) shown: bool,
    root: PathBuf,
    map: Option<CodeMap>,
    layout: Option<Layout>,
    /// The star of each node, in the map's order.
    stars: Vec<Entity>,
    /// How much each node glows above its rest, and in which colour.
    flash: Vec<(f32, LinearRgba)>,
    /// How many transcript entries were looked at for the agent's work.
    seen: usize,
    /// The map was just built: the transcript so far is not replayed.
    fresh: bool,
    /// The camera: around which point, from which angles, how far.
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
    /// How far the camera is heading: it glides there.
    target_distance: f32,
    /// Whether the wheel set the distance since the view last framed
    /// something: it is then left alone.
    pub(crate) zoomed: bool,
    /// What the panels report of the mouse, for the next frame.
    pub(crate) drag: Vec2,
    pub(crate) zoom: f32,
    /// The star under the pointer, and the one chosen with a click.
    pub(crate) hovered: Option<usize>,
    pub(crate) chosen: Option<usize>,
    /// Where each node was on screen last frame, in logical pixels.
    pub(crate) on_screen: Vec<Option<Vec2>>,
    /// How much of the window's width the panels cover on the right: the
    /// view moves so that the universe is centred in what is left.
    pub(crate) covered: f32,
}

impl Universe {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            shown: false,
            root,
            map: None,
            layout: None,
            stars: Vec::new(),
            flash: Vec::new(),
            seen: 0,
            fresh: false,
            focus: Vec3::ZERO,
            yaw: 0.6,
            pitch: 0.35,
            distance: 90.0,
            target_distance: 90.0,
            zoomed: false,
            drag: Vec2::ZERO,
            zoom: 0.0,
            hovered: None,
            chosen: None,
            on_screen: Vec::new(),
            covered: 0.0,
        }
    }

    /// The map, once built.
    pub(crate) fn map(&self) -> Option<&CodeMap> {
        self.map.as_ref()
    }

    /// How much a node glows above its rest, from 0.
    pub(crate) fn glow(&self, node: usize) -> f32 {
        self.flash.get(node).map_or(0.0, |f| f.0)
    }

    /// The project's root, to open a chosen file from.
    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }
}

/// Marks what belongs to the universe, to show or hide it all at once.
#[derive(Component)]
pub(crate) struct InUniverse;

/// The universe's camera.
#[derive(Component)]
pub(crate) struct UniverseCamera;

/// A spark thrown by an edit: where it goes, and how long it has left.
#[derive(Component)]
pub(crate) struct Spark {
    velocity: Vec3,
    life: f32,
}

/// Shared meshes and the sparks' material.
#[derive(Resource)]
pub(crate) struct Assets3d {
    sphere: Handle<Mesh>,
    spark: Handle<StandardMaterial>,
}

/// Sets up the camera, the links' look and the sky, all hidden until shown.
pub(crate) fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut gizmos: ResMut<GizmoConfigStore>,
) {
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
            is_active: false,
            clear_color: ClearColorConfig::Custom(Color::srgb(0.008, 0.01, 0.02)),
            ..default()
        },
        bevy::camera::Hdr,
        Tonemapping::AcesFitted,
        // Stronger than the natural look: stars should shine.
        Bloom {
            intensity: 0.34,
            ..Bloom::NATURAL
        },
        RenderLayers::from_layers(&[0, LINKS_LAYER]),
        Transform::from_xyz(0.0, 0.0, 90.0).looking_at(Vec3::ZERO, Vec3::Y),
        UniverseCamera,
    ));
    let (config, _) = gizmos.config_mut::<DefaultGizmoConfigGroup>();
    config.render_layers = RenderLayers::layer(LINKS_LAYER);
    config.line.width = 1.4;

    let sphere = meshes.add(Sphere::new(1.0).mesh().uv(24, 16));
    let spark = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive: LinearRgba::rgb(9.0, 3.6, 0.8),
        ..default()
    });
    // The sky: faint stars far off, placed the same each time.
    let mut seed = 0x2545_f491_u32;
    let mut random = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let dim = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive: LinearRgba::rgb(0.9, 0.95, 1.2),
        ..default()
    });
    let bright = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive: LinearRgba::rgb(3.0, 3.2, 4.0),
        ..default()
    });
    for i in 0..SKY_STARS {
        let theta = random() * std::f32::consts::TAU;
        let y = random() * 2.0 - 1.0;
        let r = (1.0 - y * y).sqrt();
        let far = 160.0 + random() * 120.0;
        let at = Vec3::new(theta.cos() * r, y, theta.sin() * r) * far;
        let size = 0.08 + random() * random() * 0.35;
        commands.spawn((
            Mesh3d(sphere.clone()),
            MeshMaterial3d(if i % 9 == 0 {
                bright.clone()
            } else {
                dim.clone()
            }),
            Transform::from_translation(at).with_scale(Vec3::splat(size)),
            Visibility::Hidden,
            InUniverse,
        ));
    }
    commands.insert_resource(Assets3d { sphere, spark });
}

/// `colour` made `brightness` times brighter, in HDR where above 1 blooms,
/// with an alpha of its own: scaling a colour scales its alpha too.
fn hdr(colour: LinearRgba, brightness: f32, alpha: f32) -> LinearRgba {
    LinearRgba::new(
        colour.red * brightness,
        colour.green * brightness,
        colour.blue * brightness,
        alpha,
    )
}

/// The colour of a node, as a direction in HDR: brightness comes after.
fn colour(kind: NodeKind) -> LinearRgba {
    match kind {
        NodeKind::Folder => LinearRgba::rgb(0.55, 0.35, 1.0),
        NodeKind::File { language, .. } => match language {
            Language::Rust => LinearRgba::rgb(1.0, 0.42, 0.18),
            Language::Python => LinearRgba::rgb(0.25, 0.65, 1.0),
            Language::TypeScript => LinearRgba::rgb(0.2, 0.5, 1.0),
            Language::JavaScript => LinearRgba::rgb(1.0, 0.85, 0.25),
            Language::Markdown => LinearRgba::rgb(0.85, 0.9, 1.0),
            Language::Config => LinearRgba::rgb(0.2, 0.95, 0.75),
            Language::Other => LinearRgba::rgb(0.5, 0.55, 0.65),
        },
    }
}

/// The colour of a node, for the panels' legend and labels.
pub(crate) fn srgb(kind: NodeKind) -> [u8; 3] {
    let c = Color::from(colour(kind)).to_srgba();
    [
        (c.red * 255.0) as u8,
        (c.green * 255.0) as u8,
        (c.blue * 255.0) as u8,
    ]
}

/// How big a node's star is: files by their length, the root the largest.
fn size(node: &ironquill_codemap::Node) -> f32 {
    match node.kind {
        NodeKind::Folder if node.depth == 0 => 0.5,
        NodeKind::Folder => 0.2,
        NodeKind::File { lines, .. } => 0.11 + 0.07 * (1.0 + lines as f32 / 25.0).ln(),
    }
}

/// Builds the map and the stars the first time the universe shows, and
/// shows or hides the scene with it.
pub(crate) fn show_or_hide(
    mut commands: Commands,
    mut universe: ResMut<Universe>,
    assets: Option<Res<Assets3d>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut camera: Query<&mut Camera, With<UniverseCamera>>,
    mut parts: Query<&mut Visibility, With<InUniverse>>,
) {
    let shown = universe.shown;
    if let Ok(mut camera) = camera.single_mut()
        && camera.is_active != shown
    {
        camera.is_active = shown;
        for mut visibility in &mut parts {
            *visibility = if shown {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
        if shown {
            // Each time it opens, the camera glides in from afar.
            universe.distance = 90.0;
            universe.zoomed = false;
        }
    }
    let Some(assets) = assets else {
        return;
    };
    if !shown || universe.map.is_some() {
        return;
    }
    let map = ironquill_codemap::map(&universe.root, MAX_FILES);
    let layout = Layout::new(&map);
    let stars = map
        .nodes
        .iter()
        .zip(layout.positions())
        .map(|(node, at)| {
            let material = materials.add(StandardMaterial {
                base_color: Color::BLACK,
                emissive: hdr(colour(node.kind), REST, 1.0),
                ..default()
            });
            commands
                .spawn((
                    Mesh3d(assets.sphere.clone()),
                    MeshMaterial3d(material),
                    Transform::from_translation(Vec3::from_array(*at))
                        .with_scale(Vec3::splat(size(node))),
                    Visibility::Visible,
                    InUniverse,
                ))
                .id()
        })
        .collect();
    universe.flash = vec![(0.0, LinearRgba::BLACK); map.nodes.len()];
    universe.on_screen = vec![None; map.nodes.len()];
    universe.stars = stars;
    universe.map = Some(map);
    universe.layout = Some(layout);
    universe.fresh = true;
}

/// Lights the files the agent read or edited since last frame, and throws
/// sparks from those it edited.
pub(crate) fn light_up(
    universe: &mut Universe,
    transcript: &[Entry],
    commands: &mut Commands,
    assets: Option<&Assets3d>,
) {
    // What the agent did before the universe was built is history: only
    // what it does from then on lights up.
    if universe.fresh {
        universe.fresh = false;
        universe.seen = transcript.len();
    }
    let from = universe.seen.min(transcript.len());
    universe.seen = transcript.len();
    let Some(map) = universe.map.as_ref() else {
        return;
    };
    let lit: Vec<(usize, bool)> = crate::activity::touched(&transcript[from..])
        .into_iter()
        .filter_map(|(path, edited)| Some((map.find(path)?, edited)))
        .collect();
    for (node, edited) in lit {
        universe.flash[node] = if edited {
            (EDIT_FLASH, LinearRgba::rgb(1.0, 0.55, 0.2))
        } else {
            (READ_FLASH, LinearRgba::rgb(0.35, 0.65, 1.0))
        };
        if edited && let Some(assets) = assets {
            let at = universe
                .layout
                .as_ref()
                .and_then(|l| l.positions().get(node).copied())
                .map_or(Vec3::ZERO, Vec3::from_array);
            for i in 0..SPARKS {
                // Spread evenly over a sphere, at speeds that vary.
                let t = (i as f32 + 0.5) / SPARKS as f32;
                let y = 1.0 - 2.0 * t;
                let r = (1.0 - y * y).sqrt();
                let a = i as f32 * 2.399_963;
                let direction = Vec3::new(a.cos() * r, y, a.sin() * r);
                let speed = SPARK_SPEED * (0.45 + 0.55 * ((i * 7) % 11) as f32 / 10.0);
                commands.spawn((
                    Mesh3d(assets.sphere.clone()),
                    MeshMaterial3d(assets.spark.clone()),
                    Transform::from_translation(at).with_scale(Vec3::splat(0.045)),
                    Spark {
                        velocity: direction * speed,
                        life: SPARK_LIFE,
                    },
                    InUniverse,
                ));
            }
        }
    }
}

/// What the camera's query reads and moves.
type CameraParts = (
    &'static mut Transform,
    &'static Camera,
    &'static GlobalTransform,
);

/// The universe's camera alone: no star, no spark.
type CameraOnly = (
    With<UniverseCamera>,
    Without<Spark>,
    Without<MeshMaterial3d<StandardMaterial>>,
);

/// Each frame while shown: the layout's next steps, the stars where it
/// put them, the glow fading, the sparks flying, the links, the camera.
#[allow(clippy::too_many_arguments)]
pub(crate) fn animate(
    mut commands: Commands,
    mut universe: ResMut<Universe>,
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut stars: Query<(&mut Transform, &MeshMaterial3d<StandardMaterial>), Without<Spark>>,
    mut sparks: Query<(Entity, &mut Transform, &mut Spark), Without<UniverseCamera>>,
    mut camera: Query<CameraParts, CameraOnly>,
    mut gizmos: Gizmos,
) {
    if !universe.shown {
        return;
    }
    let dt = time.delta_secs().min(0.1);
    let Universe {
        map,
        layout,
        stars: entities,
        flash,
        ..
    } = &mut *universe;
    let (Some(map), Some(layout)) = (map.as_ref(), layout.as_mut()) else {
        return;
    };
    if !layout.settled() {
        for _ in 0..STEPS_PER_FRAME {
            layout.step(map);
        }
    }
    let positions: Vec<Vec3> = layout
        .positions()
        .iter()
        .map(|p| Vec3::from_array(*p))
        .collect();

    for (node, entity) in entities.iter().enumerate() {
        let Ok((mut transform, material)) = stars.get_mut(*entity) else {
            continue;
        };
        transform.translation = positions[node];
        let (glow, tint) = &mut flash[node];
        if *glow > 0.0 {
            *glow = (*glow - *glow * FADE * dt - 0.05).max(0.0);
            if let Some(mut material) = materials.get_mut(&material.0) {
                let base = hdr(colour(map.nodes[node].kind), REST, 1.0);
                material.emissive = base + hdr(*tint, *glow, 0.0);
            }
            let swell = 1.0 + (*glow / EDIT_FLASH) * 1.6;
            transform.scale = Vec3::splat(size(&map.nodes[node]) * swell);
        }
    }

    for (entity, mut transform, mut spark) in &mut sparks {
        spark.life -= dt;
        if spark.life <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        transform.translation += spark.velocity * dt;
        spark.velocity *= 1.0 - 1.8 * dt;
        transform.scale = Vec3::splat(0.05 * spark.life / SPARK_LIFE + 0.005);
    }

    // The links: what holds what faint and violet, imports in the colour of
    // the file that imports, brighter when either end glows.
    for edge in &map.edges {
        let (a, b) = (positions[edge.from], positions[edge.to]);
        let lit = flash[edge.from].0.max(flash[edge.to].0) / EDIT_FLASH;
        let colour = match edge.kind {
            EdgeKind::Contains => hdr(
                LinearRgba::rgb(0.35, 0.25, 0.9),
                0.9 + lit * 4.0,
                0.25 + lit * 0.6,
            ),
            EdgeKind::Imports => hdr(
                colour(map.nodes[edge.from].kind),
                1.1 + lit * 6.0,
                0.32 + lit * 0.6,
            ),
            // Through the API, from the front end to the back: magenta.
            EdgeKind::Calls => hdr(
                LinearRgba::rgb(0.85, 0.35, 0.95),
                1.4 + lit * 6.0,
                0.45 + lit * 0.5,
            ),
        };
        gizmos.line(a, b, Color::from(colour));
    }

    // The camera: turned by dragging, drifting slowly otherwise, gliding in
    // to see the whole universe, or the star chosen.
    let drag = std::mem::take(&mut universe.drag);
    let zoom = std::mem::take(&mut universe.zoom);
    if drag == Vec2::ZERO {
        universe.yaw += 0.06 * dt;
    }
    universe.yaw -= drag.x * 0.006;
    universe.pitch = (universe.pitch + drag.y * 0.006).clamp(-1.4, 1.4);
    let extent = positions.iter().map(|p| p.length()).fold(1.0_f32, f32::max);
    let whole = (extent * 2.1).clamp(6.0, 120.0);
    let goal = match universe.chosen {
        Some(node) => (positions.get(node).copied().unwrap_or(Vec3::ZERO), 7.0),
        None => (Vec3::ZERO, whole),
    };
    // Until the wheel moves it, the camera frames the whole universe, or
    // the star chosen; after, it stays where the wheel left it. The wheel
    // scales the distance, so that a touchpad's burst cannot fly the camera
    // through the stars, where their glow would fill the window.
    if zoom != 0.0 {
        universe.zoomed = true;
        universe.target_distance *= (-zoom * 0.0015).clamp(-1.5, 1.5).exp();
    } else if !universe.zoomed {
        universe.target_distance = goal.1;
    }
    universe.target_distance = universe.target_distance.clamp(NEAREST, whole * 1.4);
    let ease = 1.0 - (-3.0 * dt).exp();
    universe.focus = universe.focus.lerp(goal.0, ease);
    universe.distance += (universe.target_distance - universe.distance) * ease;
    let Ok((mut transform, camera, global)) = camera.single_mut() else {
        return;
    };
    let offset = Vec3::new(
        universe.yaw.cos() * universe.pitch.cos(),
        universe.pitch.sin(),
        universe.yaw.sin() * universe.pitch.cos(),
    ) * universe.distance;
    let looking =
        Transform::from_translation(universe.focus + offset).looking_at(universe.focus, Vec3::Y);
    // Moved sideways, not turned: the universe stays upright and centred
    // in the part of the window the panels leave free.
    let aspect = camera
        .logical_viewport_size()
        .map_or(1.6, |size| size.x / size.y.max(1.0));
    let half_width = universe.distance * std::f32::consts::FRAC_PI_8.tan() * aspect;
    let shift = looking.right() * (universe.covered * half_width);
    *transform = looking.with_translation(looking.translation + shift);
    // Where each star is on screen, for the panels to label and pick.
    universe.on_screen = positions
        .iter()
        .map(|p| camera.world_to_viewport(global, *p).ok())
        .collect();
}

#[cfg(test)]
mod tests {
    use bevy::ecs::world::CommandQueue;
    use ironquill_codemap::Node;
    use ironquill_tools::ToolSummary;

    use super::*;

    fn universe() -> Universe {
        let node = |path: &str, kind| Node {
            path: path.into(),
            kind,
            depth: path.matches('/').count() + 1,
        };
        let file = NodeKind::File {
            language: Language::Rust,
            lines: 10,
        };
        let map = CodeMap {
            nodes: vec![
                node("", NodeKind::Folder),
                node("src/a.rs", file),
                node("src/b.rs", file),
            ],
            edges: Vec::new(),
            left_out: 0,
            operations: Vec::new(),
        };
        let mut universe = Universe::new(PathBuf::from("/p"));
        universe.layout = Some(Layout::new(&map));
        universe.flash = vec![(0.0, LinearRgba::BLACK); map.nodes.len()];
        universe.map = Some(map);
        universe.fresh = true;
        universe
    }

    fn edit(path: &str) -> Entry {
        Entry::Tool {
            name: "replace".into(),
            path: Some(path.into()),
            outcome: Ok(ToolSummary::Changed {
                path: path.into(),
                created: false,
                diff: Vec::new(),
            }),
        }
    }

    fn read(path: &str) -> Entry {
        Entry::Tool {
            name: "read_file".into(),
            path: Some(path.into()),
            outcome: Ok(ToolSummary::Read {
                path: path.into(),
                lines: 10,
            }),
        }
    }

    #[test]
    fn an_edit_flares_with_sparks_a_read_lights_up_and_history_stays_dark() {
        let mut world = World::new();
        let mut queue = CommandQueue::default();
        let assets = Assets3d {
            sphere: Handle::default(),
            spark: Handle::default(),
        };
        let mut universe = universe();
        let mut transcript = vec![edit("src/b.rs")];
        {
            let mut commands = Commands::new(&mut queue, &world);
            // Built after that edit: it is history, and stays dark.
            light_up(&mut universe, &transcript, &mut commands, Some(&assets));
        }
        assert!(universe.glow(2) == 0.0);

        transcript.push(edit("src/a.rs"));
        transcript.push(read("src/b.rs"));
        {
            let mut commands = Commands::new(&mut queue, &world);
            light_up(&mut universe, &transcript, &mut commands, Some(&assets));
        }
        queue.apply(&mut world);
        assert!((universe.glow(1) - EDIT_FLASH).abs() < f32::EPSILON);
        assert!((universe.glow(2) - READ_FLASH).abs() < f32::EPSILON);
        assert!(universe.glow(0) == 0.0);
        let sparks = world.query::<&Spark>().iter(&world).count();
        assert_eq!(sparks, SPARKS);

        // Nothing new: nothing more.
        {
            let mut commands = Commands::new(&mut queue, &world);
            light_up(&mut universe, &transcript, &mut commands, Some(&assets));
        }
        queue.apply(&mut world);
        assert_eq!(world.query::<&Spark>().iter(&world).count(), SPARKS);
    }
}
