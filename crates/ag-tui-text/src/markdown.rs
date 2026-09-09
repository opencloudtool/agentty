use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rustc_hash::FxHasher;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::style::TextRenderSettings;
use crate::text_util::wrap_styled_line;
use crate::{mermaid, style};

const USER_PROMPT_PREFIX: &str = " › ";
const CLARIFICATION_HEADER: &str = "Clarifications:";
const CLARIFICATION_PROMPT_PREFIX: &str = USER_PROMPT_PREFIX;
const STATS_LABEL_WIDTH: usize = 22;
/// Maximum number of distinct markdown blocks cached at once.
const MARKDOWN_RENDER_CACHE_ENTRY_LIMIT: usize = 64;

/// Cache key for one rendered markdown block.
///
/// The `version` field invalidates every cached render after a theme change so
/// style-bearing `Line` values are never reused across incompatible palettes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkdownRenderCacheKey {
    content_hash: u64,
    content_len: usize,
    version: u64,
    width: u16,
}

/// Cached result of a single `render_markdown` invocation.
struct MarkdownRenderCacheEntry {
    key: MarkdownRenderCacheKey,
    lines: Arc<[Line<'static>]>,
}

/// Bounded LRU cache for `render_markdown` output.
///
/// Stores the most recent rendered markdown slices keyed by a content
/// fingerprint and target width. Keeping several entries avoids cache thrash
/// when one frame renders transcript text alongside summary, review, or footer
/// markdown blocks. Interior mutability via `RefCell` allows cache updates
/// through shared references so the cache can be threaded through immutable
/// render contexts without requiring `&mut` at every call site.
///
/// Cached values are stored as `Arc<[Line<'static>]>` so cache hits can reuse
/// the rendered slice without cloning every line. Callers only clone `Line`
/// values when they need to append the cached content into a larger output
/// buffer.
pub struct MarkdownRenderCache {
    entries: RefCell<VecDeque<MarkdownRenderCacheEntry>>,
    version: Cell<u64>,
}

impl Default for MarkdownRenderCache {
    fn default() -> Self {
        Self {
            entries: RefCell::new(VecDeque::with_capacity(MARKDOWN_RENDER_CACHE_ENTRY_LIMIT)),
            version: Cell::new(0),
        }
    }
}

impl MarkdownRenderCache {
    /// Returns cached rendered lines when the text hash, width, and cache
    /// version match; otherwise renders fresh and updates the cache.
    pub fn render(&self, text: &str, width: usize) -> Arc<[Line<'static>]> {
        self.render_with_settings(text, width, TextRenderSettings::default())
    }

    /// Returns cached rendered lines using caller-provided palette and cache
    /// invalidation settings.
    pub fn render_with_settings(
        &self,
        text: &str,
        width: usize,
        settings: TextRenderSettings,
    ) -> Arc<[Line<'static>]> {
        self.render_with_settings_and_renderer(
            text,
            width,
            settings,
            render_markdown_active_settings,
        )
    }

    /// Returns cached rendered lines using caller-provided settings and
    /// renderer.
    ///
    /// Host applications use this when they need to wrap or extend markdown
    /// rendering while preserving the same content, width, and style cache
    /// key used by the default renderer.
    pub fn render_with_settings_and_renderer(
        &self,
        text: &str,
        width: usize,
        settings: TextRenderSettings,
        renderer: impl FnOnce(&str, usize) -> Vec<Line<'static>>,
    ) -> Arc<[Line<'static>]> {
        style::with_render_settings(settings, || {
            self.render_active_settings(text, width, renderer)
        })
    }

    /// Bumps the cache version and drops all rendered entries.
    ///
    /// Callers use this when non-content render settings change; the active
    /// theme is already part of each cache key.
    pub fn bump_version(&self) {
        self.version.set(self.version.get().wrapping_add(1));
        self.entries.borrow_mut().clear();
    }

    /// Returns the current style-cache version used by higher-level layout
    /// caches to avoid reusing styled lines after markdown entries are
    /// invalidated.
    pub fn version(&self) -> u64 {
        self.version.get()
    }

    fn render_active_settings(
        &self,
        text: &str,
        width: usize,
        renderer: impl FnOnce(&str, usize) -> Vec<Line<'static>>,
    ) -> Arc<[Line<'static>]> {
        let key = self.cache_key(text, width);
        if let Some(lines) = self.cached_lines(key) {
            return lines;
        }

        let lines = Arc::<[Line<'static>]>::from(renderer(text, width));

        self.store_entry(MarkdownRenderCacheEntry {
            key,
            lines: Arc::clone(&lines),
        });

        lines
    }

    /// Builds the cache key for one render call.
    fn cache_key(&self, text: &str, width: usize) -> MarkdownRenderCacheKey {
        MarkdownRenderCacheKey {
            content_hash: Self::hash_text(text),
            content_len: text.len(),
            version: self
                .version
                .get()
                .wrapping_add(style::active_theme_cache_version()),
            width: u16::try_from(width).unwrap_or(u16::MAX),
        }
    }

    /// Returns cached lines for a matching entry and promotes it to the
    /// front of the LRU queue.
    fn cached_lines(&self, key: MarkdownRenderCacheKey) -> Option<Arc<[Line<'static>]>> {
        let mut entries = self.entries.borrow_mut();
        let entry_index = entries.iter().position(|entry| entry.key == key)?;
        let entry = entries.remove(entry_index)?;
        let lines = Arc::clone(&entry.lines);
        entries.push_front(entry);

        Some(lines)
    }

    /// Stores one freshly rendered entry and evicts the oldest item when the
    /// cache reaches capacity.
    fn store_entry(&self, entry: MarkdownRenderCacheEntry) {
        let mut entries = self.entries.borrow_mut();
        entries.push_front(entry);

        while entries.len() > MARKDOWN_RENDER_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }
    }

    /// Computes a fast non-cryptographic content hash for cache-key
    /// comparison.
    fn hash_text(text: &str) -> u64 {
        let mut hasher = FxHasher::default();
        hasher.write(text.as_bytes());

        hasher.finish()
    }
}

#[derive(Clone, Copy)]
enum BlockState {
    Paragraph,
    FencedCode,
    FencedStats,
}

/// Distinguishes prompt-block payloads that share `USER_PROMPT_PREFIX`.
#[derive(Clone, Copy)]
enum PromptBlockKind {
    Clarification,
    UserPrompt,
}

/// Parsed markdown table with normalized header, alignment, and body cells.
struct MarkdownTable {
    alignments: Vec<TableAlignment>,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// Horizontal alignment requested by one markdown table separator cell.
#[derive(Clone, Copy)]
enum TableAlignment {
    Center,
    Left,
    Right,
}

/// Converts markdown text into styled, word-wrapped lines for terminal display.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    render_markdown_active_settings(text, width)
}

/// Converts markdown text into styled, word-wrapped lines using caller-provided
/// palette and cache invalidation settings.
pub fn render_markdown_with_settings(
    text: &str,
    width: usize,
    settings: TextRenderSettings,
) -> Vec<Line<'static>> {
    style::with_render_settings(settings, || render_markdown_active_settings(text, width))
}

