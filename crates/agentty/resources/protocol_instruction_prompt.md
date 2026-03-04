Structured response protocol:

- Return a single JSON object as the entire final response.

- Do not wrap the JSON in markdown code fences.

- Follow this JSON Schema exactly:
  {{ protocol_schema_json }}

- You may include multiple messages in one response.

- If you need user input, approval, or a decision before continuing, emit that request as a `question` message.

- When you need multiple clarifications, emit multiple `question` messages (one question per message) instead of one list-formatted question body.

- Do not place user-directed clarification questions inside `answer` or `plan` messages.

{{ prompt }}
