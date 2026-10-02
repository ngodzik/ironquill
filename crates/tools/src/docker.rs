use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

/// How long `docker ps` may take. A daemon that hangs should leave a note in
/// the pane, not a pane that never fills.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A running container, as `docker ps` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Container {
    /// The short identifier.
    #[serde(rename = "ID")]
    pub id: String,
    /// The container's name, or names separated by commas.
    #[serde(rename = "Names")]
    pub name: String,
    /// The image it runs.
    #[serde(rename = "Image")]
    pub image: String,
    /// Human status, such as `Up 3 hours (healthy)`.
    #[serde(rename = "Status")]
    pub status: String,
    /// Published ports, such as `0.0.0.0:8080->80/tcp`; empty when none.
    #[serde(rename = "Ports", default)]
    pub ports: String,
}

/// The containers running now.
///
/// # Errors
///
/// A sentence for the person: Docker is not installed, its daemon is not
/// running or does not answer, or its output could not be read.
pub async fn running_containers() -> Result<Vec<Container>, String> {
    let run = Command::new("docker")
        .args(["ps", "--format", "{{json .}}"])
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(TIMEOUT, run).await {
        Err(_) => return Err("Docker did not answer within 5 seconds".into()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("Docker is not installed (no docker command found)".into());
        }
        Ok(Err(e)) => return Err(format!("Cannot run docker: {e}")),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("docker ps failed");
        return Err(first.to_owned());
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// One JSON object per line, as `--format '{{json .}}'` prints them.
fn parse(text: &str) -> Result<Vec<Container>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|e| format!("Unexpected docker output: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_docker_ps_json_lines() {
        // The shape docker 29 prints, with made-up values.
        let text = concat!(
            r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2026-09-01 10:00:00 +0200 CEST","ID":"0a1b2c3d4e5f","Image":"postgres:17-alpine","Labels":"","LocalVolumes":"1","Mounts":"data","Names":"app-db","Networks":"bridge","Ports":"0.0.0.0:5432->5432/tcp","RunningFor":"2 hours ago","Size":"0B","State":"running","Status":"Up 2 hours"}"#,
            "\n",
            r#"{"ID":"6a7b8c9d0e1f","Image":"web","Names":"app-web","Ports":"","State":"running","Status":"Up 5 minutes (healthy)"}"#,
            "\n"
        );
        let containers = parse(text).unwrap();
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].name, "app-db");
        assert_eq!(containers[0].ports, "0.0.0.0:5432->5432/tcp");
        assert_eq!(containers[1].status, "Up 5 minutes (healthy)");
    }

    #[test]
    fn no_container_is_an_empty_list() {
        assert_eq!(parse(""), Ok(vec![]));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse("not json").is_err());
    }
}
