//! Live Muse read-tool check.

use std::io::{self, Write as _};
use std::path::PathBuf;

use ag_harness::{Harness, MUSE_SPARK_1_3, Muse, OutputSchema, Repository, Tool};
use serde_json::json;

use crate::DynError;

const GIT_EXECUTABLE_ENV: &str = "AG_HARNESS_GIT_EXECUTABLE";
const PROMPT: &str = concat!(
    "Use the read tool to inspect Cargo.toml. ",
    "Return the exact package name from its [package] table."
);

#[tokio::test]
#[ignore = "requires live Muse credentials"]
async fn test_muse_read() -> Result<(), DynError> {
    // Arrange
    let model = Muse::from_env(MUSE_SPARK_1_3)?;
    let git_executable = std::env::var_os(GIT_EXECUTABLE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{GIT_EXECUTABLE_ENV} must select the host Git executable"),
            )
        })?;

    // Act and Assert
    inspect_manifest(model, git_executable).await
}

async fn inspect_manifest(model: Muse, git_executable: PathBuf) -> Result<(), DynError> {
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "package": {
                "type": "string",
                "const": "ag-harness"
            }
        },
        "required": ["package"],
        "additionalProperties": false
    }))?;
    let repository = Repository::new(env!("CARGO_MANIFEST_DIR"), git_executable)?;
    let output = Harness::new(model)
        .repository(repository)
        .allow(Tool::Read)
        .run_once(PROMPT, schema)
        .await?;
    let output = serde_json::to_string_pretty(output.output())?;

    writeln!(io::stdout().lock(), "{output}")?;

    Ok(())
}
