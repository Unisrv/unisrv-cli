use indicatif::{ProgressBar, ProgressStyle};

pub mod auth;
pub mod config;
pub mod error;
pub mod hosts;
pub mod instances;
pub mod login;
pub mod networks;
pub mod registry;
pub mod services;
pub mod table;

pub fn default_spinner() -> ProgressBar {
    let spinner_style = ProgressStyle::with_template("{spinner} {prefix:.bold.dim} {wide_msg}")
        .unwrap()
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");

    let progress = ProgressBar::new_spinner();
    progress.set_style(spinner_style);
    progress.enable_steady_tick(std::time::Duration::from_millis(50));
    progress
}
