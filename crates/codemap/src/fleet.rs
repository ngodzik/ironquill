//! Every environment at once, for the views that draw them side by side:
//! each a galaxy, each of its releases a star, each object a release
//! deploys a piece of it, with what it says of itself and how it acts on
//! the others.
//!
//! Nothing is run here: it is assembled from what [`crate::deploy`] read
//! and what [`crate::render`] built, and an environment not rendered yet
//! still shows its releases, their images read from their settings.

use serde::{Deserialize, Serialize};

use crate::deploy::{Chart, Deploy, Environment, Release, Setting};
use crate::render::{Object, Rendered};
use crate::repos::Place;

/// Folders that are only ever included, as [`crate::deploy`] reads them.
const BASES: [&str; 3] = ["base", "bases", "components"];

/// Every environment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    /// One per environment, in the deployment's order: those the settings
    /// name first.
    pub galaxies: Vec<Galaxy>,
}

/// An environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Galaxy {
    /// Its name.
    pub name: String,
    /// Whether the settings name it.
    pub named: bool,
    /// Whether it was rendered: its stars then have their objects.
    pub rendered: bool,
    /// Its releases.
    pub stars: Vec<Star>,
}

/// A release of an environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Star {
    /// Its name, as the environment labels it.
    pub name: String,
    /// The namespace it is installed in.
    pub namespace: Option<String>,
    /// Its chart's name.
    pub chart: Option<String>,
    /// The images it runs, its own first.
    pub images: Vec<String>,
    /// The objects it deploys, once rendered.
    pub pieces: Vec<Piece>,
    /// Its settings, each with every layer that set it.
    pub settings: Vec<Valued>,
    /// Where it is declared.
    pub place: Place,
}

/// An object a release deploys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    /// Its kind.
    pub kind: String,
    /// Its name.
    pub name: String,
    /// Its namespace.
    pub namespace: Option<String>,
    /// The template or the file it was declared in.
    pub template: Option<Place>,
    /// What changed it after: patches, transformers.
    pub patches: Vec<Place>,
    /// What it says of itself, the main fact first.
    pub facts: Vec<String>,
    /// The object as rendered, a secret's values hidden.
    pub body: serde_json::Value,
}

/// A setting and every layer that set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Valued {
    /// Its key, dotted: `image.tag`.
    pub key: String,
    /// The layers, the lowest first: the last one's value is the one kept.
    pub layers: Vec<Layer>,
}

impl Valued {
    /// The value kept.
    #[must_use]
    pub fn value(&self) -> &str {
        self.layers.last().map_or("", |l| l.value.as_str())
    }

    /// The kind of layer that set the value kept.
    #[must_use]
    pub fn won_by(&self) -> Option<LayerKind> {
        self.layers.last().map(|l| l.kind)
    }
}

/// One layer's value of a setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// What kind of layer it is.
    pub kind: LayerKind,
    /// The value it sets.
    pub value: String,
    /// Where.
    pub place: Place,
}

/// What kind of layer sets a value, from the chart's default up: told by
/// where it is written, by the tools' conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LayerKind {
    /// Inside the chart's folder: its default.
    Chart,
    /// In a `base`, `bases` or `components` folder, shared by environments.
    Base,
    /// The environment's own.
    Environment,
    /// A file named as a patch.
    Patch,
}

impl LayerKind {
    /// Every kind, the lowest first.
    pub const ALL: [Self; 4] = [Self::Chart, Self::Base, Self::Environment, Self::Patch];

    /// Its name, as shown.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Chart => "chart default",
            Self::Base => "base",
            Self::Environment => "environment",
            Self::Patch => "patch",
        }
    }

    /// The kind of layer written at `place`, the charts read telling which
    /// folders are a chart's.
    #[must_use]
    pub fn of(place: &Place, charts: &[Chart]) -> Self {
        let inside = |c: &Chart| {
            c.repo == place.repo
                && (c.folder.is_empty() || place.path.starts_with(&format!("{}/", c.folder)))
        };
        if charts.iter().any(inside) {
            return Self::Chart;
        }
        let name = place.path.rsplit('/').next().unwrap_or(&place.path);
        if name.to_lowercase().contains("patch") {
            return Self::Patch;
        }
        if place.path.split('/').any(|part| BASES.contains(&part)) {
            return Self::Base;
        }
        Self::Environment
    }
}

