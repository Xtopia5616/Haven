//! Low-level operating system support shared by process-owning adapters.

pub mod credentials;
pub mod filesystem;
pub mod process_containment;
pub mod process_environment;
#[cfg(windows)]
pub mod sandboxed_process;
