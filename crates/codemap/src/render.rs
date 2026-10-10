//! An environment as the cluster would get it, built by the tools
//! themselves: each of its trees by `kustomize build`, asked to say where
//! every object was declared and what changed it, and each release by
//! `helm template`, from its chart and its merged values.
//!
//! Nothing is guessed when a tool is missing or fails: it is said. Work is
//! done in a scratch folder, never in a repository; a secret's values are
//! never kept.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::deploy::{Deploy, Environment, Release, encrypted_name};
use crate::repos::{Place, Repos, normal};
use crate::secret;
use crate::yaml;

/// Annotations kustomize adds when asked, and that are taken off.
const ORIGIN: &str = "config.kubernetes.io/origin";
const TRANSFORMATIONS: &str = "alpha.config.kubernetes.io/transformations";

/// A Kubernetes object as the tools render it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    /// Its kind.
    pub kind: String,
    /// Its name.
    pub name: String,
    /// Its namespace.
    pub namespace: Option<String>,
    /// The service it belongs to, by its release's label.
    pub service: Option<String>,
    /// Where it was declared.
    pub origin: Option<Place>,
    /// What changed it after: patches, transformers.
    pub changed_by: Vec<Place>,
    /// The object, a secret's values hidden.
    pub body: serde_json::Value,
}

/// An environment rendered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rendered {
    /// The objects.
    pub objects: Vec<Object>,
    /// The releases rendered, by label.
    pub releases: Vec<String>,
    /// What could not be rendered, and why.
    pub notes: Vec<String>,
}

/// Renders `environment` with `kustomize` and `helm`, working in
/// `scratch`, which is emptied first.
#[must_use]
pub fn render(
    repos: &Repos,
    deploy: &Deploy,
    environment: &Environment,
    scratch: &Path,
) -> Rendered {
    let mut rendered = Rendered::default();
    let _ = std::fs::remove_dir_all(scratch);
    if let Err(e) = std::fs::create_dir_all(scratch) {
        rendered.notes.push(format!(
            "The scratch folder {} cannot be made: {e}",
            scratch.display()
        ));
        return rendered;
    }
    for (i, part) in environment.parts.iter().enumerate() {
        let Some(folder) = repos.get(&part.repo).map(|r| r.root.join(&part.folder)) else {
            continue;
        };
        let wrapper = scratch.join("parts").join(i.to_string());
        match kustomize(repos, &folder, &wrapper) {
            Ok(objects) => rendered.objects.extend(objects),
            Err(note) => rendered.notes.push(format!(
                "{}:{} was not built: {note}",
                part.repo, part.folder
            )),
        }
    }
    // Releases are rendered by Helm, and Flux's own objects say nothing
    // the release does not.
    rendered.objects.retain(|o| {
        !o.body["apiVersion"]
            .as_str()
            .is_some_and(|v| v.contains(".toolkit.fluxcd.io"))
    });
    for (i, release) in environment.releases.iter().enumerate() {
        let work = scratch.join("releases").join(i.to_string());
        match helm(repos, deploy, release, &work) {
            Ok(objects) => {
                rendered.objects.extend(objects);
                rendered.releases.push(release.label.clone());
            }
            Err(note) => rendered
                .notes
                .push(format!("{} was not rendered: {note}", release.label)),
        }
    }
    // What kustomize built goes to the release of its namespace, when one.
    for object in &mut rendered.objects {
        if object.service.is_some() {
            continue;
        }
        let mut owners = environment
            .releases
            .iter()
            .filter(|r| r.namespace.is_some() && r.namespace == object.namespace);
        if let (Some(owner), None) = (owners.next(), owners.next()) {
            object.service = Some(owner.label.clone());
        }
    }
    rendered
}

/// Runs a tool, its output or why it failed.
fn run(command: &mut Command, tool: &str) -> Result<String, String> {
    let output = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{tool} was not found: is it installed?")
        } else {
            format!("{tool} could not be run: {e}")
        }
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    let first: Vec<&str> = error
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect();
    Err(format!(
        "{tool} failed: {}",
        secret::scrub_line(&first.join(" "))
    ))
}

