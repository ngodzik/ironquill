//! What each call to a model used, over time, for the usage pane: where the
//! money goes, how full the conversation is, and when a cache was rebuilt.

use crate::style::Rgb;
use serde::{Deserialize, Serialize};

/// How long samples are kept: the pane shows a day at most.
pub(crate) const KEEP_SECS: u64 = 24 * 60 * 60;

/// Colours told apart with any colour vision (Okabe and Ito's), given to
/// models in the order they first appear, so that one keeps its colour
/// whatever the window shows.
const PALETTE: [Rgb; 7] = [
    Rgb(230, 159, 0),
    Rgb(86, 180, 233),
    Rgb(0, 158, 115),
    Rgb(240, 228, 66),
    Rgb(0, 114, 178),
    Rgb(213, 94, 0),
    Rgb(204, 121, 167),
];

/// One call to a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Sample {
    /// When it ended, in seconds since 1970.
    pub(crate) at: u64,
    pub(crate) model: String,
    pub(crate) input: u64,
    pub(crate) output: u64,
    /// Its cost, when known and owed.
    pub(crate) cost: Option<f64>,
    /// Input read from the cache, when the provider says.
    pub(crate) cache_read: Option<u64>,
    /// Input written to the cache, when the provider says.
    pub(crate) cache_written: Option<u64>,
    /// How full the conversation's context was, for a call of the
    /// conversation itself rather than of a member it handed a task to.
    pub(crate) context: Option<u64>,
}

impl Sample {
    /// Whether the call rebuilt its cache rather than added to it: every
    /// call writes the turn it adds, but writing over a quarter of its input
    /// means the cache had expired.
    pub(crate) fn rebuilt(&self) -> bool {
        self.cache_written
            .is_some_and(|written| written * 4 > self.input && self.input > 0)
    }
}

/// The samples of the last day.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageLog {
    pub(crate) samples: Vec<Sample>,
}

/// What one model used in a window.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUse {
    /// The model's name.
    pub model: String,
    /// Its colour, the same whatever the window shows.
    pub color: Rgb,
    /// How many calls it made.
    pub calls: usize,
    /// What they cost, where known, in dollars.
    pub cost: f64,
    /// Cache read, as a share of the input, over the calls that said.
    pub cache_share: Option<f64>,
    /// How many calls rebuilt their cache, which had expired.
    pub rebuilds: usize,
}

impl UsageLog {
    /// Adds a sample, and forgets those older than a day.
    pub(crate) fn push(&mut self, sample: Sample) {
        let now = sample.at;
        self.samples.push(sample);
        self.prune(now);
    }

    /// Forgets the samples older than a day before `now`.
    pub(crate) fn prune(&mut self, now: u64) {
        self.samples
            .retain(|s| now.saturating_sub(s.at) <= KEEP_SECS);
    }

    /// A model's colour: by the order models first appear.
    pub(crate) fn color(&self, model: &str) -> Rgb {
        let mut seen: Vec<&str> = Vec::new();
        for s in &self.samples {
            if !seen.contains(&s.model.as_str()) {
                seen.push(&s.model);
            }
        }
        let index = seen.iter().position(|m| *m == model).unwrap_or(seen.len());
        PALETTE[index % PALETTE.len()]
    }

    /// The samples since `from`.
    pub(crate) fn since(&self, from: u64) -> impl Iterator<Item = &Sample> {
        self.samples.iter().filter(move |s| s.at >= from)
    }

    /// Each model's use since `from`, in the order they first appear.
    pub fn models(&self, from: u64) -> Vec<ModelUse> {
        let mut out: Vec<ModelUse> = Vec::new();
        let mut cached: Vec<(u64, u64)> = Vec::new();
        for s in self.since(from) {
            let i = match out.iter().position(|m| m.model == s.model) {
                Some(i) => i,
                None => {
                    out.push(ModelUse {
                        model: s.model.clone(),
                        color: self.color(&s.model),
                        calls: 0,
                        cost: 0.0,
                        cache_share: None,
                        rebuilds: 0,
                    });
                    cached.push((0, 0));
                    out.len() - 1
                }
            };
            let m = &mut out[i];
            m.calls += 1;
            m.cost += s.cost.unwrap_or(0.0);
            m.rebuilds += usize::from(s.rebuilt());
            if let Some(read) = s.cache_read {
                cached[i].0 += read;
                cached[i].1 += s.input;
            }
        }
        for (m, (read, input)) in out.iter_mut().zip(cached) {
            m.cache_share = (input > 0).then(|| read as f64 / input as f64);
        }
        out
    }

