//! An environment's architecture, drawn from what the tools rendered and
//! what Terraform configures: the way in (DNS, firewall, load balancer),
//! the network and the cluster, the workloads and jobs in it, the cloud
//! resources they use, and the services outside. Every element says where
//! it was read; what could not be resolved is listed, not guessed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::deploy::{Environment, Release};
use crate::render::{Object, Rendered};
use crate::repos::Place;
use crate::services::Services;
use crate::terraform::{
    self, BlockKind, CloudKind, Quality, Terraform, TfBlock, configures, makes, words,
};

/// What an element of an architecture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ElementKind {
    /// The internet, where requests come from.
    Internet,
    /// The names the environment is reached by.
    Dns,
    /// A firewall in front: a web application firewall, an allow list.
    Firewall,
    /// A load balancer.
    LoadBalancer,
    /// The private network.
    Network,
    /// The cluster the workloads run in.
    Cluster,
    /// A workload: a deployment, a stateful set, a daemon set.
    Workload,
    /// A job, run once or on a schedule.
    Job,
    /// A database.
    Database,
    /// An object storage bucket.
    Bucket,
    /// A secret in a secret store.
    Secret,
    /// A role a workload takes.
    Role,
    /// A user pool.
    Users,
    /// A service outside.
    External,
}

impl ElementKind {
    /// How the kind reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Internet => "Internet",
            Self::Dns => "DNS",
            Self::Firewall => "Firewall",
            Self::LoadBalancer => "Load balancer",
            Self::Network => "Network",
            Self::Cluster => "Cluster",
            Self::Workload => "Workload",
            Self::Job => "Job",
            Self::Database => "Database",
            Self::Bucket => "Bucket",
            Self::Secret => "Secret",
            Self::Role => "Role",
            Self::Users => "Users",
            Self::External => "External",
        }
    }
}

/// An element of an architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    /// What it is.
    pub kind: ElementKind,
    /// Its name.
    pub name: String,
    /// One line on it.
    pub summary: String,
    /// What is known of it, in order.
    pub properties: Vec<(String, String)>,
    /// Where it is configured, the most telling first.
    pub places: Vec<Place>,
    /// Its namespace, for a workload or a job.
    pub namespace: Option<String>,
    /// The service it belongs to, by its label.
    pub service: Option<String>,
    /// Its replicas, least and most.
    pub replicas: Option<(u32, u32)>,
}

/// How two elements are linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkKind {
    /// Requests go from one to the other.
    Routes,
    /// One calls the other.
    Calls,
    /// One uses the other: reads, writes, connects.
    Uses,
    /// One takes the other's role.
    Assumes,
    /// One holds the other.
    Contains,
}

/// A link of an architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchLink {
    /// From, by its place in [`Deployment::elements`].
    pub from: usize,
    /// To.
    pub to: usize,
    /// How.
    pub kind: LinkKind,
    /// What it carries, when said.
    pub label: String,
}

/// An environment's architecture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    /// The environment's name.
    pub environment: String,
    /// The elements.
    pub elements: Vec<Element>,
    /// The links.
    pub links: Vec<ArchLink>,
    /// What could not be resolved.
    pub notes: Vec<String>,
}

impl Deployment {
    fn add(&mut self, element: Element) -> usize {
        if let Some(i) = self.elements.iter().position(|e| {
            e.kind == element.kind && e.name == element.name && e.namespace == element.namespace
        }) {
            for place in element.places {
                if !self.elements[i].places.contains(&place) {
                    self.elements[i].places.push(place);
                }
            }
            return i;
        }
        self.elements.push(element);
        self.elements.len() - 1
    }

    fn link(&mut self, from: usize, to: usize, kind: LinkKind, label: &str) {
        if from == to
            || self
                .links
                .iter()
                .any(|l| l.from == from && l.to == to && l.kind == kind)
        {
            return;
        }
        self.links.push(ArchLink {
            from,
            to,
            kind,
            label: label.to_owned(),
        });
    }

    /// The elements of `kind`.
    pub fn of(&self, kind: ElementKind) -> impl Iterator<Item = (usize, &Element)> {
        self.elements
            .iter()
            .enumerate()
            .filter(move |(_, e)| e.kind == kind)
    }
}

fn element(kind: ElementKind, name: &str, summary: impl Into<String>) -> Element {
    Element {
        kind,
        name: name.to_owned(),
        summary: summary.into(),
        properties: Vec::new(),
        places: Vec::new(),
        namespace: None,
        service: None,
        replicas: None,
    }
}

fn str_of<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(value, |v, k| v.get(k))?.as_str()
}