/// Returns one flag per input line indicating that leading whitespace must be
/// preserved before Markdown parsing.
///
/// The mask uses the same fence, table, and horizontal-rule classifiers as
/// [`render_markdown`], allowing callers to protect indentation on ordinary
/// text without hiding trim-tolerant block syntax from the shared parser.
pub fn markdown_block_preservation_mask(text: &str) -> Vec<bool> {
    let raw_lines = text.split('\n').collect::<Vec<_>>();
    let mut block_state = BlockState::Paragraph;
    let mut preservation_mask = vec![false; raw_lines.len()];
    let mut line_index = 0;

    while line_index < raw_lines.len() {
        let raw_line = raw_lines[line_index];
        if is_fence_delimiter(raw_line) {
            preservation_mask[line_index] = true;
            let _ = update_fence_state(raw_line, &mut block_state);
            line_index += 1;

            continue;
        }
        if !matches!(block_state, BlockState::Paragraph) {
            preservation_mask[line_index] = true;
            line_index += 1;

            continue;
        }
        if let Some((_, next_line_index)) = parse_markdown_table(&raw_lines, line_index) {
            preservation_mask[line_index..next_line_index].fill(true);
            line_index = next_line_index;

            continue;
        }
        if is_horizontal_rule(raw_line) {
            preservation_mask[line_index] = true;
        }

        line_index += 1;
    }

    preservation_mask
}

fn render_markdown_active_settings(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut rendered_lines = Vec::new();
    let mut block_state = BlockState::Paragraph;
    let mut is_user_prompt_block = false;
    let mut active_prompt_block_kind = PromptBlockKind::UserPrompt;
    let raw_lines = text.split('\n').collect::<Vec<_>>();
    let mut line_index = 0;

    while line_index < raw_lines.len() {
        line_index = render_markdown_input_line(
            &raw_lines,
            line_index,
            width,
            &mut block_state,
            &mut is_user_prompt_block,
            &mut active_prompt_block_kind,
            &mut rendered_lines,
        );
    }

    if is_user_prompt_block {
        rendered_lines.push(prompt_block_padding_line(width, active_prompt_block_kind));
    }

    if rendered_lines.is_empty() {
        rendered_lines.push(Line::from(""));
    }

    rendered_lines
}

/// Renders or consumes one markdown input line and returns the next line index.
fn render_markdown_input_line(
    raw_lines: &[&str],
    line_index: usize,
    width: usize,
    block_state: &mut BlockState,
    is_user_prompt_block: &mut bool,
    active_prompt_block_kind: &mut PromptBlockKind,
    rendered_lines: &mut Vec<Line<'static>>,
) -> usize {
    let raw_line = raw_lines[line_index];

    if handle_prompt_block_line(
        raw_line,
        width,
        block_state,
        is_user_prompt_block,
        active_prompt_block_kind,
        rendered_lines,
    ) {
        return line_index + 1;
    }

    if let Some(next_line_index) =
        render_mermaid_block_line(raw_lines, line_index, width, *block_state, rendered_lines)
    {
        return next_line_index;
    }

    if update_fence_state(raw_line, block_state) {
        return line_index + 1;
    }

    if let Some(next_line_index) =
        render_markdown_table_line(raw_lines, line_index, width, *block_state, rendered_lines)
    {
        return next_line_index;
    }

    render_markdown_block_line(raw_line, width, *block_state, rendered_lines);

    line_index + 1
}

/// Renders a mermaid block when the current paragraph line opens one.
fn render_mermaid_block_line(
    raw_lines: &[&str],
    line_index: usize,
    width: usize,
    block_state: BlockState,
    rendered_lines: &mut Vec<Line<'static>>,
) -> Option<usize> {
    let raw_line = raw_lines[line_index];
    if !matches!(block_state, BlockState::Paragraph) || !is_mermaid_fence(raw_line) {
        return None;
    }

    let (diagram_lines, next_line_index) = render_mermaid_block(raw_lines, line_index, width)?;
    rendered_lines.extend(diagram_lines);

    Some(next_line_index)
}

/// Toggles fenced block state and returns whether the line was consumed.
fn update_fence_state(raw_line: &str, block_state: &mut BlockState) -> bool {
    if !is_fence_delimiter(raw_line) {
        return false;
    }

    *block_state = match block_state {
        BlockState::Paragraph => opening_fence_block_state(raw_line),
        BlockState::FencedCode | BlockState::FencedStats => BlockState::Paragraph,
    };

    true
}

/// Renders a pipe table when the current paragraph line starts one.
fn render_markdown_table_line(
    raw_lines: &[&str],
    line_index: usize,
    width: usize,
    block_state: BlockState,
    rendered_lines: &mut Vec<Line<'static>>,
) -> Option<usize> {
    if !matches!(block_state, BlockState::Paragraph) {
        return None;
    }

    let (table, next_line_index) = parse_markdown_table(raw_lines, line_index)?;
    rendered_lines.extend(render_markdown_table(&table, width));

    Some(next_line_index)
}

/// Renders one non-structural markdown line for the active block state.
fn render_markdown_block_line(
    raw_line: &str,
    width: usize,
    block_state: BlockState,
    rendered_lines: &mut Vec<Line<'static>>,
) {
    match block_state {
        BlockState::Paragraph => rendered_lines.extend(render_markdown_line(raw_line, width)),
        BlockState::FencedCode => rendered_lines.extend(render_code_line(raw_line, width)),
        BlockState::FencedStats => rendered_lines.extend(render_stats_line(raw_line, width)),
    }
}

