# GitHub Workflows

Automation workflows executed by GitHub Actions.

## Directory Index

- [`pages.yml`](pages.yml) - GitHub Pages deployment for `agentty.xyz` using Zola.
- [`postsubmit.yml`](postsubmit.yml) - Postsubmit workflow that generates LCOV coverage and uploads it to Codecov.
- [`publish-crates-io.yml`](publish-crates-io.yml) - Publishes the `ag-forge` and `agentty` crates to crates.io on release-tag pushes.
- [`release.yml`](release.yml) - Release pipeline for tagging and publishing.