/// The architecture of `environment`, as `rendered` and `terraform` give
/// it, the links between services from `services`.
#[must_use]
pub fn deployment(
    environment: &Environment,
    rendered: &Rendered,
    terraform: &Terraform,
    services: &Services,
) -> Deployment {
    let mut deployment = Deployment {
        environment: environment.name.clone(),
        ..Deployment::default()
    };
    let near: Vec<String> = environment
        .parts
        .iter()
        .flat_map(|p| words(&format!("{} {}", p.folder, environment.name)))
        .collect();
    deployment.notes.extend(environment.notes.iter().cloned());
    deployment.notes.extend(rendered.notes.iter().cloned());

    // The units this environment runs from, with Terragrunt.
    let (own_units, all_units) = terraform.units_of(&near);
    let pick = |test: &dyn Fn(&TfBlock) -> bool| -> Option<usize> {
        all_units
            .iter()
            .copied()
            .find(|&u| test(&terraform.blocks[u]))
            .or_else(|| terraform.nearest(|b| test(b), &near))
    };

    // The network and the cluster: those its units depend on, else the
    // nearest Terraform.
    let network = pick(&is_network).map(|i| {
        let block = &terraform.blocks[i];
        let resolved = terraform.resolve(block, &near);
        let name = resolved
            .find(&["name", "network_name", "vpc_name"])
            .filter(|n| !n.contains("${"))
            .unwrap_or(&block.name)
            .to_owned();
        let mut e = element(ElementKind::Network, &name, "The private network");
        for key in [
            "cidr",
            "cidr_block",
            "ip_range",
            "ip_cidr_range",
            "region",
            "azs",
            "private_subnets",
            "public_subnets",
            "database_subnets",
        ] {
            if let Some(v) = resolved.find(&[key]) {
                e.properties.push((key.to_owned(), v.to_owned()));
            }
        }
        e.places.push(block.place.clone());
        deployment.add(e)
    });
    // The cluster Flux runs on: what the units that install it depend on.
    let cluster_block = environment
        .declared_in
        .as_ref()
        .and_then(|declared| {
            terraform
                .callers_of(declared)
                .into_iter()
                .find_map(|caller| {
                    terraform
                        .dependencies(caller)
                        .into_iter()
                        .find(|&d| is_cluster(&terraform.blocks[d]))
                })
        })
        .or_else(|| pick(&is_cluster));
    let workloads_found = rendered.objects.iter().any(|o| {
        matches!(
            o.kind.as_str(),
            "Deployment" | "StatefulSet" | "DaemonSet" | "CronJob" | "Job"
        )
    }) || !environment.releases.is_empty();
    let cluster = if cluster_block.is_some() || workloads_found {
        let mut e = element(ElementKind::Cluster, "cluster", "Where the workloads run");
        if let Some(i) = cluster_block {
            let block = &terraform.blocks[i];
            let resolved = terraform.resolve(block, &near);
            e.name = resolved
                .find(&["cluster_name", "name"])
                .filter(|n| !n.contains("${"))
                .unwrap_or(&block.name)
                .to_owned();
            for key in [
                "cluster_version",
                "kubernetes_version",
                "min_master_version",
                "version",
                "location",
                "region",
                "machine_type",
                "node_config.machine_type",
                "instance_types",
                "size",
                "node_count",
                "autoscaling.min_node_count",
                "autoscaling.max_node_count",
                "min_size",
                "max_size",
            ] {
                if let Some(v) = resolved.find(&[key]) {
                    e.properties.push((key.to_owned(), v.to_owned()));
                }
            }
            e.places.push(block.place.clone());
        }
        Some(deployment.add(e))
    } else {
        None
    };
    if let (Some(network), Some(cluster)) = (network, cluster) {
        deployment.link(network, cluster, LinkKind::Contains, "");
    }

    // Workloads and jobs, as rendered.
    let autoscalers: HashMap<(Option<&str>, &str), (u32, u32)> = rendered
        .objects
        .iter()
        .filter(|o| o.kind == "HorizontalPodAutoscaler")
        .filter_map(|o| {
            let target = str_of(&o.body, &["spec", "scaleTargetRef", "name"])?;
            let min = o.body["spec"]["minReplicas"].as_u64().unwrap_or(1) as u32;
            let max = o.body["spec"]["maxReplicas"]
                .as_u64()
                .unwrap_or(u64::from(min)) as u32;
            Some(((o.namespace.as_deref(), target), (min, max)))
        })
        .collect();
    let mut workload_of: Vec<(usize, &Object)> = Vec::new();
    for object in &rendered.objects {
        let kind = match object.kind.as_str() {
            "Deployment" | "StatefulSet" | "DaemonSet" => ElementKind::Workload,
            "CronJob" | "Job" => ElementKind::Job,
            _ => continue,
        };
        let pod = if object.kind == "CronJob" {
            &object.body["spec"]["jobTemplate"]["spec"]["template"]["spec"]
        } else {
            &object.body["spec"]["template"]["spec"]
        };
        let mut e = element(kind, &object.name, object.kind.clone());
        e.namespace.clone_from(&object.namespace);
        e.service.clone_from(&object.service);
        if kind == ElementKind::Workload {
            let replicas = object.body["spec"]["replicas"].as_u64().unwrap_or(1) as u32;
            let (min, max) = autoscalers
                .get(&(object.namespace.as_deref(), object.name.as_str()))
                .copied()
                .unwrap_or((replicas, replicas));
            e.replicas = Some((min, max));
            e.properties.push((
                "replicas".into(),
                if min == max {
                    min.to_string()
                } else {
                    format!("{min} to {max} (autoscaled)")
                },
            ));
        }
        if let Some(schedule) = str_of(&object.body, &["spec", "schedule"]) {
            e.properties.push(("schedule".into(), schedule.to_owned()));
        }
        for container in pod["containers"].as_array().into_iter().flatten() {
            let name = container["name"].as_str().unwrap_or("");
            if let Some(image) = container["image"].as_str() {
                e.properties
                    .push((format!("{name} image"), image.to_owned()));
            }
            for (what, path) in [("requests", "requests"), ("limits", "limits")] {
                if let Some(r) = container["resources"][path].as_object() {
                    let shown: Vec<String> = r
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "{k} {}",
                                v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                            )
                        })
                        .collect();
                    e.properties
                        .push((format!("{name} {what}"), shown.join(", ")));
                }
            }
            let ports: Vec<String> = container["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["containerPort"].as_u64().map(|n| n.to_string()))
                .collect();
            if !ports.is_empty() {
                e.properties
                    .push((format!("{name} ports"), ports.join(", ")));
            }
        }
        if let Some(account) = pod["serviceAccountName"].as_str() {
            e.properties
                .push(("service account".into(), account.to_owned()));
        }
        e.places.extend(object.origin.iter().cloned());
        e.places.extend(object.changed_by.iter().cloned());
        let i = deployment.add(e);
        if let Some(cluster) = cluster {
            deployment.link(cluster, i, LinkKind::Contains, "");
        }
        workload_of.push((i, object));
    }

    // Releases the tools could not render, drawn from their settings.
    let mut from_settings: Vec<Object> = Vec::new();
    for release in &environment.releases {
        if rendered.releases.contains(&release.label) {
            continue;
        }
        let i = deployment.add(unrendered(release));
        if let Some(cluster) = cluster {
            deployment.link(cluster, i, LinkKind::Contains, "");
        }
        let body: serde_json::Map<String, serde_json::Value> = release
            .settings
            .iter()
            .map(|s| {
                (
                    s.key.rsplit('.').next().unwrap_or(&s.key).to_owned(),
                    serde_json::Value::String(s.value.clone()),
                )
            })
            .collect();
        from_settings.push(Object {
            kind: "Settings".into(),
            name: release.name.clone(),
            namespace: release.namespace.clone(),
            service: Some(release.label.clone()),
            origin: Some(release.place.clone()),
            changed_by: Vec::new(),
            body: serde_json::Value::Object(body),
        });
    }

    // The way in.
    let internet = ingress(&mut deployment, rendered, &workload_of);
    let _ = internet;

    // The cloud resources the workloads name, matched to Terraform.
    let mut objects: Vec<Object> = rendered.objects.clone();
    objects.extend(from_settings);
    let accounts: HashMap<(Option<String>, String), String> = rendered
        .objects
        .iter()
        .filter(|o| o.kind == "ServiceAccount")
        .filter_map(|o| {
            let role = str_of(
                &o.body,
                &["metadata", "annotations", "eks.amazonaws.com/role-arn"],
            )?;
            Some(((o.namespace.clone(), o.name.clone()), role.to_owned()))
        })
        .collect();
    for id in terraform::cloud_ids(&objects) {
        let kind = match id.kind {
            CloudKind::Role => ElementKind::Role,
            CloudKind::Bucket => ElementKind::Bucket,
            CloudKind::Database => ElementKind::Database,
            CloudKind::Secret => ElementKind::Secret,
            CloudKind::Users => ElementKind::Users,
        };
        let mut e = element(kind, &id.name, kind.label());
        e.places.extend(id.place.iter().cloned());
        match configures(terraform, &id, &near) {
            Some((b, quality)) => describe(&mut e, terraform, b, quality, &near),
            None => deployment.notes.push(format!(
                "No Terraform block was found that makes the {} {}",
                kind.label().to_lowercase(),
                id.name
            )),
        }
        let target = deployment.add(e);
        if let Some(network) = network
            && kind == ElementKind::Database
        {
            deployment.link(network, target, LinkKind::Contains, "");
        }
        let users: Vec<usize> = workload_of
            .iter()
            .filter(|(_, o)| id.service.is_some() && o.service == id.service)
            .map(|(i, _)| *i)
            .chain(
                deployment
                    .elements
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        e.kind == ElementKind::Workload
                            && e.service.is_some()
                            && e.service == id.service
                    })
                    .map(|(i, _)| i),
            )
            .collect();
        for user in users {
            let assumes = kind == ElementKind::Role
                && accounts
                    .values()
                    .any(|arn| arn.ends_with(&format!("/{}", id.name)));
            deployment.link(
                user,
                target,
                if assumes {
                    LinkKind::Assumes
                } else {
                    LinkKind::Uses
                },
                "",
            );
        }
    }

    // What Terraform puts in the cluster for this environment: workloads
    // and jobs, secrets the workloads read, and what those hold.
    terraform_in_cluster(
        &mut deployment,
        terraform,
        &own_units,
        &all_units,
        &near,
        rendered,
        &workload_of,
        cluster,
        network,
    );

    // Services outside that the environment's services reach, and the
    // calls between its own.
    let workload_named = |name: &str, d: &Deployment| -> Vec<usize> {
        d.elements
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(e.kind, ElementKind::Workload | ElementKind::Job)
                    && (e.name == name
                        || e.service.as_deref() == Some(name)
                        || e.name.ends_with(&format!("-{name}")))
            })
            .map(|(i, _)| i)
            .collect()
    };
    for link in &services.links {
        let from = &services.services[link.from];
        let to = &services.services[link.to];
        let sources = workload_named(&from.name, &deployment);
        if sources.is_empty() {
            continue;
        }
        let targets = if to.external {
            let mut e = element(ElementKind::External, &to.name, "A service outside");
            e.places.extend(
                link.evidence
                    .iter()
                    .take(1)
                    .map(|ev| Place::new("", &ev.path, ev.line)),
            );
            vec![deployment.add(e)]
        } else {
            workload_named(&to.name, &deployment)
        };
        let label = format!("{:?}", link.via);
        for &s in &sources {
            for &t in &targets {
                deployment.link(s, t, LinkKind::Calls, &label);
            }
        }
    }
    if !environment.encrypted.is_empty() {
        deployment.notes.push(format!(
            "{} encrypted file{} named and not read",
            environment.encrypted.len(),
            if environment.encrypted.len() == 1 {
                " is"
            } else {
                "s are"
            }
        ));
    }
    deployment
}

