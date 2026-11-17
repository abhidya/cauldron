//! The `cauldron-mcp` binary: a cauldron service. Started by `cauldron
//! start mcp` (or the daemon), which hands it every `CAULDRON_*` variable;
//! run by hand with none of them, it refuses and exits 2, because there is
//! no default data root for it to run against by mistake.

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

fn main() {
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
