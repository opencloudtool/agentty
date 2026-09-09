---
name: feature-test
description: Guide for creating E2E feature tests with VHS GIF generation and Zola feature page auto-discovery for new visible UI features.
---

# Feature Test Skill

Use this skill when adding a new visible UI feature that is user-facing and demonstrable
in a PTY scenario. The workflow produces four artifacts from one test:

1. An E2E test in `crates/agentty/tests/e2e/`.
1. A feature GIF in `docs/site/static/features/`.
1. A static PNG poster in `docs/site/static/features/`.
1. A Zola content page in `docs/site/content/features/`.

## When to Use

A feature test is warranted when **all three** criteria are met:

- **Visible UI behavior change** — the feature renders something new or different on
  screen.
- **User-facing** — an end user can trigger or observe the behavior through normal
  interaction.
- **Demonstrable in a scenario** — the behavior can be captured in a short PTY recording
  without live agent backends.

Skip this skill for internal refactors, backend-only changes, or features that require a
running agent to demonstrate.

## Naming Convention

A single name flows through the entire pipeline:

| Artifact      | Path                                                    |
| ------------- | ------------------------------------------------------- |
| Test function | `test_{name}` in `crates/agentty/tests/e2e/{module}.rs` |
| GIF file      | `docs/site/static/features/{name}.gif`                  |
| PNG poster    | `docs/site/static/features/{name}.png`                  |
| Zola page     | `docs/site/content/features/{name}.md`                  |

Choose a short, descriptive `snake_case` name that describes the feature (e.g.,
`session_creation`, `help_overlay`, `tab_switch`).

## Workflow

### 1. Choose the test module

Place the test in the E2E module that best matches the feature area:

- `session/` — topic modules for session lifecycle, prompts, diffs, reviews, and related
  interactions; `session.rs` registers these modules.
- `navigation.rs` — tab cycling, help overlay, quit dialog.
- `confirmation.rs` — confirmation dialogs.
- `project.rs` — project page and project-related flows.

If no existing module fits, create a new one and register it in
`crates/agentty/tests/e2e/main.rs`, or in `crates/agentty/tests/e2e/session.rs` for a
new session topic.

### 2. Write the test using `FeatureTest`

Use the `FeatureTest` builder from `crates/agentty/tests/e2e/common.rs`. This is the
preferred pattern — it handles `TempDir` and `BuilderEnv` creation, scenario execution,
GIF generation with content-hash caching, and optional Zola page creation in a single
declarative chain.

```rust
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::FeatureTest;

#[test]
fn test_{name}() {
    // Arrange, Act, Assert
    FeatureTest::new("{name}")
        .with_git()   // Required for features that create sessions/worktrees.
        .zola(
            "Human-readable title",
            "One-line description for the feature card.",
            50,  // Weight for ordering on the features page.
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    // Navigate to the relevant tab/state.
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    // Perform the feature interaction.
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("label", "Description of captured state")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "expected text", &full);
            },
        );
}
```

#### `FeatureTest` builder methods

- **`new(name)`** — set the feature name (used for GIF filename and Zola page).
- **`.with_git()`** — initialize a git repo in the workdir (required for
  session/worktree features).
- **`.zola(title, description, weight)`** — enable Zola page auto-generation with the
  given frontmatter fields. The page is written only if it does not already exist.
- **`.run(build_scenario, assert)`** — execute the scenario, run assertions, and
  generate the GIF.

#### Common `Journey` helpers

Reuse the shared journey builders from `common.rs` instead of repeating step sequences:

- `wait_for_agentty_startup()` — wait for the initial TUI frame.
- `switch_to_tab(name)` — press `Tab` and wait for stability.
- `switch_to_tab_reverse(name)` — press `BackTab` and wait.
- `open_quit_dialog()` — press `q` and wait.
- `open_help_overlay()` — press `?` and wait.
- `create_session_and_return_to_list()` — full session creation flow.
- `create_session_with_prompt_and_return_to_list(prompt)` — session creation with a
  custom prompt.

### 3. Verify the Zola page

If you used `.zola(...)`, `FeatureTest` auto-generates the content page at
`docs/site/content/features/{name}.md` on first run. The generated page uses this
frontmatter:

