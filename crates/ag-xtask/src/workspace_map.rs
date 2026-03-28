use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

use serde_json::{Value, json};
use tracing::info;

const OUTPUT_PATH: &str = "target/agentty/workspace-map.json";
const ARCHITECTURE_DOCS: [&str; 4] = [
    "docs/site/content/docs/architecture/module-map.md",
    "docs/site/content/docs/architecture/runtime-flow.md",
    "docs/site/content/docs/architecture/change-recipes.md",
    "docs/site/content/docs/architecture/testability-boundaries.md",
];
const AGENT_GUIDES: [&str; 14] = [
    "AGENTS.md",
    "skills/AGENTS.md",
    "docs/plan/AGENTS.md",
    "crates/AGENTS.md",
    "crates/ag-forge/AGENTS.md",
    "crates/ag-xtask/AGENTS.md",
    "crates/testty/AGENTS.md",
    "crates/agentty/AGENTS.md",
    "crates/agentty/src/AGENTS.md",
    "crates/agentty/src/app/AGENTS.md",
    "crates/agentty/src/domain/AGENTS.md",
    "crates/agentty/src/infra/AGENTS.md",
    "crates/agentty/src/runtime/AGENTS.md",
    "crates/agentty/src/ui/AGENTS.md",
];

/// Runs external commands used to assemble the generated workspace map.
#[cfg_attr(test, mockall::automock)]
trait CommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output>;
}

/// Production command runner used by the `workspace-map` command.
struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, command: &mut Command) -> std::io::Result<Output> {
        command.output()
    }
}

/// Writes one machine-readable workspace map for agent tooling and local
/// exploration.
///
/// # Errors
/// Returns an error when cargo metadata cannot be loaded or the output file
/// cannot be written.
pub(crate) fn run() -> Result<(), String> {
    let root = env::current_dir().map_err(|error| error.to_string())?;
    let output_path = run_with_runner(root.as_path(), &RealCommandRunner)?;

    info!(
        "Wrote {}",
        relative_path(root.as_path(), output_path.as_path())
    );

    Ok(())
}

/// Generates the workspace map payload and writes it to the configured output
/// path.
fn run_with_runner(root: &Path, runner: &dyn CommandRunner) -> Result<PathBuf, String> {
    let metadata = load_workspace_metadata(runner)?;
    let workspace_map = build_workspace_map(root, &metadata)?;
    let output_path = root.join(OUTPUT_PATH);

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let rendered_map =
        serde_json::to_string_pretty(&workspace_map).map_err(|error| error.to_string())?;
    fs::write(&output_path, rendered_map).map_err(|error| error.to_string())?;

    Ok(output_path)
}

