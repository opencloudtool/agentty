# SQLx Metadata

Checked-in SQLx offline query metadata used for compile-time macro validation in the `ag-agentty` crate.

## Directory Index

- [`AGENTS.md`](AGENTS.md) - Directory-specific documentation and index.
- [`CLAUDE.md`](CLAUDE.md) - Symlink to `AGENTS.md`.
- [`GEMINI.md`](GEMINI.md) - Symlink to `AGENTS.md`.
- [`query-0f580d241f3c3f8e3f8cd3e5c324709f6ec5c6d67da1f9ae51e3208002d9df5c.json`](query-0f580d241f3c3f8e3f8cd3e5c324709f6ec5c6d67da1f9ae51e3208002d9df5c.json) - Metadata for loading immutable `session_activity.created_at` timestamps in insertion order.
- [`query-1fe961aa59e6ec14e0ceda6ed64afceb25f876f5ffa3212132ab9bddda9d4952.json`](query-1fe961aa59e6ec14e0ceda6ed64afceb25f876f5ffa3212132ab9bddda9d4952.json) - Metadata for resolving a project id from its unique filesystem path.
- [`query-264978d1598f50afe4ae6c9d9545d961fdd07fc1ca0ec84bedb8ee0b7d688733.json`](query-264978d1598f50afe4ae6c9d9545d961fdd07fc1ca0ec84bedb8ee0b7d688733.json) - Metadata for loading the optional `project_id` attached to a session.
- [`query-4a70667663d0b0eb4f7479d3f7d055a856b881683653a9b2244b24a8ea1c6e99.json`](query-4a70667663d0b0eb4f7479d3f7d055a856b881683653a9b2244b24a8ea1c6e99.json) - Metadata for loading sessions for one project with joined review-request columns.
- [`query-71ecf91ed46884837a62d0401a3171aee3767399c5c92daa642c132c1cc8acd7.json`](query-71ecf91ed46884837a62d0401a3171aee3767399c5c92daa642c132c1cc8acd7.json) - Metadata for fetching a session's persisted base branch.
- [`query-752ea1df1fa1d6ddd1dcddfdc3b974a862401de3a70b0754eadabffc59ffe035.json`](query-752ea1df1fa1d6ddd1dcddfdc3b974a862401de3a70b0754eadabffc59ffe035.json) - Metadata for loading per-model `session_usage` rows for a session.
- [`query-76e8b8a274c05f2d3a7308c7113bdb14a4c39af71dcff6a37bbdf315c6a42a61.json`](query-76e8b8a274c05f2d3a7308c7113bdb14a4c39af71dcff6a37bbdf315c6a42a61.json) - Metadata for loading one `session_operation` row by id inside `db.rs` tests.
- [`query-8f192d056a69946313fc766cd20ed601f615bc5a732bff299026ed907d53520f.json`](query-8f192d056a69946313fc766cd20ed601f615bc5a732bff299026ed907d53520f.json) - Metadata for checking whether a session operation is still `queued` or `running`.
- [`query-9590953dd1df9dbf724d25369b685b19bb883a26c966fe49ba2eddcac6821d2f.json`](query-9590953dd1df9dbf724d25369b685b19bb883a26c966fe49ba2eddcac6821d2f.json) - Metadata for loading one project row by primary key.
- [`query-96cb7d5ca5e133149452314cce8fba7dd58c5cffd65c0d30a7471a5f89c74778.json`](query-96cb7d5ca5e133149452314cce8fba7dd58c5cffd65c0d30a7471a5f89c74778.json) - Metadata for reading a session's optional provider conversation id.
- [`query-9e944b14a5f319e1b97eaed13533e7b0e84bd22c8018e72df0af239b94ba74f9.json`](query-9e944b14a5f319e1b97eaed13533e7b0e84bd22c8018e72df0af239b94ba74f9.json) - Metadata for checking whether unfinished session operations have cancellation requested.
- [`query-b88f5652d96b75591911bafa64b47776ea4090007e9e4bf8d82e31b40523090b.json`](query-b88f5652d96b75591911bafa64b47776ea4090007e9e4bf8d82e31b40523090b.json) - Metadata for verifying that deleting a session nulls the retained `session_usage.session_id` reference in tests.
- [`query-bfcda87a513dbee5abbc35a95944a42a4791715faeafda11c9569b1e4f938eee.json`](query-bfcda87a513dbee5abbc35a95944a42a4791715faeafda11c9569b1e4f938eee.json) - Metadata for loading one session's persisted `created_at` and `updated_at` timestamps.
- [`query-c315669af133f3b4c7fde602d04ea1dd0cc02cf7826128f51fa05eff004d04a9.json`](query-c315669af133f3b4c7fde602d04ea1dd0cc02cf7826128f51fa05eff004d04a9.json) - Metadata for loading unfinished `session_operation` rows in queue order.
- [`query-d0bc573a187f5fdb47f296e235060d6804f187dedc08250110042d2eb157e098.json`](query-d0bc573a187f5fdb47f296e235060d6804f187dedc08250110042d2eb157e098.json) - Metadata for loading all sessions with joined review-request columns.
- [`query-d9d7ba4c1604df034b5bb706e76c80b6bce2cc4b71cb7f456cde51710568b37c.json`](query-d9d7ba4c1604df034b5bb706e76c80b6bce2cc4b71cb7f456cde51710568b37c.json) - Metadata for loading lightweight session count and max-`updated_at` refresh markers.
- [`query-df4746c402d62fd41431633089d019f3b483a0459ac77a964e60980a120aedc2.json`](query-df4746c402d62fd41431633089d019f3b483a0459ac77a964e60980a120aedc2.json) - Metadata for listing projects with aggregated session counts and latest session updates via the `stats` CTE while forcing non-null project columns.
- [`query-f25ca886ee77e8af0199a088066ebc41a63fe36bed9d6d77986e9e159c8a8a6a.json`](query-f25ca886ee77e8af0199a088066ebc41a63fe36bed9d6d77986e9e159c8a8a6a.json) - Metadata for loading a project-scoped setting value by project and key.
- [`query-fd8c45ac137d3f7494cd742e100c8b3c1e9b356c43d1ef48fb8c0a196ad86b81.json`](query-fd8c45ac137d3f7494cd742e100c8b3c1e9b356c43d1ef48fb8c0a196ad86b81.json) - Metadata for loading a global setting value by name.
- [`query-ffda22bf0f99490268c8738c79db2f6e7659e8c258809d0fa46bb1ced50757d0.json`](query-ffda22bf0f99490268c8738c79db2f6e7659e8c258809d0fa46bb1ced50757d0.json) - Metadata for loading the persisted `ActiveProjectId` setting.
