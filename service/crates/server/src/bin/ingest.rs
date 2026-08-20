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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Parsed {
    List,
    Run(String),
    Usage,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let parsed = parse_args(std::env::args().skip(1));
    if parsed == Parsed::Usage {
        usage();
    }
    run(registry(), parsed)
}

/// Parse the ingest CLI flags. Unknown flags, missing values, or no arguments
/// all map to `Parsed::Usage` so `main` can exit with code 2.
fn parse_args(mut args: impl Iterator<Item = String>) -> Parsed {
    let mut list = false;
    let mut name: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-list" | "--list" => list = true,
            "-collector" | "--collector" => {
                let Some(n) = args.next() else {
                    return Parsed::Usage;
                };
                name = Some(n);
            }
            "-rate-limit" | "--rate-limit" | "-max-retries" | "--max-retries" | "-batch-size"
            | "--batch-size" => {
                let _ = args.next();
                eprintln!("ingest: warning: {a} is ignored by the Rust runtime");
            }
            _ => return Parsed::Usage,
        }
    }

    if list {
        return Parsed::List;
    }
    match name {
        Some(n) => Parsed::Run(n),
        None => Parsed::Usage,
    }
}

/// Resolve a collector name against the registry. Errors (unknown name) are
/// fatal and propagate as `Err` so the process exits non-zero.
fn select_collectors(
    all: Vec<Box<dyn Collector>>,
    name: String,
) -> Result<Vec<Box<dyn Collector>>> {
    let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
    match all.into_iter().find(|c| c.name() == name) {
        Some(c) => Ok(vec![c]),
        None => bail!("unknown collector {name:?}; registered: {names:?}"),
    }
}

/// Run the selected action against the chassis database.
fn run(all: Vec<Box<dyn Collector>>, parsed: Parsed) -> Result<()> {
    match parsed {
        Parsed::List => {
            for c in &all {
                println!("{}", c.name());
            }
            Ok(())
        }
        Parsed::Run(name) => {
            let selected = select_collectors(all, name)?;
            let cfg = Config::load();
            let mut conn = db::open(&cfg.database_path).context("open db")?;
            db::migrate(&conn).context("migrate")?;
            run_all(&mut conn, selected)?;
            println!("Ingest complete");
            Ok(())
        }
        Parsed::Usage => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chassis::collectors::CollectedEntity;

    struct DummyCollector;

    impl Collector for DummyCollector {
        fn name(&self) -> &'static str {
            "dummy"
        }

        fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
            Ok(vec![])
        }
    }

    fn dummy_registry() -> Vec<Box<dyn Collector>> {
        vec![Box::new(DummyCollector)]
    }

    #[test]
    fn parse_args_list() {
        assert_eq!(
            parse_args(["-list"].map(String::from).into_iter()),
            Parsed::List
        );
    }

    #[test]
    fn parse_args_collector() {
        assert_eq!(
            parse_args(["-collector", "dummy"].map(String::from).into_iter()),
            Parsed::Run("dummy".to_string())
        );
    }

    #[test]
    fn parse_args_collector_double_dash() {
        assert_eq!(
            parse_args(["--collector", "dummy"].map(String::from).into_iter()),
            Parsed::Run("dummy".to_string())
        );
    }

    #[test]
    fn parse_args_ignored_flags() {
        assert_eq!(
            parse_args(
                [
                    "-collector",
                    "dummy",
                    "-rate-limit",
                    "10",
                    "--max-retries",
                    "3"
                ]
                .map(String::from)
                .into_iter()
            ),
            Parsed::Run("dummy".to_string())
        );
    }

    #[test]
    fn parse_args_no_args_is_usage() {
        assert_eq!(parse_args(std::iter::empty()), Parsed::Usage);
    }

    #[test]
    fn parse_args_unknown_flag_is_usage() {
        assert_eq!(
            parse_args(["-foo"].map(String::from).into_iter()),
            Parsed::Usage
        );
    }

    #[test]
    fn parse_args_collector_without_value_is_usage() {
        assert_eq!(
            parse_args(["-collector"].map(String::from).into_iter()),
            Parsed::Usage
        );
    }

    #[test]
    fn select_collectors_finds_match() {
        let selected = select_collectors(dummy_registry(), "dummy".to_string()).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "dummy");
    }

    #[test]
    fn select_collectors_unknown_errors() {
        let result = select_collectors(dummy_registry(), "missing".to_string());
        match result {
            Err(e) => {
                let err = e.to_string();
                assert!(err.contains("unknown collector"));
                assert!(err.contains("dummy"));
            }
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn run_list_prints_names() {
        // list returns Ok and does not touch the database.
        run(dummy_registry(), Parsed::List).unwrap();
    }

    #[test]
    fn run_with_collector_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("DATABASE_PATH", dir.path().join("test.db"));
        run(dummy_registry(), Parsed::Run("dummy".to_string())).unwrap();
    }
}
