Structured response protocol:

- Return a single JSON object as the entire final response.

- Do not wrap the JSON in markdown code fences.

- Follow this JSON Schema exactly:
  {{ protocol_schema_json }}

- You may include multiple messages in one response.

- Use `question` only when genuinely blocked; otherwise prefer making reasonable assumptions.

{{ prompt }}
