//! risk ingest: run risk's niche collectors against the platform database.
//! Thin shim over chassis::collectors — CLI contract mirrors
//! service/crates/server/src/bin/ingest.rs exactly:
//!   -collector <name>   run one registered collector
//!   -list               list registered collectors and exit
//!   (no flags)          print usage + registered collectors, exit 2
//! Go-legacy flags (-rate-limit / -max-retries / -batch-size) are accepted
//! and ignored (fixed chassis runtime: 3 fetch attempts, 1s/2s backoff,
//! single-transaction upsert).

use anyhow::{bail, Context, Result};
use chassis::collectors::{run_all, Collector};
use chassis::{config::Config, db};

/// Collectors registered into risk's ingest.
fn registry() -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(risk_collectors::github::GithubCollector::from_env()),
        Box::new(risk_collectors::depsdev::DepsDevCollector::from_env()),
    ]
}

fn usage() -> ! {
    eprintln!("Usage: ingest -collector <name> [-list]");
    eprintln!("  -collector <name>   run one registered collector");
    eprintln!("  -list               list registered collectors and exit");
    eprintln!("  (no flags)          print this usage and exit 2");
    std::process::exit(2);
}

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    run(registry(), parse_args(std::env::args().skip(1)))
}

enum Parsed {
    List,
    Run(String),
    Usage,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Parsed {
    let mut list = false;
    let mut name: Option<String> = None;
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

    if list {
        return Parsed::List;
    }
    match name {
        Some(n) => Parsed::Run(n),
        None => {
            eprintln!(
                "Usage: ingest -collector <name> [-rate-limit N] [-max-retries N] [-batch-size N]"
            );
            Parsed::Usage
        }
    }
}

fn select_collectors(all: Vec<Box<dyn Collector>>, name: &str) -> Result<Vec<Box<dyn Collector>>> {
    let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
    match all.into_iter().find(|c| c.name() == name) {
        Some(c) => Ok(vec![c]),
        None => bail!("unknown collector {name:?}; registered: {names:?}"),
    }
}

fn run(all: Vec<Box<dyn Collector>>, parsed: Parsed) -> Result<()> {
    match parsed {
        Parsed::List => {
            for c in &all {
                println!("{}", c.name());
            }
            return Ok(());
        }
        Parsed::Usage => {
            let names: Vec<&'static str> = all.iter().map(|c| c.name()).collect();
            eprintln!(
                "Usage: ingest -collector <name> [-rate-limit N] [-max-retries N] [-batch-size N]"
            );
            eprintln!("Registered collectors: [{}]", names.join(" "));
            std::process::exit(2);
        }
        Parsed::Run(name) => {
            let selected = select_collectors(all, &name)?;
            let cfg = Config::load();
            let mut conn = db::open(&cfg.database_path).context("open db")?;
            db::migrate(&conn).context("migrate")?;
            run_all(&mut conn, selected)?;
            println!("Ingest complete");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_list() {
        assert!(matches!(
            parse_args(["--list"].map(String::from).into_iter()),
            Parsed::List
        ));
    }

    #[test]
    fn parse_args_collector() {
        assert!(
            matches!(parse_args(["--collector", "github"].map(String::from).into_iter()), Parsed::Run(n) if n == "github")
        );
    }

    #[test]
    fn parse_args_usage() {
        assert!(matches!(parse_args(std::iter::empty()), Parsed::Usage));
    }

    #[test]
    fn select_collectors_known() {
        let all = registry();
        let selected = select_collectors(all, "github").unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name(), "github");
    }

    #[test]
    fn select_collectors_unknown() {
        let all = registry();
        match select_collectors(all, "nope") {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("unknown collector"));
                assert!(msg.contains("github"));
            }
            Ok(_) => panic!("unknown collector should not succeed"),
        }
    }
}