```toml
+++
title = "Feature title"
description = "One-line description shown on the card."
weight = 50

[extra]
gif = "{name}.gif"
+++
```

The `features.html` template auto-discovers all pages in `content/features/` sorted by
`weight`. No manual template edits are needed.

### 4. Create or refresh the PNG poster

Every feature GIF needs a same-named PNG poster for the site's `prefers-reduced-motion`
and `noscript` fallbacks. Create the poster when adding a GIF, and regenerate it
whenever the GIF changes. `TESTTY_GIF_MODE=check` verifies GIF freshness but does not
verify or update the poster.

First inspect the finished GIF and choose a timestamp that clearly communicates the
feature's result. Prefer a stable end state with the important UI visible. Do not
automatically use the first or midpoint frame: it may show an empty, loading, or
transitional state.

Check the GIF duration before choosing a timestamp:

```sh
ffprobe -v error \
  -show_entries format=duration \
  -of default=noprint_wrappers=1 \
  docs/site/static/features/<name>.gif
```

Extract exactly one frame with `ffmpeg`, preserving the aspect ratio and capping the
width at 1600 pixels without upscaling:

```sh
ffmpeg -y \
  -i docs/site/static/features/<name>.gif \
  -ss <timestamp> \
  -vf "scale=w='min(1600\,iw)':h=-1" \
  -frames:v 1 \
  -compression_level 9 \
  docs/site/static/features/<name>.png
```

Use a timestamp accepted by `ffmpeg`, such as `00:00:01.500`, that falls within the
reported duration. Open the resulting PNG and verify that it is sharp, legible, free of
transitional artifacts, and still represents the current GIF. If it does not, choose a
better timestamp and rerun the command.

Check that every GIF declared by a feature page has a nonempty poster before finalizing:

```sh
for feature_page in docs/site/content/features/*.md; do
  gif_name=$(sed -n 's/^gif = "\(.*\)"/\1/p' "${feature_page}")
  test -z "${gif_name}" && continue
  poster_path="docs/site/static/features/${gif_name%.gif}.png"
  test -s "${poster_path}" || {
    echo "missing PNG poster: ${poster_path}"
    exit 1
  }
done
```

### 5. Run and verify

```sh
# Run the focused E2E test for the feature.
TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}

# Validate the Zola site builds with the new feature page.
prek run zola-check --all-files --hook-stage manual
```

Routine agent validation must use `TESTTY_GIF_MODE=check` so the semantic PTY assertions
still run while GIF freshness is checked without invoking VHS or launching Chrome. Do
not run `vhs` or a generation mode directly in the developer host's operating-system
environment. Launching the pinned Podman container on that host is supported and is the
required path for intentional regeneration; the container uses
`TESTTY_GIF_MODE=generate` below.

### Record in the canonical container

Committed hash sidecars must be produced by the same pinned container definition that CI
uses. `container/e2e.Containerfile` supports both `linux/amd64` and `linux/arm64`, with
architecture-specific checksums for the Rust installer, `prek`, `cargo-nextest`, `vhs`,
and `ttyd`, plus the full recording stack (Chromium, `ffmpeg`, JetBrains Mono).
Presubmit, postsubmit, and release checks call `.github/workflows/e2e.yml`, whose job
uses the published image index from GHCR as its digest-pinned container runtime, selects
the `linux/amd64` variant explicitly, overrides the image user with the root user GitHub
requires for workspace access, and runs the `test-agentty-e2e` hook directly. The same
index contains a native `linux/arm64` variant for recording on ARM64 hosts. The host
needs a running Podman environment only — no local Chrome or VHS — and the
localhost-socket sandbox restriction below does not apply inside the container.

The canonical feature preset records at 1600×800 with an 18-point font, matching the
site poster dimensions and avoiding the excessive VHS and FFmpeg memory use caused by
larger canvases during long scenarios. Rendering settings participate in the freshness
hash, so changing the canvas, font, theme, framerate, or padding makes existing
recordings stale even when their captured terminal frames are unchanged.

