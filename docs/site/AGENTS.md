# Documentation Site

Applies to site configuration, templates, content, and static assets.

## Search

- Keep `build_search_index`, the `[search]` configuration, generated-index loading,
  sidebar behavior, and the `/docs/` result filter consistent when changing search.
- For search changes or broad documentation changes, verify rendered search by title and
  body terms in addition to the root docs-site checks.

## Feature Pages and Assets

- Each E2E test's `FeatureTest::zola()` metadata owns its generated feature title,
  description, and weight.
- Keep feature GIFs and same-named PNG posters paired under
  `docs/site/static/features/`; use `skills/feature-test/SKILL.md` for generation and
  verification.
