# Demo

Demo recording assets for Agentty. The demo GIF is manually recorded using
[asciinema](https://asciinema.org/) and converted to GIF with
[agg](https://github.com/asciinema/agg).

## Recording

```bash
# Install tools (macOS)
brew install asciinema agg

# Start recording (use an isolated data directory)
export AGENTTY_ROOT="/tmp/agentty-demo"
rm -rf "$AGENTTY_ROOT"
asciinema rec docs/assets/demo/demo.cast

# In the recording session, launch agentty from a git repo and
# demonstrate tab navigation, session creation, and agent interaction.
# Press Ctrl-D or type `exit` to stop recording.

# Convert to GIF
agg --theme asciinema docs/assets/demo/demo.cast docs/assets/demo/demo.gif

# Clean up the cast file (not committed)
rm docs/assets/demo/demo.cast
```

## Directory Index

- [`demo.gif`](demo.gif) - Manually recorded demo GIF.
- [`AGENTS.md`](AGENTS.md) - Context and instructions for AI agents.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to AGENTS.md.
- [`GEMINI.md`](GEMINI.md) - Symlink to AGENTS.md.
