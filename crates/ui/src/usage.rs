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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// What the call was for: `chat`, `plan/review`, `code`, `other`,
    /// `tick-chat` or `tick-pair`. Calls logged before it was kept were the
    /// chat's.
    #[serde(default = "chat")]
    pub(crate) role: String,
    /// What the call would have cost more had what it read from the cache
    /// been written to it again, as after an expiry, at the model's list
    /// prices, when known: what a warm cache saved it.
    #[serde(default)]
    pub(crate) rewrite_extra: Option<f64>,
}

/// The role of a call logged before roles were kept.
fn chat() -> String {
    "chat".to_owned()
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
    /// How many of its calls were ticks, and what they cost: counted in
    /// `calls` and `cost` too.
    pub ticks: usize,
    /// What its ticks cost, in dollars.
    pub tick_cost: f64,
}

/// What ticks cost and saved over a window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TickBalance {
    /// What the ticks cost, in dollars.
    pub spent: f64,
    /// What the calls they kept warm would have cost more, in dollars.
    pub saved: f64,
    /// How many ticks.
    pub ticks: usize,
}

/// How long a prompt cache lives unread, in seconds.
const CACHE_LIVES: u64 = 5 * 60;

/// The roles whose context is drawn, in order: ticks have none worth it.
pub const ROLES: [&str; 4] = ["chat", "plan/review", "code", "other"];

impl Sample {
    /// Whether the call only kept a cache warm.
    pub(crate) fn is_tick(&self) -> bool {
        self.role.starts_with("tick")
    }

