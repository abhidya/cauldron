//! The `cauldron-mcp` binary: a cauldron service. Started by `cauldron
//! start mcp` (or the daemon), which hands it every `CAULDRON_*` variable;
//! run by hand with none of them, it refuses and exits 2, because there is
//! no default data root for it to run against by mistake.
//!
//! `cauldron-mcp --build-stamp` is the one thing it answers with no
//! environment: which source produced these bytes (`build.rs`). It reads and
//! writes nothing.

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// `commit=<sha> provenance=<pin|git|unknown> dirty=<1|0|unknown>
/// dirt_scope=<a,b,...>`: the line cauldron's `Stamp` parser reads, in the
/// order `cauldron --build-stamp` prints its own. `dirty` is passed through
/// only as `0` or `1`; anything else is `unknown`, never clean.
fn build_stamp() -> String {
    let dirty = match env!("MCP_BUILD_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={dirty} dirt_scope={}",
        env!("MCP_BUILD_COMMIT"),
        env!("MCP_BUILD_PROVENANCE"),
        env!("MCP_BUILD_DIRT_SCOPE")
    )
}

fn main() {
    // Before the environment is read: a freshly built image has none.
    if std::env::args().nth(1).as_deref() == Some("--build-stamp") {
        println!("{}", build_stamp());
        return;
    }
    let se = match env::seams::ServiceEnv::from_env() {
        Ok(se) => se,
        Err(e) => {
            eprintln!("cauldron-mcp: {e:#}");
            std::process::exit(2);
        }
    };
    tracing_subscriber::registry()
        // No colour codes: cauldron writes this to a log file.
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::io::stderr).with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .with(cauldron_mcp::seam::WarningsLayer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    if let Err(e) = rt.block_on(cauldron_mcp::daemon::run(se)) {
        tracing::error!("cauldron-mcp: {e:#}");
        eprintln!("cauldron-mcp: {e:#}");
        std::process::exit(1);
    }
}
