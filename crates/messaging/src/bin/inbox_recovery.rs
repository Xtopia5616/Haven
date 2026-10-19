use std::process::ExitCode;

use haven_messaging::InboxRecovery;

const USAGE: &str = "Usage:
  haven-inbox-recovery inspect <session_id>
  haven-inbox-recovery quarantine <session_id> <offset_bytes> <sha256>

`inspect` reports the next oversized row at the durable inbox cursor.
Review its offset and digest, then pass those exact values to `quarantine`.
Quarantined bytes remain under the configured Haven inbox directory.";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> anyhow::Result<()> {
    match args.as_slice() {
        [command] if command == "--help" || command == "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        [command, session_id] if command == "inspect" => {
            let recovery = InboxRecovery::default_root();
            match recovery.inspect_next_oversized_row(session_id)? {
                Some(row) => {
                    println!("Oversized row at the current inbox cursor:");
                    println!("  session_id: {session_id}");
                    println!("  offset_bytes: {}", row.offset_bytes);
                    println!("  row_bytes: {}", row.row_bytes);
                    println!("  sha256: {}", row.sha256);
                    println!("To isolate exactly this row, run:");
                    println!(
                        "  haven-inbox-recovery quarantine {session_id} {} {}",
                        row.offset_bytes, row.sha256
                    );
                    Ok(())
                }
                None => {
                    println!("No oversized row blocks the current inbox cursor for {session_id}.");
                    Ok(())
                }
            }
        }
        [command, session_id, offset_bytes, sha256] if command == "quarantine" => {
            let offset_bytes = offset_bytes.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("offset_bytes must be an unsigned integer\n\n{USAGE}")
            })?;
            let recovery = InboxRecovery::default_root();
            let result =
                recovery.quarantine_next_oversized_row(session_id, offset_bytes, sha256)?;
            println!("Quarantined the reviewed oversized row:");
            println!("  session_id: {session_id}");
            println!("  offset_bytes: {}", result.row.offset_bytes);
            println!("  row_bytes: {}", result.row.row_bytes);
            println!("  sha256: {}", result.row.sha256);
            println!("  evidence_file: {}", result.quarantine_file.display());
            println!("The durable inbox cursor now continues after this row.");
            Ok(())
        }
        _ => anyhow::bail!("{USAGE}"),
    }
}