/// Renders prompt-block lines and returns whether the line was consumed.
fn handle_prompt_block_line(
    raw_line: &str,
    width: usize,
    block_state: &mut BlockState,
    is_user_prompt_block: &mut bool,
    active_prompt_block_kind: &mut PromptBlockKind,
    rendered_lines: &mut Vec<Line<'static>>,
) -> bool {
    let starts_user_prompt_block = raw_line.starts_with(USER_PROMPT_PREFIX);
    let Some(prompt_line) = user_prompt_block_line(raw_line, is_user_prompt_block) else {
        return false;
    };

    if starts_user_prompt_block {
        // Prompt lines are session metadata, not markdown content.
        *active_prompt_block_kind = prompt_block_kind(raw_line);
        *block_state = BlockState::Paragraph;
        rendered_lines.push(prompt_block_padding_line(width, *active_prompt_block_kind));
    }

    let closes_user_prompt_block = prompt_line.is_empty() && !*is_user_prompt_block;
    rendered_lines.extend(render_prompt_block_line(
        prompt_line,
        starts_user_prompt_block,
        width,
        *active_prompt_block_kind,
    ));
    if closes_user_prompt_block {
        rendered_lines.push(Line::from(""));
    }

    true
}

/// Resolves one prompt block style from the first prefixed line.
fn prompt_block_kind(raw_line: &str) -> PromptBlockKind {
    let is_clarification_header = raw_line
        .strip_prefix(USER_PROMPT_PREFIX)
        .is_some_and(|content| content.trim() == CLARIFICATION_HEADER);
    if is_clarification_header {
        return PromptBlockKind::Clarification;
    }

    PromptBlockKind::UserPrompt
}

/// Returns prompt block lines that must be rendered with prompt styling.
///
/// Prompt blocks start with `USER_PROMPT_PREFIX` and continue until the first
/// empty line.
fn user_prompt_block_line<'a>(
    raw_line: &'a str,
    is_user_prompt_block: &mut bool,
) -> Option<&'a str> {
    if *is_user_prompt_block && raw_line.is_empty() {
        *is_user_prompt_block = false;

        return Some(raw_line);
    }

    if raw_line.starts_with(USER_PROMPT_PREFIX) {
        *is_user_prompt_block = true;

        return Some(raw_line);
    }

    if *is_user_prompt_block {
        return Some(raw_line);
    }

    None
}

/// Renders one prompt block line using the style rules for the current prompt
/// block kind.
fn render_prompt_block_line(
    raw_line: &str,
    starts_user_prompt_block: bool,
    width: usize,
    prompt_block_kind: PromptBlockKind,
) -> Vec<Line<'static>> {
    match prompt_block_kind {
        PromptBlockKind::Clarification => {
            render_clarification_prompt_line(raw_line, starts_user_prompt_block, width)
        }
        PromptBlockKind::UserPrompt => {
            render_user_prompt_line(raw_line, starts_user_prompt_block, width)
        }
    }
}

/// Renders a user prompt line verbatim so markdown syntax in prompts is not
/// parsed.
///
/// The first prompt line keeps the `USER_PROMPT_PREFIX` marker while all
/// continuation lines are padded to align with the prompt text.
fn render_user_prompt_line(
    raw_line: &str,
    starts_user_prompt_block: bool,
    width: usize,
) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![prompt_block_padding_line(
            width,
            PromptBlockKind::UserPrompt,
        )];
    }

    let continuation_padding = prompt_block_continuation_padding();
    let content_style = user_prompt_content_style();

    let prompt_lines = if starts_user_prompt_block
        && let Some(content) = raw_line.strip_prefix(USER_PROMPT_PREFIX)
    {
        render_prefixed_verbatim_line(
            USER_PROMPT_PREFIX,
            &continuation_padding,
            content,
            user_prompt_prefix_style(),
            content_style,
            width,
            user_prompt_lookup_spans,
        )
    } else {
        let continuation_content = raw_line
            .strip_prefix(continuation_padding.as_str())
            .unwrap_or(raw_line);

        render_prefixed_verbatim_line(
            &continuation_padding,
            &continuation_padding,
            continuation_content,
            content_style,
            content_style,
            width,
            user_prompt_lookup_spans,
        )
    };

    prompt_lines
        .into_iter()
        .map(|line| pad_line_to_width(line, width, content_style))
        .collect()
}

/// Renders one clarification line with distinct prompt visuals and
/// question/answer marker highlighting.
fn render_clarification_prompt_line(
    raw_line: &str,
    starts_user_prompt_block: bool,
    width: usize,
) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![prompt_block_padding_line(
            width,
            PromptBlockKind::Clarification,
        )];
    }

    let continuation_padding = prompt_block_continuation_padding();
    let content_style = clarification_content_style();

    let prompt_lines = if starts_user_prompt_block
        && let Some(content) = raw_line.strip_prefix(USER_PROMPT_PREFIX)
    {
        render_prefixed_verbatim_line(
            CLARIFICATION_PROMPT_PREFIX,
            &continuation_padding,
            content,
            clarification_prompt_prefix_style(),
            content_style,
            width,
            clarification_prompt_spans,
        )
    } else {
        let continuation_content = raw_line
            .strip_prefix(continuation_padding.as_str())
            .unwrap_or(raw_line);

        render_prefixed_verbatim_line(
            &continuation_padding,
            &continuation_padding,
            continuation_content,
            content_style,
            content_style,
            width,
            clarification_prompt_spans,
        )
    };

    prompt_lines
        .into_iter()
        .map(|line| pad_line_to_width(line, width, content_style))
        .collect()
}

/// Returns one full-width line used as top or bottom padding inside prompt
/// blocks.
fn prompt_block_padding_line(width: usize, prompt_block_kind: PromptBlockKind) -> Line<'static> {
    pad_line_to_width(
        Line::from(""),
        width,
        prompt_block_content_style(prompt_block_kind),
    )
}

/// Returns the base style used across a full prompt-block row.
fn prompt_block_content_style(prompt_block_kind: PromptBlockKind) -> Style {
    match prompt_block_kind {
        PromptBlockKind::Clarification => clarification_content_style(),
        PromptBlockKind::UserPrompt => user_prompt_content_style(),
    }
}

/// Pads one rendered line to the target width using one style for trailing
/// cells.
fn pad_line_to_width(mut line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    if width == 0 {
        return line;
    }

    let line_width = line.width();
    if line_width >= width {
        return line;
    }

    line.spans
        .push(Span::styled(" ".repeat(width - line_width), style));

    line
}

