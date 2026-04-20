//! Generator for the marketing demo GIF served at
//! `docs/site/static/demo/demo.gif`.
//!
//! Marked `#[ignore]` so the regular E2E suite stays fast; run explicitly with
//! `cargo test -p agentty --test e2e demo -- --ignored` to regenerate the GIF.
//! The test is self-contained: it seeds fake projects, installs a scripted
//! `claude` stub that emits a canned reply over the stream-json protocol, and
//! drives VHS with a hand-crafted tape.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use agentty::db::{DB_DIR, DB_FILE, Database};
use assert_cmd::cargo::cargo_bin;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor};

use crate::common::BuilderEnv;

type DemoResult = Result<(), Box<dyn std::error::Error>>;

const GIF_NAME: &str = "demo";

/// Regenerates the marketing demo GIF under `docs/site/static/demo/`.
///
/// Gated behind `#[ignore]` because it requires VHS, the real `agentty`
/// binary, and ~2 minutes of wall clock time.
#[test]
#[ignore = "requires VHS and regenerates a marketing asset"]
fn generate_marketing_demo_gif() -> DemoResult {
    // Arrange
    if Command::new("vhs").arg("--version").output().is_err() {
        return Ok(());
    }

    // Use a short, clean /tmp path so project rows and the footer path read
    // nicely in the final GIF (macOS tempdirs live under /var/folders/...).
    let demo_root = make_fresh_demo_root()?;
    let env = make_demo_env(&demo_root)?;

    install_scripted_claude_stub(&env)?;
    symlink_agentty_into_stub_bin(&env)?;

    let fake_project_paths = create_fake_project_dirs(&demo_root)?;
    // Agentty reads `current_dir()` which is canonicalized; seed using the
    // same canonical form the app will upsert itself.
    let canonical_cwd = env.workdir.canonicalize()?;
    seed_database(&env, &fake_project_paths, &canonical_cwd)?;

    let output_dir = repo_demo_dir();
    std::fs::create_dir_all(&output_dir)?;
    let gif_path = output_dir.join(format!("{GIF_NAME}.gif"));

    let tape = build_demo_tape(&env, &gif_path);
    let tape_path = demo_root.join("demo.tape");
    std::fs::write(&tape_path, &tape)?;

    // Act
    let output = Command::new("vhs").arg(&tape_path).output()?;

    // Assert
    if !output.status.success() {
        return Err(format!(
            "vhs failed: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        )
        .into());
    }
    if !gif_path.exists() {
        return Err(format!("demo gif not produced at {}", gif_path.display()).into());
    }

    // Best-effort cleanup of the scratch directory.
    let _ = std::fs::remove_dir_all(&demo_root);

    Ok(())
}

/// Returns a fresh `/tmp/agentty-demo-<nanos>/` directory so paths shown in
/// the GIF are short and do not leak the operator's personal temp path.
fn make_fresh_demo_root() -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let root = PathBuf::from(format!("/tmp/agentty-demo-{nanos}"));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;

    Ok(root)
}

/// Builds a `BuilderEnv` whose workdir is named `my_project` so the project
/// row and path shown in the demo read cleanly.
fn make_demo_env(demo_root: &Path) -> std::io::Result<BuilderEnv> {
    let agentty_root = demo_root.join("agentty_root");
    let workdir = demo_root.join("my_project");
    let stub_bin = demo_root.join("stub-bin");

    std::fs::create_dir_all(&agentty_root)?;
    std::fs::create_dir_all(&workdir)?;
    std::fs::create_dir_all(&stub_bin)?;

    let env = BuilderEnv {
        agentty_root,
        stub_bin,
        workdir,
    };
    env.init_git()?;

    Ok(env)
}

/// Creates the filesystem directories that correspond to the seeded fake
/// project rows. Rows whose paths do not exist on disk are filtered out of
/// the Projects tab by `AppStartup::visible_project_rows`.
fn create_fake_project_dirs(demo_root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let fake_parent = demo_root.join("projects");
    std::fs::create_dir_all(&fake_parent)?;
    let paths = ["notes", "api-service", "playground"]
        .iter()
        .map(|name| {
            let path = fake_parent.join(name);
            std::fs::create_dir_all(&path)?;

            Ok(path)
        })
        .collect::<std::io::Result<Vec<_>>>()?;

    Ok(paths)
}