impl Star {
    /// How many settings each kind of layer won, the lowest kind first.
    #[must_use]
    pub fn won_by(&self) -> [(LayerKind, usize); 4] {
        LayerKind::ALL.map(|kind| {
            let won = self
                .settings
                .iter()
                .filter(|s| s.won_by() == Some(kind))
                .count();
            (kind, won)
        })
    }

    /// The deepest kind of layer that set any of its values.
    #[must_use]
    pub fn deepest(&self) -> Option<LayerKind> {
        self.settings.iter().filter_map(Valued::won_by).max()
    }
}

/// Assembles every environment of `deploy`, each with what `rendered`
/// gives of it, when it was rendered.
#[must_use]
pub fn fleet<'a>(
    deploy: &Deploy,
    rendered: impl Fn(&Environment) -> Option<&'a Rendered>,
) -> Fleet {
    let galaxies = deploy
        .environments
        .iter()
        .map(|environment| {
            let rendered = rendered(environment);
            Galaxy {
                name: environment.name.clone(),
                named: environment.chosen,
                rendered: rendered.is_some(),
                stars: environment
                    .releases
                    .iter()
                    .map(|release| star(release, rendered, &deploy.charts))
                    .collect(),
            }
        })
        .collect();
    Fleet { galaxies }
}

fn star(release: &Release, rendered: Option<&Rendered>, charts: &[Chart]) -> Star {
    let pieces: Vec<Piece> = rendered
        .map(|r| {
            r.objects
                .iter()
                .filter(|o| o.service.as_deref() == Some(release.label.as_str()))
                .map(piece)
                .collect()
        })
        .unwrap_or_default();
    let settings: Vec<Valued> = release
        .settings
        .iter()
        .map(|setting| valued(setting, charts))
        .collect();
    let mut images: Vec<String> = Vec::new();
    for piece in &pieces {
        for image in images_of(&piece.body) {
            if !images.contains(&image) {
                images.push(image);
            }
        }
    }
    if rendered.is_none() {
        images = images_in_settings(&release.settings);
    }
    Star {
        name: release.label.clone(),
        namespace: release.namespace.clone(),
        chart: release.chart_name.clone(),
        images,
        pieces,
        settings,
        place: release.place.clone(),
    }
}

fn piece(object: &Object) -> Piece {
    Piece {
        kind: object.kind.clone(),
        name: object.name.clone(),
        namespace: object.namespace.clone(),
        template: object.origin.clone(),
        patches: object.changed_by.clone(),
        facts: facts(&object.body),
        body: object.body.clone(),
    }
}

/// A setting's layers, the lowest first, each told its kind.
fn valued(setting: &Setting, charts: &[Chart]) -> Valued {
    let layers = setting
        .replaced
        .iter()
        .rev()
        .map(|e| (&e.value, &e.place))
        .chain(std::iter::once((&setting.value, &setting.place)))
        .map(|(value, place)| Layer {
            kind: LayerKind::of(place, charts),
            value: value.clone(),
            place: place.clone(),
        })
        .collect();
    Valued {
        key: setting.key.clone(),
        layers,
    }
}

/// The pod spec of a workload, where its containers are.
fn pod_spec(object: &serde_json::Value) -> Option<&serde_json::Value> {
    let spec = &object["spec"];
    let pod = match object["kind"].as_str()? {
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" => {
            &spec["template"]["spec"]
        }
        "CronJob" => &spec["jobTemplate"]["spec"]["template"]["spec"],
        "Pod" => spec,
        _ => return None,
    };
    pod.is_object().then_some(pod)
}