fn render_markdown_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![Line::from("")];
    }

    if raw_line.starts_with(USER_PROMPT_PREFIX) {
        return render_prompt_block_line(raw_line, true, width, PromptBlockKind::UserPrompt);
    }

    if let Some((level, content)) = parse_heading(raw_line) {
        return render_inline_line(content, heading_style(level), width);
    }

    if is_horizontal_rule(raw_line) {
        return vec![horizontal_rule_line(width)];
    }

    if let Some(content) = raw_line.strip_prefix("> ") {
        return render_prefixed_inline_line(
            "│ ",
            "│ ",
            content,
            blockquote_prefix_style(),
            Style::default().fg(style::palette::text_muted()),
            width,
        );
    }

    if let Some(content) = parse_bullet_content(raw_line) {
        return render_prefixed_inline_line(
            "- ",
            "  ",
            content,
            list_prefix_style(),
            Style::default(),
            width,
        );
    }

    if let Some((prefix, content)) = parse_numbered_content(raw_line) {
        let continuation_prefix = " ".repeat(prefix.chars().count());

        return render_prefixed_inline_line(
            &prefix,
            &continuation_prefix,
            content,
            list_prefix_style(),
            Style::default(),
            width,
        );
    }

    render_inline_line(raw_line, Style::default(), width)
}

fn render_prefixed_inline_line(
    prefix: &str,
    continuation_prefix: &str,
    content: &str,
    prefix_style: Style,
    content_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    if width <= prefix_width {
        let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
        spans.extend(parse_inline_spans(content, content_style));

        return wrap_styled_line(spans, width);
    }

    let wrapped_content = render_inline_line(content, content_style, width - prefix_width);
    let mut lines = Vec::with_capacity(wrapped_content.len());

    for (index, line) in wrapped_content.into_iter().enumerate() {
        let marker = if index == 0 {
            prefix
        } else {
            continuation_prefix
        };
        let mut spans = vec![Span::styled(marker.to_string(), prefix_style)];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

/// Wraps one verbatim line while preserving a fixed prefix for wrapped
/// continuations.
///
/// Prompt content wraps on word boundaries when possible to mirror chat-input
/// wrapping, and falls back to character wrapping for words wider than the
/// available width.
fn render_prefixed_verbatim_line(
    prefix: &str,
    continuation_prefix: &str,
    content: &str,
    prefix_style: Style,
    content_style: Style,
    width: usize,
    content_span_builder: fn(&str, Style) -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    if width <= prefix_width {
        let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
        spans.extend(content_span_builder(content, content_style));

        return wrap_styled_line(spans, width);
    }

    let wrapped_content = wrap_verbatim_spans_with_word_boundaries(
        content_span_builder(content, content_style),
        width - prefix_width,
    );
    let mut lines = Vec::with_capacity(wrapped_content.len());

    for (index, line) in wrapped_content.into_iter().enumerate() {
        let marker = if index == 0 {
            prefix
        } else {
            continuation_prefix
        };
        let marker_style = if index == 0 {
            prefix_style
        } else {
            content_style
        };
        let mut spans = vec![Span::styled(marker.to_string(), marker_style)];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }

    lines
}

/// Splits one prompt content line into styled spans with `@` lookup token
/// highlighting.
fn user_prompt_lookup_spans(content: &str, content_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut is_lookup = false;
    let mut previous_character = None;

    for character in content.chars() {
        if character == '@' && previous_character.is_none_or(char::is_whitespace) {
            is_lookup = true;
        } else if character.is_whitespace() {
            is_lookup = false;
        }

        let style = if is_lookup {
            user_prompt_lookup_style()
        } else {
            content_style
        };
        push_verbatim_span_character(&mut spans, style, character);
        previous_character = Some(character);
    }

    spans
}

/// Splits one clarification prompt content line into styled spans for
/// clarification headings and `Q:` / `A:` labels.
fn clarification_prompt_spans(content: &str, content_style: Style) -> Vec<Span<'static>> {
    if content.trim().is_empty() {
        return vec![Span::styled(content.to_string(), content_style)];
    }

    let leading_padding_width = content
        .chars()
        .take_while(|character| character.is_whitespace())
        .count();
    let (leading_padding, trimmed_content) = content.split_at(leading_padding_width);
    let mut spans = Vec::new();
    if !leading_padding.is_empty() {
        spans.push(Span::styled(leading_padding.to_string(), content_style));
    }

    if trimmed_content == CLARIFICATION_HEADER {
        spans.push(Span::styled(
            trimmed_content.to_string(),
            clarification_header_style(),
        ));

        return spans;
    }

    if let Some((question_index, question_text)) =
        parse_clarification_question_line(trimmed_content)
    {
        spans.push(Span::styled(
            question_index,
            clarification_question_index_style(),
        ));
        spans.push(Span::styled(
            "Q: ".to_string(),
            clarification_question_label_style(),
        ));
        spans.push(Span::styled(question_text.to_string(), content_style));

        return spans;
    }

    if let Some(answer_text) = trimmed_content.strip_prefix("A: ") {
        spans.push(Span::styled(
            "A: ".to_string(),
            clarification_answer_label_style(),
        ));
        spans.push(Span::styled(answer_text.to_string(), content_style));

        return spans;
    }

    spans.push(Span::styled(trimmed_content.to_string(), content_style));

    spans
}

fn render_inline_line(content: &str, base_style: Style, width: usize) -> Vec<Line<'static>> {
    let inline_spans = parse_inline_spans(content, base_style);

    wrap_styled_line(inline_spans, width)
}

fn render_code_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    wrap_verbatim_spans_with_word_boundaries(
        vec![Span::styled(raw_line.to_string(), code_block_style())],
        width,
    )
}

fn render_stats_line(raw_line: &str, width: usize) -> Vec<Line<'static>> {
    if raw_line.is_empty() {
        return vec![Line::from("")];
    }

    if let Some((metric, value)) = parse_stats_metric_line(raw_line) {
        let metric_cell = format!("{metric:<STATS_LABEL_WIDTH$}");
        let spans = vec![
            Span::styled(metric_cell, stats_metric_style()),
            Span::styled(value.to_string(), stats_value_style()),
        ];

        return wrap_verbatim_spans(spans, width);
    }

    if raw_line == "Tokens Usage" {
        return wrap_verbatim_line(raw_line, stats_section_style(), width);
    }

    wrap_verbatim_line(raw_line, Style::default(), width)
}

/// Returns whether the line opens a ```` ```mermaid ```` fenced block.
fn is_mermaid_fence(raw_line: &str) -> bool {
    let Some(suffix) = raw_line.trim().strip_prefix("```mermaid") else {
        return false;
    };

    suffix
        .chars()
        .next()
        .is_none_or(|character| character.is_ascii_whitespace() || character == '{')
}