A published digest is multi-architecture only when it resolves to an image index or
manifest list containing both `linux/amd64` and `linux/arm64`; the Containerfile's
support for both architectures does not prove that the registry reference contains both.
Pull the immutable digest with an explicit platform so a missing variant fails instead
of silently running the wrong architecture through emulation.

On macOS or Windows, initialize the Podman machine once with `podman machine init`, then
start it with `podman machine start` before pulling or running the image. Linux hosts
run Podman directly without a machine.

Record or refresh feature artifacts with a writable workspace mount and `generate` mode,
which re-records only missing or stale GIFs. Run the local container as the host user so
Linux bind mounts remain writable. A host-owned cache directory provides writable home,
Cargo, `prek`, and build locations while preserving them between runs:

```sh
published_e2e_image=ghcr.io/agentty-xyz/agentty-e2e@sha256:d8bcf1bcc38f051c583ed75b614bde552df28646dc74772e31e61453b1b00079
e2e_cache_root="${XDG_CACHE_HOME:-${HOME}/.cache}/agentty-e2e"
mkdir -p \
  "${e2e_cache_root}/home" \
  "${e2e_cache_root}/cargo" \
  "${e2e_cache_root}/prek" \
  "${e2e_cache_root}/target"

case "$(uname -m)" in
  x86_64 | amd64)
    e2e_platform=linux/amd64
    ;;
  arm64 | aarch64)
    e2e_platform=linux/arm64
    ;;
  *)
    echo "unsupported recording architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

e2e_image="${published_e2e_image}"
podman pull --platform "${e2e_platform}" "${e2e_image}"

podman run --rm \
  --platform "${e2e_platform}" \
  --user "$(id -u):$(id -g)" \
  --mount type=bind,source="$PWD",target=/workspace \
  --mount type=bind,source="${e2e_cache_root}",target=/cache \
  --env HOME=/cache/home \
  --env CARGO_HOME=/cache/cargo \
  --env PREK_HOME=/cache/prek \
  --env CARGO_TARGET_DIR=/cache/target \
  --env TESTTY_GIF_MODE=generate \
  "${e2e_image}" \
  cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}

test -s docs/site/static/features/{name}.gif || {
  echo "generated feature GIF is missing or empty: {name}.gif" >&2
  exit 1
}
```

Both host branches pull their native variant from the same published image index; do not
run the other architecture through emulation because Rust compiler probes can crash
before the test starts. Local recording uses the host user's UID so the bind-mounted
workspace remains writable. The reusable CI workflow instead overrides the image's
unprivileged default user with root so `actions/checkout` can populate the job's
writable workspace; `check` mode verifies recording freshness without rewriting the
feature artifacts. Because GitHub's mounted checkout retains a different owner, the
workflow also registers the exact `GITHUB_WORKSPACE` path as a Git `safe.directory`
before invoking `prek`. Always perform the nonempty-file check after recording: VHS can
exit successfully after creating its screenshots even when GIF finalization has not
produced a usable artifact.

Review the changed GIF and `.{name}.hash` sidecar, then refresh the PNG poster for every
regenerated GIF (section 4) before committing all three together. Testty records to a
hidden staging file and replaces the committed GIF only after the recording is nonempty.
Successful generation removes the previous same-named PNG intentionally so a stale
poster cannot pass the nonempty-poster integrity check; a failed recording preserves the
last valid GIF, hash sidecar, and poster.

#### Refresh the canonical image

Only maintainers changing `container/e2e.Containerfile` should build and publish a
replacement image. Build both supported platforms into one manifest, run affected
focused feature tests against the native candidate for each available architecture, then
push `latest`. The preferred path is the manual **Publish E2E Image** workflow in
`.github/workflows/publish-e2e-image.yml`, dispatched from the repository's default
branch. It builds and runs the E2E suite on native `ubuntu-24.04` AMD64 and
`ubuntu-24.04-arm` ARM64 runners, publishes architecture-specific candidates, assembles
the manifest, logs out of GHCR, verifies anonymous access to both variants, and reports
the digest in the workflow summary.

