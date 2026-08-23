/// The `status` command — a one-shot node probe with output to stdout.
///
/// Algorithm:
/// 1. Probe target and reference nodes in parallel (`probe_both`)
/// 2. Analyse the result (`analyze`)
/// 3. Print a human-readable report to stdout
/// 4. Exit with code 1 if a problem is detected
///
/// Example output (healthy):
/// ```text
/// Опрашиваю ноды...
///   target    http://192.168.1.10:8899              slot=300123456   rtt=45ms
///   reference https://api.mainnet-beta.solana.com   slot=300123460   rtt=120ms
/// delta=-4   target_rtt=45ms   статус=OK
/// ```
///
/// Example output (problem detected):
/// ```text
/// Опрашиваю ноды...
///   target    http://192.168.1.10:8899              slot=300000100   rtt=45ms
///   reference https://api.mainnet-beta.solana.com   slot=300000200   rtt=120ms
/// delta=-100   target_rtt=45ms   статус=отставание слотов: -100 (порог: нарушен)
/// [exit code 1]
/// ```
use anyhow::Result;

use crate::analysis;
use crate::config::Config;
use crate::metrics::RpcProbe;

/// Performs a single node probe and prints the result to stdout.
///
/// Exits the process with code 1 if `analysis.needs_alert = true`.
/// This allows use in shell scripts:
/// ```sh
/// solana-cli-sentinel status || notify_oncall.sh
/// ```
pub async fn run(cfg: Config) -> Result<()> {
    println!("Опрашиваю ноды...");

    // probe_both runs both requests in parallel
    let rpc = RpcProbe::new()?;
    let probe = rpc.probe_both(&cfg).await?;
    let analysis = analysis::analyze(&probe, &cfg);

    // Print one line per node
    println!(
        "  target    {:<45}  slot={:<12}  rtt={}ms",
        probe.target.node_url, probe.target.slot, probe.target.rtt_ms
    );
    println!(
        "  reference {:<45}  slot={:<12}  rtt={}ms",
        probe.reference.node_url, probe.reference.slot, probe.reference.rtt_ms
    );

    // Summary line: slot delta and status text
    let status_line = format!(
        "delta={:+}   target_rtt={}ms   статус={}",
        analysis.slot_delta,
        analysis.target_rtt_ms,
        analysis.status_text(),
    );

    if analysis.needs_alert {
        // Print the problem status to stderr so scripts can separate stdout and stderr
        eprintln!("{status_line}");
        // Exit code 1 — standard Unix signal for "problem detected"
        std::process::exit(1);
    } else {
        println!("{status_line}");
    }

    Ok(())
}