    /// The session the call read: the chat's, or a pair's for every other
    /// role, as ticks keep them.
    fn session(&self) -> bool {
        self.role == "chat" || self.role == "tick-chat"
    }
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
                        ticks: 0,
                        tick_cost: 0.0,
                    });
                    cached.push((0, 0));
                    out.len() - 1
                }
            };
            let m = &mut out[i];
            m.calls += 1;
            m.cost += s.cost.unwrap_or(0.0);
            m.rebuilds += usize::from(s.rebuilt());
            if s.is_tick() {
                m.ticks += 1;
                m.tick_cost += s.cost.unwrap_or(0.0);
            }
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

    /// What ticks saved the calls between `from` and `to`: a call saved
    /// what writing its cache again would have cost more when its own
    /// session's previous call was more than five minutes earlier, a tick
    /// of that session read it within the five minutes before, and it did
    /// read from the cache. The chat's calls go with the chat's ticks, the
    /// others with a pair's.
    pub fn saved_by_ticks(&self, from: u64, to: u64) -> f64 {
        let mut last_call: [Option<u64>; 2] = [None, None];
        let mut last_tick: [Option<u64>; 2] = [None, None];
        let mut saved = 0.0;
        for s in &self.samples {
            let session = usize::from(s.session());
            if s.is_tick() {
                last_tick[session] = Some(s.at);
                continue;
            }
            let lapsed = last_call[session].is_some_and(|at| s.at.saturating_sub(at) > CACHE_LIVES);
            let ticked =
                last_tick[session].is_some_and(|at| s.at.saturating_sub(at) <= CACHE_LIVES);
            let read = s.cache_read.is_some_and(|r| r > 0);
            if lapsed && ticked && read && (from..=to).contains(&s.at) {
                saved += s.rewrite_extra.unwrap_or(0.0);
            }
            last_call[session] = Some(s.at);
        }
        saved
    }

    /// What ticks cost and saved since `from`.
    pub fn tick_balance(&self, from: u64) -> TickBalance {
        let ticks: Vec<&Sample> = self.since(from).filter(|s| s.is_tick()).collect();
        TickBalance {
            spent: ticks.iter().map(|s| s.cost.unwrap_or(0.0)).sum(),
            saved: self.saved_by_ticks(from, u64::MAX),
            ticks: ticks.len(),
        }
    }

    /// The context over time of the calls of `role`: (seconds after
    /// `from`, tokens). The chat's runs on to `now`; another role's to the
    /// chat's next call, which ends the request it worked for.
    pub fn context_of(&self, role: &str, from: u64, now: u64) -> Vec<(f64, f64)> {
        let mut points: Vec<(f64, f64)> = Vec::new();
        let x = |at: u64| at.saturating_sub(from) as f64;
        let mut open: Option<f64> = None;
        for s in self.since(from) {
            if s.role == role {
                if let Some(c) = s.context {
                    points.push((x(s.at), c as f64));
                    open = Some(c as f64);
                }
            } else if s.role == "chat"
                && role != "chat"
                && let Some(c) = open.take()
            {
                points.push((x(s.at), c));
            }
        }
        if role == "chat"
            && let Some(c) = open
        {
            points.push((x(now), c));
        }
        points
    }

    /// The chat's cached prefix over time: at each chat call or chat tick,
    /// what it read from the cache and wrote to it, held until the next,
    /// and down to nothing once five minutes go by without one: when the
    /// cache expired.
    pub fn chat_cache(&self, from: u64, now: u64) -> Vec<(f64, f64)> {
        let x = |at: u64| at.saturating_sub(from) as f64;
        let mut points: Vec<(f64, f64)> = Vec::new();
        let mut held: Option<(u64, f64)> = None;
        let expire = |points: &mut Vec<(f64, f64)>, held: &Option<(u64, f64)>, until: u64| {
            if let Some((at, size)) = *held
                && until.saturating_sub(at) > CACHE_LIVES
            {
                points.push((x(at + CACHE_LIVES), size));
                points.push((x(at + CACHE_LIVES), 0.0));
            }
        };
        for s in self
            .since(from)
            .filter(|s| s.session() && s.role.contains("chat"))
        {
            let size = (s.cache_read.unwrap_or(0) + s.cache_written.unwrap_or(0)) as f64;
            expire(&mut points, &held, s.at);
            if let Some((_, before)) = held {
                points.push((x(s.at), before));
            }
            points.push((x(s.at), size));
            held = Some((s.at, size));
        }
        expire(&mut points, &held, now);
        if let Some((at, size)) = held
            && now.saturating_sub(at) <= CACHE_LIVES
        {
            points.push((x(now), size));
        }
        points
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

    /// A model's ticks on its cost line: (seconds after `from`, dollars
    /// added up once the tick is paid).
    pub fn tick_points(&self, model: &str, from: u64) -> Vec<(f64, f64)> {
        let mut total = 0.0;
        self.since(from)
            .filter(|s| s.model == model)
            .filter_map(|s| {
                total += s.cost.unwrap_or(0.0);
                s.is_tick().then(|| ((s.at - from) as f64, total))
            })
            .collect()
    }

    /// The conversation's context over time, ticks left out: (seconds
    /// after `from`, tokens).
    pub fn context_line(&self, from: u64) -> Vec<(f64, f64)> {
        self.since(from)
            .filter(|s| !s.is_tick())
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
            role: "chat".into(),
            rewrite_extra: None,
        }
    }

    fn call(at: u64, role: &str, read: u64, extra: f64) -> Sample {
        Sample {
            at,
            model: "claude-code".into(),
            input: 10_000,
            output: 10,
            cost: Some(0.01),
            cache_read: Some(read),
            cache_written: Some(100),
            context: (!role.starts_with("tick")).then_some(10_000),
            role: role.into(),
            rewrite_extra: Some(extra),
        }
    }

    #[test]
    fn a_tick_saved_a_call_only_after_a_lapse_with_its_own_session_read() {
        let mut log = UsageLog::default();
        let t = 1_000_000;
        log.push(call(t, "chat", 9_000, 0.5));
        // Six minutes later, read by a tick a minute before: saved.
        log.push(call(t + 300, "tick-chat", 9_000, 0.5));
        log.push(call(t + 360, "chat", 9_000, 0.5));
        // A minute later: no lapse, nothing saved.
        log.push(call(t + 420, "chat", 9_000, 0.5));
        // A pair's call after a lapse, read only by the chat's tick: not
        // saved; then by its own tick: saved.
        log.push(call(t + 430, "code", 9_000, 0.2));
        log.push(call(t + 1_000, "tick-chat", 9_000, 0.5));
        log.push(call(t + 1_100, "plan/review", 9_000, 0.2));
        log.push(call(t + 1_700, "tick-pair", 9_000, 0.2));
        log.push(call(t + 1_800, "code", 9_000, 0.2));
        let saved = log.saved_by_ticks(t, t + 10_000);
        assert!((saved - 0.7).abs() < 1e-9, "{saved}");
        // Only what fell in the window counts.
        assert!((log.saved_by_ticks(t + 1_500, t + 10_000) - 0.2).abs() < 1e-9);
        let balance = log.tick_balance(t);
        assert_eq!(balance.ticks, 3);
        assert!((balance.spent - 0.03).abs() < 1e-9);
        let models = log.models(t);
        assert_eq!((models[0].calls, models[0].ticks), (9, 3));
        let ticks = log.tick_points("claude-code", t);
        assert_eq!(ticks.len(), 3);
        assert_eq!(ticks[0].0, 300.0);
        assert!((ticks[0].1 - 0.02).abs() < 1e-9);
    }

    #[test]
    fn the_chat_cache_drops_once_five_minutes_pass_unread() {
        let mut log = UsageLog::default();
        let t = 1_000_000;
        log.push(call(t, "chat", 9_000, 0.5));
        log.push(call(t + 200, "tick-chat", 9_000, 0.5));
        let line = log.chat_cache(t, t + 1_000);
        // Held from the tick, then dropped five minutes after it.
        assert_eq!(line.last(), Some(&(500.0, 0.0)));
        assert!(line.contains(&(200.0, 9_100.0)));
        // The chat's context runs on to now; a pair's to the chat's next
        // call.
        log.push(call(t + 300, "code", 9_000, 0.2));
        log.push(call(t + 400, "chat", 9_000, 0.5));
        assert_eq!(
            log.context_of("chat", t, t + 900).last(),
            Some(&(900.0, 10_000.0))
        );
        assert_eq!(
            log.context_of("code", t, t + 900),
            [(300.0, 10_000.0), (400.0, 10_000.0)]
        );
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