/// The path from `from` to `to`, both absolute.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let (from, to) = (normal(from), normal(to));
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c);
    }
    out
}

/// Builds the tree at `folder` through a wrapper in `wrapper` that asks
/// kustomize for every object's origin and transformations.
fn kustomize(repos: &Repos, folder: &Path, wrapper: &Path) -> Result<Vec<Object>, String> {
    std::fs::create_dir_all(wrapper).map_err(|e| e.to_string())?;
    let has_tree = ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .any(|n| folder.join(n).is_file());
    // A folder without a tree is every manifest in it, as Flux reads it.
    let resources: Vec<String> = if has_tree {
        vec![relative(wrapper, folder).to_string_lossy().into_owned()]
    } else {
        crate::repos::files(folder)
            .into_iter()
            .filter(|(_, rel)| {
                (rel.ends_with(".yaml") || rel.ends_with(".yml")) && !encrypted_name(rel)
            })
            .map(|(path, _)| relative(wrapper, &path).to_string_lossy().into_owned())
            .collect()
    };
    let text = format!(
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n{}buildMetadata: [originAnnotations, transformerAnnotations]\n",
        resources
            .iter()
            .map(|r| format!("  - {}\n", serde_json::Value::String(r.clone())))
            .collect::<String>()
    );
    std::fs::write(wrapper.join("kustomization.yaml"), text).map_err(|e| e.to_string())?;
    let output = run(
        Command::new("kustomize")
            .arg("build")
            .arg("--load-restrictor")
            .arg("LoadRestrictionsNone")
            .arg(wrapper),
        "kustomize",
    )?;
    let mut objects = Vec::new();
    for doc in yaml::docs(&output) {
        let mut body = doc.to_json();
        let annotations = body["metadata"]["annotations"].as_object_mut();
        let (origin, changes) = annotations.map_or((None, None), |a| {
            (
                a.remove(ORIGIN).and_then(|v| v.as_str().map(str::to_owned)),
                a.remove(TRANSFORMATIONS)
                    .and_then(|v| v.as_str().map(str::to_owned)),
            )
        });
        if body["metadata"]["annotations"]
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
            && let Some(m) = body["metadata"].as_object_mut()
        {
            m.remove("annotations");
        }
        let kind = body["kind"].as_str().unwrap_or("").to_owned();
        let name = body["metadata"]["name"].as_str().unwrap_or("").to_owned();
        let origin = origin
            .and_then(|o| yaml::doc(&o))
            .and_then(|o| o.str_at(&["path"]).map(str::to_owned))
            .and_then(|p| {
                let file = normal(&wrapper.join(p));
                let (repo, rel) = repos.locate(&file)?;
                Some(Place::new(&repo.name, &rel, line_of(&file, &kind, &name)))
            });
        let changed_by = changes
            .and_then(|c| yaml::doc(&c))
            .map(|c| {
                c.items()
                    .iter()
                    .filter_map(|t| t.str_at(&["configuredIn"]))
                    .filter_map(|p| {
                        let (repo, rel) = repos.locate(&normal(&wrapper.join(p)))?;
                        Some(Place::new(&repo.name, &rel, 0))
                    })
                    .fold(Vec::new(), |mut all, p| {
                        if !all.contains(&p) {
                            all.push(p);
                        }
                        all
                    })
            })
            .unwrap_or_default();
        objects.push(object(body, None, origin, changed_by));
    }
    Ok(objects)
}

/// The line where the object `kind` named `name` starts in `file`.
fn line_of(file: &Path, kind: &str, name: &str) -> usize {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| {
            yaml::docs(&t)
                .into_iter()
                .find(|d| {
                    d.str_at(&["kind"]) == Some(kind)
                        && d.str_at(&["metadata", "name"]) == Some(name)
                })
                .map(|d| d.line)
        })
        .unwrap_or(0)
}