/// Whether a block makes a private network.
fn is_network(b: &TfBlock) -> bool {
    let what = b.what.to_lowercase();
    matches!(
        what.as_str(),
        "aws_vpc" | "google_compute_network" | "digitalocean_vpc"
    ) || (b.kind == BlockKind::Module && (what.contains("vpc") || what.contains("network")))
}

/// Whether a block makes a Kubernetes cluster.
fn is_cluster(b: &TfBlock) -> bool {
    let what = b.what.to_lowercase();
    let module = what.rsplit('/').next().unwrap_or(&what);
    matches!(
        what.as_str(),
        "aws_eks_cluster" | "google_container_cluster" | "digitalocean_kubernetes_cluster"
    ) || (b.kind == BlockKind::Module
        && ["eks", "gke", "doks", "aks", "kubernetes-cluster"]
            .iter()
            .any(|k| module.split(['-', '_']).any(|w| w == *k) || module == *k))
}

/// Says which Terraform block configures `e`, and how sure that is.
fn describe(e: &mut Element, terraform: &Terraform, b: usize, quality: Quality, near: &[String]) {
    let block = &terraform.blocks[b];
    let resolved = terraform.resolve(block, near);
    e.properties.push((
        "configured by".into(),
        match block.kind {
            BlockKind::Module if block.place.path.ends_with("terragrunt.hcl") => {
                format!("unit {}", block.folder)
            }
            BlockKind::Module => format!("module {}", block.name),
            _ => format!("{} {}", block.what, block.name),
        },
    ));
    if block.kind == BlockKind::Module {
        e.properties
            .push(("module source".into(), block.what.clone()));
    }
    e.properties.push((
        "match".into(),
        match quality {
            Quality::Exact => "exact",
            Quality::Pattern => "by its pattern",
            Quality::Part => "by a shared part of its name",
            Quality::Nearest => "the nearest of its kind (its id only exists once applied)",
        }
        .into(),
    ));
    if e.kind == ElementKind::Database {
        for (label, keys) in [
            ("engine", &["engine", "database_version"][..]),
            ("engine version", &["engine_version", "version"][..]),
            (
                "instance class",
                &["instance_class", "class", "tier", "settings.tier", "size"][..],
            ),
            (
                "instances",
                &["count", "instance_count", "replica_count", "node_count"][..],
            ),
        ] {
            if let Some(v) = resolved.find(keys) {
                e.properties.push((label.into(), v.to_owned()));
            }
        }
    }
    e.places.insert(0, block.place.clone());
}

