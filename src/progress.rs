//! Live step feedback for the long, multi-step `up` / `destroy` flows.
//!
//! A [`Progress`] is injected like the [`Prompter`](crate::commands::up::env_resolve::Prompter)
//! and `Waiter` seams so tests run silent. Each network step opens a
//! [`Step`]: an animated spinner on a TTY, plain lines when piped, nothing in
//! tests. The [`Step`] owns both the transient spinner *and* the permanent
//! result line — drop it without a terminal call and it reports a failure, so
//! any `?` early-return self-reports which step was in flight.
//!
//! Streams: spinner → stderr (auto-hidden off-TTY); result lines → stdout
//! (so a piped run still gets the `+`/`~`/`-` audit log). Colour is gated on
//! stdout, animation on stderr — they can differ.

use std::time::Duration;

use console::style;
use indicatif::{ProgressBar, ProgressStyle};

/// The resource a step acts on. Picks the leading emoji.
#[derive(Clone, Copy)]
pub enum Icon {
    Environment,
    Service,
    Deployment,
    Host,
    Instance,
    Lookup,
}

impl Icon {
    fn emoji(self) -> &'static str {
        match self {
            Icon::Environment => "🌍",
            Icon::Service => "📦",
            Icon::Deployment => "🚀",
            Icon::Host => "🌐",
            Icon::Instance => "🖥️",
            Icon::Lookup => "🔍",
        }
    }
}

/// The action a finished step represents. Picks the sigil + colour, mirroring
/// the plan-diff vocabulary: `+` create, `~` change, `-` remove, `-/+`
/// recreate, `!` warn.
#[derive(Clone, Copy)]
pub enum Tone {
    Add,
    Change,
    Remove,
    Recreate,
    Warn,
}

impl Tone {
    fn sigil(self) -> &'static str {
        match self {
            Tone::Add => "+",
            Tone::Change => "~",
            Tone::Remove => "-",
            Tone::Recreate => "-/+",
            Tone::Warn => "!",
        }
    }
}

/// Render the permanent success line: `  {sigil} {emoji} {summary}`. Pure so it
/// can be unit-tested without a terminal; the sigil is coloured per [`Tone`]
/// only when `color` is set.
fn success_line(icon: Icon, tone: Tone, summary: &str, color: bool) -> String {
    let sigil = if color {
        let s = style(tone.sigil());
        let s = match tone {
            Tone::Add => s.green(),
            Tone::Change => s.cyan(),
            Tone::Remove => s.red(),
            Tone::Recreate => s.magenta(),
            Tone::Warn => s.yellow(),
        };
        s.to_string()
    } else {
        tone.sigil().to_string()
    };
    format!("  {sigil} {} {summary}", icon.emoji())
}

/// Render the failure line for a step dropped before completion: a red `✗`,
/// the resource emoji, and the step's in-flight (active) message.
fn failure_line(icon: Icon, active: &str, color: bool) -> String {
    let mark = if color {
        style("✗").red().to_string()
    } else {
        "✗".to_string()
    };
    format!("  {mark} {} {active}", icon.emoji())
}

