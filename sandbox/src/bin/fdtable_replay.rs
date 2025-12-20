use agentfs_sandbox::vfs::fdtable::repro::{self, ReproCase};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("proptest-artifacts")
                .join("fdtable")
                .join("last_failure.json")
        });

    let data = std::fs::read_to_string(&path)?;
    let case: ReproCase = serde_json::from_str(&data)?;

    match repro::run_case(&case) {
        Ok(()) => {
            eprintln!("OK: case passed (no invariant violations).");
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "FAILED: invariant violated.\n{}\n\nCase file: {}",
                e,
                path.display()
            );
            std::process::exit(1);
        }
    }
}
