# Real-model compatibility benchmark

This manual benchmark exercises `ag-harness` against every supported live provider. It
is a small system benchmark, not an official leaderboard submission. Each case has a
deterministic scorer: schema validation, exact tool activity, or final filesystem state.
Expected answers are deliberately absent from tool and memory schemas so models cannot
solve a case by copying constraints instead of using tools or persisted history.

## Benchmark coverage

| Public benchmark   | Applicable case                      | Scope boundary                                                                                                                                                                                                                                                  |
| ------------------ | ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| JSONSchemaBench    | `structured`                         | Exercises a nested object, constants, and a fixed tuple. The official 10K-schema constrained-decoding benchmark is not run because `ag-harness` intentionally accepts only explicit object-root response schemas and providers impose their own schema subsets. |
| BFCL               | `parallel_read`                      | Scores exact batched function selection and arguments. Full BFCL needs arbitrary user-defined functions, parallel execution, and categories outside the closed repository-tool surface.                                                                         |
| tau-bench          | `read_recovery`, `persistent_memory` | Scores multi-step recovery after a rejected tool and cross-process-style state restored from SQLite. Full tau-bench needs domain APIs, policy documents, and a simulated user.                                                                                  |
| GAIA               | `parallel_read`                      | Covers bounded local-file retrieval only. Full GAIA needs web, Python, and shell tools plus its gated validation data.                                                                                                                                          |
| SWE-bench Verified | `write`                              | Verifies the resulting file rather than trusting the terminal answer. Full SWE-bench needs per-task repositories, dependency installation, shell execution, and test-based patch scoring.                                                                       |

EleutherAI's LM Evaluation Harness, OpenAI Evals, and Inspect AI are evaluation
frameworks rather than additional task datasets. Their model adapters and task catalogs
are useful integration targets, but an adapter alone would not expand what this crate's
fixed tool surface can validly measure.

## Run

Configure `KIMI_API_KEY`, `KIMI_BASE_URL`, `KIMI_MODEL`, `MODEL_API_KEY`,
`DASHSCOPE_API_KEY`, and `DASHSCOPE_BASE_URL`. Repository-backed cases also require
`AG_HARNESS_GIT_EXECUTABLE` to be an absolute path to a host-controlled Git executable
outside the worktree. To run the full benchmark:

```sh
AG_HARNESS_GIT_EXECUTABLE=/absolute/path/to/git \
AG_HARNESS_BENCHMARK_REPETITIONS=2 \
  cargo test --locked -p ag-harness --test benchmark
```

Set `AG_HARNESS_BENCHMARK_PROVIDER` to `kimi`, `muse`, or `qwen`, and
`AG_HARNESS_BENCHMARK_CASE` to one case name for targeted reruns.

`MODEL_API_BASE_URL` and `MODEL_API_MODEL` remain optional. Kimi cases use a full-minute
cooldown to respect its three-request-per-minute organization limit. Results stream
after every case and record pass/fail, wall time, summed provider and turn latency,
SQLite create/reopen latency for `persistent_memory`, successful model requests,
executed tool calls, and provider-reported total tokens. Provider error bodies are
redacted. Failed cases report zero activity because the public turn report is available
only after a successful turn.

The benchmark captures OpenTelemetry metrics and spans in-process. It verifies client
duration/token metrics plus lifecycle duration/call metrics, and requires one correlated
agent and model span for every persistent-session turn. It does not export telemetry,
estimate missing tokens, or calculate cost.

See `results/2026-09-03.md` for the completed live persistence and compatibility report.