/// Renders one complete ```` ```mermaid ```` fenced block as diagram lines.
///
/// Returns the rendered diagram plus the index just past the closing fence
/// when the block is complete, the diagram type is supported, and the diagram
/// fits `width`. Any other case returns `None` so the block keeps the plain
/// fenced-code presentation.
fn render_mermaid_block(
    raw_lines: &[&str],
    start_index: usize,
    width: usize,
) -> Option<(Vec<Line<'static>>, usize)> {
    let mut source = String::new();
    let mut next_line_index = start_index + 1;
    let mut source_byte_count = 0_usize;
    let mut source_line_count = 0_usize;

    loop {
        let raw_line = raw_lines.get(next_line_index)?;
        next_line_index += 1;
        if is_fence_delimiter(raw_line) {
            break;
        }

        source_line_count += 1;
        if source_line_count > mermaid::MAX_SOURCE_LINE_COUNT {
            return None;
        }

        let newline_byte_count = usize::from(source_line_count > 1);
        source_byte_count = source_byte_count
            .checked_add(newline_byte_count)?
            .checked_add(raw_line.len())?;
        if source_byte_count > mermaid::MAX_SOURCE_BYTE_COUNT {
            return None;
        }

        if source_line_count > 1 {
            source.push('\n');
        }
        source.push_str(raw_line);
    }

    let diagram = mermaid::render_mermaid_for_width(&source, width)?;

    Some((diagram.lines, next_line_index))
}

/// Parses a GitHub-style markdown table starting at `start_index`.
fn parse_markdown_table(raw_lines: &[&str], start_index: usize) -> Option<(MarkdownTable, usize)> {
    let header_cells = parse_table_row(raw_lines.get(start_index)?)?;
    let alignments = parse_table_separator(raw_lines.get(start_index + 1)?, header_cells.len())?;
    let mut next_line_index = start_index + 2;
    let mut rows = Vec::new();

    while let Some(raw_line) = raw_lines.get(next_line_index) {
        if raw_line.trim().is_empty() {
            break;
        }

        let Some(row) = parse_table_row(raw_line) else {
            break;
        };
        if parse_table_separator(raw_line, header_cells.len()).is_some() {
            break;
        }

        rows.push(normalize_table_row(row, header_cells.len()));
        next_line_index += 1;
    }

    Some((
        MarkdownTable {
            alignments,
            headers: header_cells,
            rows,
        },
        next_line_index,
    ))
}

/// Renders a parsed markdown table with aligned cells and no markdown
/// separator row.
fn render_markdown_table(table: &MarkdownTable, width: usize) -> Vec<Line<'static>> {
    let column_widths = table_column_widths(table, width);
    let mut lines = Vec::new();

    lines.push(table_border_line('┌', '┬', '┐', &column_widths));
    lines.extend(render_table_row(
        &table.headers,
        &table.alignments,
        &column_widths,
        table_header_style(),
    ));
    lines.push(table_border_line('├', '┼', '┤', &column_widths));

    for row in &table.rows {
        lines.extend(render_table_row(
            row,
            &table.alignments,
            &column_widths,
            table_cell_style(),
        ));
    }

    lines.push(table_border_line('└', '┴', '┘', &column_widths));

    lines
}

/// Parses one pipe-delimited table row, accepting optional outer pipes.
fn parse_table_row(raw_line: &str) -> Option<Vec<String>> {
    let trimmed = raw_line.trim();
    if !trimmed.contains('|') {
        return None;
    }

    let mut cells = trimmed.split('|').collect::<Vec<_>>();
    if trimmed.starts_with('|') {
        cells.remove(0);
    }
    if trimmed.ends_with('|') {
        cells.pop();
    }
    if cells.len() < 2 {
        return None;
    }

    Some(
        cells
            .into_iter()
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

/// Parses the markdown table separator row and returns requested alignments.
fn parse_table_separator(
    raw_line: &str,
    expected_cell_count: usize,
) -> Option<Vec<TableAlignment>> {
    let separator_cells = parse_table_row(raw_line)?;
    if separator_cells.len() != expected_cell_count {
        return None;
    }

    separator_cells
        .iter()
        .map(|cell| parse_table_alignment(cell))
        .collect()
}

/// Parses a single table separator cell like `---`, `:---`, `---:`, or
/// `:---:`.
fn parse_table_alignment(cell: &str) -> Option<TableAlignment> {
    let trimmed = cell.trim();
    let dash_count = trimmed
        .chars()
        .filter(|character| *character == '-')
        .count();
    if dash_count < 3
        || !trimmed
            .chars()
            .all(|character| character == '-' || character == ':')
    {
        return None;
    }

    match (trimmed.starts_with(':'), trimmed.ends_with(':')) {
        (true, true) => Some(TableAlignment::Center),
        (false, true) => Some(TableAlignment::Right),
        _ => Some(TableAlignment::Left),
    }
}

/// Pads or truncates body rows to the header column count.
fn normalize_table_row(mut row: Vec<String>, column_count: usize) -> Vec<String> {
    row.truncate(column_count);
    while row.len() < column_count {
        row.push(String::new());
    }

    row
}

/// Calculates display widths for each table column and shrinks wide columns
/// to fit the target render width when practical.
fn table_column_widths(table: &MarkdownTable, width: usize) -> Vec<usize> {
    let mut column_widths = table
        .headers
        .iter()
        .enumerate()
        .map(|(column_index, header)| {
            let body_width = table
                .rows
                .iter()
                .filter_map(|row| row.get(column_index))
                .map(|cell| UnicodeWidthStr::width(cell.as_str()))
                .max()
                .unwrap_or(0);

            UnicodeWidthStr::width(header.as_str())
                .max(body_width)
                .max(3)
        })
        .collect::<Vec<_>>();

    shrink_table_columns_to_width(&mut column_widths, width);

    column_widths
}

/// Shrinks the widest table columns one cell at a time until the table fits
/// within `width`, or until every column has reached its minimum width.
fn shrink_table_columns_to_width(column_widths: &mut [usize], width: usize) {
    if width == 0 {
        return;
    }

    while table_rendered_width(column_widths) > width {
        let Some((largest_index, largest_width)) = column_widths
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, column_width)| *column_width > 3)
            .max_by_key(|(_, column_width)| *column_width)
        else {
            break;
        };

        column_widths[largest_index] = largest_width.saturating_sub(1);
    }
}

/// Returns the full terminal cell width of a bordered table row.
fn table_rendered_width(column_widths: &[usize]) -> usize {
    1 + column_widths
        .iter()
        .map(|column_width| column_width + 3)
        .sum::<usize>()
}

