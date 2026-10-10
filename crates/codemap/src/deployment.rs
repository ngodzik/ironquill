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
use crate::terraform::{self, BlockKind, CloudKind, Quality, Terraform, configures, makes, words};

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

    // The network and the cluster, from the nearest Terraform.
    let network = terraform
        .nearest(
            |b| {
                b.what == "aws_vpc"
                    || (b.kind == BlockKind::Module && b.what.to_lowercase().contains("vpc"))
            },
            &near,
        )
        .map(|i| {
            let block = &terraform.blocks[i];
            let resolved = terraform.resolve(block, &near);
            let mut e = element(ElementKind::Network, &block.name, "The private network");
            for key in [
                "cidr",
                "cidr_block",
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
    let cluster_block = terraform.nearest(
        |b| {
            b.what == "aws_eks_cluster"
                || (b.kind == BlockKind::Module && b.what.to_lowercase().contains("eks"))
        },
        &near,
    );
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
                .unwrap_or(&block.name)
                .to_owned();
            for key in [
                "cluster_version",
                "version",
                "instance_types",
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
            Some((b, quality)) => {
                let block = &terraform.blocks[b];
                let resolved = terraform.resolve(block, &near);
                e.properties.push((
                    "configured by".into(),
                    format!(
                        "{} {}",
                        block.what.rsplit("//").next().unwrap_or(&block.what),
                        block.name
                    ),
                ));
                e.properties.push((
                    "match".into(),
                    match quality {
                        Quality::Exact => "exact",
                        Quality::Pattern => "by its pattern",
                        Quality::Part => "by a shared part of its name",
                        Quality::Nearest => {
                            "the nearest of its kind (its id only exists once applied)"
                        }
                    }
                    .into(),
                ));
                if kind == ElementKind::Database {
                    for (label, keys) in [
                        ("engine", &["engine"][..]),
                        ("engine version", &["engine_version"][..]),
                        ("instance class", &["instance_class", "class"][..]),
                        (
                            "instances",
                            &["count", "instance_count", "replica_count"][..],
                        ),
                    ] {
                        if let Some(v) = resolved.find(keys) {
                            e.properties.push((label.into(), v.to_owned()));
                        }
                    }
                }
                e.places.insert(0, block.place.clone());
            }
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

    // Services outside that the environment's services reach, and the
    // calls between its own.
    let workload_named = |name: &str, d: &Deployment| -> Vec<usize> {
        d.elements
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.kind == ElementKind::Workload
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
}
