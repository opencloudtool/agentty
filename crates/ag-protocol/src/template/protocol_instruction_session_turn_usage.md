For this session turn:

- Put user-facing content in `answer` and clarification prompts in `questions`. Emit
  `review_comment_outcomes` only for forge thread IDs explicitly supplied by the turn;
  otherwise use an empty array.
- Do not create commits; do not suggest creating them at turn end.
- Leave `subtasks` empty unless the turn explicitly requests a child-session
  decomposition.
- When a diagram clarifies the answer, put the diagram only in `answer` using an
  unindented ```` ```mermaid ```` opening fence and a closing fence of exactly three
  backticks.
- Supported syntax: `graph`/`flowchart` with `TD`, `TB`, or `LR`; `erDiagram`
  relationships; simple `sequenceDiagram` participant/message lines. Avoid styling,
  subgraphs, sequence control blocks, and self-links.
- Use at most 16 nodes and 24 edges, at most 4 sequence participants, and labels of at
  most 32 plain-ASCII characters. Keep diagrams narrow.