/// Renders one horizontal table border.
fn table_border_line(
    left_corner: char,
    join: char,
    right_corner: char,
    column_widths: &[usize],
) -> Line<'static> {
    let mut border = String::new();
    border.push(left_corner);

    for (column_index, column_width) in column_widths.iter().enumerate() {
        border.push_str(&"─".repeat(column_width + 2));
        if column_index + 1 == column_widths.len() {
            border.push(right_corner);
        } else {
            border.push(join);
        }
    }

    Line::from(vec![Span::styled(border, table_border_style())])
}

/// Renders a logical table row, expanding to multiple terminal rows when a
/// cell wraps inside its column.
fn render_table_row(
    cells: &[String],
    alignments: &[TableAlignment],
    column_widths: &[usize],
    cell_style: Style,
) -> Vec<Line<'static>> {
    let wrapped_cells = cells
        .iter()
        .zip(column_widths)
        .map(|(cell, column_width)| wrap_table_cell(cell, *column_width, cell_style))
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut lines = Vec::with_capacity(row_height);

    for row_index in 0..row_height {
        let mut spans = vec![Span::styled("│".to_string(), table_border_style())];

        for (column_index, column_width) in column_widths.iter().enumerate() {
            let cell_line = wrapped_cells
                .get(column_index)
                .and_then(|cell_lines| cell_lines.get(row_index));
            let alignment = alignments
                .get(column_index)
                .copied()
                .unwrap_or(TableAlignment::Left);
            append_table_cell_spans(&mut spans, cell_line, *column_width, alignment, cell_style);
            spans.push(Span::styled("│".to_string(), table_border_style()));
        }

        lines.push(Line::from(spans));
    }

    lines
}

/// Wraps one table cell, preserving inline markdown styles.
fn wrap_table_cell(cell: &str, width: usize, base_style: Style) -> Vec<Line<'static>> {
    if cell.is_empty() {
        return vec![Line::from("")];
    }

    wrap_styled_line(parse_inline_spans(cell, base_style), width)
}

/// Appends one padded and aligned cell to an in-progress table row.
fn append_table_cell_spans(
    spans: &mut Vec<Span<'static>>,
    cell_line: Option<&Line<'static>>,
    column_width: usize,
    alignment: TableAlignment,
    cell_style: Style,
) {
    let cell_width = cell_line.map_or(0, Line::width);
    let available_padding = column_width.saturating_sub(cell_width);
    let (left_padding, right_padding) = table_cell_padding(available_padding, alignment);

    spans.push(Span::styled(" ".repeat(left_padding + 1), cell_style));
    if let Some(cell_line) = cell_line {
        spans.extend(cell_line.spans.iter().cloned());
    }
    spans.push(Span::styled(" ".repeat(right_padding + 1), cell_style));
}

/// Returns the left and right padding needed for one aligned table cell.
fn table_cell_padding(available_padding: usize, alignment: TableAlignment) -> (usize, usize) {
    match alignment {
        TableAlignment::Center => {
            let left_padding = available_padding / 2;

            (left_padding, available_padding - left_padding)
        }
        TableAlignment::Left => (0, available_padding),
        TableAlignment::Right => (available_padding, 0),
    }
}

fn wrap_verbatim_line(content: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(vec![Span::styled(content.to_string(), style)])];
    }

    if content.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_segment = String::new();
    let mut current_width = 0;

    for character in content.chars() {
        let character_width = character_display_width(character);
        if current_width > 0 && character_width > 0 && current_width + character_width > width {
            wrapped_lines.push(Line::from(vec![Span::styled(
                std::mem::take(&mut current_segment),
                style,
            )]));
            current_width = 0;
        }

        current_segment.push(character);
        current_width += character_width;
    }

    if !current_segment.is_empty() {
        wrapped_lines.push(Line::from(vec![Span::styled(current_segment, style)]));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

fn wrap_verbatim_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(spans)];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();

        for character in content.chars() {
            let character_width = character_display_width(character);
            if current_width > 0 && character_width > 0 && current_width + character_width > width {
                wrapped_lines.push(Line::from(std::mem::take(&mut current_spans)));
                current_width = 0;
            }

            push_verbatim_span_character(&mut current_spans, style, character);
            current_width += character_width;
        }
    }

    if !current_spans.is_empty() {
        wrapped_lines.push(Line::from(current_spans));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

/// Wraps verbatim spans while preferring word boundaries.
///
/// The wrapper keeps original whitespace and style spans intact, unlike
/// markdown inline wrapping which normalizes whitespace.
///
/// Implementation detail: this is a single-pass algorithm with a buffered
/// pending word, so long lines with sparse whitespace stay linear-time without
/// repeated lookahead scans.
fn wrap_verbatim_spans_with_word_boundaries(
    spans: Vec<Span<'static>>,
    width: usize,
) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(spans)];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0;
    let mut pending_word_spans = Vec::new();
    let mut pending_word_width = 0;
    let mut has_characters = false;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();

        for character in content.chars() {
            has_characters = true;

            if character.is_whitespace() {
                flush_pending_word_with_wrap(
                    &mut wrapped_lines,
                    &mut current_spans,
                    &mut current_width,
                    &mut pending_word_spans,
                    &mut pending_word_width,
                    width,
                );
                push_character_with_hard_wrap(
                    &mut wrapped_lines,
                    &mut current_spans,
                    &mut current_width,
                    style,
                    character,
                    width,
                );

                continue;
            }

            push_verbatim_span_character(&mut pending_word_spans, style, character);
            pending_word_width += character_display_width(character);
        }
    }

    flush_pending_word_with_wrap(
        &mut wrapped_lines,
        &mut current_spans,
        &mut current_width,
        &mut pending_word_spans,
        &mut pending_word_width,
        width,
    );

    if !has_characters {
        return vec![Line::from("")];
    }

    if !current_spans.is_empty() {
        wrapped_lines.push(Line::from(current_spans));
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line::from(""));
    }

    wrapped_lines
}

