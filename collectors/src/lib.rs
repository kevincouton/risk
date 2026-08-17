//! risk-collectors: clone-owned collectors for risk (dependency-risk scoring).
//! Standalone crate, deliberately outside the service workspace and the
//! template sync manifest (spine §layout).

pub mod depsdev;
pub mod github;
pub mod seeds;
