//! The `cauldron-inference` binary: a cauldron service, started by `cauldron
//! start inference`, which hands it every `CAULDRON_*` variable; run by hand
//! with none of them, it refuses and exits 2.
//!
//! Two things it answers with no environment: `--build-stamp` (which source
//! produced these bytes, `build.rs`) and `--guard -- <program> <args…>` (the
//! guard it runs its own `llama-server` child under, `guard`).

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

fn build_stamp() -> String {
    let dirty = match env!("INFERENCE_BUILD_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={dirty} dirt_scope={}",
        env!("INFERENCE_BUILD_COMMIT"),
        env!("INFERENCE_BUILD_PROVENANCE"),
        env!("INFERENCE_BUILD_DIRT_SCOPE")
    )
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-stamp") {
        println!("{}", build_stamp());
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--guard") {
        let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(2).skip_while(|a| a == "--").collect();
        std::process::exit(cauldron_inference::guard::run(&argv));
    }
    let se = match env::seams::ServiceEnv::from_env() {
        Ok(se) => se,
        Err(e) => {
            eprintln!("cauldron-inference: {e:#}");
            std::process::exit(2);
        }
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::io::stderr).with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .with(cauldron_inference::seam::WarningsLayer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
        .init();
    if let Err(e) = cauldron_inference::daemon::run(se) {
        tracing::error!("cauldron-inference: {e:#}");
        eprintln!("cauldron-inference: {e:#}");
        std::process::exit(1);
    }
}
