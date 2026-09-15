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
struct IngestArgs {
    list: bool,
    collector: Option<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Option<IngestArgs> {
    let mut list = false;
    let mut collector = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-list" | "--list" => list = true,
            "-collector" | "--collector" => collector = Some(args.next()?),
            "-rate-limit" | "--rate-limit" | "-max-retries" | "--max-retries" | "-batch-size"
            | "--batch-size" => {
                let _ = args.next();
                eprintln!("ingest: warning: {a} is ignored by the Rust runtime");
            }
            _ => return None,
        }
    }
    Some(IngestArgs { list, collector })
}

fn select_by_name(all: Vec<Box<dyn Collector>>, name: String) -> Result<Vec<Box<dyn Collector>>> {
    let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
    match all.into_iter().find(|c| c.name() == name) {
        Some(c) => Ok(vec![c]),
        None => bail!("unknown collector {name:?}; registered: {names:?}"),
    }
}

fn list_collectors(all: &[Box<dyn Collector>]) {
    for c in all {
        println!("{}", c.name());
    }
}

fn run_selected(cfg: &Config, selected: Vec<Box<dyn Collector>>) -> Result<()> {
    let mut conn = db::open(&cfg.database_path).context("open db")?;
    db::migrate(&conn).context("migrate")?;
    run_all(&mut conn, selected)?;
    println!("Ingest complete");
    Ok(())
}

fn dispatch_ingest(cfg: &Config, all: Vec<Box<dyn Collector>>, args: &IngestArgs) -> Result<()> {
    if args.list {
        list_collectors(&all);
        return Ok(());
    }
    match &args.collector {
        Some(name) => run_selected(cfg, select_by_name(all, name.clone())?),
        None => usage(),
    }
}

fn run(cfg: &Config, args: &IngestArgs) -> Result<()> {
    dispatch_ingest(cfg, registry(), args)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args(std::env::args().skip(1)).unwrap_or_else(|| usage());
    let cfg = Config::load();
    run(&cfg, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chassis::collectors::{CollectedEntity, Collector};

    struct Dummy(&'static str);
    impl Collector for Dummy {
        fn name(&self) -> &'static str {
            self.0
        }
        fn fetch(&self) -> anyhow::Result<Vec<CollectedEntity>> {
            Ok(vec![])
        }
    }

    #[test]
    fn parse_args_handles_all_flags() {
        let args = parse_args(["-list"].into_iter().map(String::from));
        assert_eq!(
            args,
            Some(IngestArgs {
                list: true,
                collector: None
            })
        );

        let args = parse_args(["--collector", "foo"].into_iter().map(String::from));
        assert_eq!(
            args,
            Some(IngestArgs {
                list: false,
                collector: Some("foo".into()),
            })
        );

        let args = parse_args(
            ["--rate-limit", "5", "--collector", "bar"]
                .into_iter()
                .map(String::from),
        );
        assert_eq!(
            args,
            Some(IngestArgs {
                list: false,
                collector: Some("bar".into()),
            })
        );
    }

    #[test]
    fn parse_args_rejects_unknown_or_missing_collector() {
        assert!(parse_args(["--unknown"].into_iter().map(String::from)).is_none());
        assert!(parse_args(["--collector"].into_iter().map(String::from)).is_none());
    }

    #[test]
    fn select_by_name_finds_or_errors() {
        let all: Vec<Box<dyn Collector>> = vec![Box::new(Dummy("a")), Box::new(Dummy("b"))];
        match select_by_name(all, "a".into()) {
            Ok(selected) => {
                assert_eq!(selected.len(), 1);
                assert_eq!(selected[0].name(), "a");
            }
            Err(_) => panic!("expected to find collector a"),
        }

        let all: Vec<Box<dyn Collector>> = vec![Box::new(Dummy("a")), Box::new(Dummy("b"))];
        match select_by_name(all, "c".into()) {
            Err(e) => assert!(e.to_string().contains("unknown collector")),
            Ok(_) => panic!("expected unknown collector error"),
        }
    }

    #[test]
    fn list_collectors_prints_names() {
        let all: Vec<Box<dyn Collector>> = vec![Box::new(Dummy("a")), Box::new(Dummy("b"))];
        list_collectors(&all);
    }

    #[test]
    fn run_selected_ingests_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            database_path: dir.path().join("test.db").to_string_lossy().into(),
            ..Config::load()
        };
        let selected: Vec<Box<dyn Collector>> = vec![Box::new(Dummy("empty"))];
        run_selected(&cfg, selected).unwrap();
    }

    #[test]
    fn dispatch_ingest_lists_and_selects() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            database_path: dir.path().join("test.db").to_string_lossy().into(),
            ..Config::load()
        };

        let args = IngestArgs {
            list: true,
            collector: None,
        };
        dispatch_ingest(
            &cfg,
            vec![Box::new(Dummy("empty")) as Box<dyn Collector>],
            &args,
        )
        .unwrap();

        let args = IngestArgs {
            list: false,
            collector: Some("empty".into()),
        };
        dispatch_ingest(
            &cfg,
            vec![Box::new(Dummy("empty")) as Box<dyn Collector>],
            &args,
        )
        .unwrap();
    }
}
