---
name: release
description: Guide for releasing a new version of the project, including version bumping, changelog updates, and tagging.
---

# Release Workflow

This skill guides you through the process of releasing a new version of the project.

## Workflow

1.  **Preparation**
    *   Ensure the git working directory is clean: `git status`.
    *   Pull the latest changes: `git pull origin main`.

2.  **Version Bump**
    *   Update the `version` field in the root `Cargo.toml`.
    *   Verify `Cargo.lock` is updated (e.g., run `cargo check` to trigger update).

3.  **Verification**
    *   Run tests: `cargo test -q`.

4.  **Changelog**
    *   Update `CHANGELOG.md`.
    *   Ensure there is an entry for the new version with the current date: `## [vX.Y.Z] - YYYY-MM-DD`.
    *   Ensure content adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

5.  **Commit**
    *   Stage changes: `git add Cargo.toml Cargo.lock CHANGELOG.md`.
    *   Commit with message: `git commit -m "Release vX.Y.Z"`.

6.  **Tagging**
    *   Create a git tag **with the 'v' prefix**: `git tag vX.Y.Z`.
    *   **Important:** The release workflow depends on the `v` prefix (e.g., `v0.1.4`).

7.  **Push**
    *   Push the commit: `git push origin main`.
    *   Push the tag: `git push origin vX.Y.Z`.

8.  **Completion**
    *   The GitHub Actions workflow will automatically create the release and publish artifacts.
