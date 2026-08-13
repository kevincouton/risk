//! ingest: run niche collectors against the platform database.
//! Synchronous twin of go-service/cmd/ingest. Flags (single or double dash):
//!   -collector <name>   run one registered collector
//!   -list               list registered collectors and exit
//!   (no flags)          print usage + registered collectors, exit 2 (Go parity:
//!                       go-service/cmd/ingest/main.go:33-37 — R-4 review Finding 1)
//! Go's -rate-limit / -max-retries / -batch-size are accepted and ignored
//! (RunOptions was folded into the fixed chassis runtime: 3 fetch attempts
//! with 1s/2s backoff, single-transaction upsert).

use anyhow::{bail, Context, Result};
use chassis::collectors::{run_all, Collector};
use chassis::{config::Config, db};

/// Collectors registered into the TEMPLATE ingest.
///
/// !!! The template ships ZERO collectors on purpose. Clone-owned
/// `collectors/` crates (deliberately OUTSIDE sync-manifest.txt, spine
/// §layout / spec §4) provide their own thin `ingest` shim binary that
/// registers the clone's collectors and calls `chassis::collectors::run_all`.
/// Do NOT add clone collectors here — this synced binary stays empty.
fn registry() -> Vec<Box<dyn Collector>> {
    vec![]
}

fn usage() -> ! {
    eprintln!("Usage: ingest -collector <name> [-list]");
    eprintln!("  -collector <name>   run one registered collector");
    eprintln!("  -list               list registered collectors and exit");
    eprintln!("  (no flags)          print this usage and exit 2");
    std::process::exit(2);
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let mut list = false;
    let mut name: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-list" | "--list" => list = true,
            "-collector" | "--collector" => name = Some(args.next().unwrap_or_else(|| usage())),
            "-rate-limit" | "--rate-limit" | "-max-retries" | "--max-retries" | "-batch-size"
            | "--batch-size" => {
                let _ = args.next();
                eprintln!("ingest: warning: {a} is ignored by the Rust runtime");
            }
            _ => usage(),
        }
    }

    let all = registry();
    if list {
        for c in &all {
            println!("{}", c.name());
        }
        return Ok(());
    }
    let selected: Vec<Box<dyn Collector>> = match name {
        Some(n) => {
            let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
            match all.into_iter().find(|c| c.name() == n) {
                Some(c) => vec![c],
                None => bail!("unknown collector {n:?}; registered: {names:?}"),
            }
        }
        None => {
            // Go parity (cmd/ingest/main.go:33-37): empty -collector prints
            // usage + the registered names to stderr and exits 2.
            let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
            eprintln!(
                "Usage: ingest -collector <name> [-rate-limit N] [-max-retries N] [-batch-size N]"
            );
            eprintln!("Registered collectors: [{}]", names.join(" "));
            std::process::exit(2);
        }
    };

    let cfg = Config::load();
    let mut conn = db::open(&cfg.database_path).context("open db")?;
    db::migrate(&conn).context("migrate")?;
    run_all(&mut conn, selected)?;
    println!("Ingest complete");
    Ok(())
}
