# Proof Pipeline

Proof report collector, backend trait, and output format implementations.

## Directory Index

- [`backend.rs`](backend.rs) - `ProofBackend` trait and `ProofFormat` enum for swappable proof output.
- [`report.rs`](report.rs) - `ProofReport` collector, `ProofCapture`, `AssertionResult`, and annotated text output.
- [`frame_text.rs`](frame_text.rs) - Annotated plain-text proof backend.
- [`strip.rs`](strip.rs) - Vertical PNG screenshot strip proof backend.
- [`gif.rs`](gif.rs) - Animated GIF proof backend with configurable frame delays.
- [`html.rs`](html.rs) - Self-contained HTML report backend with embedded images and diff summaries.