/// The labels a workload's pods carry.
fn pod_labels(object: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match object["kind"].as_str()? {
        "Pod" => object["metadata"]["labels"].as_object(),
        "CronJob" => {
            object["spec"]["jobTemplate"]["spec"]["template"]["metadata"]["labels"].as_object()
        }
        _ => object["spec"]["template"]["metadata"]["labels"].as_object(),
    }
}

/// A workload's containers, its main ones first, then those that start
/// before them.
fn containers(object: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    let pod = pod_spec(object);
    ["containers", "initContainers"]
        .into_iter()
        .filter_map(move |key| pod?[key].as_array())
        .flatten()
}

/// The images a rendered workload runs, its main containers' first.
fn images_of(object: &serde_json::Value) -> Vec<String> {
    let mut images: Vec<String> = Vec::new();
    for image in containers(object).filter_map(|c| c["image"].as_str()) {
        if !images.iter().any(|i| i == image) {
            images.push(image.to_owned());
        }
    }
    images
}

/// The images settings name: `image.repository` with its `image.tag`, at
/// the top or under any key.
fn images_in_settings(settings: &[Setting]) -> Vec<String> {
    let mut images = Vec::new();
    for setting in settings {
        let Some(prefix) = setting.key.strip_suffix("image.repository") else {
            continue;
        };
        if !(prefix.is_empty() || prefix.ends_with('.')) || setting.value.is_empty() {
            continue;
        }
        let tag = settings
            .iter()
            .find(|s| s.key == format!("{prefix}image.tag"))
            .map(|s| s.value.as_str())
            .filter(|t| !t.is_empty());
        let image = match tag {
            Some(tag) => format!("{}:{tag}", setting.value),
            None => setting.value.clone(),
        };
        if !images.contains(&image) {
            images.push(image);
        }
    }
    images
}