/// Flushes one buffered word into the current wrapped output.
///
/// A buffered word moves to the next line when appending it would reach or
/// exceed the available width on a non-empty line, keeping one-cell breathing
/// room against the right edge in prompt blocks.
fn flush_pending_word_with_wrap(
    wrapped_lines: &mut Vec<Line<'static>>,
    current_spans: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    pending_word_spans: &mut Vec<Span<'static>>,
    pending_word_width: &mut usize,
    width: usize,
) {
    if pending_word_spans.is_empty() {
        return;
    }

    if *current_width > 0 && *current_width + *pending_word_width >= width {
        wrapped_lines.push(Line::from(std::mem::take(current_spans)));
        *current_width = 0;
    }

    let word_spans = std::mem::take(pending_word_spans);
    for span in word_spans {
        let style = span.style;
        let content = span.content.into_owned();

        for character in content.chars() {
            push_character_with_hard_wrap(
                wrapped_lines,
                current_spans,
                current_width,
                style,
                character,
                width,
            );
        }
    }

    // The pending word buffer is always fully consumed above, including the
    // hard-wrap fallback for overlong words, so width can be reset
    // unconditionally.
    *pending_word_width = 0;
}

/// Appends one character to the current line, wrapping immediately when the
/// line is already full.
fn push_character_with_hard_wrap(
    wrapped_lines: &mut Vec<Line<'static>>,
    current_spans: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    style: Style,
    character: char,
    width: usize,
) {
    let character_width = character_display_width(character);
    if *current_width > 0 && character_width > 0 && *current_width + character_width > width {
        wrapped_lines.push(Line::from(std::mem::take(current_spans)));
        *current_width = 0;
    }

    push_verbatim_span_character(current_spans, style, character);
    *current_width += character_width;
}

fn push_verbatim_span_character(spans: &mut Vec<Span<'static>>, style: Style, character: char) {
    if let Some(last_span) = spans.last_mut()
        && last_span.style == style
    {
        last_span.content.to_mut().push(character);

        return;
    }

    spans.push(Span::styled(character.to_string(), style));
}

/// Returns terminal display width for one Unicode scalar value.
fn character_display_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Parses inline markdown markers (`**bold**`, `*italic*`, `` `code` ``) and
/// supported dollar-delimited math symbols into styled spans.
///
/// Text outside markers inherits `base_style`. Bold adds `Modifier::BOLD`,
/// italic adds `Modifier::ITALIC`, and backtick-delimited code uses the
/// dedicated inline-code style. Supported math commands render as terminal-
/// friendly Unicode symbols, while unsupported expressions remain literal.
pub fn parse_inline_spans(content: &str, base_style: Style) -> Vec<Span<'static>> {
    let characters: Vec<char> = content.chars().collect();
    let mut spans = Vec::new();
    let mut literal = String::new();
    let mut index = 0;

    while index < characters.len() {
        if characters[index] == '$' && characters.get(index + 1) == Some(&'$') {
            let Some(end_index) = find_matching_double_dollar(&characters, index + 2) else {
                literal.extend(characters[index..].iter());

                break;
            };

            literal.extend(characters[index..end_index + 2].iter());
            index = end_index + 2;

            continue;
        }

        if characters[index] == '`'
            && let Some(end_index) = find_matching_backtick(&characters, index + 1)
            && end_index > index + 1
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let inline_code: String = characters[index + 1..end_index].iter().collect();
            spans.push(Span::styled(inline_code, inline_code_style()));
            index = end_index + 1;

            continue;
        }

        if characters[index] == '*'
            && index + 1 < characters.len()
            && characters[index + 1] == '*'
            && let Some(end_index) = find_matching_double_asterisk(&characters, index + 2)
            && end_index > index + 2
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let bold_content: String = characters[index + 2..end_index].iter().collect();
            spans.push(Span::styled(
                render_inline_math_symbols(&bold_content),
                base_style.add_modifier(Modifier::BOLD),
            ));
            index = end_index + 2;

            continue;
        }

        if characters[index] == '*'
            && let Some(end_index) = find_matching_single_asterisk(&characters, index + 1)
            && end_index > index + 1
        {
            flush_literal_span(&mut spans, &mut literal, base_style);
            let italic_content: String = characters[index + 1..end_index].iter().collect();
            spans.push(Span::styled(
                render_inline_math_symbols(&italic_content),
                base_style.add_modifier(Modifier::ITALIC),
            ));
            index = end_index + 1;

            continue;
        }

        literal.push(characters[index]);
        index += 1;
    }

    flush_literal_span(&mut spans, &mut literal, base_style);

    spans
}

/// Finds the opening index of a closing `$$` delimiter.
fn find_matching_double_dollar(characters: &[char], start_index: usize) -> Option<usize> {
    characters[start_index..]
        .windows(2)
        .position(|window| window == ['$', '$'])
        .map(|relative_index| relative_index + start_index)
}

/// Converts supported inline math while preserving display-math delimiters.
fn render_inline_math_symbols(content: &str) -> String {
    let mut rendered = String::with_capacity(content.len());
    let mut remaining = content;

    while let Some(character) = remaining.chars().next() {
        if let Some((inline_code, suffix)) = split_delimited_prefix(remaining, "`") {
            rendered.push_str(inline_code);
            remaining = suffix;

            continue;
        }

        if let Some((display_math, suffix)) = split_delimited_prefix(remaining, "$$") {
            rendered.push_str(display_math);
            remaining = suffix;

            continue;
        }

        if remaining.starts_with("$$") {
            rendered.push_str(remaining);

            break;
        }

        if let Some(suffix) = remaining.strip_prefix(r"$\rightarrow$") {
            rendered.push('→');
            remaining = suffix;

            continue;
        }

        rendered.push(character);
        remaining = &remaining[character.len_utf8()..];
    }

    rendered
}

/// Splits one complete delimited prefix from its trailing content.
fn split_delimited_prefix<'a>(content: &'a str, delimiter: &str) -> Option<(&'a str, &'a str)> {
    let delimited_content = content.strip_prefix(delimiter)?;
    let closing_index = delimited_content.find(delimiter)?;
    let expression_end = delimiter.len() + closing_index + delimiter.len();

    Some(content.split_at(expression_end))
}

fn flush_literal_span(spans: &mut Vec<Span<'static>>, literal: &mut String, style: Style) {
    if literal.is_empty() {
        return;
    }

    let content = render_inline_math_symbols(&std::mem::take(literal));
    spans.push(Span::styled(content, style));
}

fn parse_heading(raw_line: &str) -> Option<(usize, &str)> {
    if let Some(content) = raw_line.strip_prefix("#### ") {
        return Some((4, content));
    }

    if let Some(content) = raw_line.strip_prefix("### ") {
        return Some((3, content));
    }

    if let Some(content) = raw_line.strip_prefix("## ") {
        return Some((2, content));
    }

    raw_line.strip_prefix("# ").map(|content| (1, content))
}