/// Overwrites the `claude` stub in `stub_bin` with a script that ignores its
/// CLI args and stdin, pauses briefly, then emits Claude's `stream-json`
/// protocol output with a canned reply.
fn install_scripted_claude_stub(env: &BuilderEnv) -> std::io::Result<()> {
    let claude_path = env.stub_bin.join("claude");
    let script = r#"#!/bin/sh
# Consume stdin so the parent's writer does not block.
cat > /dev/null 2>&1
# Simulate a short "thinking" pause so the session view shows the in-progress state.
sleep 2
printf '%s\n' '{"type":"system","subtype":"init"}'
sleep 1
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi! I am Agentty, ready to help."}]}}'
sleep 1
printf '%s\n' '{"type":"result","subtype":"success","result":"{\"answer\":\"Hi! I am Agentty, ready to help.\",\"questions\":[],\"summary\":null}","usage":{"input_tokens":5,"output_tokens":9}}'
"#;
    std::fs::write(&claude_path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude_path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

/// Symlinks the compiled `agentty` binary into `stub_bin` so that typing
/// `agentty` at the shell (inside the VHS recording) resolves to the real
/// binary through `PATH`.
fn symlink_agentty_into_stub_bin(env: &BuilderEnv) -> std::io::Result<()> {
    let real_binary = cargo_bin("agentty");
    let link_path = env.stub_bin.join("agentty");
    if link_path.exists() {
        std::fs::remove_file(&link_path)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real_binary, &link_path)?;
    }

    Ok(())
}

/// Seeds fake projects and forces a Claude-backed default model for the
/// cwd project so the on-screen session uses the scripted `claude` stub
/// instead of the Gemini app-server backend.
fn seed_database(
    env: &BuilderEnv,
    fake_project_paths: &[PathBuf],
    canonical_cwd: &Path,
) -> DemoResult {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;

        for (path, branch) in fake_project_paths.iter().zip(["main", "develop", "main"]) {
            let id = database
                .upsert_project(&path.display().to_string(), Some(branch))
                .await?;
            database.touch_project_last_opened(id).await?;
        }

        // Pre-register the cwd project so we can attach project-scoped
        // default-model settings before launch. Agentty's own startup upsert
        // matches on `path` and will reuse this row.
        let cwd_project_id = database
            .upsert_project(&canonical_cwd.display().to_string(), Some("main"))
            .await?;

        // Model defaults are stored in `project_setting`, not the global
        // `setting` table. The repo helper is crate-private, so drive a
        // second connection directly against the same SQLite file.
        drop(database);
        let mut connection = SqliteConnectOptions::new()
            .filename(&db_path)
            .connect()
            .await?;
        for name in [
            "DefaultSmartModel",
            "DefaultFastModel",
            "DefaultReviewModel",
        ] {
            let query = sqlx::query(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE SET value = excluded.value
",
            )
            .bind(cwd_project_id)
            .bind(name)
            .bind("claude-sonnet-4-6");
            connection.execute(query).await?;
        }
        connection.close().await?;

        Result::<(), Box<dyn std::error::Error>>::Ok(())
    })?;

    Ok(())
}

/// Returns the on-disk destination directory for the marketing demo GIF.
fn repo_demo_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Path::new(manifest_dir).join("../../docs/site/static/demo")
}

/// Hand-crafts a VHS tape that shows the user typing `agentty`, landing on
/// the Projects tab briefly, opening a session with "Hi Agentty!" and
/// reading the reply.
fn build_demo_tape(env: &BuilderEnv, gif_path: &Path) -> String {
    let agentty_root = env.agentty_root.display().to_string();
    let path_env = {
        let system_path = std::env::var("PATH").unwrap_or_default();
        let mut parts = vec![env.stub_bin.clone()];
        parts.extend(std::env::split_paths(&system_path));
        std::env::join_paths(parts).map_or_else(
            |_| env.stub_bin.display().to_string(),
            |value| value.to_string_lossy().into_owned(),
        )
    };
    let workdir = env.workdir.display().to_string();

    format!(
        r#"Set Shell "bash"
Set FontSize 20
Set Width 1600
Set Height 900
Set Padding 28
Set TypingSpeed 55ms
Set Framerate 30
Set Theme "Dracula"

Output "{gif}"

Hide
Type "export AGENTTY_ROOT='{root}'"
Enter
Sleep 80ms
Type "export PATH='{path}'"
Enter
Sleep 80ms
Type "export HOME='{root}'"
Enter
Sleep 80ms
Type "cd '{cwd}'"
Enter
Sleep 80ms
Type "clear"
Enter
Sleep 200ms
Show

Sleep 600ms
Type "agentty"
Sleep 400ms
Enter
Sleep 2500ms

Tab
Sleep 900ms

Type "a"
Sleep 900ms

Type "Hi Agentty!"
Sleep 1200ms

Enter
Sleep 8s

Hide
Type "q"
Sleep 400ms
"#,
        gif = gif_path.display(),
        root = agentty_root,
        path = path_env,
        cwd = workdir,
    )
}
