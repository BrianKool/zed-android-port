//! Zdroid runtime port + adapters.
//!
//! The editor itself never calls a userland directly. It calls into this
//! crate's [`RuntimeProvider`] port, and a concrete adapter (chroot,
//! bootstrap, or external Termux) handles the actual spawn. Users pick
//! their adapter via `runtime.toml`; switching is one app restart away.
//!
//! # Crate layout
//!
//! - [`port`] — the [`RuntimeProvider`] trait + spawn types. The contract
//!   every adapter satisfies.
//! - [`config`] — `runtime.toml` schema + serde, plus the
//!   [`config::ResolvedConfig`] that downstream code actually consumes.
//! - [`health`] — [`health::HealthStatus`] feasibility-ping primitive +
//!   the [`health::ProgressSink`] hook used by long-running install ops.
//! - [`setup`] — install/uninstall lifecycle types and the install
//!   wizard step descriptor that the settings UI renders.
//! - [`adapters`] — concrete provider impls. Each lives in its own
//!   submodule and is feature-gated for compile-out scenarios.
//!
//! # Architecture
//!
//! See `memory/project_runtime_swap_architecture.md` for the design
//! decision and trade-offs. tl;dr:
//!
//! ```text
//! Zed editor ───► PATH lookup ───► $PREFIX/zd-runtime/<name> ───► zd-exec
//!                                                                   │
//!                                                                   ▼
//!                                            ┌───────────────────────────────┐
//!                                            │ zdroid_runtime (this crate)   │
//!                                            │   reads runtime.toml          │
//!                                            │   picks adapter               │
//!                                            │   calls .spawn()              │
//!                                            └─────────────┬─────────────────┘
//!                                                          │
//!                              ┌───────────────────────────┼───────────────────────────┐
//!                              ▼                           ▼                           ▼
//!                       chroot adapter            bootstrap adapter         external Termux adapter
//!                       (zd-spawnd socket)        (own $PREFIX, opt. proot) (RUN_COMMAND intent)
//! ```

pub mod adapters;
pub mod config;
pub mod elf;
pub mod health;
pub mod port;
pub mod setup;

pub use config::{AdapterConfig, ResolvedConfig, RuntimeId};
pub use elf::{Architecture, BinaryFormat, ElfInfo, LibcFamily, inspect_binary};
pub use health::{HealthStatus, ProgressSink};
pub use port::{RuntimeProvider, SpawnHandle, SpawnRequest};
pub use setup::{InstallStep, SetupOutcome};
