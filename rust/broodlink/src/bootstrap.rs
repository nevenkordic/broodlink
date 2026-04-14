/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Bootstrap graph — formalized, ordered startup pipeline.
//!
//! Instead of an ad-hoc sequence of setup calls, the bootstrap graph defines
//! explicit stages with dependencies.  Each stage runs only after its
//! prerequisites succeed, and failures are reported with context.
//!
//! # Stage pipeline (default)
//!
//! ```text
//! Prefetch → Config → Dependencies → Databases → Services → Deferred → Ready
//! ```
//!
//! Adding a new initialization concern means adding a stage — the graph
//! ensures it runs in the right order relative to everything else.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

/// Unique identifier for a bootstrap stage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StageId {
    /// Pre-flight checks: detect platform, load environment.
    Prefetch,
    /// Load and validate configuration (config.toml).
    Config,
    /// Ensure external dependencies are running (Ollama, Dolt, PG, NATS, Qdrant).
    Dependencies,
    /// Initialize database schemas and run pending migrations.
    Databases,
    /// Start Broodlink services (beads-bridge, coordinator, etc.).
    Services,
    /// Trust-gated deferred init: plugins, MCP, hooks (loaded after services are healthy).
    DeferredInit,
    /// All stages complete — system is ready to serve.
    Ready,
    /// Custom extension stages added by plugins or configuration.
    Custom(String),
}

impl fmt::Display for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefetch => write!(f, "prefetch"),
            Self::Config => write!(f, "config"),
            Self::Dependencies => write!(f, "dependencies"),
            Self::Databases => write!(f, "databases"),
            Self::Services => write!(f, "services"),
            Self::DeferredInit => write!(f, "deferred_init"),
            Self::Ready => write!(f, "ready"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Outcome of a single stage execution.
#[derive(Debug, Clone)]
pub enum StageResult {
    /// Stage completed successfully.
    Ok,
    /// Stage completed with warnings (non-fatal issues).
    Warn(String),
    /// Stage failed — downstream stages should not run.
    Failed(String),
    /// Stage was skipped (e.g. not applicable in this configuration).
    Skipped(String),
}

impl StageResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok | Self::Warn(_))
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// Recorded result of a completed stage.
#[derive(Debug, Clone)]
pub struct StageRecord {
    pub id: StageId,
    pub result: StageResult,
    pub duration: Duration,
}

/// A bootstrap stage definition.
struct StageDef {
    id: StageId,
    /// Stages that must complete successfully before this one runs.
    depends_on: Vec<StageId>,
    /// Human-readable description for logging.
    description: String,
}

/// The bootstrap graph — manages ordered stage execution.
///
/// # Usage
///
/// ```rust,no_run
/// use broodlink::bootstrap::{BootstrapGraph, StageId, StageResult};
///
/// let mut graph = BootstrapGraph::default_pipeline();
///
/// // Register handlers
/// graph.on_stage(StageId::Prefetch, |_| {
///     // detect platform, load env
///     Ok(StageResult::Ok)
/// });
///
/// // Run the full pipeline
/// let report = graph.run()?;
/// ```
pub struct BootstrapGraph {
    stages: Vec<StageDef>,
    handlers: HashMap<StageId, Box<dyn FnOnce(&[StageRecord]) -> Result<StageResult, String>>>,
    records: Vec<StageRecord>,
}

impl BootstrapGraph {
    /// Create a new empty bootstrap graph.
    pub fn new() -> Self {
        Self {
            stages: Vec::new(),
            handlers: HashMap::new(),
            records: Vec::new(),
        }
    }

    /// Build the default Broodlink startup pipeline with standard stage ordering.
    pub fn default_pipeline() -> Self {
        let mut graph = Self::new();

        graph.add_stage(StageId::Prefetch, vec![], "Pre-flight checks: platform, environment");
        graph.add_stage(StageId::Config, vec![StageId::Prefetch], "Load and validate configuration");
        graph.add_stage(StageId::Dependencies, vec![StageId::Config], "Ensure external dependencies are running");
        graph.add_stage(StageId::Databases, vec![StageId::Dependencies], "Initialize databases and run migrations");
        graph.add_stage(StageId::Services, vec![StageId::Databases], "Start Broodlink backend services");
        graph.add_stage(StageId::DeferredInit, vec![StageId::Services], "Trust-gated deferred initialization");
        graph.add_stage(StageId::Ready, vec![StageId::DeferredInit], "System ready");

        graph
    }