/// A JSON scalar as text.
fn text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// What an object says of itself, by its kind, the main fact first: its
/// replicas, its containers' ports and resources, a Service's ports, an
/// Ingress's hosts, an autoscaler's range, a schedule, how many keys a
/// ConfigMap or Secret holds (never their values), a ServiceAccount's
/// cloud role.
///
/// # Examples
///
/// ```
/// let service = serde_json::json!({
///     "kind": "Service",
///     "spec": {"type": "ClusterIP", "ports": [{"port": 80, "targetPort": 8000}]}
/// });
/// assert_eq!(universe_codemap::facts(&service), ["ClusterIP 80→8000"]);
/// ```
#[must_use]
pub fn facts(object: &serde_json::Value) -> Vec<String> {
    let spec = &object["spec"];
    let mut facts = Vec::new();
    match object["kind"].as_str().unwrap_or("") {
        "Service" => {
            let ports: Vec<String> = spec["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let port = text(&p["port"])?;
                    Some(match text(&p["targetPort"]) {
                        Some(target) if target != port => format!("{port}→{target}"),
                        _ => port,
                    })
                })
                .collect();
            let kind = spec["type"].as_str().unwrap_or("ClusterIP");
            facts.push(if ports.is_empty() {
                kind.to_owned()
            } else {
                format!("{kind} {}", ports.join(", "))
            });
        }
        "Ingress" => {
            let hosts: Vec<&str> = spec["rules"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["host"].as_str())
                .collect();
            if !hosts.is_empty() {
                facts.push(hosts.join(", "));
            }
            let class = spec["ingressClassName"].as_str().or_else(|| {
                object["metadata"]["annotations"]["kubernetes.io/ingress.class"].as_str()
            });
            if let Some(class) = class {
                facts.push(format!("class {class}"));
            }
        }
        "HorizontalPodAutoscaler" => {
            let least = text(&spec["minReplicas"]).unwrap_or_else(|| "1".to_owned());
            if let Some(most) = text(&spec["maxReplicas"]) {
                facts.push(format!("{least} to {most} replicas"));
            }
        }
        "CronJob" => {
            if let Some(schedule) = spec["schedule"].as_str() {
                facts.push(format!("at {schedule}"));
            }
        }
        "ConfigMap" | "Secret" => {
            let keys: usize = ["data", "stringData", "binaryData"]
                .iter()
                .filter_map(|k| object[*k].as_object())
                .map(serde_json::Map::len)
                .sum();
            facts.push(format!("{keys} key{}", if keys == 1 { "" } else { "s" }));
        }
        "ServiceAccount" => {
            let annotations = &object["metadata"]["annotations"];
            for key in [
                "eks.amazonaws.com/role-arn",
                "iam.gke.io/gcp-service-account",
                "azure.workload.identity/client-id",
            ] {
                if let Some(role) = annotations[key].as_str() {
                    facts.push(format!("cloud role {role}"));
                }
            }
        }
        "PodDisruptionBudget" => {
            for key in ["minAvailable", "maxUnavailable"] {
                if let Some(value) = text(&spec[key]) {
                    let name = if key == "minAvailable" {
                        "at least"
                    } else {
                        "at most"
                    };
                    let down = if key == "minAvailable" { "up" } else { "down" };
                    facts.push(format!("{name} {value} {down}"));
                }
            }
        }
        _ => {}
    }
    if pod_spec(object).is_some() {
        if let Some(replicas) = text(&spec["replicas"]) {
            facts.insert(
                0,
                format!(
                    "{replicas} replica{}",
                    if replicas == "1" { "" } else { "s" }
                ),
            );
        }
        for container in containers(object) {
            let name = container["name"].as_str().unwrap_or("container");
            let ports: Vec<String> = container["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| text(&p["containerPort"]))
                .collect();
            if !ports.is_empty() {
                facts.push(format!(
                    "{name}: port{} {}",
                    if ports.len() == 1 { "" } else { "s" },
                    ports.join(", ")
                ));
            }
            let resources = &container["resources"];
            let range = |what: &str| {
                let asked = text(&resources["requests"][what]);
                let most = text(&resources["limits"][what]);
                match (asked, most) {
                    (Some(a), Some(m)) if a != m => Some(format!("{a} to {m}")),
                    (Some(a), _) => Some(a),
                    (None, Some(m)) => Some(format!("up to {m}")),
                    (None, None) => None,
                }
            };
            let used: Vec<String> = [("CPU", "cpu"), ("memory", "memory")]
                .into_iter()
                .filter_map(|(label, key)| Some(format!("{label} {}", range(key)?)))
                .collect();
            if !used.is_empty() {
                facts.push(format!("{name}: {}", used.join(", ")));
            }
        }
    }
    facts
}

/// How one object acts on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Acts {
    /// An Ingress routes to a Service.
    Routes,
    /// A Service sends to the workloads its selector matches.
    Selects,
    /// An autoscaler scales its target.
    Scales,
    /// A disruption budget protects the pods it selects.
    Protects,
    /// A workload reads a ConfigMap or a Secret.
    Reads,
    /// A workload runs as a ServiceAccount.
    RunsAs,
    /// A workload mounts a claim.
    Mounts,
}

impl Acts {
    /// What it reads as, between the two objects' names.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Routes => "routes to",
            Self::Selects => "selects",
            Self::Scales => "scales",
            Self::Protects => "protects",
            Self::Reads => "reads",
            Self::RunsAs => "runs as",
            Self::Mounts => "mounts",
        }
    }
}

/// One object acting on another, by their places in the list given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Interaction {
    /// The one that acts.
    pub from: usize,
    /// The one acted on.
    pub to: usize,
    /// How.
    pub how: Acts,
}

/// Whether every label `wanted` asks for is in `labels`; an empty
/// selector selects nothing, as no one writes one to select everything.
fn selects(
    wanted: Option<&serde_json::Map<String, serde_json::Value>>,
    labels: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    match (wanted, labels) {
        (Some(wanted), Some(labels)) if !wanted.is_empty() => {
            wanted.iter().all(|(k, v)| labels.get(k) == Some(v))
        }
        _ => false,
    }
}