/// Loads `cargo metadata` for the current workspace.
fn load_workspace_metadata(runner: &dyn CommandRunner) -> Result<Value, String> {
    let output = runner
        .run(Command::new("cargo").args(["metadata", "--format-version", "1", "--no-deps"]))
        .map_err(|error| error.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

/// Builds the final machine-readable workspace map document.
fn build_workspace_map(root: &Path, metadata: &Value) -> Result<Value, String> {
    let workspace_members = collect_workspace_members(root, metadata)?;
    let architecture_docs = collect_existing_paths(root, &ARCHITECTURE_DOCS);
    let agent_guides = collect_existing_paths(root, &AGENT_GUIDES);
    let major_module_routers = collect_router_modules(root, &root.join("crates/agentty/src"))?;

    Ok(json!({
        "workspace_root": ".",
        "workspace_members": workspace_members,
        "architecture_docs": architecture_docs,
        "agent_guides": agent_guides,
        "major_module_routers": major_module_routers,
    }))
}

/// Collects one summary object for every workspace crate.
fn collect_workspace_members(root: &Path, metadata: &Value) -> Result<Vec<Value>, String> {
    let workspace_member_ids = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| "Cargo metadata does not include `workspace_members`".to_string())?
        .iter()
        .filter_map(Value::as_str)
        .map(std::string::ToString::to_string)
        .collect::<BTreeSet<_>>();
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Cargo metadata does not include `packages`".to_string())?;

    let mut workspace_members = Vec::new();
    for package in packages {
        let Some(package_id) = package.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !workspace_member_ids.contains(package_id) {
            continue;
        }

        let Some(manifest_path) = package.get("manifest_path").and_then(Value::as_str) else {
            continue;
        };
        let manifest_path = PathBuf::from(manifest_path);
        let Some(package_root) = manifest_path.parent() else {
            continue;
        };
        let package_root = package_root.to_path_buf();
        let source_root = package_root.join("src");
        let package_targets = collect_target_kinds(package);
        let top_level_entries = list_direct_children(&package_root)?;
        let router_modules = collect_router_modules(root, &source_root)?;

        workspace_members.push(json!({
            "name": package.get("name").and_then(Value::as_str).unwrap_or("unknown"),
            "path": relative_path(root, &package_root),
            "manifest_path": relative_path(root, &manifest_path),
            "source_root": source_root.is_dir().then(|| relative_path(root, &source_root)),
            "targets": package_targets,
            "top_level_entries": top_level_entries,
            "router_modules": router_modules,
        }));
    }

    workspace_members.sort_by(|first, second| first["path"].as_str().cmp(&second["path"].as_str()));

    Ok(workspace_members)
}

/// Collects the distinct target kinds declared by one cargo package.
fn collect_target_kinds(package: &Value) -> Vec<String> {
    let mut target_kinds = package
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|target| target.get("kind").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(std::string::ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    target_kinds.sort();

    target_kinds
}

/// Returns only the configured guide or doc paths that currently exist.
fn collect_existing_paths(root: &Path, entries: &[&str]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| {
            let path = root.join(entry);
            path.exists().then(|| normalize_path(Path::new(entry)))
        })
        .collect()
}

/// Finds router-style module pairs such as `app.rs` with `app/`.
fn collect_router_modules(root: &Path, source_root: &Path) -> Result<Vec<Value>, String> {
    if !source_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut router_modules = Vec::new();
    for entry in fs::read_dir(source_root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let module_path = entry.path();
        if !module_path.is_dir() {
            continue;
        }

        let Some(module_name) = module_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let router_path = source_root.join(format!("{module_name}.rs"));
        if !router_path.is_file() {
            continue;
        }

        router_modules.push(json!({
            "name": module_name,
            "router_path": relative_path(root, &router_path),
            "module_dir": relative_path(root, &module_path),
        }));
    }

    router_modules.sort_by(|first, second| {
        first["router_path"]
            .as_str()
            .cmp(&second["router_path"].as_str())
    });

    Ok(router_modules)
}

/// Lists direct child files and directories for one path.
fn list_direct_children(path: &Path) -> Result<Vec<String>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| error.to_string())?
        .flatten()
        .filter_map(|entry| {
            let entry_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if entry_path.is_dir() {
                Some(format!("{name}/"))
            } else if entry_path.is_file() {
                Some(name)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    entries.sort();

    Ok(entries)
}

/// Converts one path to a normalized workspace-relative path when possible.
fn relative_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative_path) => normalize_path(relative_path),
        Err(_) => normalize_path(path),
    }
}

