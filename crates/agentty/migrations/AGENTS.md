# Agentty Migrations

SQLite migrations for the `agentty` crate.

## Directory Index
- [001_create_session.sql](001_create_session.sql) - Creates the `session` table.
- [002_add_status_to_session.sql](002_add_status_to_session.sql) - Adds `status` to `session`.
- [003_create_project.sql](003_create_project.sql) - Creates the `project` table.
- [004_add_project_to_session.sql](004_add_project_to_session.sql) - Adds `project_id` relation to `session`.
- [005_add_timestamps_to_session.sql](005_add_timestamps_to_session.sql) - Adds created/updated timestamps.
- [006_add_title_to_session.sql](006_add_title_to_session.sql) - Adds `title` to `session`.
- [007_recreate_session_with_id_primary_key.sql](007_recreate_session_with_id_primary_key.sql) - Recreates `session` with text id primary key.
- [008_add_model_to_session.sql](008_add_model_to_session.sql) - Adds model metadata to `session`.
- [009_add_prompt_output_to_session.sql](009_add_prompt_output_to_session.sql) - Adds prompt/output persistence columns to `session`.