/// The kinds of Terraform resources that run something in the cluster.
const TERRAFORM_WORKLOADS: [&str; 9] = [
    "kubernetes_deployment",
    "kubernetes_deployment_v1",
    "kubernetes_stateful_set",
    "kubernetes_stateful_set_v1",
    "kubernetes_daemonset",
    "kubernetes_cron_job_v1",
    "kubernetes_job",
    "kubernetes_job_v1",
    "helm_release",
];

/// The secrets a workload reads: from its variables, all of one, or a
/// volume.
fn secrets_read(object: &Object) -> Vec<String> {
    let pod = if object.kind == "CronJob" {
        &object.body["spec"]["jobTemplate"]["spec"]["template"]["spec"]
    } else {
        &object.body["spec"]["template"]["spec"]
    };
    let mut out: Vec<String> = Vec::new();
    let mut add = |name: Option<&str>| {
        if let Some(name) = name
            && !out.iter().any(|n| n == name)
        {
            out.push(name.to_owned());
        }
    };
    for container in pod["containers"].as_array().into_iter().flatten() {
        for from in container["envFrom"].as_array().into_iter().flatten() {
            add(from["secretRef"]["name"].as_str());
        }
        for env in container["env"].as_array().into_iter().flatten() {
            add(env["valueFrom"]["secretKeyRef"]["name"].as_str());
        }
    }
    for volume in pod["volumes"].as_array().into_iter().flatten() {
        add(volume["secret"]["secretName"].as_str());
    }
    out
}