    /// Register a new stage with its dependencies.
    pub fn add_stage(
        &mut self,
        id: StageId,
        depends_on: Vec<StageId>,
        description: impl Into<String>,
    ) {
        self.stages.push(StageDef {
            id,
            depends_on,
            description: description.into(),
        });
    }

    /// Insert a custom stage before an existing stage.
    pub fn insert_before(
        &mut self,
        new_id: StageId,
        before: &StageId,
        description: impl Into<String>,
    ) {
        // Find the target stage's dependencies and use them as our deps
        let deps = self.stages
            .iter()
            .find(|s| &s.id == before)
            .map(|s| s.depends_on.clone())
            .unwrap_or_default();

        // Update the target to depend on us instead
        if let Some(target) = self.stages.iter_mut().find(|s| &s.id == before) {
            target.depends_on = vec![new_id.clone()];
        }

        self.add_stage(new_id, deps, description);
    }

    /// Register a handler for a stage.  The handler receives all completed
    /// stage records for context and returns a result.
    pub fn on_stage<F>(&mut self, id: StageId, handler: F)
    where
        F: FnOnce(&[StageRecord]) -> Result<StageResult, String> + 'static,
    {
        self.handlers.insert(id, Box::new(handler));
    }

    /// Execute the full bootstrap pipeline in dependency order.
    ///
    /// Returns all stage records.  Stops on the first failed stage (unless
    /// the failure is in a non-blocking stage).
    pub fn run(&mut self) -> Result<Vec<StageRecord>, BootstrapError> {
        // Topological sort of stages
        let order = self.topo_sort()?;

        for stage_id in order {
            let stage = self.stages.iter().find(|s| s.id == stage_id).unwrap();
            let description = stage.description.clone();

            // Check that all dependencies succeeded
            for dep in &stage.depends_on {
                let dep_record = self.records.iter().find(|r| &r.id == dep);
                match dep_record {
                    Some(r) if r.result.is_failed() => {
                        let result = StageResult::Skipped(format!(
                            "dependency '{}' failed", dep
                        ));
                        self.records.push(StageRecord {
                            id: stage_id.clone(),
                            result,
                            duration: Duration::ZERO,
                        });
                        continue;
                    }
                    None => {
                        // Dependency hasn't run — should not happen after topo sort
                        return Err(BootstrapError::DependencyNotRun(
                            stage_id.to_string(),
                            dep.to_string(),
                        ));
                    }
                    _ => {} // dependency succeeded, continue
                }
            }

            // Execute the handler
            let start = Instant::now();
            let result = if let Some(handler) = self.handlers.remove(&stage_id) {
                match handler(&self.records) {
                    Ok(r) => r,
                    Err(e) => StageResult::Failed(e),
                }
            } else {
                // No handler registered — auto-succeed (allows stages to be
                // structural markers like Ready)
                StageResult::Ok
            };

            let duration = start.elapsed();

            tracing::info!(
                stage = %stage_id,
                description = %description,
                result = ?result,
                duration_ms = duration.as_millis() as u64,
                "bootstrap stage completed"
            );

            let failed = result.is_failed();
            self.records.push(StageRecord {
                id: stage_id.clone(),
                result,
                duration,
            });

            // Stop pipeline on hard failure (except for DeferredInit which is soft)
            if failed && stage_id != StageId::DeferredInit {
                return Err(BootstrapError::StageFailed(
                    stage_id.to_string(),
                    self.records.last().unwrap().result.clone(),
                ));
            }
        }

        Ok(self.records.clone())
    }

    /// Return the current stage records (useful for status reporting).
    pub fn records(&self) -> &[StageRecord] {
        &self.records
    }