/// What a workload reads, runs as and mounts, by kind and name.
fn uses(object: &serde_json::Value) -> Vec<(Acts, &'static str, String)> {
    let mut used = Vec::new();
    let Some(pod) = pod_spec(object) else {
        return used;
    };
    let mut add = |how, kind, name: Option<&str>| {
        if let Some(name) = name {
            let entry = (how, kind, name.to_owned());
            if !used.contains(&entry) {
                used.push(entry);
            }
        }
    };
    for volume in pod["volumes"].as_array().into_iter().flatten() {
        add(
            Acts::Reads,
            "ConfigMap",
            volume["configMap"]["name"].as_str(),
        );
        add(
            Acts::Reads,
            "Secret",
            volume["secret"]["secretName"].as_str(),
        );
        for source in volume["projected"]["sources"]
            .as_array()
            .into_iter()
            .flatten()
        {
            add(
                Acts::Reads,
                "ConfigMap",
                source["configMap"]["name"].as_str(),
            );
            add(Acts::Reads, "Secret", source["secret"]["name"].as_str());
        }
        add(
            Acts::Mounts,
            "PersistentVolumeClaim",
            volume["persistentVolumeClaim"]["claimName"].as_str(),
        );
    }
    for container in containers(object) {
        for from in container["envFrom"].as_array().into_iter().flatten() {
            add(
                Acts::Reads,
                "ConfigMap",
                from["configMapRef"]["name"].as_str(),
            );
            add(Acts::Reads, "Secret", from["secretRef"]["name"].as_str());
        }
        for env in container["env"].as_array().into_iter().flatten() {
            let from = &env["valueFrom"];
            add(
                Acts::Reads,
                "ConfigMap",
                from["configMapKeyRef"]["name"].as_str(),
            );
            add(Acts::Reads, "Secret", from["secretKeyRef"]["name"].as_str());
        }
    }
    add(
        Acts::RunsAs,
        "ServiceAccount",
        pod["serviceAccountName"]
            .as_str()
            .or_else(|| pod["serviceAccount"].as_str()),
    );
    used
}

