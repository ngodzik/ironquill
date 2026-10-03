use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::LlmError;
use crate::openai_compat::{malformed, read_body, transport};

/// Where the scores come from. Their free API asks to be credited.
pub const RANKINGS_SOURCE: &str = "https://artificialanalysis.ai/";

const URL: &str = "https://artificialanalysis.ai/api/v2/data/llms/models";

/// Artificial Analysis, whose benchmarks score models on the same tests:
/// an intelligence index and a coding index, from 0 to 100.
#[derive(Clone)]
pub struct ArtificialAnalysis {
    http: reqwest::Client,
    api_key: String,
}

impl fmt::Debug for ArtificialAnalysis {
    // Written by hand so that the key never reaches a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArtificialAnalysis")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// One model's scores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    /// Their identifier, such as `glm-5-3-flash`.
    pub slug: String,
    /// Its name as they write it.
    pub name: String,
    /// The coding index, from 0 to 100.
    pub coding: Option<f64>,
    /// The intelligence index, from 0 to 100.
    pub intelligence: Option<f64>,
}

impl ArtificialAnalysis {
    /// Their API, with the person's own key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
        }
    }

    /// Every model they score.
    ///
    /// # Errors
    ///
    /// The transport errors of the request, an error status, or an answer
    /// that is not the documented shape.
    pub async fn rankings(&self) -> Result<Vec<Ranking>, LlmError> {
        let response = self
            .http
            .get(URL)
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|source| transport(URL, source))?;
        let body = read_body(URL, response).await?;
        parse(&body)
    }
}

fn parse(body: &str) -> Result<Vec<Ranking>, LlmError> {
    let value: Value = serde_json::from_str(body).map_err(|e| malformed(URL, e.to_string()))?;
    let Some(models) = value["data"].as_array() else {
        return Err(malformed(URL, "no data list"));
    };
    Ok(models
        .iter()
        .filter_map(|m| {
            let score = |key: &str| m["evaluations"][key].as_f64();
            Some(Ranking {
                slug: m["slug"].as_str()?.to_owned(),
                name: m["name"].as_str().unwrap_or_default().to_owned(),
                coding: score("artificial_analysis_coding_index"),
                intelligence: score("artificial_analysis_intelligence_index"),
            })
        })
        .collect())
}

/// A name reduced to its letters and digits, so that `glm-5.3-flash`,
/// `glm-5-3-flash` and `GLM 5.3 Flash` read the same.
pub fn plain(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The ranking of a model known to the provider as `id`, with `canonical`
/// its name without the host, when the provider gives one.
pub fn find<'a>(rankings: &'a [Ranking], id: &str, canonical: Option<&str>) -> Option<&'a Ranking> {
    let wanted = plain(canonical.unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id)));
    if wanted.is_empty() {
        return None;
    }
    rankings
        .iter()
        .find(|r| plain(&r.slug) == wanted || plain(&r.name) == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The documented shape, cut to two models.
    const BODY: &str = r#"{"status": 200, "data": [
        {"id": "1", "name": "GLM-5.3 Flash", "slug": "glm-5-3-flash",
         "model_creator": {"name": "Z.ai"},
         "evaluations": {"artificial_analysis_intelligence_index": 41.2,
                         "artificial_analysis_coding_index": 38.5}},
        {"id": "2", "name": "Mystery", "slug": "mystery", "evaluations": {}}
    ]}"#;

    #[test]
    fn scores_are_read_and_found_by_name() {
        let rankings = parse(BODY).unwrap();
        assert_eq!(rankings[0].coding, Some(38.5));
        assert_eq!(rankings[1].intelligence, None);
        let found = find(&rankings, "tensorx/glm-5.3-flash", Some("glm-5.3-flash"));
        assert_eq!(found.map(|r| r.slug.as_str()), Some("glm-5-3-flash"));
        // Without a canonical name, the part after the host is used.
        assert!(find(&rankings, "zai/glm-5.3-flash", None).is_some());
        assert!(find(&rankings, "zai/glm-5.3", None).is_none());
        assert!(parse("{}").is_err());
    }
}
