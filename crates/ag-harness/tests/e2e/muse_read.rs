//! Live Muse read-tool check.

use std::io::{self, Write as _};

use ag_harness::{Harness, MUSE_SPARK_1_3, Muse, OutputSchema, Tool};
use serde_json::json;

use crate::DynError;

const PROMPT: &str = concat!(
    "Use the read tool to inspect Cargo.toml. ",
    "Return the exact package name from its [package] table."
);

#[tokio::test]
#[ignore = "requires live Muse credentials"]
async fn test_muse_read() -> Result<(), DynError> {
    // Arrange
    let model = Muse::from_env(MUSE_SPARK_1_3)?;

    // Act and Assert
    inspect_manifest(model).await
}

async fn inspect_manifest(model: Muse) -> Result<(), DynError> {
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
    let output = Harness::new(model)
        .repository(env!("CARGO_MANIFEST_DIR"))
        .allow(Tool::Read)
        .run_once(PROMPT, schema)
        .await?;
    let output = serde_json::to_string_pretty(output.output())?;

    writeln!(io::stdout().lock(), "{output}")?;

    Ok(())
}