Before its first run, a package administrator must connect the existing `agentty-e2e`
package to this repository or grant this repository write access under the package's
**Manage Actions access** settings. The package predates this workflow, so the
workflow's `packages: write` permission cannot authorize an unconnected package by
itself. `container/e2e.Containerfile` carries the `org.opencontainers.image.source`
label to preserve the repository association on subsequent publications.

Copy the reported digest into the `container.image` value in `.github/workflows/e2e.yml`
and the `published_e2e_image` assignment above. The pinned digest must remain an image
index with both platforms; do not update the repository when either native test or
either platform pull fails. Re-record every feature affected by a tool, browser, font,
or rendering change and refresh its PNG poster before updating the digest and artifacts
together.

The following local flow remains available to maintainers with GHCR package-write
permission. Use an explicit registry destination so a later single-image push cannot be
confused with the manifest-list publication. Because the Containerfile contains `RUN`
instructions, the combined build requires binfmt/QEMU emulation for the non-native
platform; without it, use the manual workflow instead.

The publication verification command also requires `jq` on the maintainer host.

```sh
e2e_repository=ghcr.io/agentty-xyz/agentty-e2e
e2e_digest_file=$(mktemp)
trap 'rm -f "${e2e_digest_file}"' EXIT

podman build --jobs 2 \
  --platform linux/amd64,linux/arm64 \
  --manifest "${e2e_repository}:latest" \
  --file container/e2e.Containerfile \
  container

podman manifest push --all \
  --digestfile "${e2e_digest_file}" \
  "${e2e_repository}:latest" \
  "docker://${e2e_repository}:latest"

e2e_digest=$(sed -n '1p' "${e2e_digest_file}")
test -n "${e2e_digest}"
rm "${e2e_digest_file}"
trap - EXIT
e2e_published_image="${e2e_repository}@${e2e_digest}"
```

Before copying the digest reported by `podman manifest push`, inspect its
digest-qualified remote reference and require both Linux platforms. Using the digest
instead of `latest` prevents Podman from satisfying the inspection with the local
pre-push manifest. This check rejects a single-image manifest even if that image itself
is `linux/amd64` or `linux/arm64`:

```sh
podman logout ghcr.io

podman manifest inspect "${e2e_published_image}" | jq -e '
  (.mediaType == "application/vnd.oci.image.index.v1+json"
    or .mediaType == "application/vnd.docker.distribution.manifest.list.v2+json")
  and ([
    .manifests[].platform
    | select(.os == "linux")
    | .architecture
  ] | unique | sort == ["amd64", "arm64"])
'

podman pull --platform linux/amd64 "${e2e_published_image}"
podman pull --platform linux/arm64 "${e2e_published_image}"
```

Do not update the repository when logout, inspection, or either platform pull fails. The
logout makes the inspection and pulls exercise the same anonymous access that forked
pull-request CI requires.

### Bare-host recording prohibition

Do not run VHS recording directly in the developer host's operating-system environment.
This restriction does not prohibit running the pinned recording container on that host
with Podman; that is the supported workflow described above. Bare-host VHS records
through localhost sockets (`ttyd` plus the Chrome DevTools protocol), and sandboxed
agent shells can deny that network access and crash `vhs` before Chrome launches. On
macOS, even an unsandboxed bare-host run launches a local Chromium process that can
abort during AppKit registration and show a **Chromium quit unexpectedly** dialog. The
bare-host prohibition also covers direct `vhs` commands and ignored tests that
regenerate demo assets. Use the platform-explicit Podman workflow above for every
intentional recording; do not suppress macOS crash reporting to hide a bare-host browser
failure.

In default `generate-if-stale` mode, machines without VHS still run and assert the test
correctly, then gracefully skip GIF generation. In `force` mode, VHS must be installed
because regeneration was explicitly requested.

The `TESTTY_GIF_MODE` env var selects the freshness mode used by `FeatureTest`:

- unset — leave GIF work off while still running the PTY scenario and assertions.
- `generate` / `generate-if-stale` — regenerate when the on-disk hash sidecar is missing
  or stale, otherwise reuse the committed GIF. Use only inside the canonical container,
  which may be launched with Podman on a developer host.
