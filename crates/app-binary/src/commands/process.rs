//! Shared process-start plumbing for App commands.

use std::process::Command;

/// Start a detached host process while serializing it with process containment
/// setup. Callers retain ownership of authorization, executable choice, and
/// argument construction.
pub(crate) fn spawn_detached(command: &mut Command) -> std::io::Result<()> {
    let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
    command.spawn().map(|_| ())
}
