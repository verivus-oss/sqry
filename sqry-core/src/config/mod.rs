//! Configuration module for sqry.
//!
//! This module contains all configuration-related functionality,
//! including buffer sizes, tuning parameters, and environment variable overrides.
//!
//! # Project Configuration
//!
//! Project-level configuration is loaded from `.sqry-config.toml` files.
//! See [`crate::config::ProjectConfig`] for details.
//!
//! # Configuration Snapshots
//!
//! The [`snapshot`](crate::config::snapshot) module captures effective configuration for embedding
//! into the CodeGraph, enabling provenance tracking and reproducibility.
//! See [`ConfigSnapshot`](crate::config::snapshot::ConfigSnapshot) for details.

pub mod buffers;
pub mod graph_config_persistence;
pub mod graph_config_schema;
pub mod graph_config_store;
pub mod migration;
pub mod project_config;
pub mod recursion;
pub mod snapshot;
pub mod workspace;

pub use graph_config_persistence::{
    ConfigPersistence, IntegrityStatus, LoadReport, LockInfo, PersistenceError, PersistenceResult,
    RepairReport, SchemaStatus,
};

pub use graph_config_schema::{
    AliasEntry, BuffersConfig, CliPreferences, DurabilityConfig, GraphConfig,
    GraphConfigExtensions, GraphConfigFile, GraphConfigIntegrity, GraphConfigMetadata,
    LimitsConfig, LockingConfig, OutputConfig, ParallelismConfig, PersistenceConfig,
    SCHEMA_VERSION, SchemaValidationError, TimeoutsConfig, ValidationConfig, ValidationResult,
    WrittenByInfo,
};

pub use graph_config_store::{
    GraphConfigError, GraphConfigPaths, GraphConfigStore, Result as GraphConfigResult,
};

pub use project_config::{
    CONFIG_FILE_NAME, CacheConfig, ConfigError, IgnoreConfig, IncludeConfig, IndexingConfig,
    LanguageConfig, ProjectConfig,
};

pub use snapshot::{
    CONFIG_INVENTORY, CONFIG_PROVENANCE_FILENAME, CONFIG_SCHEMA_VERSION, ConfigEntry,
    ConfigProvenance, ConfigRisk, ConfigScope, ConfigSnapshot, ConfigSnapshotBuilder, ConfigSource,
    collect_snapshot, validate_completeness,
};

pub use migration::{
    MigrationError, MigrationReport, MigrationResult, detect_legacy_config,
    is_new_config_initialized, log_deprecation_warning_if_legacy_exists, migrate_legacy_config,
};

pub use recursion::RecursionLimits;

pub use workspace::WorkspaceConfig;
