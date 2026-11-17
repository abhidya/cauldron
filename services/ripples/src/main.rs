//! The `cauldron-ripples` binary: a cauldron service, started by `cauldron
//! start ripples`, which hands it every `CAULDRON_*` variable; run by hand
//! with none of them, it refuses and exits 2.
//!
//! `cauldron-ripples --build-stamp` is the one thing it answers with no
//! environment: which source produced these bytes (`build.rs`).

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

fn build_stamp() -> String {
    let dirty = match env!("RIPPLES_BUILD_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={dirty} dirt_scope={}",
        env!("RIPPLES_BUILD_COMMIT"),
        env!("RIPPLES_BUILD_PROVENANCE"),
        env!("RIPPLES_BUILD_DIRT_SCOPE")
    )
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-stamp") {
        println!("{}", build_stamp());
        return;
    }
    let se = match env::seams::ServiceEnv::from_env() {
        Ok(se) => se,
        Err(e) => {
            eprintln!("cauldron-ripples: {e:#}");
            std::process::exit(2);
        }
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::io::stderr).with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .with(cauldron_ripples::seam::WarningsLayer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
        .init();
    if let Err(e) = cauldron_ripples::daemon::run(se) {
        tracing::error!("cauldron-ripples: {e:#}");
        eprintln!("cauldron-ripples: {e:#}");
        std::process::exit(1);
    }
}