/// How the objects of a service act on each other: a Service selects the
/// workloads its selector matches, an Ingress routes to Services, an
/// autoscaler scales its target, a disruption budget protects the pods it
/// selects, a workload reads ConfigMaps and Secrets, runs as a
/// ServiceAccount and mounts claims. Only between objects of the same
/// namespace, as Kubernetes resolves them.
#[must_use]
pub fn interactions(objects: &[&serde_json::Value]) -> Vec<Interaction> {
    let kind = |o: &serde_json::Value| o["kind"].as_str().unwrap_or("").to_owned();
    let name = |o: &serde_json::Value| o["metadata"]["name"].as_str().unwrap_or("").to_owned();
    let namespace = |o: &serde_json::Value| o["metadata"]["namespace"].as_str().map(str::to_owned);
    let find = |k: &str, n: &str, ns: &Option<String>| {
        objects
            .iter()
            .position(|o| kind(o) == k && name(o) == n && namespace(o) == *ns)
    };
    let mut found: Vec<Interaction> = Vec::new();
    let mut add = |from, to: Option<usize>, how| {
        if let Some(to) = to {
            let interaction = Interaction { from, to, how };
            if from != to && !found.contains(&interaction) {
                found.push(interaction);
            }
        }
    };
    for (from, object) in objects.iter().enumerate() {
        let ns = namespace(object);
        let spec = &object["spec"];
        let workloads = || {
            objects
                .iter()
                .enumerate()
                .filter(|(_, o)| pod_spec(o).is_some() && namespace(o) == ns)
        };
        match kind(object).as_str() {
            "Service" => {
                for (to, workload) in workloads() {
                    if selects(spec["selector"].as_object(), pod_labels(workload)) {
                        add(from, Some(to), Acts::Selects);
                    }
                }
            }
            "Ingress" => {
                let rules = spec["rules"].as_array().into_iter().flatten();
                let backends = rules
                    .flat_map(|r| r["http"]["paths"].as_array().into_iter().flatten())
                    .map(|p| &p["backend"])
                    .chain(std::iter::once(&spec["defaultBackend"]))
                    .chain(std::iter::once(&spec["backend"]));
                for backend in backends {
                    let service = backend["service"]["name"]
                        .as_str()
                        .or_else(|| backend["serviceName"].as_str());
                    if let Some(service) = service {
                        add(from, find("Service", service, &ns), Acts::Routes);
                    }
                }
            }
            "HorizontalPodAutoscaler" => {
                let target = &spec["scaleTargetRef"];
                if let (Some(k), Some(n)) = (target["kind"].as_str(), target["name"].as_str()) {
                    add(from, find(k, n, &ns), Acts::Scales);
                }
            }
            "PodDisruptionBudget" => {
                for (to, workload) in workloads() {
                    if selects(
                        spec["selector"]["matchLabels"].as_object(),
                        pod_labels(workload),
                    ) {
                        add(from, Some(to), Acts::Protects);
                    }
                }
            }
            _ => {
                for (how, k, n) in uses(object) {
                    add(from, find(k, &n, &ns), how);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::deploy::Earlier;

    fn chart() -> Chart {
        Chart {
            name: "api".into(),
            version: "1.0.0".into(),
            repo: "app".into(),
            folder: "charts/api".into(),
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn a_layer_is_told_by_where_it_is_written() {
        let charts = [chart()];
        let kind = |repo: &str, path: &str| LayerKind::of(&Place::new(repo, path, 1), &charts);
        assert_eq!(kind("app", "charts/api/values.yaml"), LayerKind::Chart);
        // The same path in another repository is not the chart's.
        assert_eq!(
            kind("ops", "charts/api/values.yaml"),
            LayerKind::Environment
        );
        assert_eq!(kind("ops", "apps/base/api.yaml"), LayerKind::Base);
        assert_eq!(kind("ops", "components/tls/values.yaml"), LayerKind::Base);
        assert_eq!(
            kind("ops", "apps/prod/replicas-patch.yaml"),
            LayerKind::Patch
        );
        assert_eq!(kind("ops", "apps/base/patch.yaml"), LayerKind::Patch);
        assert_eq!(kind("ops", "apps/prod/values.yaml"), LayerKind::Environment);
        assert_eq!(
            kind("app", "charts/apiserver/values.yaml"),
            LayerKind::Environment
        );
    }

    fn release(settings: Vec<Setting>) -> Release {
        Release {
            name: "api".into(),
            label: "api".into(),
            namespace: Some("shop".into()),
            chart_name: Some("api".into()),
            chart: Some(0),
            source: None,
            settings,
            values: json!({}),
            place: Place::new("ops", "apps/base/api.yaml", 1),
            notes: Vec::new(),
        }
    }

    fn setting(key: &str, value: &str, place: Place, replaced: Vec<Earlier>) -> Setting {
        Setting {
            key: key.into(),
            value: value.into(),
            place,
            replaced,
        }
    }

    fn rendered(objects: Vec<serde_json::Value>) -> Rendered {
        Rendered {
            objects: objects
                .into_iter()
                .map(|body| Object {
                    kind: body["kind"].as_str().unwrap().into(),
                    name: body["metadata"]["name"].as_str().unwrap().into(),
                    namespace: body["metadata"]["namespace"].as_str().map(str::to_owned),
                    service: Some("api".into()),
                    origin: Some(Place::new("app", "charts/api/templates/x.yaml", 0)),
                    changed_by: Vec::new(),
                    body,
                })
                .collect(),
            releases: vec!["api".into()],
            notes: Vec::new(),
        }
    }

    fn deploy(settings: Vec<Setting>) -> Deploy {
        Deploy {
            charts: vec![chart()],
            environments: vec![Environment {
                name: "prod".into(),
                chosen: true,
                parts: Vec::new(),
                releases: vec![release(settings)],
                manifests: Vec::new(),
                encrypted: Vec::new(),
                notes: Vec::new(),
                declared_in: None,
            }],
            notes: Vec::new(),
        }
    }

    #[test]
    fn images_from_the_rendered_workloads_else_from_the_settings() {
        let settings = vec![
            setting(
                "image.repository",
                "registry.example.com/api",
                Place::new("app", "charts/api/values.yaml", 2),
                Vec::new(),
            ),
            setting(
                "image.tag",
                "1.2.0",
                Place::new("ops", "apps/prod/patch.yaml", 4),
                vec![Earlier {
                    value: "latest".into(),
                    place: Place::new("app", "charts/api/values.yaml", 3),
                }],
            ),
            setting(
                "worker.image.repository",
                "registry.example.com/worker",
                Place::new("app", "charts/api/values.yaml", 9),
                Vec::new(),
            ),
            // Not an image: a key that only ends the same way.
            setting(
                "sidecarimage.repository",
                "registry.example.com/no",
                Place::new("app", "charts/api/values.yaml", 12),
                Vec::new(),
            ),
        ];
        let deploy = deploy(settings);
        let unrendered = fleet(&deploy, |_| None);
        let star = &unrendered.galaxies[0].stars[0];
        assert!(!unrendered.galaxies[0].rendered && unrendered.galaxies[0].named);
        assert_eq!(
            star.images,
            [
                "registry.example.com/api:1.2.0",
                "registry.example.com/worker"
            ]
        );
        // Layers kept in order, the lowest first, each told its kind.
        let tag = star.settings.iter().find(|s| s.key == "image.tag").unwrap();
        let layers: Vec<(LayerKind, &str)> = tag
            .layers
            .iter()
            .map(|l| (l.kind, l.value.as_str()))
            .collect();
        assert_eq!(
            layers,
            [(LayerKind::Chart, "latest"), (LayerKind::Patch, "1.2.0")]
        );
        assert_eq!(tag.value(), "1.2.0");
        assert_eq!(star.deepest(), Some(LayerKind::Patch));
        assert_eq!(
            star.won_by(),
            [
                (LayerKind::Chart, 3),
                (LayerKind::Base, 0),
                (LayerKind::Environment, 0),
                (LayerKind::Patch, 1)
            ]
        );

        let objects = rendered(vec![json!({
            "kind": "Deployment",
            "metadata": {"name": "api", "namespace": "shop"},
            "spec": {"template": {"spec": {
                "initContainers": [{"name": "migrate", "image": "registry.example.com/api:1.2.0"},
                                   {"name": "wait", "image": "busybox:1.36"}],
                "containers": [{"name": "api", "image": "registry.example.com/api:1.2.0"},
                               {"name": "proxy", "image": "envoy:1.30"}]
            }}}
        })]);
        let read = fleet(&deploy, |_| Some(&objects));
        let star = &read.galaxies[0].stars[0];
        assert!(read.galaxies[0].rendered);
        assert_eq!(
            star.images,
            [
                "registry.example.com/api:1.2.0",
                "envoy:1.30",
                "busybox:1.36"
            ]
        );
        assert_eq!(star.pieces.len(), 1);
        assert_eq!(star.pieces[0].template.as_ref().unwrap().repo, "app");
    }

    #[test]
    fn what_each_kind_says_of_itself() {
        let deployment = json!({
            "kind": "Deployment",
            "spec": {"replicas": 3, "template": {"spec": {"containers": [{
                "name": "api",
                "ports": [{"containerPort": 8000}, {"containerPort": 9090}],
                "resources": {"requests": {"cpu": "100m", "memory": "256Mi"},
                              "limits": {"cpu": "500m", "memory": "256Mi"}}
            }]}}}
        });
        assert_eq!(
            facts(&deployment),
            [
                "3 replicas",
                "api: ports 8000, 9090",
                "api: CPU 100m to 500m, memory 256Mi"
            ]
        );
        let ingress = json!({
            "kind": "Ingress",
            "spec": {"ingressClassName": "nginx",
                     "rules": [{"host": "shop.example.com"}, {"host": "api.example.com"}]}
        });
        assert_eq!(
            facts(&ingress),
            ["shop.example.com, api.example.com", "class nginx"]
        );
        let hpa = json!({"kind": "HorizontalPodAutoscaler",
                         "spec": {"minReplicas": 2, "maxReplicas": 10}});
        assert_eq!(facts(&hpa), ["2 to 10 replicas"]);
        let cron = json!({"kind": "CronJob", "spec": {"schedule": "0 3 * * *",
            "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "report"}]}}}}}});
        assert_eq!(facts(&cron), ["at 0 3 * * *"]);
        let secret = json!({"kind": "Secret", "data": {"a": "hidden"},
                            "stringData": {"b": "hidden"}});
        assert_eq!(facts(&secret), ["2 keys"]);
        let account = json!({"kind": "ServiceAccount", "metadata": {"annotations": {
            "eks.amazonaws.com/role-arn": "arn:aws:iam::123456789012:role/api"}}});
        assert_eq!(
            facts(&account),
            ["cloud role arn:aws:iam::123456789012:role/api"]
        );
        let service =
            json!({"kind": "Service", "spec": {"ports": [{"port": 443, "targetPort": 443}]}});
        assert_eq!(facts(&service), ["ClusterIP 443"]);
    }

    #[test]
    fn the_six_ways_a_typical_service_acts_on_itself_and_none_more() {
        let objects = [
            json!({"kind": "Ingress", "metadata": {"name": "api", "namespace": "shop"},
                   "spec": {"rules": [{"http": {"paths": [{"backend": {"service": {"name": "api"}}}]}}]}}),
            json!({"kind": "Service", "metadata": {"name": "api", "namespace": "shop"},
                   "spec": {"selector": {"app": "api"}}}),
            json!({"kind": "Deployment", "metadata": {"name": "api", "namespace": "shop"},
                   "spec": {"template": {
                       "metadata": {"labels": {"app": "api", "tier": "web"}},
                       "spec": {"serviceAccountName": "api",
                                "containers": [{"name": "api",
                                    "envFrom": [{"configMapRef": {"name": "api"}}],
                                    "env": [{"name": "TOKEN", "valueFrom": {"secretKeyRef": {"name": "elsewhere"}}}]}]}}}}),
            json!({"kind": "HorizontalPodAutoscaler", "metadata": {"name": "api", "namespace": "shop"},
                   "spec": {"scaleTargetRef": {"kind": "Deployment", "name": "api"}}}),
            json!({"kind": "PodDisruptionBudget", "metadata": {"name": "api", "namespace": "shop"},
                   "spec": {"selector": {"matchLabels": {"app": "api"}}}}),
            json!({"kind": "ConfigMap", "metadata": {"name": "api", "namespace": "shop"}}),
            json!({"kind": "ServiceAccount", "metadata": {"name": "api", "namespace": "shop"}}),
            // The same names in another namespace: never reached.
            json!({"kind": "ConfigMap", "metadata": {"name": "api", "namespace": "other"}}),
            json!({"kind": "Deployment", "metadata": {"name": "web", "namespace": "other"},
                   "spec": {"template": {"metadata": {"labels": {"app": "api"}}, "spec": {}}}}),
        ];
        let refs: Vec<&serde_json::Value> = objects.iter().collect();
        let mut found: Vec<(usize, usize, Acts)> = interactions(&refs)
            .into_iter()
            .map(|i| (i.from, i.to, i.how))
            .collect();
        found.sort_by_key(|(from, to, _)| (*from, *to));
        assert_eq!(
            found,
            [
                (0, 1, Acts::Routes),
                (1, 2, Acts::Selects),
                (2, 5, Acts::Reads),
                (2, 6, Acts::RunsAs),
                (3, 2, Acts::Scales),
                (4, 2, Acts::Protects),
            ]
        );
    }
}