    /// Simple topological sort based on dependency declarations.
    fn topo_sort(&self) -> Result<Vec<StageId>, BootstrapError> {
        let mut sorted: Vec<StageId> = Vec::new();
        let mut visited: HashMap<StageId, bool> = HashMap::new(); // true = permanent mark

        for stage in &self.stages {
            if !visited.contains_key(&stage.id) {
                self.visit(&stage.id, &mut visited, &mut sorted)?;
            }
        }

        Ok(sorted)
    }

    fn visit(
        &self,
        id: &StageId,
        visited: &mut HashMap<StageId, bool>,
        sorted: &mut Vec<StageId>,
    ) -> Result<(), BootstrapError> {
        if let Some(&permanent) = visited.get(id) {
            if permanent {
                return Ok(()); // already processed
            }
            return Err(BootstrapError::CyclicDependency(id.to_string()));
        }

        visited.insert(id.clone(), false); // temporary mark

        if let Some(stage) = self.stages.iter().find(|s| &s.id == id) {
            for dep in &stage.depends_on {
                self.visit(dep, visited, sorted)?;
            }
        }

        visited.insert(id.clone(), true); // permanent mark
        sorted.push(id.clone());
        Ok(())
    }
}

/// Errors during bootstrap execution.
#[derive(Debug)]
pub enum BootstrapError {
    /// A required stage failed.
    StageFailed(String, StageResult),
    /// A dependency cycle was detected.
    CyclicDependency(String),
    /// A dependency stage didn't run when expected.
    DependencyNotRun(String, String),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StageFailed(stage, result) => {
                write!(f, "bootstrap stage '{stage}' failed: {result:?}")
            }
            Self::CyclicDependency(stage) => {
                write!(f, "cyclic dependency detected involving stage '{stage}'")
            }
            Self::DependencyNotRun(stage, dep) => {
                write!(f, "stage '{stage}' requires '{dep}' which hasn't run")
            }
        }
    }
}

impl std::error::Error for BootstrapError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pipeline_has_seven_stages() {
        let graph = BootstrapGraph::default_pipeline();
        assert_eq!(graph.stages.len(), 7);
    }

    #[test]
    fn topo_sort_produces_valid_order() {
        let graph = BootstrapGraph::default_pipeline();
        let order = graph.topo_sort().unwrap();
        // Prefetch must come before Config
        let prefetch_pos = order.iter().position(|s| *s == StageId::Prefetch).unwrap();
        let config_pos = order.iter().position(|s| *s == StageId::Config).unwrap();
        assert!(prefetch_pos < config_pos);
        // Services must come before DeferredInit
        let services_pos = order.iter().position(|s| *s == StageId::Services).unwrap();
        let deferred_pos = order.iter().position(|s| *s == StageId::DeferredInit).unwrap();
        assert!(services_pos < deferred_pos);
    }

    #[test]
    fn pipeline_runs_with_default_handlers() {
        let mut graph = BootstrapGraph::default_pipeline();
        // No handlers registered — all stages auto-succeed
        let records = graph.run().unwrap();
        assert_eq!(records.len(), 7);
        assert!(records.iter().all(|r| r.result.is_ok()));
    }

    #[test]
    fn failed_stage_stops_pipeline() {
        let mut graph = BootstrapGraph::default_pipeline();
        graph.on_stage(StageId::Dependencies, |_| {
            Err("NATS not found".to_string())
        });
        let result = graph.run();
        assert!(result.is_err());
    }

    #[test]
    fn insert_before_works() {
        let mut graph = BootstrapGraph::default_pipeline();
        graph.insert_before(
            StageId::Custom("tls_setup".into()),
            &StageId::Services,
            "Generate TLS certificates",
        );
        let order = graph.topo_sort().unwrap();
        let tls_pos = order.iter().position(|s| *s == StageId::Custom("tls_setup".into())).unwrap();
        let services_pos = order.iter().position(|s| *s == StageId::Services).unwrap();
        assert!(tls_pos < services_pos);
    }
}