fn spinner_style() -> ProgressStyle {
    // Trailing space is the "finished" frame; we clear before it shows anyway.
    ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .expect("static spinner template is valid")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

/// A step in progress. Created by [`Progress::step`], finished by [`Step::finish`]
/// (prints the result line) or [`Step::clear`] (silent, for read steps).
/// Dropping it without either reports a failure — the `?` early-return path.
pub struct Step {
    state: StepState,
    icon: Icon,
    /// The in-flight message, replayed on the failure line if the step is
    /// dropped before a terminal call.
    active: String,
    color: bool,
    /// Whether result/failure lines are printed at all. `false` only for the
    /// silent test channel, which suppresses everything.
    emit: bool,
    /// Set by `finish`/`clear` so `Drop` knows the step was handled.
    done: bool,
}

enum StepState {
    /// No animation, but result lines still print (piped/non-TTY, and the
    /// silent test channel which additionally sets `emit = false`).
    Plain,
    /// TTY: an animated stderr spinner backs the step.
    Animated(ProgressBar),
}

impl Step {
    /// Replace the spinner message mid-flight (e.g. a draining counter). No-op
    /// when not animating.
    pub fn update(&self, active: &str) {
        if let StepState::Animated(bar) = &self.state {
            bar.set_message(format!("{} {active}", self.icon.emoji()));
        }
    }

    /// Successful terminal: clear the spinner and print the permanent result
    /// line to stdout.
    pub fn finish(mut self, tone: Tone, summary: &str) {
        self.clear_spinner();
        if self.emit {
            println!("{}", success_line(self.icon, tone, summary, self.color));
        }
        self.done = true;
    }

    /// Transient terminal: clear the spinner, print nothing. For read steps
    /// (fetch/resolve) whose result is the work that follows, not a line.
    pub fn clear(mut self) {
        self.clear_spinner();
        self.done = true;
    }

    fn clear_spinner(&self) {
        if let StepState::Animated(bar) = &self.state {
            bar.finish_and_clear();
        }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // Reached without `finish`/`clear`: an error propagated past us. Clear
        // the spinner and mark the failed step (the real cause bubbles via the
        // anyhow context and prints at the top level).
        self.clear_spinner();
        if self.emit {
            eprintln!("{}", failure_line(self.icon, &self.active, self.color));
        }
    }
}

/// Injected feedback channel. Production uses [`SpinnerProgress`]; tests use
/// [`SilentProgress`].
pub trait Progress {
    /// Open a step. `active` is the in-flight description, e.g. "Creating
    /// service web".
    fn step(&self, icon: Icon, active: &str) -> Step;
}

/// Terminal-aware progress: spinner on a TTY, plain lines when piped.
pub struct SpinnerProgress {
    animate: bool,
    color: bool,
}

impl SpinnerProgress {
    /// Spinner on stderr, colour gated on stdout. They can differ (e.g. stdout
    /// piped, stderr a terminal).
    pub fn new() -> Self {
        Self {
            animate: console::user_attended_stderr(),
            color: console::Term::stdout().features().colors_supported(),
        }
    }
}

impl Default for SpinnerProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress for SpinnerProgress {
    fn step(&self, icon: Icon, active: &str) -> Step {
        let state = if self.animate {
            let bar = ProgressBar::new_spinner();
            bar.set_style(spinner_style());
            bar.enable_steady_tick(Duration::from_millis(80));
            bar.set_message(format!("{} {active}", icon.emoji()));
            StepState::Animated(bar)
        } else {
            StepState::Plain
        };
        Step {
            state,
            icon,
            active: active.to_string(),
            color: self.color,
            emit: true,
            done: false,
        }
    }
}

/// No-op progress for tests: every step is silent and reports nothing on drop.
#[cfg(test)]
pub struct SilentProgress;

#[cfg(test)]
impl Progress for SilentProgress {
    fn step(&self, icon: Icon, active: &str) -> Step {
        Step {
            state: StepState::Plain,
            icon,
            active: active.to_string(),
            color: false,
            emit: false,
            done: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_line_uncolored_uses_sigil_emoji_and_summary() {
        assert_eq!(
            success_line(Icon::Service, Tone::Add, "service web created", false),
            "  + 📦 service web created"
        );
    }

    #[test]
    fn success_line_sigils_match_the_diff_vocabulary() {
        let cases = [
            (Tone::Add, "+"),
            (Tone::Change, "~"),
            (Tone::Remove, "-"),
            (Tone::Recreate, "-/+"),
            (Tone::Warn, "!"),
        ];
        for (tone, sigil) in cases {
            let line = success_line(Icon::Deployment, tone, "x", false);
            assert_eq!(line, format!("  {sigil} 🚀 x"));
        }
    }

    #[test]
    fn failure_line_uncolored_marks_the_active_step() {
        assert_eq!(
            failure_line(Icon::Service, "Creating service api", false),
            "  ✗ 📦 Creating service api"
        );
    }

    #[test]
    fn colored_line_has_same_visible_text_as_uncolored() {
        // Whether `console` actually emits ANSI depends on the runtime terminal
        // (off in tests), but the colour path must never change the *visible*
        // text — stripping any codes yields exactly the uncolored line.
        let colored = success_line(Icon::Service, Tone::Add, "service web created", true);
        let plain = success_line(Icon::Service, Tone::Add, "service web created", false);
        assert_eq!(console::strip_ansi_codes(&colored), plain);
    }
}