fn parse_bullet_content(raw_line: &str) -> Option<&str> {
    if let Some(content) = raw_line.strip_prefix("- ") {
        return Some(content);
    }

    raw_line.strip_prefix("* ")
}

fn parse_numbered_content(raw_line: &str) -> Option<(String, &str)> {
    let digit_count = raw_line.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }

    let (digits, suffix) = raw_line.split_at(digit_count);
    let content = suffix.strip_prefix(". ")?;

    Some((format!("{digits}. "), content))
}

/// Parses one clarification question line like `1. Q: Need tests?`.
fn parse_clarification_question_line(raw_line: &str) -> Option<(String, &str)> {
    let digit_count = raw_line.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }

    let (digits, suffix) = raw_line.split_at(digit_count);
    let content = suffix.strip_prefix(". Q: ")?;

    Some((format!("{digits}. "), content))
}

fn opening_fence_block_state(raw_line: &str) -> BlockState {
    if is_stats_fence(raw_line) {
        return BlockState::FencedStats;
    }

    BlockState::FencedCode
}

fn is_fence_delimiter(raw_line: &str) -> bool {
    raw_line.trim().starts_with("```")
}

fn is_stats_fence(raw_line: &str) -> bool {
    raw_line.trim().starts_with("```stats")
}

fn parse_stats_metric_line(raw_line: &str) -> Option<(&str, &str)> {
    let (metric, value) = raw_line.split_once('\t')?;

    Some((metric, value))
}

fn is_horizontal_rule(raw_line: &str) -> bool {
    let trimmed = raw_line.trim();
    if trimmed.len() < 3 {
        return false;
    }

    trimmed.chars().all(|character| character == '-')
        || trimmed.chars().all(|character| character == '*')
}

fn horizontal_rule_line(width: usize) -> Line<'static> {
    if width == 0 {
        return Line::from("");
    }

    Line::from(vec![Span::styled(
        "-".repeat(width),
        horizontal_rule_style(),
    )])
}

fn heading_style(level: usize) -> Style {
    let color = match level {
        1 => style::palette::accent(),
        2 => style::palette::info(),
        3 => style::palette::success(),
        _ => style::palette::warning(),
    };

    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn list_prefix_style() -> Style {
    Style::default().fg(style::palette::text_subtle())
}

fn blockquote_prefix_style() -> Style {
    Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(Modifier::DIM)
}

fn horizontal_rule_style() -> Style {
    Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(Modifier::DIM)
}

fn table_header_style() -> Style {
    Style::default()
        .fg(style::palette::text())
        .bg(style::palette::surface_elevated())
        .add_modifier(Modifier::BOLD)
}

fn table_cell_style() -> Style {
    Style::default().fg(style::palette::text())
}

fn table_border_style() -> Style {
    Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(Modifier::DIM)
}

fn code_block_style() -> Style {
    Style::default()
        .fg(style::palette::text_muted())
        .bg(style::palette::surface_overlay())
}

fn stats_metric_style() -> Style {
    Style::default()
        .fg(style::palette::accent())
        .add_modifier(Modifier::BOLD)
}

fn stats_section_style() -> Style {
    Style::default()
        .fg(style::palette::success())
        .add_modifier(Modifier::BOLD)
}

fn stats_value_style() -> Style {
    inline_code_style()
}

/// Returns the background color used for clarification prompt blocks.
///
/// Resolves to a recessed dark blue-gray surface so the inset reads as a
/// quiet quote block rather than the bright `surface_elevated` tone used for
/// table headers.
fn clarification_background_color() -> Color {
    style::palette::surface_clarification()
}

/// Returns the style for the visible `CLARIFICATION_PROMPT_PREFIX` marker.
fn clarification_prompt_prefix_style() -> Style {
    Style::default()
        .fg(style::palette::warning())
        .bg(clarification_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for clarification heading text.
fn clarification_header_style() -> Style {
    Style::default()
        .fg(style::palette::warning_soft())
        .bg(clarification_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for numbered clarification question indexes.
fn clarification_question_index_style() -> Style {
    Style::default()
        .fg(style::palette::text())
        .bg(clarification_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for `Q:` labels in clarification blocks.
fn clarification_question_label_style() -> Style {
    Style::default()
        .fg(style::palette::accent())
        .bg(clarification_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for `A:` labels in clarification blocks.
fn clarification_answer_label_style() -> Style {
    Style::default()
        .fg(style::palette::success())
        .bg(clarification_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for clarification text content.
fn clarification_content_style() -> Style {
    Style::default()
        .fg(style::palette::text_muted())
        .bg(clarification_background_color())
}

/// Returns the background color used for rendered user prompt blocks.
fn user_prompt_background_color() -> Color {
    style::palette::surface()
}

/// Returns the style for the visible `USER_PROMPT_PREFIX` marker.
fn user_prompt_prefix_style() -> Style {
    Style::default()
        .fg(style::palette::accent())
        .bg(user_prompt_background_color())
        .add_modifier(Modifier::BOLD)
}

/// Returns the style for user prompt text content.
///
/// Prompt transcript text uses the same semantic foreground as live typed
/// input while retaining a shaded background so persisted prompts remain
/// visually grouped without becoming brighter than the surrounding UI.
fn user_prompt_content_style() -> Style {
    Style::default()
        .fg(style::palette::text())
        .bg(user_prompt_background_color())
}

/// Returns the style for one `@` lookup token within user prompt content.
fn user_prompt_lookup_style() -> Style {
    Style::default()
        .fg(style::palette::info())
        .bg(user_prompt_background_color())
}

/// Returns continuation padding that aligns with prompt prefix width.
fn prompt_block_continuation_padding() -> String {
    " ".repeat(USER_PROMPT_PREFIX.chars().count())
}

fn inline_code_style() -> Style {
    Style::default().fg(style::palette::warning())
}

fn find_matching_backtick(characters: &[char], start_index: usize) -> Option<usize> {
    characters[start_index..]
        .iter()
        .position(|character| *character == '`')
        .map(|index| index + start_index)
}

fn find_matching_double_asterisk(characters: &[char], start_index: usize) -> Option<usize> {
    let mut index = start_index;

    while index + 1 < characters.len() {
        if characters[index] == '*' && characters[index + 1] == '*' {
            return Some(index);
        }

        index += 1;
    }

    None
}

fn find_matching_single_asterisk(characters: &[char], start_index: usize) -> Option<usize> {
    characters[start_index..]
        .iter()
        .position(|character| *character == '*')
        .map(|index| index + start_index)
}

#[cfg(test)]
#[path = "markdown_test.rs"]
mod tests;