- `check` / `check-only` — compute the would-be hash and compare it to the on-disk
  sidecar without invoking VHS or touching the GIF output directory. The harness fails
  the test when a committed sidecar has drifted, an existing sidecar is invalid, or the
  GIF itself is missing or empty, and surfaces the current/committed hashes plus sidecar
  errors so CI catches drift. Existing GIFs that predate sidecars are tolerated until a
  recording run creates their baseline. `.zola(...)` tests without any committed docs
  page, GIF, or sidecar are treated as unpublished and skipped by check mode until a
  recording run publishes their artifacts.
- `force` / `always` / `always-generate` — bypass the hash cache and re-run VHS
  unconditionally. VHS must be installed: a missing VHS binary fails the test instead of
  being silently skipped, because regeneration was explicitly requested. Use this mode
  only inside the canonical container, never directly in the bare-host environment; the
  routine recording workflow above uses `generate` instead.

Run `prek run zola-check --all-files --hook-stage manual` after the test to catch broken
frontmatter or template integration before the page reaches CI. This requires Zola to be
installed locally — if unavailable, CI catches it via the `pages.yml` workflow.

### Freshness hash determinism

The hash only means "the UI moved" if the same UI hashes the same way every run — and on
every machine, because sidecars are committed locally and checked on Linux CI. The
harness already neutralizes the known variance:

- Temp paths are normalized by testty, and `BuilderEnv` keeps every painted directory
  under the test `HOME` so paths render home-collapsed (`~/test-project`,
  `~/.agentty/wt/<hash>`) with a platform-independent length.
- `FeatureTest` pins the wall clock, UTC offset, and rendered version label before
  capture; it also redacts the `wt/<hash>` worktree name a session derives from its
  generated UUID (see `common::session_worktree_redaction`) and the pinned version
  label. Pinning before rendering prevents a wider version from moving styled terminal
  cells and staling every GIF.
- `BuilderEnv` stubs every supported agent CLI, so agent availability — and the default
  agent a new session resolves — does not depend on which real CLIs a machine has.

Anything else volatile a scenario puts on screen — another generated id, a live
duration, a random port — makes every run look stale and re-records the GIF for nothing.
Freeze it in the app under test, or declare it with `FeatureDemo::redact`.

## Legacy Pattern

Older tests manage `TempDir`, `BuilderEnv`, `Scenario`, and `save_feature_gif` manually
instead of using `FeatureTest`. This pattern still works but is not recommended for new
tests. Prefer `FeatureTest` for all new feature tests.

```rust
#[test]
fn legacy_example() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder env");

    let scenario = Scenario::new("{name}")
        .compose(&common::wait_for_agentty_startup())
        // ... build the scenario ...
        .capture_labeled("label", "description");

    // Act
    let (frame, report) = scenario
        .run_with_proof(env.builder())
        .expect("scenario execution failed");

    // Assert
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "expected", &full);

    // GIF generation (manual).
    common::save_feature_gif(&scenario, &report, &env, "{name}");
}
```

When using the legacy pattern, create the Zola page manually at
`docs/site/content/features/{name}.md`.

## Checklist

- [ ] Feature name follows `snake_case` naming convention.
- [ ] Test uses `FeatureTest` builder (preferred) or the legacy `Scenario` +
  `save_feature_gif` pattern.
- [ ] `.with_git()` is set if the feature requires session creation or worktrees.
- [ ] `.zola(...)` is set with a clear title, description, and appropriate weight.
- [ ] Test includes `// Arrange`, `// Act`, and `// Assert` comments (or combined
  `// Arrange, Act, Assert` for declarative builders).
- [ ] Assertions verify visible UI text or state, not internal implementation details.
- [ ] A same-named PNG poster exists, shows a meaningful stable frame, and was refreshed
  after the latest GIF change.
- [ ] The generated GIF exists and is nonempty before its hash sidecar is accepted; a
  failed recording preserves the previous GIF, hash sidecar, and poster.
- [ ] Container recording selects `linux/amd64` or `linux/arm64` explicitly, and every
  pulled digest contains the selected platform.
- [ ] Focused E2E workflow passes with
  `TESTTY_GIF_MODE=check cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}`.
- [ ] Zola site validates with `prek run zola-check --all-files --hook-stage manual`
  (when `.zola(...)` is used).