/// What Terraform puts in the cluster for an environment, from its units:
/// the workloads and jobs its own units deploy, each linked to the cloud
/// resources its unit depends on; and the secrets the rendered workloads
/// read that no manifest declares, found in the units that make them,
/// linked to what their values come from.
#[allow(clippy::too_many_arguments)]
fn terraform_in_cluster(
    deployment: &mut Deployment,
    terraform: &Terraform,
    own_units: &[usize],
    all_units: &[usize],
    near: &[String],
    rendered: &Rendered,
    workloads: &[(usize, &Object)],
    cluster: Option<usize>,
    network: Option<usize>,
) {
    // A unit a block depends on, as a cloud resource of the architecture.
    let resource = |deployment: &mut Deployment, unit: usize| -> Option<usize> {
        let block = &terraform.blocks[unit];
        let kind = [
            (CloudKind::Database, ElementKind::Database),
            (CloudKind::Bucket, ElementKind::Bucket),
            (CloudKind::Secret, ElementKind::Secret),
        ]
        .into_iter()
        .find(|(k, _)| makes(block, *k))?
        .1;
        let resolved = terraform.resolve(block, near);
        let name = resolved
            .find(&[
                "instance_name",
                "name",
                "bucket",
                "secret_id",
                "cluster_name",
            ])
            .filter(|n| !n.contains("${") && !n.contains('.'))
            .unwrap_or(&block.name)
            .to_owned();
        let mut e = element(kind, &name, kind.label());
        describe(&mut e, terraform, unit, Quality::Exact, near);
        // Named by the unit, not matched by a name: say so.
        if let Some(m) = e.properties.iter_mut().find(|(k, _)| k == "match") {
            m.1 = "what a unit depends on".into();
        }
        let i = deployment.add(e);
        if kind == ElementKind::Database
            && let Some(network) = network
        {
            deployment.link(network, i, LinkKind::Contains, "");
        }
        Some(i)
    };

    for &unit in own_units {
        let resolved = terraform.resolve(&terraform.blocks[unit], near);
        let mut deployed: Vec<usize> = Vec::new();
        for inner in &resolved.inner {
            if !TERRAFORM_WORKLOADS.contains(&inner.what.as_str()) {
                continue;
            }
            let value = |keys: &[&str]| {
                keys.iter().find_map(|k| {
                    inner
                        .values
                        .iter()
                        .find(|(key, v)| key == k && !v.contains("var.") && !v.contains("${"))
                        .map(|(_, v)| v.clone())
                })
            };
            let Some(name) = value(&["metadata.name", "name"]) else {
                continue;
            };
            let kind = if inner.what.contains("job") {
                ElementKind::Job
            } else {
                ElementKind::Workload
            };
            let mut e = element(kind, &name, format!("{} in Terraform", inner.what));
            e.namespace = value(&["metadata.namespace", "namespace"]);
            e.service = Some(name.clone());
            let replicas = value(&["spec.replicas"]).and_then(|r| r.parse::<u32>().ok());
            e.replicas = Some((replicas.unwrap_or(1), replicas.unwrap_or(1)));
            for (label, keys) in [
                ("chart", &["chart"][..]),
                ("chart version", &["version"][..]),
                ("repository", &["repository"][..]),
                ("image", &["spec.template.spec.container.image"][..]),
            ] {
                if let Some(v) = value(keys) {
                    e.properties.push((label.into(), v));
                }
            }
            e.places.push(inner.place.clone());
            e.places.push(terraform.blocks[unit].place.clone());
            let i = deployment.add(e);
            if let Some(cluster) = cluster {
                deployment.link(cluster, i, LinkKind::Contains, "");
            }
            deployed.push(i);
        }
        // What the unit depends on, its deployments use.
        if !deployed.is_empty() {
            for dependency in terraform.dependencies(unit) {
                if let Some(target) = resource(deployment, dependency) {
                    for &w in &deployed {
                        deployment.link(w, target, LinkKind::Uses, "its unit depends on it");
                    }
                }
            }
        }
    }

    // Secrets the workloads read that no manifest declares.
    let declared: Vec<(&Option<String>, &str)> = rendered
        .objects
        .iter()
        .filter(|o| o.kind == "Secret")
        .map(|o| (&o.namespace, o.name.as_str()))
        .collect();
    for (w, object) in workloads {
        for secret in secrets_read(object) {
            if declared
                .iter()
                .any(|(ns, n)| *n == secret && **ns == object.namespace)
            {
                continue;
            }
            // The unit that makes it, those of the environment first.
            let made = all_units.iter().find_map(|&unit| {
                let resolved = terraform.resolve(&terraform.blocks[unit], near);
                resolved
                    .inner
                    .into_iter()
                    .filter(|i| i.what.starts_with("kubernetes_secret"))
                    .find(|i| {
                        i.values
                            .iter()
                            .any(|(k, v)| k == "metadata.name" && *v == secret)
                    })
                    .map(|inner| (unit, inner))
            });
            let Some((unit, inner)) = made else {
                continue;
            };
            let mut e = element(
                ElementKind::Secret,
                &secret,
                "A Kubernetes secret Terraform makes",
            );
            e.namespace.clone_from(&object.namespace);
            // Its keys, never its values.
            let keys: Vec<(String, &str)> = inner
                .values
                .iter()
                .filter_map(|(k, v)| {
                    let key = k
                        .strip_prefix("data.")
                        .or_else(|| k.strip_prefix("string_data."))?;
                    Some((key.to_owned(), v.as_str()))
                })
                .collect();
            if !keys.is_empty() {
                e.properties.push((
                    "keys".into(),
                    keys.iter()
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }
            e.places.push(inner.place.clone());
            e.places.push(terraform.blocks[unit].place.clone());
            let s = deployment.add(e);
            deployment.link(*w, s, LinkKind::Uses, "");
            // A value taken from what a unit depends on: the workload uses
            // it, through this key.
            for (key, value) in keys {
                for dependency in dependency_names(value) {
                    if let Some(d) = terraform.dependency(unit, &dependency)
                        && let Some(target) = resource(deployment, d)
                    {
                        deployment.link(*w, target, LinkKind::Uses, &key);
                    }
                }
            }
        }
    }
}

/// The Terragrunt dependencies a value names: `dependency.NAME.outputs…`.
fn dependency_names(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in value.split("dependency.").skip(1) {
        let name: String = part
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A release not rendered, as its settings describe it.
fn unrendered(release: &Release) -> Element {
    let setting = |keys: &[&str]| {
        release
            .settings
            .iter()
            .find(|s| keys.contains(&s.key.as_str()))
            .map(|s| s.value.clone())
    };
    let mut e = element(
        ElementKind::Workload,
        &release.name,
        "Not rendered: drawn from its settings",
    );
    e.namespace.clone_from(&release.namespace);
    e.service = Some(release.label.clone());
    let replicas = setting(&["replicaCount", "replicas"])
        .and_then(|r| r.parse::<u32>().ok())
        .unwrap_or(1);
    let min = setting(&["autoscaling.minReplicas"]).and_then(|r| r.parse().ok());
    let max = setting(&["autoscaling.maxReplicas"]).and_then(|r| r.parse().ok());
    let enabled = setting(&["autoscaling.enabled"]).is_some_and(|v| v == "true");
    e.replicas = Some(match (enabled, min, max) {
        (true, Some(min), Some(max)) => (min, max),
        _ => (replicas, replicas),
    });
    if let (Some(repository), Some(tag)) = (setting(&["image.repository"]), setting(&["image.tag"]))
    {
        e.properties
            .push(("image".into(), format!("{repository}:{tag}")));
    }
    e.properties.push(("replicas".into(), replicas.to_string()));
    e.properties
        .extend(release.notes.iter().map(|n| ("note".to_owned(), n.clone())));
    e.places.push(release.place.clone());
    e
}

/// The way in, from the ingresses and load-balanced services: the
/// internet, the names, a firewall when one is configured, and the load
/// balancers, routed to the workloads behind.
fn ingress(
    deployment: &mut Deployment,
    rendered: &Rendered,
    workloads: &[(usize, &Object)],
) -> Option<usize> {
    let services: Vec<&Object> = rendered
        .objects
        .iter()
        .filter(|o| o.kind == "Service")
        .collect();
    let behind = |namespace: &Option<String>, service: &str| -> Vec<usize> {
        let Some(svc) = services
            .iter()
            .find(|s| s.name == service && s.namespace == *namespace)
        else {
            return workloads
                .iter()
                .filter(|(_, o)| o.name == service && o.namespace == *namespace)
                .map(|(i, _)| *i)
                .collect();
        };
        let selector = svc.body["spec"]["selector"].as_object();
        workloads
            .iter()
            .filter(|(_, o)| o.namespace == *namespace)
            .filter(|(_, o)| {
                let labels = &o.body["spec"]["template"]["metadata"]["labels"];
                selector.is_some_and(|sel| {
                    !sel.is_empty() && sel.iter().all(|(k, v)| labels.get(k) == Some(v))
                })
            })
            .map(|(i, _)| *i)
            .collect()
    };
    let mut internet = None;
    for object in rendered.objects.iter().filter(|o| o.kind == "Ingress") {
        let annotations = object.body["metadata"]["annotations"].as_object();
        let annotation = |key: &str| {
            annotations
                .and_then(|a| a.get(key))
                .and_then(|v| v.as_str())
        };
        let class = object.body["spec"]["ingressClassName"]
            .as_str()
            .or_else(|| annotation("kubernetes.io/ingress.class"))
            .unwrap_or("ingress");
        let internet_i = *internet.get_or_insert_with(|| {
            deployment.add(element(
                ElementKind::Internet,
                "Internet",
                "Where requests come from",
            ))
        });
        let hosts: Vec<String> = object.body["spec"]["rules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["host"].as_str().map(str::to_owned))
            .collect();
        let mut dns = element(ElementKind::Dns, "DNS", "The names it is reached by");
        if let Some(external) = annotation("external-dns.alpha.kubernetes.io/hostname") {
            dns.properties
                .push(("external-dns".into(), external.to_owned()));
        }
        let dns_i = deployment.add(dns);
        for host in &hosts {
            let entry = ("host".to_owned(), host.clone());
            if !deployment.elements[dns_i].properties.contains(&entry) {
                deployment.elements[dns_i].properties.push(entry);
            }
        }
        deployment.elements[dns_i]
            .places
            .extend(object.origin.iter().cloned());
        deployment.link(internet_i, dns_i, LinkKind::Routes, "");
        let mut before = dns_i;
        let waf = annotation("alb.ingress.kubernetes.io/wafv2-acl-arn")
            .or_else(|| annotation("alb.ingress.kubernetes.io/waf-acl-id"))
            .map(|w| {
                (
                    "web application firewall",
                    w.rsplit('/').nth(1).unwrap_or(w).to_owned(),
                )
            })
            .or_else(|| {
                annotation("nginx.ingress.kubernetes.io/whitelist-source-range")
                    .or_else(|| annotation("nginx.ingress.kubernetes.io/allowlist-source-range"))
                    .map(|r| ("allow list", r.to_owned()))
            });
        if let Some((what, value)) = waf {
            let mut fw = element(ElementKind::Firewall, &value, what);
            fw.places.extend(object.origin.iter().cloned());
            let fw_i = deployment.add(fw);
            deployment.link(before, fw_i, LinkKind::Routes, "");
            before = fw_i;
        }
        let group = annotation("alb.ingress.kubernetes.io/group.name").unwrap_or(&object.name);
        let name = if class == "alb" {
            format!("alb {group}")
        } else {
            class.to_owned()
        };
        let mut lb = element(
            ElementKind::LoadBalancer,
            &name,
            format!("{class} load balancer"),
        );
        for (key, label) in [
            ("cert-manager.io/cluster-issuer", "certificates from"),
            ("cert-manager.io/issuer", "certificates from"),
            ("alb.ingress.kubernetes.io/scheme", "scheme"),
            ("alb.ingress.kubernetes.io/certificate-arn", "certificate"),
            ("alb.ingress.kubernetes.io/listen-ports", "listens on"),
            (
                "alb.ingress.kubernetes.io/security-groups",
                "security groups",
            ),
        ] {
            if let Some(v) = annotation(key) {
                lb.properties.push((label.into(), v.to_owned()));
            }
        }
        lb.places.extend(object.origin.iter().cloned());
        let lb_i = deployment.add(lb);
        deployment.link(before, lb_i, LinkKind::Routes, "");
        for rule in object.body["spec"]["rules"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for path in rule["http"]["paths"].as_array().into_iter().flatten() {
                let Some(service) = path["backend"]["service"]["name"].as_str() else {
                    continue;
                };
                let label = format!(
                    "{}{}",
                    rule["host"].as_str().unwrap_or(""),
                    path["path"].as_str().unwrap_or("")
                );
                for w in behind(&object.namespace, service) {
                    deployment.link(lb_i, w, LinkKind::Routes, &label);
                }
            }
        }
    }
    for svc in services
        .iter()
        .filter(|s| s.body["spec"]["type"].as_str() == Some("LoadBalancer"))
    {
        let internet_i = *internet.get_or_insert_with(|| {
            deployment.add(element(
                ElementKind::Internet,
                "Internet",
                "Where requests come from",
            ))
        });
        let mut lb = element(
            ElementKind::LoadBalancer,
            &svc.name,
            "a service's load balancer",
        );
        lb.namespace.clone_from(&svc.namespace);
        lb.places.extend(svc.origin.iter().cloned());
        let lb_i = deployment.add(lb);
        deployment.link(internet_i, lb_i, LinkKind::Routes, "");
        for w in behind(&svc.namespace, &svc.name) {
            deployment.link(lb_i, w, LinkKind::Routes, "");
        }
    }
    internet
}

/// `services` with the cloud resources `deployment` has them use: each
/// resource a service, outside the project, reached by a cloud link from
/// the service whose workloads use it. A deployed service that the code
/// does not declare is added too.
#[must_use]
pub fn with_cloud(services: &Services, deployment: &Deployment) -> Services {
    let mut out = services.clone();
    let index = |out: &mut Services, name: &str, cloud: Option<ElementKind>| -> usize {
        if let Some(i) = out
            .services
            .iter()
            .position(|s| s.name == name && s.cloud == cloud)
        {
            return i;
        }
        out.services.push(crate::services::Service {
            name: name.to_owned(),
            folder: None,
            external: cloud.is_some(),
            operations: Vec::new(),
            tools: Vec::new(),
            cloud,
        });
        out.services.len() - 1
    };
    for link in &deployment.links {
        if !matches!(link.kind, LinkKind::Uses | LinkKind::Assumes) {
            continue;
        }
        let (from, to) = (
            &deployment.elements[link.from],
            &deployment.elements[link.to],
        );
        let Some(service) = from.service.as_deref().or(Some(from.name.as_str())) else {
            continue;
        };
        // A label `api (ns)` is the service `api`.
        let service = service.split(" (").next().unwrap_or(service);
        let from = index(&mut out, service, None);
        let to = index(&mut out, &to.name, Some(to.kind));
        if !out
            .links
            .iter()
            .any(|l| l.from == from && l.to == to && l.via == crate::services::Via::Cloud)
        {
            out.links.push(crate::services::ServiceLink {
                from,
                to,
                via: crate::services::Via::Cloud,
                operations: Vec::new(),
                evidence: Vec::new(),
            });
        }
    }
    out
}

/// Whether a block makes one of the cloud resources an architecture shows.
#[must_use]
pub fn cloud_block(block: &crate::terraform::TfBlock) -> bool {
    [
        CloudKind::Role,
        CloudKind::Bucket,
        CloudKind::Database,
        CloudKind::Secret,
        CloudKind::Users,
    ]
    .into_iter()
    .any(|k| makes(block, k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::{Part, Setting};
    use crate::repos::Repos;
    use serde_json::json;

    fn object(kind: &str, name: &str, body: serde_json::Value) -> Object {
        Object {
            kind: kind.into(),
            name: name.into(),
            namespace: Some("shop".into()),
            service: Some("api".into()),
            origin: Some(Place::new("ops", "apps/base/x.yaml", 1)),
            changed_by: Vec::new(),
            body,
        }
    }

    #[test]
    fn an_architecture_from_rendered_objects_and_terraform() {
        let dir = tempfile::tempdir().unwrap();
        let repos: Repos = crate::terraform::tests::infrastructure(dir.path());
        let terraform = crate::terraform::terraform(&repos);
        let rendered = Rendered {
            objects: vec![
                object(
                    "Deployment",
                    "api",
                    json!({"spec": {"replicas": 2, "template": {"metadata": {"labels": {"app": "api"}}, "spec": {
                        "serviceAccountName": "api",
                        "containers": [{"name": "api", "image": "api:1.2", "ports": [{"containerPort": 8000}],
                            "resources": {"requests": {"cpu": "250m"}},
                            "env": [{"name": "DB_HOST", "value": "prod-shop.cluster-abc123xyz.eu-west-1.rds.amazonaws.com"},
                                    {"name": "FILES_BUCKET", "value": "shop-prod-files"}]}]}}}}),
                ),
                object(
                    "HorizontalPodAutoscaler",
                    "api",
                    json!({"spec": {"scaleTargetRef": {"name": "api"}, "minReplicas": 2, "maxReplicas": 6}}),
                ),
                object(
                    "CronJob",
                    "report",
                    json!({"spec": {"schedule": "0 1 * * *", "jobTemplate": {"spec": {"template": {"spec": {"containers": []}}}}}}),
                ),
                object(
                    "Service",
                    "api",
                    json!({"spec": {"selector": {"app": "api"}}}),
                ),
                object(
                    "ServiceAccount",
                    "api",
                    json!({"metadata": {"annotations": {"eks.amazonaws.com/role-arn": "arn:aws:iam::123456789012:role/shop-prod-api"}}}),
                ),
                object(
                    "Ingress",
                    "api",
                    json!({"metadata": {"annotations": {
                        "alb.ingress.kubernetes.io/scheme": "internet-facing",
                        "alb.ingress.kubernetes.io/wafv2-acl-arn": "arn:aws:wafv2:eu-west-1:123456789012:regional/webacl/shop-waf/abc"}},
                        "spec": {"ingressClassName": "alb", "rules": [{"host": "shop.example.com", "http": {"paths": [{"path": "/api", "backend": {"service": {"name": "api"}}}]}}]}}),
                ),
            ],
            releases: vec!["api".into()],
            notes: Vec::new(),
        };
        let environment = Environment {
            name: "prod".into(),
            chosen: true,
            parts: vec![Part {
                repo: "ops".into(),
                folder: "clusters/prod".into(),
            }],
            releases: vec![Release {
                name: "worker".into(),
                label: "worker".into(),
                namespace: Some("shop".into()),
                chart_name: None,
                chart: None,
                source: None,
                settings: vec![Setting {
                    key: "replicaCount".into(),
                    value: "4".into(),
                    place: Place::new("ops", "apps/base/worker.yaml", 9),
                    replaced: Vec::new(),
                }],
                values: json!({}),
                place: Place::new("ops", "apps/base/worker.yaml", 1),
                notes: Vec::new(),
            }],
            manifests: Vec::new(),
            encrypted: Vec::new(),
            notes: Vec::new(),
            declared_in: None,
        };
        let d = deployment(&environment, &rendered, &terraform, &Services::default());
        let find = |kind: ElementKind| d.elements.iter().position(|e| e.kind == kind).unwrap();
        let api = d
            .elements
            .iter()
            .position(|e| e.kind == ElementKind::Workload && e.name == "api")
            .unwrap();
        assert_eq!(d.elements[api].replicas, Some((2, 6)));
        assert!(
            d.elements[api]
                .properties
                .contains(&("api ports".into(), "8000".into()))
        );
        let worker = d.elements.iter().find(|e| e.name == "worker").unwrap();
        assert_eq!(worker.replicas, Some((4, 4)));
        assert_eq!(d.elements[find(ElementKind::Job)].name, "report");
        let db = &d.elements[find(ElementKind::Database)];
        assert_eq!(db.name, "prod-shop");
        assert!(
            db.properties
                .contains(&("engine".into(), "postgres".into()))
        );
        assert!(db.properties.contains(&("instances".into(), "2".into())));
        assert_eq!(db.places[0].path, "envs/prod/main.tf");
        let role = find(ElementKind::Role);
        assert!(
            d.links
                .iter()
                .any(|l| l.from == api && l.to == role && l.kind == LinkKind::Assumes)
        );
        let bucket = find(ElementKind::Bucket);
        assert!(
            d.links
                .iter()
                .any(|l| l.from == api && l.to == bucket && l.kind == LinkKind::Uses)
        );
        // Internet, names, firewall, load balancer, workload.
        let (internet, dns, fw, lb) = (
            find(ElementKind::Internet),
            find(ElementKind::Dns),
            find(ElementKind::Firewall),
            find(ElementKind::LoadBalancer),
        );
        assert_eq!(d.elements[fw].name, "shop-waf");
        for (from, to) in [(internet, dns), (dns, fw), (fw, lb), (lb, api)] {
            assert!(
                d.links
                    .iter()
                    .any(|l| l.from == from && l.to == to && l.kind == LinkKind::Routes),
                "{from} {to}"
            );
        }
        assert!(
            d.elements[dns]
                .properties
                .contains(&("host".into(), "shop.example.com".into()))
        );

        let services = with_cloud(&Services::default(), &d);
        let api = services
            .services
            .iter()
            .position(|s| s.name == "api")
            .unwrap();
        let db = services
            .services
            .iter()
            .position(|s| s.cloud == Some(ElementKind::Database))
            .unwrap();
        assert!(
            services
                .links
                .iter()
                .any(|l| l.from == api && l.to == db && l.via == crate::services::Via::Cloud)
        );
    }

    #[test]
    fn terragrunt_units_flux_in_terraform_and_the_secrets_they_make() {
        use crate::deploy::tests::write;
        let dir = tempfile::tempdir().unwrap();
        let ops = dir.path().join("ops");
        let w = |path: &str, text: &str| write(&ops, path, text);
        // Flux, installed by Terraform, builds a folder of plain manifests.
        w(
            "modules/flux/main.tf",
            "resource \"kubectl_manifest\" \"prod\" {\n  yaml_body = <<-YAML\n    apiVersion: kustomize.toolkit.fluxcd.io/v1\n    kind: Kustomization\n    metadata:\n      name: prod\n    spec:\n      path: ./k8s/prod\n      targetNamespace: shop\n  YAML\n}\n",
        );
        w(
            "k8s/prod/api.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: api\nspec:\n  template:\n    spec:\n      containers:\n        - name: api\n          image: api:1\n          envFrom:\n            - secretRef:\n                name: shop-secrets\n",
        );
        w(
            "modules/gke/main.tf",
            "resource \"google_container_cluster\" \"this\" {\n  name     = var.name\n  location = var.region\n}\nvariable \"name\" {}\nvariable \"region\" {}\n",
        );
        w(
            "modules/sql/main.tf",
            "resource \"google_sql_database_instance\" \"this\" {\n  name             = var.instance_name\n  database_version = \"POSTGRES_16\"\n  settings {\n    tier = var.tier\n  }\n}\nvariable \"instance_name\" {}\nvariable \"tier\" {}\n",
        );
        w(
            "modules/app/main.tf",
            "resource \"kubernetes_namespace\" \"this\" {\n  metadata {\n    name = var.namespace\n  }\n}\nresource \"kubernetes_secret\" \"app\" {\n  metadata {\n    name      = \"shop-secrets\"\n    namespace = kubernetes_namespace.this.metadata[0].name\n  }\n  data = {\n    DATABASE_URL = var.database_url\n  }\n}\nresource \"helm_release\" \"auth\" {\n  name      = \"auth\"\n  namespace = kubernetes_namespace.this.metadata[0].name\n  chart     = \"auth\"\n}\nvariable \"namespace\" {}\nvariable \"database_url\" {}\n",
        );
        w(
            "environments/shared/env.hcl",
            "locals {\n  region = \"europe-west1\"\n}\n",
        );
        let unit = |source: &str, rest: &str| {
            format!(
                "locals {{\n  env = read_terragrunt_config(find_in_parent_folders(\"env.hcl\"))\n}}\nterraform {{\n  source = \"${{get_repo_root()}}/modules/{source}\"\n}}\n{rest}"
            )
        };
        w(
            "environments/shared/gke/terragrunt.hcl",
            &unit(
                "gke",
                "inputs = {\n  name   = \"shop-cluster\"\n  region = local.env.locals.region\n}\n",
            ),
        );
        w(
            "environments/shared/sql/terragrunt.hcl",
            &unit(
                "sql",
                "inputs = {\n  instance_name = \"shop-db\"\n  tier          = \"db-custom-1-3840\"\n}\n",
            ),
        );
        w(
            "environments/shared/flux/terragrunt.hcl",
            &unit(
                "flux",
                "dependency \"gke\" {\n  config_path = \"../gke\"\n}\n",
            ),
        );
        w(
            "environments/prod/app/terragrunt.hcl",
            &unit(
                "app",
                "dependency \"sql\" {\n  config_path = \"../../shared/sql\"\n}\ninputs = {\n  namespace    = \"shop\"\n  database_url = format(\"postgres://%s/shop\", dependency.sql.outputs.private_ip)\n}\n",
            ),
        );
        let repos = Repos::new(&dir.path().join("app"), &[ops]);
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        let deploy = crate::deploy::deploy(&repos, &[]);
        let env = &deploy.environments[0];
        assert_eq!(env.name, "prod");
        assert_eq!(
            env.declared_in.as_ref().unwrap().path,
            "modules/flux/main.tf"
        );
        let terraform = crate::terraform::terraform(&repos);
        let rendered = Rendered {
            objects: vec![Object {
                kind: "Deployment".into(),
                name: "api".into(),
                namespace: Some("shop".into()),
                service: None,
                origin: None,
                changed_by: Vec::new(),
                body: json!({"spec": {"template": {"spec": {"containers": [
                    {"name": "api", "image": "api:1", "envFrom": [{"secretRef": {"name": "shop-secrets"}}]}
                ]}}}}),
            }],
            releases: Vec::new(),
            notes: Vec::new(),
        };
        let d = deployment(env, &rendered, &terraform, &Services::default());
        let named = |kind: ElementKind| d.elements.iter().position(|e| e.kind == kind).unwrap();
        let cluster = &d.elements[named(ElementKind::Cluster)];
        assert_eq!(cluster.name, "shop-cluster");
        assert!(
            cluster
                .properties
                .contains(&("location".into(), "europe-west1".into()))
        );
        let db = named(ElementKind::Database);
        assert_eq!(d.elements[db].name, "shop-db");
        assert!(
            d.elements[db]
                .properties
                .contains(&("engine".into(), "POSTGRES_16".into()))
        );
        assert!(
            d.elements[db]
                .properties
                .contains(&("instance class".into(), "db-custom-1-3840".into()))
        );
        let secret = &d.elements[named(ElementKind::Secret)];
        assert_eq!(
            secret.properties,
            [("keys".to_owned(), "DATABASE_URL".to_owned())]
        );
        let api = d.elements.iter().position(|e| e.name == "api").unwrap();
        assert!(
            d.links
                .iter()
                .any(|l| l.from == api && l.to == db && l.label == "DATABASE_URL")
        );
        let auth = d.elements.iter().find(|e| e.name == "auth").unwrap();
        assert_eq!(auth.namespace.as_deref(), Some("shop"));
        assert!(auth.properties.contains(&("chart".into(), "auth".into())));
    }
}
