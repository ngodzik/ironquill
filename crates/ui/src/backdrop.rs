//! A quiet pattern behind the window's panels, as `/background` and Ctrl-X
//! choose it. The window draws it; this is only what was chosen.

use serde::{Deserialize, Serialize};

/// What is drawn behind the panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    /// Scattered points of light.
    Stars,
    /// Long soft curves.
    Waves,
    /// Thin lines, evenly spaced.
    Grid,
    /// Short falling streaks.
    Rain,
}

impl Pattern {
    /// Every pattern, in the order Ctrl-X goes through them.
    pub const ALL: [Self; 4] = [Self::Stars, Self::Waves, Self::Grid, Self::Rain];

    /// Its name, as `/background` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stars => "stars",
            Self::Waves => "waves",
            Self::Grid => "grid",
            Self::Rain => "rain",
        }
    }

    /// The pattern named `name`.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// What comes after `current` on Ctrl-X: the next pattern, none after
    /// the last, the first after none.
    pub fn next(current: Option<Self>) -> Option<Self> {
        match current {
            None => Some(Self::ALL[0]),
            Some(pattern) => {
                let at = Self::ALL.iter().position(|p| *p == pattern).unwrap_or(0);
                Self::ALL.get(at + 1).copied()
            }
        }
    }
}

/// Whether the pattern moves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Drawn only when the window draws anyway: it costs nothing.
    #[default]
    Still,
    /// About twelve frames a second while the window has the focus, asleep
    /// behind other windows.
    Slow,
}

impl Motion {
    /// Its name, as `/background` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::Slow => "slow",
        }
    }
}

/// The pattern behind the panels, if any, and whether it moves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Backdrop {
    /// What is drawn; `None` draws nothing, the default.
    pub pattern: Option<Pattern>,
    /// Whether it moves.
    pub motion: Motion,
}

impl Backdrop {
    /// Applies the words of `/background`: a pattern, `off`, `still` or
    /// `slow`, in any order. Returns what is now set, to say so.
    ///
    /// # Errors
    ///
    /// A sentence for the person naming the word not understood.
    pub fn apply(&mut self, words: &str) -> Result<String, String> {
        let mut next = *self;
        for word in words.split_whitespace() {
            let word = word.to_lowercase();
            match word.as_str() {
                "off" | "none" => next.pattern = None,
                "still" => next.motion = Motion::Still,
                "slow" => next.motion = Motion::Slow,
                name => match Pattern::named(name) {
                    Some(pattern) => next.pattern = Some(pattern),
                    None => {
                        return Err(format!(
                            "No background {name}: stars, waves, grid, rain or off, then still \
                             or slow"
                        ));
                    }
                },
            }
        }
        *self = next;
        Ok(self.describe())
    }

    /// What is set, in a sentence.
    pub fn describe(&self) -> String {
        match self.pattern {
            None => {
                "No background (stars, waves, grid or rain; Ctrl-X goes through them)".to_owned()
            }
            Some(pattern) => format!(
                "Background: {}, {} (Ctrl-X: the next one; /background off)",
                pattern.name(),
                self.motion.name()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_of_background_set_a_pattern_and_its_motion() {
        let mut backdrop = Backdrop::default();
        assert!(backdrop.describe().starts_with("No background"));
        assert_eq!(
            backdrop.apply("waves slow").unwrap(),
            "Background: waves, slow (Ctrl-X: the next one; /background off)"
        );
        assert_eq!(backdrop.pattern, Some(Pattern::Waves));
        backdrop.apply("still").unwrap();
        assert_eq!(
            backdrop,
            Backdrop {
                pattern: Some(Pattern::Waves),
                motion: Motion::Still
            }
        );
        // A word not understood changes nothing.
        assert!(backdrop.apply("off clouds").unwrap_err().contains("clouds"));
        assert_eq!(backdrop.pattern, Some(Pattern::Waves));
        backdrop.apply("off").unwrap();
        assert_eq!(backdrop.pattern, None);
    }

    #[test]
    fn ctrl_x_goes_round_then_to_none() {
        let mut seen = Vec::new();
        let mut current = None;
        for _ in 0..6 {
            current = Pattern::next(current);
            seen.push(current.map(Pattern::name));
        }
        assert_eq!(
            seen,
            [
                Some("stars"),
                Some("waves"),
                Some("grid"),
                Some("rain"),
                None,
                Some("stars")
            ]
        );
    }
}
