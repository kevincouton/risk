//! Shared seed set: the GitHub searches that seed risk's entity table and the
//! deps.dev ecosystem each search maps to. The `github` collector (seed) and
//! the `depsdev` collector (enrich) iterate this exact list so their entity
//! sets line up 1:1 and the upsert-by-(platform, full_name) merge works.
//!
//! Query caps: 34 + 33 + 34 = 101 candidate packages ("~100", spec §W2-3).
//! Topic counts verified live 2026-08-14: npm-package 12614, pypi 3680,
//! crates-io 383 — all far above the caps.

/// One GitHub search seed query and its deps.dev ecosystem mapping.
pub struct SeedSearch {
    /// Niche facet written to `CollectedEntity.category` ("npm" | "pypi" | "cargo").
    pub ecosystem: &'static str,
    /// deps.dev v3 system name (uppercase): "NPM" | "PYPI" | "CARGO".
    pub depsdev_system: &'static str,
    /// GitHub search query (the `q` parameter).
    pub query: &'static str,
    /// `per_page` for the search request (single page; GitHub search caps at 100).
    pub per_page: u32,
}

pub const SEED_SEARCHES: &[SeedSearch] = &[
    SeedSearch {
        ecosystem: "npm",
        depsdev_system: "NPM",
        query: "topic:npm-package",
        per_page: 34,
    },
    SeedSearch {
        ecosystem: "pypi",
        depsdev_system: "PYPI",
        query: "topic:pypi",
        per_page: 33,
    },
    SeedSearch {
        ecosystem: "cargo",
        depsdev_system: "CARGO",
        query: "topic:crates-io",
        per_page: 34,
    },
];