/// Normalizes path separators to POSIX style.
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use tempfile::tempdir;

    use super::*;

    fn mock_output(status: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn test_collect_router_modules_returns_router_pairs() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let source_root = dir.path().join("src");
        fs::create_dir_all(source_root.join("app")).expect("Failed to create app module dir");
        fs::create_dir_all(source_root.join("infra")).expect("Failed to create infra module dir");
        fs::write(source_root.join("app.rs"), "").expect("Failed to write app router");
        fs::write(source_root.join("infra.rs"), "").expect("Failed to write infra router");
        fs::create_dir_all(source_root.join("ignored")).expect("Failed to create ignored dir");

        // Act
        let router_modules = collect_router_modules(dir.path(), &source_root)
            .expect("router collection should work");

        // Assert
        assert_eq!(
            router_modules,
            vec![
                json!({
                    "name": "app",
                    "router_path": "src/app.rs",
                    "module_dir": "src/app",
                }),
                json!({
                    "name": "infra",
                    "router_path": "src/infra.rs",
                    "module_dir": "src/infra",
                }),
            ]
        );
    }

    #[test]
    fn test_collect_workspace_members_uses_workspace_metadata() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let crate_root = dir.path().join("crates/example");
        let source_root = crate_root.join("src");
        fs::create_dir_all(source_root.join("app")).expect("Failed to create source tree");
        fs::write(crate_root.join("Cargo.toml"), "").expect("Failed to write Cargo.toml");
        fs::write(source_root.join("lib.rs"), "").expect("Failed to write lib.rs");
        fs::write(source_root.join("app.rs"), "").expect("Failed to write app router");
        let metadata = json!({
            "workspace_members": ["example 0.1.0 (path+file:///tmp/example)"],
            "packages": [{
                "id": "example 0.1.0 (path+file:///tmp/example)",
                "name": "example",
                "manifest_path": normalize_path(&crate_root.join("Cargo.toml")),
                "targets": [{
                    "kind": ["lib"]
                }]
            }]
        });

        // Act
        let workspace_members = collect_workspace_members(dir.path(), &metadata)
            .expect("workspace members should collect");

        // Assert
        assert_eq!(workspace_members.len(), 1);
        assert_eq!(workspace_members[0]["name"], "example");
        assert_eq!(workspace_members[0]["path"], "crates/example");
        assert_eq!(
            workspace_members[0]["manifest_path"],
            "crates/example/Cargo.toml"
        );
        assert_eq!(workspace_members[0]["source_root"], "crates/example/src");
        assert_eq!(workspace_members[0]["targets"], json!(["lib"]));
        assert_eq!(
            workspace_members[0]["router_modules"],
            json!([{
                "name": "app",
                "router_path": "crates/example/src/app.rs",
                "module_dir": "crates/example/src/app",
            }])
        );
    }

    #[test]
    fn test_run_with_runner_writes_workspace_map() {
        // Arrange
        let dir = tempdir().expect("Failed to create temp dir");
        let crate_root = dir.path().join("crates/example");
        let source_root = crate_root.join("src");
        fs::create_dir_all(&source_root).expect("Failed to create source root");
        fs::create_dir_all(dir.path().join("docs/site/content/docs/architecture"))
            .expect("Failed to create architecture docs");
        fs::write(crate_root.join("Cargo.toml"), "").expect("Failed to write Cargo.toml");
        fs::write(source_root.join("lib.rs"), "").expect("Failed to write lib.rs");
        fs::write(
            dir.path()
                .join("docs/site/content/docs/architecture/module-map.md"),
            "",
        )
        .expect("Failed to write module map");
        fs::write(dir.path().join("AGENTS.md"), "").expect("Failed to write root AGENTS");

        let metadata = json!({
            "workspace_members": ["example 0.1.0 (path+file:///tmp/example)"],
            "packages": [{
                "id": "example 0.1.0 (path+file:///tmp/example)",
                "name": "example",
                "manifest_path": normalize_path(&crate_root.join("Cargo.toml")),
                "targets": [{
                    "kind": ["lib"]
                }]
            }]
        });
        let mut runner = MockCommandRunner::new();
        runner.expect_run().returning(move |_| {
            Ok(mock_output(
                0,
                &serde_json::to_string(&metadata).expect("metadata should serialize"),
                "",
            ))
        });

        // Act
        let output_path =
            run_with_runner(dir.path(), &runner).expect("workspace map generation should succeed");
        let rendered_map =
            fs::read_to_string(&output_path).expect("workspace map should be written to disk");

        // Assert
        assert_eq!(relative_path(dir.path(), &output_path), OUTPUT_PATH);
        assert!(
            rendered_map.contains("\"workspace_members\""),
            "{rendered_map}"
        );
        assert!(rendered_map.contains("\"agent_guides\""), "{rendered_map}");
    }
}
