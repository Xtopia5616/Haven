//! Low-level operating system support shared by process-owning adapters.

#[cfg(windows)]
pub mod appcontainer_process;
pub mod credentials;
pub mod filesystem;
pub mod process_containment;
pub mod process_environment;
#[cfg(windows)]
mod sandbox_scratch;
#[cfg(windows)]
pub mod windows_sandbox;

#[cfg(all(test, windows))]
#[path = "appcontainer_probe_tests.rs"]
mod appcontainer_probe_tests;