    /// The share of the input read from the cache since `from`, over the
    /// calls whose provider said; `None` when none did.
    pub fn cache_share(&self, from: u64) -> Option<f64> {
        let (read, input) = self
            .since(from)
            .filter_map(|s| s.cache_read.map(|read| (read, s.input)))
            .fold((0, 0), |(r, i), (read, input)| (r + read, i + input));
        (input > 0).then(|| read as f64 / input as f64)
    }

    /// A model's cost added up since `from`, as the points of a line that
    /// steps up at each call: (seconds after `from`, dollars).
    pub fn cost_steps(&self, model: &str, from: u64) -> Vec<(f64, f64)> {
        let mut total = 0.0;
        let mut points = vec![(0.0, 0.0)];
        for s in self.since(from).filter(|s| s.model == model) {
            let x = (s.at - from) as f64;
            points.push((x, total));
            total += s.cost.unwrap_or(0.0);
            points.push((x, total));
        }
        points
    }

    /// The conversation's context over time: (seconds after `from`, tokens).
    pub fn context_line(&self, from: u64) -> Vec<(f64, f64)> {
        self.since(from)
            .filter_map(|s| s.context.map(|c| ((s.at - from) as f64, c as f64)))
            .collect()
    }

    /// The calls that rebuilt their cache: (seconds after `from`, tokens
    /// written).
    pub fn rebuilds(&self, from: u64) -> Vec<(f64, f64)> {
        self.since(from)
            .filter(|s| s.rebuilt())
            .map(|s| ((s.at - from) as f64, s.cache_written.unwrap_or(0) as f64))
            .collect()
    }
}

/// A window in seconds as a person writes it: `90m`, `6h`, `1d`.
pub fn window_name(secs: u64) -> String {
    match secs {
        s if s % (24 * 3600) == 0 => format!("{}d", s / (24 * 3600)),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{}m", s.div_ceil(60)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: u64, model: &str, cost: f64, read: Option<u64>, written: Option<u64>) -> Sample {
        Sample {
            at,
            model: model.into(),
            input: 10_000,
            output: 100,
            cost: Some(cost),
            cache_read: read,
            cache_written: written,
            context: Some(at * 10),
        }
    }

    #[test]
    fn a_day_is_kept_and_models_keep_their_colour() {
        let mut log = UsageLog::default();
        log.push(sample(100, "glm", 0.01, Some(9_000), Some(500)));
        log.push(sample(200, "opus", 0.10, Some(0), Some(9_000)));
        log.push(sample(300, "glm", 0.01, Some(9_500), Some(400)));
        let glm = log.color("glm");
        assert_ne!(glm, log.color("opus"));

        let models = log.models(0);
        assert_eq!(models[0].model, "glm");
        assert_eq!(models[0].calls, 2);
        assert!((models[0].cost - 0.02).abs() < 1e-12);
        assert_eq!(models[0].cache_share, Some(0.925));
        // Writing most of its input to the cache: the cache had expired.
        assert_eq!(models[1].rebuilds, 1);
        assert_eq!(log.rebuilds(0), [(200.0, 9_000.0)]);
        assert_eq!(
            log.cost_steps("glm", 0),
            [
                (0.0, 0.0),
                (100.0, 0.0),
                (100.0, 0.01),
                (300.0, 0.01),
                (300.0, 0.02)
            ]
        );

        log.push(sample(100 + KEEP_SECS + 1, "opus", 0.1, None, None));
        assert_eq!(log.samples.len(), 3);
        // The window moved, the colour stays with the model while it shows.
        assert_eq!(log.models(0)[0].model, "opus");
    }

    #[test]
    fn the_cache_share_counts_only_the_calls_that_said() {
        let mut log = UsageLog::default();
        log.push(sample(100, "glm", 0.01, Some(9_000), None));
        log.push(sample(200, "opus", 0.10, Some(1_000), None));
        log.push(sample(300, "tensorx", 0.01, None, None));
        assert_eq!(log.cache_share(0), Some(0.5));
        assert_eq!(log.cache_share(250), None);
    }

    #[test]
    fn a_provider_that_says_nothing_of_its_cache_is_unknown() {
        let mut log = UsageLog::default();
        log.push(sample(1, "tensorx/glm", 0.01, None, None));
        assert_eq!(log.models(0)[0].cache_share, None);
        assert_eq!(log.models(0)[0].rebuilds, 0);
    }
}