/// An object, its secret values hidden.
fn object(
    mut body: serde_json::Value,
    service: Option<String>,
    origin: Option<Place>,
    changed_by: Vec<Place>,
) -> Object {
    let kind = body["kind"].as_str().unwrap_or("").to_owned();
    if kind == "Secret" {
        for key in ["data", "stringData"] {
            if let Some(data) = body[key].as_object_mut() {
                for value in data.values_mut() {
                    *value = serde_json::Value::String(secret::HIDDEN.to_owned());
                }
            }
        }
    }
    scrub(&mut body, "");
    Object {
        name: body["metadata"]["name"].as_str().unwrap_or("").to_owned(),
        namespace: body["metadata"]["namespace"].as_str().map(str::to_owned),
        kind,
        service,
        origin,
        changed_by,
        body,
    }
}

/// Hides every value that is, or is named as, a secret.
fn scrub(value: &mut serde_json::Value, key: &str) {
    match value {
        serde_json::Value::Object(map) => {
            let name = map.get("name").and_then(|n| n.as_str()).map(str::to_owned);
            for (k, v) in map.iter_mut() {
                if k == "value"
                    && let (Some(name), serde_json::Value::String(s)) = (&name, &mut *v)
                {
                    *s = secret::keep(name, s);
                    continue;
                }
                scrub(v, k);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|i| scrub(i, key)),
        serde_json::Value::String(s) => *s = secret::keep(key, s),
        _ => {}
    }
}

/// Renders a release with `helm template`, from a copy of its chart when
/// it has local dependencies to build.
fn helm(
    repos: &Repos,
    deploy: &Deploy,
    release: &Release,
    work: &Path,
) -> Result<Vec<Object>, String> {
    let chart_index = release
        .chart
        .ok_or_else(|| "its chart is in none of the repositories read".to_owned())?;
    let chart = &deploy.charts[chart_index];
    let folder = deploy
        .chart_folder(repos, chart_index)
        .ok_or_else(|| "its chart's folder is not there".to_owned())?;
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let local = chart
        .dependencies
        .iter()
        .any(|(_, r)| r.starts_with("file://"));
    let chart_dir = if local || !chart.dependencies.is_empty() {
        let copy = work.join("chart").join(&chart.name);
        copy_dir(&folder, &copy).map_err(|e| format!("its chart could not be copied: {e}"))?;
        point_dependencies(&folder, &copy).map_err(|e| e.to_string())?;
        run(
            Command::new("helm")
                .arg("dependency")
                .arg("build")
                .arg("--skip-refresh")
                .arg(&copy),
            "helm",
        )?;
        copy
    } else {
        folder.clone()
    };
    let values = work.join("values.json");
    std::fs::write(&values, release.values.to_string()).map_err(|e| e.to_string())?;
    let mut command = Command::new("helm");
    command
        .arg("template")
        .arg(&release.name)
        .arg(&chart_dir)
        .arg("--values")
        .arg(&values);
    if let Some(namespace) = &release.namespace {
        command.arg("--namespace").arg(namespace);
    }
    let output = run(&mut command, "helm")?;
    let mut objects = Vec::new();
    for piece in output.split("\n---") {
        let source = piece
            .lines()
            .find_map(|l| l.strip_prefix("# Source: "))
            .map(str::to_owned);
        let Some(doc) = yaml::doc(piece) else {
            continue;
        };
        let mut body = doc.to_json();
        if body["metadata"]["namespace"].is_null()
            && let (Some(ns), Some(m)) = (&release.namespace, body["metadata"].as_object_mut())
        {
            m.insert("namespace".into(), serde_json::Value::String(ns.clone()));
        }
        // `# Source: api/templates/deployment.yaml`, from the chart's
        // folder; a dependency's under `charts/`.
        let origin = source.map(|s| {
            let within = s.split_once('/').map_or(s.as_str(), |(_, rest)| rest);
            let path = if chart.folder.is_empty() {
                within.to_owned()
            } else {
                format!("{}/{within}", chart.folder)
            };
            Place::new(&chart.repo, &path, 0)
        });
        objects.push(object(
            body,
            Some(release.label.clone()),
            origin,
            Vec::new(),
        ));
    }
    Ok(objects)
}

/// Copies the folder `from` to `to`.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Makes the copied chart's `file://` dependencies point at the original
/// folders, which a copy elsewhere would not find.
fn point_dependencies(original: &Path, copy: &Path) -> std::io::Result<()> {
    let file = copy.join("Chart.yaml");
    let text = std::fs::read_to_string(&file)?;
    let rewritten: String = text
        .lines()
        .map(|line| match line.split_once("file://") {
            Some((before, path)) => {
                let path = path.trim().trim_matches(['"', '\'']);
                let absolute = normal(&original.join(path));
                format!("{before}\"file://{}\"\n", absolute.display())
            }
            None => format!("{line}\n"),
        })
        .collect();
    std::fs::write(file, rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::tests::{project, write};

    fn installed(tool: &str) -> bool {
        Command::new(tool).arg("version").output().is_ok()
    }

    #[test]
    fn an_environment_rendered_by_the_tools_with_its_origins() {
        if !installed("kustomize") || !installed("helm") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repos = project(dir.path());
        let app = dir.path().join("app");
        write(
            &app,
            "charts/api/templates/deployment.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Release.Name }}\nspec:\n  replicas: {{ .Values.replicaCount }}\n  template:\n    spec:\n      containers:\n        - name: api\n          image: \"{{ .Values.image.repository }}:{{ .Values.image.tag }}\"\n          env:\n            {{- toYaml .Values.env | nindent 12 }}\n",
        );
        write(
            &app,
            "charts/api/templates/secret.yaml",
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: api\nstringData:\n  password: {{ .Values.database.password }}\n",
        );
        write(
            &dir.path().join("ops"),
            "apps/base/account.yaml",
            "apiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: api\n  annotations:\n    eks.amazonaws.com/role-arn: arn:aws:iam::123456789012:role/shop-prod-api\n",
        );
        write(
            &dir.path().join("ops"),
            "apps/base/kustomization.yaml",
            "resources:\n  - api.yaml\n  - source.yaml\n  - account.yaml\n",
        );
        let deploy = crate::deploy::deploy(&repos, &[]);
        let prod = deploy
            .environments
            .iter()
            .find(|e| e.name == "clusters/prod")
            .unwrap();
        let scratch = dir.path().join("scratch");
        let rendered = render(&repos, &deploy, prod, &scratch);
        assert_eq!(rendered.releases, ["api"], "{:?}", rendered.notes);
        let deployment = rendered
            .objects
            .iter()
            .find(|o| o.kind == "Deployment")
            .unwrap();
        assert_eq!(deployment.body["spec"]["replicas"], serde_json::json!(3));
        assert_eq!(
            deployment.body["spec"]["template"]["spec"]["containers"][0]["image"],
            serde_json::json!("registry.example.com/api:1.2.0")
        );
        assert_eq!(deployment.namespace.as_deref(), Some("shop"));
        assert_eq!(
            deployment.origin.as_ref().unwrap().path,
            "charts/api/templates/deployment.yaml"
        );
        let env = &deployment.body["spec"]["template"]["spec"]["containers"][0]["env"];
        assert_eq!(env[1]["value"], serde_json::json!(secret::HIDDEN));
        let secret = rendered
            .objects
            .iter()
            .find(|o| o.kind == "Secret" && o.name == "api")
            .unwrap();
        let sealed = rendered.objects.iter().find(|o| o.name == "keys").unwrap();
        assert_eq!(
            sealed.body["data"]["key"],
            serde_json::json!(secret::HIDDEN)
        );
        assert_eq!(
            secret.body["stringData"]["password"],
            serde_json::json!(secret::HIDDEN)
        );
        let account = rendered
            .objects
            .iter()
            .find(|o| o.kind == "ServiceAccount")
            .unwrap();
        assert_eq!(
            account.origin,
            Some(Place::new("ops", "apps/base/account.yaml", 1))
        );
        assert_eq!(account.service.as_deref(), Some("api"));
        assert!(
            account
                .changed_by
                .iter()
                .any(|p| p.path == "apps/prod/kustomization.yaml")
        );
        // No Flux object is left: the release stands for itself.
        assert!(!rendered.objects.iter().any(|o| o.kind == "HelmRelease"));
        // Nothing was written in the repositories.
        assert!(!app.join("charts/api/charts").exists());
    }
}
