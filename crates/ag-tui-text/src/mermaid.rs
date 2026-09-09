use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::style::{self, TextRenderSettings};

const CONNECT_UP: u8 = 1;
const CONNECT_DOWN: u8 = 2;
const CONNECT_LEFT: u8 = 4;
const CONNECT_RIGHT: u8 = 8;
const LAYER_GAP_COLUMNS: usize = 3;
const LAYER_GAP_ROWS: usize = 1;
const MAX_DUMMY_NODE_COUNT: usize = 32;
const MAX_EDGE_COUNT: usize = 24;
const MAX_LABEL_WIDTH: usize = 32;
const MAX_NODE_COUNT: usize = 16;
/// Maximum bytes accepted for one mermaid source preview.
pub const MAX_SOURCE_BYTE_COUNT: usize = 16 * 1024;
/// Maximum source lines accepted for one mermaid source preview.
pub const MAX_SOURCE_LINE_COUNT: usize = 128;
const NODE_BOX_HEIGHT: usize = 3;
const PARSED_MERMAID_CACHE_ENTRY_LIMIT: usize = 64;
const SEQUENCE_MAX_GAP_COLUMNS: usize = MAX_LABEL_WIDTH + 2;
const SEQUENCE_MIN_GAP_COLUMNS: usize = 8;
const SEQUENCE_SELF_LOOP_COLUMNS: usize = 3;

/// Rendered mermaid diagram rows plus the widest row width in cells.
pub struct MermaidDiagram {
    /// Styled diagram rows ready for terminal painting.
    pub lines: Vec<Line<'static>>,
    /// Display width of the widest diagram row.
    pub width: usize,
}

/// Renders one ```` ```mermaid ```` source block into Unicode box-drawing
/// lines.
///
/// Supports `graph`/`flowchart` headers with `TD`, `TB`, and `LR` directions,
/// node statements (including `&` fan-out groups and common node shapes), and
/// edge chains with solid, dotted, thick, and bidirectional arrow variants,
/// plus invisible links that affect layout without being painted,
/// `erDiagram` headers with crow's-foot relationship statements, and simple
/// `sequenceDiagram` participant/message statements. Flowchart subgraphs are
/// flattened, styling statements are skipped, over-long labels are truncated
/// with an ellipsis, and sequence notes, activations, and control blocks are
/// skipped rather than drawn.
/// Returns `None` for unsupported diagram types or layouts so callers can keep
/// the plain code-block presentation.
pub fn render_mermaid(source: &str) -> Option<MermaidDiagram> {
    render_mermaid_active_settings(source)
}

/// Renders one ```` ```mermaid ```` source block using caller-provided palette
/// settings.
pub fn render_mermaid_with_settings(
    source: &str,
    settings: TextRenderSettings,
) -> Option<MermaidDiagram> {
    style::with_render_settings(settings, || render_mermaid_active_settings(source))
}

/// Renders a diagram within `max_width`, stacking an over-wide left-to-right
/// graph from top to bottom before giving up on its terminal preview.
pub(crate) fn render_mermaid_for_width(source: &str, max_width: usize) -> Option<MermaidDiagram> {
    let parsed = parsed_mermaid(source)?;
    let diagram = parsed.render()?;
    if diagram.width <= max_width {
        return Some(diagram);
    }

    let ParsedMermaid::Graph(graph) = parsed.as_ref() else {
        return None;
    };
    if !matches!(graph.direction, FlowDirection::LeftRight) {
        return None;
    }
    let mut graph = graph.clone();
    graph.direction = FlowDirection::TopDown;

    let diagram = render_graph(graph)?;
    (diagram.width <= max_width).then_some(diagram)
}

fn render_mermaid_active_settings(source: &str) -> Option<MermaidDiagram> {
    parsed_mermaid(source)?.render()
}

/// Parsing is independent of width and palette. Cache at most 64 bounded
/// sources, including unsupported syntax, so resizing and scrollbar probes
/// reuse parsing without retaining styled output from another theme.
struct ParsedMermaidCacheEntry {
    diagram: Option<Arc<ParsedMermaid>>,
    source: Box<str>,
}

thread_local! {
    static PARSED_MERMAID_CACHE: RefCell<VecDeque<ParsedMermaidCacheEntry>> =
        const { RefCell::new(VecDeque::new()) };
}

enum ParsedMermaid {
    Graph(MermaidGraph),
    Sequence(SequenceDiagram),
}

impl ParsedMermaid {
    fn render(&self) -> Option<MermaidDiagram> {
        match self {
            Self::Graph(graph) => render_graph(graph.clone()),
            Self::Sequence(diagram) => Some(draw_sequence_diagram(diagram)),
        }
    }
}

fn parsed_mermaid(source: &str) -> Option<Arc<ParsedMermaid>> {
    if !is_source_within_bounds(source) {
        return None;
    }

    PARSED_MERMAID_CACHE.with(|cache| {
        let mut entries = cache.borrow_mut();
        if let Some(index) = entries
            .iter()
            .position(|entry| entry.source.as_ref() == source)
        {
            let entry = entries.remove(index)?;
            let diagram = entry.diagram.clone();
            entries.push_front(entry);

            return diagram;
        }

        let diagram = parse_sequence_diagram(source)
            .map(ParsedMermaid::Sequence)
            .or_else(|| parse_graph(source).map(ParsedMermaid::Graph))
            .map(Arc::new);
        entries.push_front(ParsedMermaidCacheEntry {
            diagram: diagram.clone(),
            source: source.into(),
        });
        if entries.len() > PARSED_MERMAID_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }

        diagram
    })
}

/// Lays out one parsed graph using its current direction.
fn render_graph(graph: MermaidGraph) -> Option<MermaidDiagram> {
    if let Some(diagram) = draw_left_right_feedback_graph(&graph) {
        return Some(diagram);
    }

    let (mut graph, feedback_edges) = split_feedback_edges(graph)?;
    if !feedback_edges.is_empty() && matches!(graph.direction, FlowDirection::LeftRight) {
        graph.direction = FlowDirection::TopDown;
    }
    let graph = expand_long_edges(graph)?;
    let layout = layout_layers(&graph)?;

    let diagram = match graph.direction {
        FlowDirection::TopDown => draw_top_down(&graph, &layout, &feedback_edges),
        FlowDirection::LeftRight => draw_left_right(&graph, &layout),
    };

    Some(diagram)
}

/// Returns whether a source block stays within the preview parsing budget.
fn is_source_within_bounds(source: &str) -> bool {
    source.len() <= MAX_SOURCE_BYTE_COUNT
        && source.split('\n').take(MAX_SOURCE_LINE_COUNT + 1).count() <= MAX_SOURCE_LINE_COUNT
}

/// Flow direction taken from the diagram header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlowDirection {
    LeftRight,
    TopDown,
}

/// Node outline shape derived from the mermaid label delimiters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeShape {
    Rectangle,
    Rounded,
}

/// One declared diagram node.
#[derive(Clone)]
struct MermaidNode {
    is_hidden: bool,
    is_visible: bool,
    label: String,
    shape: NodeShape,
}

/// One directed or undirected link between two nodes.
#[derive(Clone)]
struct MermaidEdge {
    from_index: usize,
    has_arrow: bool,
    has_source_arrow: bool,
    is_visible: bool,
    label: Option<String>,
    source_marker: Option<char>,
    target_marker: Option<char>,
    to_index: usize,
}

/// Parsed mermaid diagram ready for layered layout.
#[derive(Clone)]
struct MermaidGraph {
    direction: FlowDirection,
    edges: Vec<MermaidEdge>,
    nodes: Vec<MermaidNode>,
}

/// One participant in a simple sequence diagram.
struct SequenceParticipant {
    label: String,
}

/// One message row in a simple sequence diagram.
struct SequenceMessage {
    from_index: usize,
    label: String,
    to_index: usize,
}

/// Parsed sequence diagram ready for terminal drawing.
struct SequenceDiagram {
    messages: Vec<SequenceMessage>,
    participants: Vec<SequenceParticipant>,
}

/// Parses a simple `sequenceDiagram` block into participants and messages.
fn parse_sequence_diagram(source: &str) -> Option<SequenceDiagram> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"));

    if !lines.next()?.eq_ignore_ascii_case("sequenceDiagram") {
        return None;
    }

    let mut participant_indexes: HashMap<String, usize> = HashMap::new();
    let mut participants = Vec::new();
    let mut messages = Vec::new();

    for line in lines {
        let participant_text = line
            .strip_prefix("participant ")
            .or_else(|| line.strip_prefix("actor "));
        if let Some(participant_text) = participant_text {
            parse_sequence_participant(
                participant_text,
                &mut participant_indexes,
                &mut participants,
            )?;

            continue;
        }

        if is_ignorable_sequence_statement(line) {
            continue;
        }

        parse_sequence_message(
            line,
            &mut participant_indexes,
            &mut participants,
            &mut messages,
        )?;
    }

    if participants.is_empty() || messages.is_empty() {
        return None;
    }

    Some(SequenceDiagram {
        messages,
        participants,
    })
}

/// Returns whether a sequence statement is decorative and safe to skip.
///
/// Notes, autonumbering, activations, and `alt`/`opt`/`loop`-style control
/// blocks cannot be drawn by this preview; skipping them keeps the remaining
/// participant and message lines renderable instead of dropping the diagram.
fn is_ignorable_sequence_statement(line: &str) -> bool {
    const IGNORABLE_KEYWORDS: [&str; 16] = [
        "activate",
        "alt",
        "and",
        "autonumber",
        "box",
        "break",
        "critical",
        "deactivate",
        "else",
        "end",
        "loop",
        "note",
        "opt",
        "option",
        "par",
        "rect",
    ];

    let Some(first_token) = line.split_whitespace().next() else {
        return true;
    };

    IGNORABLE_KEYWORDS
        .iter()
        .any(|keyword| first_token.eq_ignore_ascii_case(keyword))
}

/// Parses one sequence participant declaration.
fn parse_sequence_participant(
    participant_text: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
) -> Option<()> {
    let (identifier, label) = if let Some((identifier, label)) = participant_text.split_once(" as ")
    {
        (identifier.trim(), label.trim().trim_matches('"').trim())
    } else {
        let identifier = participant_text.trim();

        (identifier, identifier)
    };

    sequence_participant_index(identifier, label, participant_indexes, participants)?;

    Some(())
}

/// Parses one sequence message line such as `User->>Agentty: Start`.
fn parse_sequence_message(
    line: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
    messages: &mut Vec<SequenceMessage>,
) -> Option<()> {
    if messages.len() >= MAX_EDGE_COUNT {
        return None;
    }

    let (link_text, label_text) = line.split_once(':')?;
    let (from_identifier, to_identifier) = split_sequence_link(link_text)?;
    let label = truncated_renderable_label(label_text)?;

    let from_index = sequence_participant_index(
        from_identifier,
        from_identifier,
        participant_indexes,
        participants,
    )?;
    let to_index = sequence_participant_index(
        to_identifier,
        to_identifier,
        participant_indexes,
        participants,
    )?;

    messages.push(SequenceMessage {
        from_index,
        label,
        to_index,
    });

    Some(())
}

/// Splits the supported sequence message operators into source and target IDs.
///
/// Cross (`-x`) and async (`-)`) endings render as plain arrows, and a leading
/// `+`/`-` activation shorthand on the target identifier is stripped.
fn split_sequence_link(link_text: &str) -> Option<(&str, &str)> {
    const OPERATORS: [&str; 8] = ["-->>", "->>", "--x", "--)", "-->", "-x", "-)", "->"];

    for operator in OPERATORS {
        let Some((from_identifier, to_identifier)) = link_text.split_once(operator) else {
            continue;
        };
        let to_identifier = to_identifier.trim().trim_start_matches(['+', '-']).trim();

        return Some((from_identifier.trim(), to_identifier));
    }

    None
}

/// Normalizes one node, edge, or sequence label, truncating labels wider than
/// `MAX_LABEL_WIDTH` to that width with a trailing ellipsis.
///
/// Returns `None` for empty labels and for labels containing zero-width or
/// double-width glyphs, which would break cell-grid column alignment.
fn truncated_renderable_label(label_text: &str) -> Option<String> {
    let label = normalized_mermaid_label(label_text);
    if label.is_empty()
        || !label
            .chars()
            .all(|character| UnicodeWidthChar::width(character) == Some(1))
    {
        return None;
    }

    if label.chars().count() <= MAX_LABEL_WIDTH {
        return Some(label.to_string());
    }

    let mut truncated: String = label.chars().take(MAX_LABEL_WIDTH - 1).collect();
    truncated.push('…');

    Some(truncated)
}

/// Normalized edge label, separating an absent label from a present one.
enum EdgeLabel {
    Absent,
    Present(String),
}

impl EdgeLabel {
    /// Returns the label text, treating an absent label as no label.
    fn into_label(self) -> Option<String> {
        match self {
            Self::Absent => None,
            Self::Present(label) => Some(label),
        }
    }
}

/// Normalizes one edge label, separating an absent label from a label that is
/// present but unrenderable.
///
/// Returns `EdgeLabel::Absent` when the text normalizes to nothing, and `None`
/// when a non-empty label carries glyphs the cell grid cannot paint. Callers
/// reject the whole diagram in the latter case, matching node and participant
/// labels, so a meaningful relationship label is never silently dropped from an
/// otherwise complete drawing.
fn parsed_edge_label(label_text: &str) -> Option<EdgeLabel> {
    if normalized_mermaid_label(label_text).is_empty() {
        return Some(EdgeLabel::Absent);
    }

    Some(EdgeLabel::Present(truncated_renderable_label(label_text)?))
}

/// Returns the dense participant index, inserting a participant on first use.
fn sequence_participant_index(
    identifier: &str,
    label: &str,
    participant_indexes: &mut HashMap<String, usize>,
    participants: &mut Vec<SequenceParticipant>,
) -> Option<usize> {
    if !is_renderable_identifier(identifier) {
        return None;
    }

    if let Some(existing_index) = participant_indexes.get(identifier) {
        return Some(*existing_index);
    }

    if participants.len() >= MAX_NODE_COUNT {
        return None;
    }

    let label = truncated_renderable_label(label)?;

    participants.push(SequenceParticipant { label });
    participant_indexes.insert(identifier.to_string(), participants.len() - 1);

    Some(participants.len() - 1)
}

/// Parses mermaid source into a bounded diagram graph.
fn parse_graph(source: &str) -> Option<MermaidGraph> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("%%"))
        .peekable();

    if lines.peek()?.eq_ignore_ascii_case("erDiagram") {
        lines.next();

        return parse_er_graph(lines);
    }

    parse_flow_graph(lines)
}

/// Parses `erDiagram` statements into a top-down entity-relationship graph.
///
/// Supports entity declarations and crow's-foot relationship statements.
/// Entity attribute blocks are consumed but omitted from the preview, which
/// keeps entity boxes, relationship lines, labels, and cardinality markers.
fn parse_er_graph<'source>(lines: impl Iterator<Item = &'source str>) -> Option<MermaidGraph> {
    let mut node_indexes: HashMap<String, usize> = HashMap::new();
    let mut nodes: Vec<MermaidNode> = Vec::new();
    let mut edges: Vec<MermaidEdge> = Vec::new();
    let mut in_attribute_block = false;

    for line in lines {
        if in_attribute_block {
            in_attribute_block = line != "}";

            continue;
        }

        let (statement, opens_attribute_block) = match line.strip_suffix('{') {
            Some(before_brace) => (before_brace.trim(), true),
            None => (line, false),
        };

        if !statement.contains(char::is_whitespace) {
            er_entity_index(statement, &mut node_indexes, &mut nodes)?;
            in_attribute_block = opens_attribute_block;

            continue;
        }

        if opens_attribute_block {
            return None;
        }

        parse_er_relationship(statement, &mut node_indexes, &mut nodes, &mut edges)?;
    }

    bounded_graph(FlowDirection::TopDown, nodes, edges)
}

/// Parses one `A ||--o{ B : label` crow's-foot relationship statement.
fn parse_er_relationship(
    statement: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
    edges: &mut Vec<MermaidEdge>,
) -> Option<()> {
    let (link_text, label_text) = match statement.split_once(':') {
        Some((link_text, label_text)) => (link_text, Some(label_text)),
        None => (statement, None),
    };

    let mut tokens = link_text.split_whitespace();
    let from_index = er_entity_index(tokens.next()?, node_indexes, nodes)?;
    let operator = tokens.next()?;
    let to_index = er_entity_index(tokens.next()?, node_indexes, nodes)?;
    if tokens.next().is_some() {
        return None;
    }

    let source_marker = er_cardinality_marker(operator.get(..2)?)?;
    let connector = operator.get(2..4)?;
    if connector != "--" && connector != ".." {
        return None;
    }
    let target_marker = er_cardinality_marker(operator.get(4..)?)?;

    let label = match label_text {
        Some(label_text) => parsed_edge_label(label_text)?.into_label(),
        None => None,
    };

    if edges.len() >= MAX_EDGE_COUNT {
        return None;
    }

    edges.push(MermaidEdge {
        from_index,
        has_arrow: false,
        has_source_arrow: false,
        is_visible: true,
        label,
        source_marker: Some(source_marker),
        target_marker: Some(target_marker),
        to_index,
    });

    Some(())
}

/// Returns the dense node index for one ER entity, inserting it on first use.
fn er_entity_index(
    identifier: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
) -> Option<usize> {
    if !is_renderable_identifier(identifier) {
        return None;
    }

    if let Some(existing_index) = node_indexes.get(identifier) {
        return Some(*existing_index);
    }

    if nodes.len() >= MAX_NODE_COUNT {
        return None;
    }

    nodes.push(MermaidNode {
        is_hidden: false,
        is_visible: true,
        label: truncated_renderable_label(identifier)?,
        shape: NodeShape::Rectangle,
    });
    node_indexes.insert(identifier.to_string(), nodes.len() - 1);

    Some(nodes.len() - 1)
}

/// Maps one crow's-foot cardinality token to its compact marker glyph.
///
/// `1` marks exactly one, `?` zero or one, `*` zero or more, and `+` one or
/// more, matching UML-style multiplicity shorthand.
fn er_cardinality_marker(token: &str) -> Option<char> {
    match token {
        "||" => Some('1'),
        "|o" | "o|" => Some('?'),
        "}o" | "o{" => Some('*'),
        "}|" | "|{" => Some('+'),
        _ => None,
    }
}

/// Parses `graph`/`flowchart` statements into a bounded flowchart graph.
fn parse_flow_graph<'source>(lines: impl Iterator<Item = &'source str>) -> Option<MermaidGraph> {
    let mut direction: Option<FlowDirection> = None;
    let mut node_indexes: HashMap<String, usize> = HashMap::new();
    let mut nodes: Vec<MermaidNode> = Vec::new();
    let mut edges: Vec<MermaidEdge> = Vec::new();

    for line in lines {
        let mut statements = line.split(';');
        if direction.is_none() {
            direction = Some(parse_direction_header(statements.next()?.trim())?);
        }

        for statement in statements {
            let statement = statement.trim();
            if statement.is_empty() || is_ignorable_flow_statement(statement) {
                continue;
            }

            parse_statement(statement, &mut node_indexes, &mut nodes, &mut edges)?;
        }
    }

    bounded_graph(direction?, nodes, edges)
}

/// Returns whether a flowchart statement is decorative and safe to skip.
///
/// Styling and interaction statements do not affect the drawn structure, and
/// skipping `subgraph`, `direction`, and `end` lines flattens subgraphs into
/// the surrounding graph instead of dropping the diagram.
fn is_ignorable_flow_statement(statement: &str) -> bool {
    const IGNORABLE_KEYWORDS: [&str; 8] = [
        "class",
        "classDef",
        "click",
        "direction",
        "end",
        "linkStyle",
        "style",
        "subgraph",
    ];

    let Some(first_token) = statement.split_whitespace().next() else {
        return true;
    };

    IGNORABLE_KEYWORDS
        .iter()
        .any(|keyword| first_token.eq_ignore_ascii_case(keyword))
}

/// Builds the graph when node and edge counts stay within preview bounds and
/// every final node label is renderable.
///
/// Bare identifiers become node labels without upfront validation, so the
/// label bounds are enforced here on the final labels — truncating over-long
/// labels — before layout sizes the canvas from their widths.
fn bounded_graph(
    direction: FlowDirection,
    mut nodes: Vec<MermaidNode>,
    edges: Vec<MermaidEdge>,
) -> Option<MermaidGraph> {
    if nodes.is_empty() || nodes.len() > MAX_NODE_COUNT || edges.len() > MAX_EDGE_COUNT {
        return None;
    }
    for node in &mut nodes {
        node.label = truncated_renderable_label(&node.label)?;
    }

    Some(MermaidGraph {
        direction,
        edges,
        nodes,
    })
}

/// Separates edges that close cycles from the acyclic graph used for layered
/// layout.
///
/// Feedback edges are retained for a dedicated outer routing lane. Self-links
/// remain unsupported because they have no distinct source and target layer.
fn split_feedback_edges(mut graph: MermaidGraph) -> Option<(MermaidGraph, Vec<MermaidEdge>)> {
    let mut feedback_edges = Vec::new();
    let mut layout_edges = Vec::with_capacity(graph.edges.len());

    for edge in graph.edges.drain(..) {
        if edge.from_index == edge.to_index {
            return None;
        }

        if graph_path_exists(
            edge.to_index,
            edge.from_index,
            &layout_edges,
            graph.nodes.len(),
        ) {
            feedback_edges.push(edge);
        } else {
            layout_edges.push(edge);
        }
    }

    graph.edges = layout_edges;

    Some((graph, feedback_edges))
}

/// Returns whether `target_index` is reachable from `source_index` through
/// edges already admitted to the layered graph.
fn graph_path_exists(
    source_index: usize,
    target_index: usize,
    edges: &[MermaidEdge],
    node_count: usize,
) -> bool {
    let mut pending = VecDeque::from([source_index]);
    let mut visited = vec![false; node_count];
    visited[source_index] = true;

    while let Some(node_index) = pending.pop_front() {
        for target in edges
            .iter()
            .filter(|edge| edge.from_index == node_index)
            .map(|edge| edge.to_index)
        {
            if target == target_index {
                return true;
            }
            if !visited[target] {
                visited[target] = true;
                pending.push_back(target);
            }
        }
    }

    false
}

/// Splits long graph edges into adjacent-layer segments through hidden nodes.
fn expand_long_edges(mut graph: MermaidGraph) -> Option<MermaidGraph> {
    let node_layers = assign_node_layers(&graph)?;
    let mut edges = Vec::with_capacity(graph.edges.len());
    let mut dummy_node_count = 0;

    for edge in graph.edges {
        let from_layer = node_layers[edge.from_index];
        let to_layer = node_layers[edge.to_index];
        if to_layer <= from_layer {
            return None;
        }

        if to_layer == from_layer + 1 {
            edges.push(edge);

            continue;
        }

        let mut from_index = edge.from_index;
        let mut has_source_arrow = edge.has_source_arrow;
        let mut source_marker = edge.source_marker;
        let mut label = edge.label;
        for _ in from_layer + 1..to_layer {
            if dummy_node_count >= MAX_DUMMY_NODE_COUNT {
                return None;
            }

            let dummy_index = graph.nodes.len();
            graph.nodes.push(MermaidNode {
                is_hidden: true,
                is_visible: edge.is_visible,
                label: String::new(),
                shape: NodeShape::Rectangle,
            });
            dummy_node_count += 1;

            edges.push(MermaidEdge {
                from_index,
                has_arrow: false,
                has_source_arrow,
                is_visible: edge.is_visible,
                label: label.take(),
                source_marker: source_marker.take(),
                target_marker: None,
                to_index: dummy_index,
            });
            from_index = dummy_index;
            has_source_arrow = false;
        }

        edges.push(MermaidEdge {
            from_index,
            has_arrow: edge.has_arrow,
            has_source_arrow,
            is_visible: edge.is_visible,
            label,
            source_marker,
            target_marker: edge.target_marker,
            to_index: edge.to_index,
        });
    }

    graph.edges = edges;

    Some(graph)
}

/// Parses the `graph`/`flowchart` header line into a supported direction.
fn parse_direction_header(line: &str) -> Option<FlowDirection> {
    let mut tokens = line.split_whitespace();
    let keyword = tokens.next()?;
    if !keyword.eq_ignore_ascii_case("graph") && !keyword.eq_ignore_ascii_case("flowchart") {
        return None;
    }

    let direction_token = tokens.next()?;
    let direction = if direction_token.eq_ignore_ascii_case("TD")
        || direction_token.eq_ignore_ascii_case("TB")
    {
        FlowDirection::TopDown
    } else if direction_token.eq_ignore_ascii_case("LR") {
        FlowDirection::LeftRight
    } else {
        return None;
    };
    if tokens.next().is_some() {
        return None;
    }

    Some(direction)
}

/// Parses one statement: a node declaration or an edge chain.
///
/// `&` groups on either side of an operator fan out into one edge per
/// source/target pair, matching mermaid's `A --> B & C` shorthand.
///
/// A source-only arrow (`A <-- B`) is a reverse edge, so it is stored with its
/// endpoints swapped and a target arrow. Layering, long-edge expansion, and
/// cycle detection all read the stored endpoints, so the swap keeps them on
/// mermaid's semantic direction instead of the syntactic one. Chains still
/// continue from the syntactic right-hand group, so `A <-- B --> C` fans both
/// edges out of `B`.
fn parse_statement(
    statement: &str,
    node_indexes: &mut HashMap<String, usize>,
    nodes: &mut Vec<MermaidNode>,
    edges: &mut Vec<MermaidEdge>,
) -> Option<()> {
    let mut cursor = StatementCursor { rest: statement };
    let mut from_indexes = cursor.parse_node_group(node_indexes, nodes)?;

    loop {
        cursor.skip_whitespace();
        if cursor.rest.is_empty() {
            return Some(());
        }

        let (has_source_arrow, has_arrow, is_visible, label) = cursor.parse_edge_operator()?;
        let to_indexes = cursor.parse_node_group(node_indexes, nodes)?;
        append_group_edges(
            &from_indexes,
            &to_indexes,
            has_source_arrow,
            has_arrow,
            is_visible,
            label.as_deref(),
            edges,
        )?;
        from_indexes = to_indexes;
    }
}

/// Appends the bounded Cartesian product of two node groups for one operator.
fn append_group_edges(
    from_indexes: &[usize],
    to_indexes: &[usize],
    has_source_arrow: bool,
    has_arrow: bool,
    is_visible: bool,
    label: Option<&str>,
    edges: &mut Vec<MermaidEdge>,
) -> Option<()> {
    let is_reversed = has_source_arrow && !has_arrow;

    for from_index in from_indexes {
        for to_index in to_indexes {
            if edges.len() >= MAX_EDGE_COUNT {
                return None;
            }

            let (source_index, target_index) = if is_reversed {
                (*to_index, *from_index)
            } else {
                (*from_index, *to_index)
            };

            edges.push(MermaidEdge {
                from_index: source_index,
                has_arrow: has_arrow || is_reversed,
                has_source_arrow: has_source_arrow && !is_reversed,
                is_visible,
                label: label.map(str::to_owned),
                source_marker: None,
                target_marker: None,
                to_index: target_index,
            });
        }
    }

    Some(())
}

/// Successful outcome of parsing an optional node-shape suffix.
enum NodeShapeParse {
    Bare,
    Labeled(NodeShape, String),
}

/// Successful outcome of probing for an inline edge-label operator.
enum InlineLabelParse {
    Absent,
    Present {
        has_arrow: bool,
        label: Option<String>,
    },
}

/// Incremental parser over one mermaid statement.
struct StatementCursor<'a> {
    rest: &'a str,
}

impl StatementCursor<'_> {
    /// Parses a `node & node & …` group and returns its dense node indexes.
    fn parse_node_group(
        &mut self,
        node_indexes: &mut HashMap<String, usize>,
        nodes: &mut Vec<MermaidNode>,
    ) -> Option<Vec<usize>> {
        let mut group = vec![self.parse_node(node_indexes, nodes)?];

        loop {
            self.skip_whitespace();
            let Some(after_ampersand) = self.rest.strip_prefix('&') else {
                break;
            };
            self.rest = after_ampersand;
            group.push(self.parse_node(node_indexes, nodes)?);
        }

        Some(group)
    }

    /// Parses one node reference and returns its dense node index.
    fn parse_node(
        &mut self,
        node_indexes: &mut HashMap<String, usize>,
        nodes: &mut Vec<MermaidNode>,
    ) -> Option<usize> {
        self.skip_whitespace();
        let identifier_length = self
            .rest
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        if identifier_length == 0 {
            return None;
        }

        let (identifier, remaining) = self.rest.split_at(identifier_length);
        self.rest = remaining;
        let shape_parse = self.parse_node_shape()?;
        self.skip_class_annotation();

        let node_index = if let Some(existing_index) = node_indexes.get(identifier) {
            *existing_index
        } else {
            if nodes.len() >= MAX_NODE_COUNT {
                return None;
            }

            nodes.push(MermaidNode {
                is_hidden: false,
                is_visible: true,
                label: identifier.to_string(),
                shape: NodeShape::Rectangle,
            });
            node_indexes.insert(identifier.to_string(), nodes.len() - 1);

            nodes.len() - 1
        };

        if let NodeShapeParse::Labeled(shape, label) = shape_parse {
            nodes[node_index].label = label;
            nodes[node_index].shape = shape;
        }

        Some(node_index)
    }

    /// Parses an optional bracketed node label, returning `None` only for an
    /// unterminated shape delimiter.
    ///
    /// Every mermaid shape maps onto the rectangle or rounded outline the
    /// canvas can draw; longer delimiters come first so composite shapes such
    /// as stadiums and cylinders win over their plain prefixes.
    fn parse_node_shape(&mut self) -> Option<NodeShapeParse> {
        let delimiters: [(&str, &str, NodeShape); 10] = [
            ("(((", ")))", NodeShape::Rounded),
            ("((", "))", NodeShape::Rounded),
            ("([", "])", NodeShape::Rounded),
            ("[[", "]]", NodeShape::Rectangle),
            ("[(", ")]", NodeShape::Rounded),
            ("{{", "}}", NodeShape::Rectangle),
            ("(", ")", NodeShape::Rounded),
            ("[", "]", NodeShape::Rectangle),
            ("{", "}", NodeShape::Rectangle),
            (">", "]", NodeShape::Rectangle),
        ];

        for (open_delimiter, close_delimiter, shape) in delimiters {
            let Some(after_open) = self.rest.strip_prefix(open_delimiter) else {
                continue;
            };
            let close_index = after_open.find(close_delimiter)?;
            let label = normalized_mermaid_label(&after_open[..close_index]).to_string();
            self.rest = &after_open[close_index + close_delimiter.len()..];

            return Some(NodeShapeParse::Labeled(shape, label));
        }

        Some(NodeShapeParse::Bare)
    }

    /// Parses one edge operator, returning source/target arrows, visibility,
    /// and an optional label.
    fn parse_edge_operator(&mut self) -> Option<(bool, bool, bool, Option<String>)> {
        self.skip_whitespace();

        if let InlineLabelParse::Present { has_arrow, label } =
            self.parse_inline_label_operator()?
        {
            let label = match label {
                Some(label) => Some(label),
                None => self.parse_edge_label_suffix()?.into_label(),
            };

            return Some((false, has_arrow, true, label));
        }

        let (has_source_arrow, has_arrow, is_visible) = self.parse_plain_edge_operator()?;
        let label = self.parse_edge_label_suffix()?.into_label();

        Some((has_source_arrow, has_arrow, is_visible, label))
    }

    /// Parses one plain edge operator as an end marker, a run of at least two
    /// line characters (`-`, `=`, `.`, `~`), and an optional arrow head.
    ///
    /// Circle (`o`) and cross (`x`) ends render as plain line ends. A run made
    /// entirely of tildes is an invisible layout link. Returns source and
    /// target arrow presence plus visibility, leaving the cursor untouched
    /// when no operator starts here.
    fn parse_plain_edge_operator(&mut self) -> Option<(bool, bool, bool)> {
        let operator_bytes = self.rest.as_bytes();
        let mut operator_length = 0;
        let mut has_source_arrow = false;
        let mut has_target_arrow = false;

        if let Some(source_end) = operator_bytes.first()
            && matches!(source_end, b'<' | b'o' | b'x')
        {
            operator_length = 1;
            has_source_arrow = *source_end == b'<';
        }

        let run_start = operator_length;
        while matches!(
            operator_bytes.get(operator_length),
            Some(b'-' | b'=' | b'.' | b'~')
        ) {
            operator_length += 1;
        }
        if operator_length - run_start < 2 {
            return None;
        }
        let is_visible = operator_bytes[run_start..operator_length]
            .iter()
            .any(|character| *character != b'~');

        match operator_bytes.get(operator_length) {
            Some(b'>') => {
                has_target_arrow = true;
                operator_length += 1;
            }
            Some(b'o' | b'x') => {
                operator_length += 1;
            }
            _ => {}
        }
        self.rest = &self.rest[operator_length..];

        Some((has_source_arrow, has_target_arrow, is_visible))
    }

    /// Parses an optional `|label|` suffix after a plain edge operator.
    ///
    /// Returns `EdgeLabel::Absent` when no suffix opens here, and `None` for an
    /// unterminated label delimiter or an unrenderable label.
    fn parse_edge_label_suffix(&mut self) -> Option<EdgeLabel> {
        let Some(after_pipe) = self.rest.strip_prefix('|') else {
            return Some(EdgeLabel::Absent);
        };
        let close_index = after_pipe.find('|')?;
        let label = parsed_edge_label(&after_pipe[..close_index])?;
        self.rest = &after_pipe[close_index + 1..];

        Some(label)
    }

    /// Parses the inline edge-label operator forms `A -- label --> B`,
    /// `A -.label.-> B`, and `A ==label==> B`, plus their arrowless `---`,
    /// `.-`, and `===` endings.
    ///
    /// A label may not start with an operator character, so longer plain
    /// operators such as `---`, `-.->`, and `====>` stay out of this form.
    /// Leaves the cursor untouched and returns `InlineLabelParse::Absent` when
    /// the current position does not open such a form, so plain operators still
    /// parse through `parse_plain_edge_operator` afterwards. Returns `None`
    /// when the form opens but carries an unrenderable label.
    fn parse_inline_label_operator(&mut self) -> Option<InlineLabelParse> {
        let label_operators: [(&str, &str, &str); 3] = [
            ("--", "-->", "---"),
            ("-.", ".->", ".-"),
            ("==", "==>", "==="),
        ];
        for (open_operator, arrow_ending, line_ending) in label_operators {
            let Some(after_open) = self.rest.strip_prefix(open_operator) else {
                continue;
            };
            if matches!(
                after_open.as_bytes().first(),
                Some(b'-' | b'=' | b'.' | b'~' | b'>')
            ) {
                continue;
            }
            let (label_text, has_arrow, after_operator) =
                if let Some(arrow_index) = after_open.find(arrow_ending) {
                    (
                        &after_open[..arrow_index],
                        true,
                        &after_open[arrow_index + arrow_ending.len()..],
                    )
                } else if let Some(line_index) = after_open.find(line_ending) {
                    (
                        &after_open[..line_index],
                        false,
                        &after_open[line_index + line_ending.len()..],
                    )
                } else {
                    continue;
                };
            self.rest = after_operator;
            let label = parsed_edge_label(label_text)?.into_label();

            return Some(InlineLabelParse::Present { has_arrow, label });
        }

        Some(InlineLabelParse::Absent)
    }

    /// Advances the cursor past an optional `:::class` styling annotation.
    fn skip_class_annotation(&mut self) {
        let Some(after_marker) = self.rest.strip_prefix(":::") else {
            return;
        };

        let class_length = after_marker
            .chars()
            .take_while(|character| {
                character.is_alphanumeric() || *character == '_' || *character == '-'
            })
            .map(char::len_utf8)
            .sum::<usize>();
        self.rest = &after_marker[class_length..];
    }

    /// Advances the cursor past leading whitespace.
    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }
}

/// Normalizes Mermaid label text to the single-line subset this renderer
/// supports.
fn normalized_mermaid_label(label_text: &str) -> &str {
    first_mermaid_label_line(label_text)
        .trim()
        .trim_matches('"')
        .trim()
}

/// Returns the first line from Mermaid's common HTML line-break label syntax.
fn first_mermaid_label_line(label_text: &str) -> &str {
    for delimiter in ["<br/>", "<br />", "<br>"] {
        if let Some((first_line, _)) = label_text.split_once(delimiter) {
            return first_line;
        }
    }

    label_text
}

/// Returns whether an identifier uses the preview's supported token syntax.
fn is_renderable_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '-')
}

/// Node-to-layer assignment produced by longest-path layering.
struct GraphLayout {
    layer_members: Vec<Vec<usize>>,
    node_layers: Vec<usize>,
}

/// Draws a compact two-node left-right feedback graph.
///
/// The layered graph renderer intentionally rejects cycles. This preview keeps
/// the common `A --> B` / `B --> A` shape useful in chat by placing both nodes
/// once and rendering the reciprocal arrows underneath them.
fn draw_left_right_feedback_graph(graph: &MermaidGraph) -> Option<MermaidDiagram> {
    if !is_left_right_feedback_graph(graph) {
        return None;
    }

    let node_widths = left_right_node_widths(graph);
    let label_width = graph
        .edges
        .iter()
        .filter_map(|edge| edge.label.as_deref())
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0);
    let gap_width = label_width.max(LAYER_GAP_COLUMNS * 2 + 4);
    let right_column = node_widths[0] + gap_width;
    let canvas_width = right_column + node_widths[1];
    let canvas_height = NODE_BOX_HEIGHT + graph.edges.len() * 2;
    let left_center = node_widths[0] / 2;
    let right_center = right_column + node_widths[1] / 2;
    let mut canvas = Canvas::new(canvas_width, canvas_height);

    draw_node_box(&mut canvas, &graph.nodes[0], 0, 0, node_widths[0]);
    draw_node_box(
        &mut canvas,
        &graph.nodes[1],
        right_column,
        0,
        node_widths[1],
    );
    for (edge_index, edge) in graph.edges.iter().enumerate() {
        if !edge.is_visible {
            continue;
        }

        let label_row = NODE_BOX_HEIGHT + edge_index * 2;
        let arrow_row = label_row + 1;
        draw_feedback_edge_label(&mut canvas, edge, left_center, right_center, label_row);
        draw_feedback_edge_arrow(&mut canvas, edge, left_center, right_center, arrow_row);
    }

    Some(canvas.into_diagram())
}

/// Returns whether a graph is the two-node reciprocal shape.
fn is_left_right_feedback_graph(graph: &MermaidGraph) -> bool {
    if !matches!(graph.direction, FlowDirection::LeftRight)
        || graph.nodes.len() != 2
        || graph.edges.len() != 2
    {
        return false;
    }

    graph.edges.iter().all(|edge| {
        (edge.from_index == 0 && edge.to_index == 1) || (edge.from_index == 1 && edge.to_index == 0)
    }) && graph
        .edges
        .iter()
        .any(|edge| edge.from_index == 0 && edge.to_index == 1)
        && graph
            .edges
            .iter()
            .any(|edge| edge.from_index == 1 && edge.to_index == 0)
}

/// Writes one feedback edge label between the node centers when it fits.
fn draw_feedback_edge_label(
    canvas: &mut Canvas,
    edge: &MermaidEdge,
    left_center: usize,
    right_center: usize,
    row: usize,
) {
    let Some(label) = &edge.label else {
        return;
    };

    let label_width = UnicodeWidthStr::width(label.as_str());
    let run_width = right_center.saturating_sub(left_center + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_center + 1 + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, row, label);
}

/// Draws one horizontal feedback arrow between the node centers.
fn draw_feedback_edge_arrow(
    canvas: &mut Canvas,
    edge: &MermaidEdge,
    left_center: usize,
    right_center: usize,
    row: usize,
) {
    for column in left_center..=right_center {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if edge.has_source_arrow {
        if edge.from_index == 1 {
            canvas.put_arrow(right_center, row, '▶');
        } else {
            canvas.put_arrow(left_center, row, '◀');
        }
    }

    if edge.has_arrow {
        if edge.to_index == 1 {
            canvas.put_arrow(right_center, row, '▶');
        } else {
            canvas.put_arrow(left_center, row, '◀');
        }
    }
}

/// Draws a simple sequence diagram with participant boxes and message arrows.
fn draw_sequence_diagram(diagram: &SequenceDiagram) -> MermaidDiagram {
    let gap_columns = sequence_gap_columns(diagram);
    let participant_widths: Vec<usize> = diagram
        .participants
        .iter()
        .map(|participant| UnicodeWidthStr::width(participant.label.as_str()) + 4)
        .collect();
    let mut participant_columns = Vec::with_capacity(diagram.participants.len());
    let mut next_column = 0;
    for participant_width in &participant_widths {
        participant_columns.push(next_column);
        next_column += *participant_width + gap_columns;
    }
    let lifeline_columns: Vec<usize> = participant_columns
        .iter()
        .zip(&participant_widths)
        .map(|(left_column, width)| left_column + width / 2)
        .collect();
    let participant_width = next_column.saturating_sub(gap_columns);
    let self_message_width = sequence_self_message_width(diagram, &lifeline_columns);
    let canvas_width = participant_width.max(self_message_width).max(1);
    let canvas_height = NODE_BOX_HEIGHT + 1 + diagram.messages.len() * 2;
    let mut canvas = Canvas::new(canvas_width, canvas_height);

    for column in &lifeline_columns {
        for row in NODE_BOX_HEIGHT..canvas_height {
            canvas.merge_connector(*column, row, CONNECT_UP | CONNECT_DOWN);
        }
    }

    for (participant_index, participant) in diagram.participants.iter().enumerate() {
        draw_node_box(
            &mut canvas,
            &MermaidNode {
                is_hidden: false,
                is_visible: true,
                label: participant.label.clone(),
                shape: NodeShape::Rectangle,
            },
            participant_columns[participant_index],
            0,
            participant_widths[participant_index],
        );
    }

    for (message_index, message) in diagram.messages.iter().enumerate() {
        let row = NODE_BOX_HEIGHT + 1 + message_index * 2;
        draw_sequence_message(&mut canvas, &lifeline_columns, message, row);
    }

    canvas.into_diagram()
}

/// Returns the lifeline gap sized to the widest message label.
///
/// Any message label fits between adjacent lifelines at this gap, while short
/// labels keep the whole diagram narrow enough for typical chat pane widths.
fn sequence_gap_columns(diagram: &SequenceDiagram) -> usize {
    let widest_label = diagram
        .messages
        .iter()
        .map(|message| UnicodeWidthStr::width(message.label.as_str()))
        .max()
        .unwrap_or(0);

    (widest_label + 2).clamp(SEQUENCE_MIN_GAP_COLUMNS, SEQUENCE_MAX_GAP_COLUMNS)
}

/// Returns the canvas width needed by self-message loops and their labels.
fn sequence_self_message_width(diagram: &SequenceDiagram, lifeline_columns: &[usize]) -> usize {
    diagram
        .messages
        .iter()
        .filter(|message| message.from_index == message.to_index)
        .map(|message| {
            let lifeline_column = lifeline_columns[message.from_index];
            let label_width = UnicodeWidthStr::width(message.label.as_str());
            let label_end = sequence_self_label_column(lifeline_column, label_width) + label_width;

            label_end.max(lifeline_column + SEQUENCE_SELF_LOOP_COLUMNS + 1)
        })
        .max()
        .unwrap_or(0)
}

/// Draws one sequence message as a horizontal arrow between lifelines.
fn draw_sequence_message(
    canvas: &mut Canvas,
    lifeline_columns: &[usize],
    message: &SequenceMessage,
    row: usize,
) {
    let source_column = lifeline_columns[message.from_index];
    let target_column = lifeline_columns[message.to_index];
    if source_column == target_column {
        draw_sequence_self_message(canvas, source_column, &message.label, row);

        return;
    }

    let left_column = source_column.min(target_column);
    let right_column = source_column.max(target_column);

    for column in left_column..=right_column {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if target_column >= source_column {
        canvas.put_arrow(target_column, row, '▶');
    } else {
        canvas.put_arrow(target_column, row, '◀');
    }

    draw_sequence_message_label(canvas, left_column, right_column, row, &message.label);
}

/// Draws one self-message as a rightward loop returning to its lifeline.
///
/// The loop occupies the message's label and arrow rows; the label sits left
/// of the lifeline when it fits there and right of the loop otherwise.
fn draw_sequence_self_message(
    canvas: &mut Canvas,
    lifeline_column: usize,
    label: &str,
    arrow_row: usize,
) {
    let loop_column = lifeline_column + SEQUENCE_SELF_LOOP_COLUMNS;
    let label_row = arrow_row.saturating_sub(1);

    canvas.merge_connector(lifeline_column, label_row, CONNECT_RIGHT);
    for column in lifeline_column + 1..loop_column {
        canvas.merge_connector(column, label_row, CONNECT_LEFT | CONNECT_RIGHT);
        canvas.merge_connector(column, arrow_row, CONNECT_LEFT | CONNECT_RIGHT);
    }
    canvas.merge_connector(loop_column, label_row, CONNECT_DOWN | CONNECT_LEFT);
    canvas.merge_connector(loop_column, arrow_row, CONNECT_UP | CONNECT_LEFT);
    canvas.put_arrow(lifeline_column, arrow_row, '◀');

    let start_column = sequence_self_label_column(lifeline_column, UnicodeWidthStr::width(label));
    for (character_index, character) in label.chars().enumerate() {
        canvas.put_label(start_column + character_index, label_row, character);
    }
}

/// Returns the start column for one self-message label: left of the lifeline
/// when the label fits there, otherwise right of the loop.
fn sequence_self_label_column(lifeline_column: usize, label_width: usize) -> usize {
    if label_width < lifeline_column {
        return lifeline_column - 1 - label_width;
    }

    lifeline_column + SEQUENCE_SELF_LOOP_COLUMNS + 2
}

/// Writes a sequence message label above its arrow run when it fits.
fn draw_sequence_message_label(
    canvas: &mut Canvas,
    left_column: usize,
    right_column: usize,
    arrow_row: usize,
    label: &str,
) {
    let label_width = UnicodeWidthStr::width(label);
    let run_width = right_column.saturating_sub(left_column + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_column + 1 + (run_width - label_width) / 2;
    let label_row = arrow_row.saturating_sub(1);
    for (character_index, character) in label.chars().enumerate() {
        canvas.put_label(start_column + character_index, label_row, character);
    }
}

/// Assigns nodes to layers via longest-path topological layering.
///
/// Returns `None` for cyclic graphs or for edges that still skip layers after
/// long-edge expansion.
fn layout_layers(graph: &MermaidGraph) -> Option<GraphLayout> {
    let node_layers = assign_node_layers(graph)?;

    for edge in &graph.edges {
        if node_layers[edge.to_index] != node_layers[edge.from_index] + 1 {
            return None;
        }
    }

    let layer_count = node_layers
        .iter()
        .max()
        .map_or(0, |max_layer| max_layer + 1);
    let mut layer_members: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (node_index, layer) in node_layers.iter().enumerate() {
        layer_members[*layer].push(node_index);
    }

    Some(GraphLayout {
        layer_members,
        node_layers,
    })
}

/// Assigns every node to a topological layer using longest-path layering.
fn assign_node_layers(graph: &MermaidGraph) -> Option<Vec<usize>> {
    let node_count = graph.nodes.len();
    let mut indegrees = vec![0_usize; node_count];
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); node_count];

    for edge in &graph.edges {
        if edge.from_index == edge.to_index {
            return None;
        }

        indegrees[edge.to_index] += 1;
        outgoing[edge.from_index].push(edge.to_index);
    }

    let mut node_layers = vec![0_usize; node_count];
    let mut ready: VecDeque<usize> = (0..node_count)
        .filter(|node_index| indegrees[*node_index] == 0)
        .collect();
    let mut processed_count = 0;

    while let Some(node_index) = ready.pop_front() {
        processed_count += 1;
        for target_index in &outgoing[node_index] {
            node_layers[*target_index] =
                node_layers[*target_index].max(node_layers[node_index] + 1);
            indegrees[*target_index] -= 1;
            if indegrees[*target_index] == 0 {
                ready.push_back(*target_index);
            }
        }
    }

    if processed_count != node_count {
        return None;
    }

    Some(node_layers)
}

/// Routed geometry for one top-down edge.
struct TopDownEdgePath {
    arrow_row: usize,
    has_arrow: bool,
    has_source_arrow: bool,
    is_visible: bool,
    label: Option<String>,
    region_top: usize,
    source_column: usize,
    source_marker: Option<char>,
    target_column: usize,
    target_marker: Option<char>,
    track_row: usize,
}

/// Draws a top-down diagram with forward edges as vertical elbows and feedback
/// edges as independent return rows beneath the layered graph.
fn draw_top_down(
    graph: &MermaidGraph,
    layout: &GraphLayout,
    feedback_edges: &[MermaidEdge],
) -> MermaidDiagram {
    let box_widths = top_down_node_widths(graph);
    let layer_widths = top_down_layer_widths(layout, &box_widths);
    let canvas_width = layer_widths.iter().copied().max().unwrap_or(1);

    let region_edges = graph_region_edges(graph, layout);
    let (layer_top_rows, canvas_height) = top_down_layer_top_rows(layout, &region_edges);
    let mut canvas = Canvas::new(canvas_width, canvas_height);
    let box_columns = draw_top_down_nodes(
        &mut canvas,
        graph,
        layout,
        &box_widths,
        &layer_widths,
        &layer_top_rows,
        canvas_width,
    );
    let edge_paths = top_down_edge_paths(
        graph,
        &region_edges,
        &layer_top_rows,
        &box_columns,
        &box_widths,
    );

    draw_top_down_edge_paths(&mut canvas, &edge_paths);
    let mut diagram = canvas.into_diagram();
    append_feedback_edge_lines(&mut diagram, graph, feedback_edges);

    diagram
}

/// Appends each visible feedback edge on its own row so unrelated cycles
/// cannot share tracks or appear connected.
fn append_feedback_edge_lines(
    diagram: &mut MermaidDiagram,
    graph: &MermaidGraph,
    feedback_edges: &[MermaidEdge],
) {
    for edge in feedback_edges.iter().filter(|edge| edge.is_visible) {
        let line = feedback_edge_line(graph, edge);
        diagram.width = diagram.width.max(line.width());
        diagram.lines.push(line);
    }
}

/// Formats one feedback edge as an independent source-to-target return row.
fn feedback_edge_line(graph: &MermaidGraph, edge: &MermaidEdge) -> Line<'static> {
    let source_end =
        edge.source_marker
            .unwrap_or(if edge.has_source_arrow { '◀' } else { '─' });
    let target_end = edge
        .target_marker
        .unwrap_or(if edge.has_arrow { '▶' } else { '─' });
    let label = edge.label.as_deref().map_or_else(
        || Span::raw(""),
        |label| Span::styled(format!(" {label} "), label_style()),
    );

    Line::from(vec![
        Span::styled(graph.nodes[edge.from_index].label.clone(), label_style()),
        Span::styled(format!(" {source_end}─"), structure_style()),
        label,
        Span::styled(format!("─{target_end} "), structure_style()),
        Span::styled(graph.nodes[edge.to_index].label.clone(), label_style()),
    ])
}

/// Returns the rendered width of each top-down graph node.
fn top_down_node_widths(graph: &MermaidGraph) -> Vec<usize> {
    graph.nodes.iter().map(top_down_node_width).collect()
}

/// Returns the width reserved for one top-down graph node.
fn top_down_node_width(node: &MermaidNode) -> usize {
    if node.is_hidden {
        return 1;
    }

    UnicodeWidthStr::width(node.label.as_str()) + 4
}

/// Returns the rendered width of each top-down graph layer.
fn top_down_layer_widths(layout: &GraphLayout, box_widths: &[usize]) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members
                .iter()
                .map(|node_index| box_widths[*node_index])
                .sum::<usize>()
                + LAYER_GAP_COLUMNS * members.len().saturating_sub(1)
        })
        .collect()
}

/// Groups graph edges by the routing region after their source layer.
fn graph_region_edges<'graph>(
    graph: &'graph MermaidGraph,
    layout: &GraphLayout,
) -> Vec<Vec<&'graph MermaidEdge>> {
    let mut region_edges = vec![Vec::new(); layout.layer_members.len().saturating_sub(1)];
    for edge in &graph.edges {
        region_edges[layout.node_layers[edge.from_index]].push(edge);
    }

    region_edges
}

/// Returns top row coordinates for top-down layers and the canvas height.
fn top_down_layer_top_rows(
    layout: &GraphLayout,
    region_edges: &[Vec<&MermaidEdge>],
) -> (Vec<usize>, usize) {
    let layer_count = layout.layer_members.len();
    let mut layer_top_rows = Vec::with_capacity(layer_count);
    let mut next_row = 0;
    for region_edges_after_layer in region_edges.iter().take(layer_count.saturating_sub(1)) {
        layer_top_rows.push(next_row);
        next_row += NODE_BOX_HEIGHT;
        next_row += region_edges_after_layer.len() + 2;
    }
    if layer_count > 0 {
        layer_top_rows.push(next_row);
        next_row += NODE_BOX_HEIGHT;
    }

    (layer_top_rows, next_row)
}

/// Draws top-down nodes and returns the left column of each node box.
fn draw_top_down_nodes(
    canvas: &mut Canvas,
    graph: &MermaidGraph,
    layout: &GraphLayout,
    box_widths: &[usize],
    layer_widths: &[usize],
    layer_top_rows: &[usize],
    canvas_width: usize,
) -> Vec<usize> {
    let mut box_columns = vec![0_usize; graph.nodes.len()];
    for (layer_index, members) in layout.layer_members.iter().enumerate() {
        let mut cursor_column = (canvas_width - layer_widths[layer_index]) / 2;
        for node_index in members {
            box_columns[*node_index] = cursor_column;
            if graph.nodes[*node_index].is_hidden && graph.nodes[*node_index].is_visible {
                draw_top_down_hidden_node(canvas, cursor_column, layer_top_rows[layer_index]);
            } else if graph.nodes[*node_index].is_visible {
                draw_node_box(
                    canvas,
                    &graph.nodes[*node_index],
                    cursor_column,
                    layer_top_rows[layer_index],
                    box_widths[*node_index],
                );
            }
            cursor_column += box_widths[*node_index] + LAYER_GAP_COLUMNS;
        }
    }

    box_columns
}

/// Builds routed top-down edge geometry for every graph edge.
fn top_down_edge_paths(
    graph: &MermaidGraph,
    region_edges: &[Vec<&MermaidEdge>],
    layer_top_rows: &[usize],
    box_columns: &[usize],
    box_widths: &[usize],
) -> Vec<TopDownEdgePath> {
    let mut edge_paths = Vec::with_capacity(graph.edges.len());
    for (layer_index, edges) in region_edges.iter().enumerate() {
        let region_top = layer_top_rows[layer_index] + NODE_BOX_HEIGHT;
        for (track_index, edge) in edges.iter().enumerate() {
            edge_paths.push(TopDownEdgePath {
                arrow_row: region_top + edges.len() + 1,
                has_arrow: edge.has_arrow,
                has_source_arrow: edge.has_source_arrow,
                is_visible: edge.is_visible,
                label: edge.label.clone(),
                region_top,
                source_column: box_columns[edge.from_index] + box_widths[edge.from_index] / 2,
                source_marker: edge.source_marker,
                target_column: box_columns[edge.to_index] + box_widths[edge.to_index] / 2,
                target_marker: edge.target_marker,
                track_row: region_top + 1 + track_index,
            });
        }
    }

    edge_paths
}

/// Draws routed top-down edge connectors, labels, and markers.
fn draw_top_down_edge_paths(canvas: &mut Canvas, edge_paths: &[TopDownEdgePath]) {
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        draw_top_down_edge_connectors(canvas, edge_path);
    }
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        draw_top_down_edge_label(canvas, edge_path);
    }
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        canvas.try_put_marker(
            edge_path.source_column,
            edge_path.region_top,
            edge_path.source_marker,
        );
        canvas.try_put_marker(
            edge_path.target_column,
            edge_path.arrow_row,
            edge_path.target_marker,
        );
    }
}

/// Draws the through-connector for a hidden top-down routing node.
fn draw_top_down_hidden_node(canvas: &mut Canvas, column: usize, top_row: usize) {
    for row in top_row..top_row + NODE_BOX_HEIGHT {
        canvas.merge_connector(column, row, CONNECT_UP | CONNECT_DOWN);
    }
}

/// Draws the connector cells for one top-down edge.
fn draw_top_down_edge_connectors(canvas: &mut Canvas, edge_path: &TopDownEdgePath) {
    for row in edge_path.region_top..edge_path.track_row {
        canvas.merge_connector(edge_path.source_column, row, CONNECT_UP | CONNECT_DOWN);
    }

    if edge_path.source_column == edge_path.target_column {
        for row in edge_path.track_row..edge_path.arrow_row {
            canvas.merge_connector(edge_path.source_column, row, CONNECT_UP | CONNECT_DOWN);
        }
    } else {
        let left_column = edge_path.source_column.min(edge_path.target_column);
        let right_column = edge_path.source_column.max(edge_path.target_column);
        let (source_mask, target_mask) = if edge_path.target_column > edge_path.source_column {
            (CONNECT_UP | CONNECT_RIGHT, CONNECT_DOWN | CONNECT_LEFT)
        } else {
            (CONNECT_UP | CONNECT_LEFT, CONNECT_DOWN | CONNECT_RIGHT)
        };

        canvas.merge_connector(edge_path.source_column, edge_path.track_row, source_mask);
        canvas.merge_connector(edge_path.target_column, edge_path.track_row, target_mask);
        for column in left_column + 1..right_column {
            canvas.merge_connector(column, edge_path.track_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
        for row in edge_path.track_row + 1..edge_path.arrow_row {
            canvas.merge_connector(edge_path.target_column, row, CONNECT_UP | CONNECT_DOWN);
        }
    }

    if edge_path.has_arrow {
        canvas.put_arrow(edge_path.target_column, edge_path.arrow_row, '▼');
    } else {
        canvas.merge_connector(
            edge_path.target_column,
            edge_path.arrow_row,
            CONNECT_UP | CONNECT_DOWN,
        );
    }
    if edge_path.has_source_arrow {
        canvas.put_arrow(edge_path.source_column, edge_path.region_top, '▲');
    }
}

/// Writes one top-down edge label onto its horizontal track when it fits.
fn draw_top_down_edge_label(canvas: &mut Canvas, edge_path: &TopDownEdgePath) {
    let Some(label) = &edge_path.label else {
        return;
    };
    let label_width = UnicodeWidthStr::width(label.as_str());

    if edge_path.source_column == edge_path.target_column {
        canvas.try_write_label(edge_path.source_column + 2, edge_path.track_row, label);

        return;
    }

    let left_column = edge_path.source_column.min(edge_path.target_column);
    let right_column = edge_path.source_column.max(edge_path.target_column);
    let run_width = right_column.saturating_sub(left_column + 1);
    if label_width > run_width {
        return;
    }

    let start_column = left_column + 1 + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, edge_path.track_row, label);
}

/// Routed geometry for one left-right edge.
struct LeftRightEdgePath {
    arrow_column: usize,
    has_arrow: bool,
    has_source_arrow: bool,
    is_visible: bool,
    label: Option<String>,
    region_left: usize,
    source_marker: Option<char>,
    source_row: usize,
    target_marker: Option<char>,
    target_row: usize,
    track_column: usize,
}

/// Draws a left-right diagram: layers as columns, edges as horizontal elbows.
fn draw_left_right(graph: &MermaidGraph, layout: &GraphLayout) -> MermaidDiagram {
    let node_widths = left_right_node_widths(graph);
    let layer_widths = left_right_layer_widths(layout, &node_widths);
    let layer_heights = left_right_layer_heights(layout);
    let canvas_height = layer_heights.iter().copied().max().unwrap_or(1);

    let region_edges = graph_region_edges(graph, layout);
    let (layer_left_columns, canvas_width) =
        left_right_layer_left_columns(layout, &region_edges, &layer_widths);
    let mut canvas = Canvas::new(canvas_width, canvas_height);
    let box_rows = draw_left_right_nodes(
        &mut canvas,
        graph,
        layout,
        &layer_widths,
        &layer_heights,
        &layer_left_columns,
        canvas_height,
    );
    let edge_paths = left_right_edge_paths(
        graph,
        &region_edges,
        &layer_left_columns,
        &layer_widths,
        &box_rows,
    );

    draw_left_right_edge_paths(&mut canvas, &edge_paths);

    canvas.into_diagram()
}

/// Returns the rendered width of each left-right graph node.
fn left_right_node_widths(graph: &MermaidGraph) -> Vec<usize> {
    graph.nodes.iter().map(left_right_node_width).collect()
}

/// Returns the width reserved for one left-right graph node.
fn left_right_node_width(node: &MermaidNode) -> usize {
    if node.is_hidden {
        return 1;
    }

    UnicodeWidthStr::width(node.label.as_str()) + 4
}

/// Returns the rendered width of each left-right graph layer.
fn left_right_layer_widths(layout: &GraphLayout, node_widths: &[usize]) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members
                .iter()
                .map(|node_index| node_widths[*node_index])
                .max()
                .unwrap_or(1)
        })
        .collect()
}

/// Returns the rendered height of each left-right graph layer.
fn left_right_layer_heights(layout: &GraphLayout) -> Vec<usize> {
    layout
        .layer_members
        .iter()
        .map(|members| {
            members.len() * NODE_BOX_HEIGHT + LAYER_GAP_ROWS * members.len().saturating_sub(1)
        })
        .collect()
}

/// Returns left column coordinates for left-right layers and the canvas width.
fn left_right_layer_left_columns(
    layout: &GraphLayout,
    region_edges: &[Vec<&MermaidEdge>],
    layer_widths: &[usize],
) -> (Vec<usize>, usize) {
    let layer_count = layout.layer_members.len();
    let mut layer_left_columns = Vec::with_capacity(layer_count);
    let mut next_column = 0;
    for layer_index in 0..layer_count {
        layer_left_columns.push(next_column);
        next_column += layer_widths[layer_index];
        if layer_index + 1 < layer_count {
            next_column += region_edges[layer_index].len() + 2;
        }
    }

    (layer_left_columns, next_column)
}

/// Draws left-right nodes and returns the top row of each node box.
fn draw_left_right_nodes(
    canvas: &mut Canvas,
    graph: &MermaidGraph,
    layout: &GraphLayout,
    layer_widths: &[usize],
    layer_heights: &[usize],
    layer_left_columns: &[usize],
    canvas_height: usize,
) -> Vec<usize> {
    let mut box_rows = vec![0_usize; graph.nodes.len()];
    for (layer_index, members) in layout.layer_members.iter().enumerate() {
        let mut cursor_row = (canvas_height - layer_heights[layer_index]) / 2;
        for node_index in members {
            box_rows[*node_index] = cursor_row;
            if graph.nodes[*node_index].is_hidden && graph.nodes[*node_index].is_visible {
                draw_left_right_hidden_node(
                    canvas,
                    layer_left_columns[layer_index],
                    cursor_row + 1,
                    layer_widths[layer_index],
                );
            } else if graph.nodes[*node_index].is_visible {
                draw_node_box(
                    canvas,
                    &graph.nodes[*node_index],
                    layer_left_columns[layer_index],
                    cursor_row,
                    layer_widths[layer_index],
                );
            }
            cursor_row += NODE_BOX_HEIGHT + LAYER_GAP_ROWS;
        }
    }

    box_rows
}

/// Builds routed left-right edge geometry for every graph edge.
fn left_right_edge_paths(
    graph: &MermaidGraph,
    region_edges: &[Vec<&MermaidEdge>],
    layer_left_columns: &[usize],
    layer_widths: &[usize],
    box_rows: &[usize],
) -> Vec<LeftRightEdgePath> {
    let mut edge_paths = Vec::with_capacity(graph.edges.len());
    for (layer_index, edges) in region_edges.iter().enumerate() {
        let region_left = layer_left_columns[layer_index] + layer_widths[layer_index];
        for (track_index, edge) in edges.iter().enumerate() {
            edge_paths.push(LeftRightEdgePath {
                arrow_column: region_left + edges.len() + 1,
                has_arrow: edge.has_arrow,
                has_source_arrow: edge.has_source_arrow,
                is_visible: edge.is_visible,
                label: edge.label.clone(),
                region_left,
                source_marker: edge.source_marker,
                source_row: box_rows[edge.from_index] + 1,
                target_marker: edge.target_marker,
                target_row: box_rows[edge.to_index] + 1,
                track_column: region_left + 1 + track_index,
            });
        }
    }

    edge_paths
}

/// Draws routed left-right edge connectors, labels, and markers.
fn draw_left_right_edge_paths(canvas: &mut Canvas, edge_paths: &[LeftRightEdgePath]) {
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        draw_left_right_edge_connectors(canvas, edge_path);
    }
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        draw_left_right_edge_label(canvas, edge_path);
    }
    for edge_path in edge_paths.iter().filter(|edge_path| edge_path.is_visible) {
        canvas.try_put_marker(
            edge_path.region_left,
            edge_path.source_row,
            edge_path.source_marker,
        );
        canvas.try_put_marker(
            edge_path.arrow_column,
            edge_path.target_row,
            edge_path.target_marker,
        );
    }
}

/// Draws the through-connector for a hidden left-right routing node.
fn draw_left_right_hidden_node(canvas: &mut Canvas, left_column: usize, row: usize, width: usize) {
    for column in left_column..left_column + width {
        canvas.merge_connector(column, row, CONNECT_LEFT | CONNECT_RIGHT);
    }
}

/// Draws the connector cells for one left-right edge.
fn draw_left_right_edge_connectors(canvas: &mut Canvas, edge_path: &LeftRightEdgePath) {
    for column in edge_path.region_left..edge_path.track_column {
        canvas.merge_connector(column, edge_path.source_row, CONNECT_LEFT | CONNECT_RIGHT);
    }

    if edge_path.source_row == edge_path.target_row {
        for column in edge_path.track_column..edge_path.arrow_column {
            canvas.merge_connector(column, edge_path.source_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
    } else {
        let top_row = edge_path.source_row.min(edge_path.target_row);
        let bottom_row = edge_path.source_row.max(edge_path.target_row);
        let (source_mask, target_mask) = if edge_path.target_row > edge_path.source_row {
            (CONNECT_LEFT | CONNECT_DOWN, CONNECT_UP | CONNECT_RIGHT)
        } else {
            (CONNECT_LEFT | CONNECT_UP, CONNECT_DOWN | CONNECT_RIGHT)
        };

        canvas.merge_connector(edge_path.track_column, edge_path.source_row, source_mask);
        canvas.merge_connector(edge_path.track_column, edge_path.target_row, target_mask);
        for row in top_row + 1..bottom_row {
            canvas.merge_connector(edge_path.track_column, row, CONNECT_UP | CONNECT_DOWN);
        }
        for column in edge_path.track_column + 1..edge_path.arrow_column {
            canvas.merge_connector(column, edge_path.target_row, CONNECT_LEFT | CONNECT_RIGHT);
        }
    }

    if edge_path.has_arrow {
        canvas.put_arrow(edge_path.arrow_column, edge_path.target_row, '▶');
    } else {
        canvas.merge_connector(
            edge_path.arrow_column,
            edge_path.target_row,
            CONNECT_LEFT | CONNECT_RIGHT,
        );
    }
    if edge_path.has_source_arrow {
        canvas.put_arrow(edge_path.region_left, edge_path.source_row, '◀');
    }
}

/// Writes one left-right edge label onto its entry run when it fits.
fn draw_left_right_edge_label(canvas: &mut Canvas, edge_path: &LeftRightEdgePath) {
    let Some(label) = &edge_path.label else {
        return;
    };
    let label_width = UnicodeWidthStr::width(label.as_str());
    let run_end = if edge_path.source_row == edge_path.target_row {
        edge_path.arrow_column
    } else {
        edge_path.track_column
    };
    let run_width = run_end.saturating_sub(edge_path.region_left);
    if label_width > run_width {
        return;
    }

    let start_column = edge_path.region_left + (run_width - label_width) / 2;
    canvas.try_write_label(start_column, edge_path.source_row, label);
}

/// Draws one node as a bordered box with a centered single-line label.
fn draw_node_box(
    canvas: &mut Canvas,
    node: &MermaidNode,
    left_column: usize,
    top_row: usize,
    box_width: usize,
) {
    let (top_left, top_right, bottom_left, bottom_right) = match node.shape {
        NodeShape::Rectangle => ('┌', '┐', '└', '┘'),
        NodeShape::Rounded => ('╭', '╮', '╰', '╯'),
    };
    let right_column = left_column + box_width - 1;
    let middle_row = top_row + 1;
    let bottom_row = top_row + 2;

    canvas.put_border(left_column, top_row, top_left);
    canvas.put_border(right_column, top_row, top_right);
    canvas.put_border(left_column, bottom_row, bottom_left);
    canvas.put_border(right_column, bottom_row, bottom_right);
    for column in left_column + 1..right_column {
        canvas.put_border(column, top_row, '─');
        canvas.put_border(column, bottom_row, '─');
    }
    canvas.put_border(left_column, middle_row, '│');
    canvas.put_border(right_column, middle_row, '│');

    let label_width = UnicodeWidthStr::width(node.label.as_str());
    let inner_width = box_width - 2;
    let label_column = left_column + 1 + (inner_width - label_width) / 2;
    for (character_index, character) in node.label.chars().enumerate() {
        canvas.put_label(label_column + character_index, middle_row, character);
    }
}

/// One drawable cell in the diagram canvas.
#[derive(Clone, Copy, PartialEq)]
enum CanvasCell {
    Arrow(char),
    Border(char),
    Connector(u8),
    Empty,
    Label(char),
}

/// Fixed-size character canvas that merges box-drawing connectors.
struct Canvas {
    cells: Vec<Vec<CanvasCell>>,
}

impl Canvas {
    /// Creates an empty canvas of the given cell dimensions.
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![CanvasCell::Empty; width]; height],
        }
    }

    /// Places one box-border glyph, overwriting any existing cell.
    fn put_border(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Border(character));
    }

    /// Places one label glyph, overwriting any existing cell.
    fn put_label(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Label(character));
    }

    /// Places one arrow-head glyph, overwriting any existing cell.
    fn put_arrow(&mut self, column: usize, row: usize, character: char) {
        self.put(column, row, CanvasCell::Arrow(character));
    }

    /// ORs one connector mask into a cell, leaving glyph cells untouched.
    fn merge_connector(&mut self, column: usize, row: usize, mask: u8) {
        let Some(cell) = self.cell_mut(column, row) else {
            return;
        };

        match cell {
            CanvasCell::Empty => *cell = CanvasCell::Connector(mask),
            CanvasCell::Connector(existing_mask) => *existing_mask |= mask,
            CanvasCell::Arrow(_) | CanvasCell::Border(_) | CanvasCell::Label(_) => {}
        }
    }

    /// Writes label text when every target cell is empty or a plain
    /// horizontal connector, keeping other edges' verticals intact.
    fn try_write_label(&mut self, start_column: usize, row: usize, label: &str) {
        let character_count = label.chars().count();
        let Some(row_cells) = self.cells.get(row) else {
            return;
        };
        if start_column + character_count > row_cells.len() {
            return;
        }

        let is_writable = row_cells[start_column..start_column + character_count].iter().all(|cell| {
            matches!(cell, CanvasCell::Empty)
                || matches!(cell, CanvasCell::Connector(mask) if *mask == CONNECT_LEFT | CONNECT_RIGHT)
        });
        if !is_writable {
            return;
        }

        for (character_index, character) in label.chars().enumerate() {
            self.put_label(start_column + character_index, row, character);
        }
    }

    /// Replaces one straight connector cell with a cardinality marker glyph.
    ///
    /// Junction cells and cells already holding glyphs are left untouched so
    /// shared edge tracks keep their box-drawing shape.
    fn try_put_marker(&mut self, column: usize, row: usize, marker: Option<char>) {
        let Some(marker) = marker else {
            return;
        };
        let Some(cell) = self.cell_mut(column, row) else {
            return;
        };

        let is_straight_connector = matches!(
            cell,
            CanvasCell::Connector(mask)
                if *mask == CONNECT_UP | CONNECT_DOWN || *mask == CONNECT_LEFT | CONNECT_RIGHT
        );
        if is_straight_connector {
            *cell = CanvasCell::Label(marker);
        }
    }

    /// Converts the canvas into styled lines plus the widest row width.
    fn into_diagram(self) -> MermaidDiagram {
        let mut lines = Vec::with_capacity(self.cells.len());
        let mut width = 0;

        for row_cells in &self.cells {
            let trimmed_length = row_cells
                .iter()
                .rposition(|cell| *cell != CanvasCell::Empty)
                .map_or(0, |last_index| last_index + 1);
            width = width.max(trimmed_length);

            let mut spans: Vec<Span<'static>> = Vec::new();
            for cell in &row_cells[..trimmed_length] {
                let (character, cell_style) = Self::cell_presentation(*cell);
                match spans.last_mut() {
                    Some(last_span) if last_span.style == cell_style => {
                        last_span.content.to_mut().push(character);
                    }
                    _ => spans.push(Span::styled(character.to_string(), cell_style)),
                }
            }

            lines.push(Line::from(spans));
        }

        MermaidDiagram { lines, width }
    }

    /// Returns the character and style used to paint one cell.
    fn cell_presentation(cell: CanvasCell) -> (char, Style) {
        match cell {
            CanvasCell::Empty => (' ', Style::default()),
            CanvasCell::Connector(mask) => (connector_character(mask), structure_style()),
            CanvasCell::Arrow(character) | CanvasCell::Border(character) => {
                (character, structure_style())
            }
            CanvasCell::Label(character) => (character, label_style()),
        }
    }

    /// Returns a mutable cell reference when the coordinates are in bounds.
    fn cell_mut(&mut self, column: usize, row: usize) -> Option<&mut CanvasCell> {
        self.cells.get_mut(row)?.get_mut(column)
    }

    /// Places one glyph cell, overwriting any existing content.
    fn put(&mut self, column: usize, row: usize, cell: CanvasCell) {
        if let Some(existing_cell) = self.cell_mut(column, row) {
            *existing_cell = cell;
        }
    }
}

/// Maps one connector direction mask to its box-drawing character.
fn connector_character(mask: u8) -> char {
    const UP: u8 = CONNECT_UP;
    const DOWN: u8 = CONNECT_DOWN;
    const LEFT: u8 = CONNECT_LEFT;
    const RIGHT: u8 = CONNECT_RIGHT;

    match mask {
        mask if mask == UP | DOWN | LEFT | RIGHT => '┼',
        mask if mask == UP | DOWN | LEFT => '┤',
        mask if mask == UP | DOWN | RIGHT => '├',
        mask if mask == UP | LEFT | RIGHT => '┴',
        mask if mask == DOWN | LEFT | RIGHT => '┬',
        mask if mask == UP | LEFT => '┘',
        mask if mask == UP | RIGHT => '└',
        mask if mask == DOWN | LEFT => '┐',
        mask if mask == DOWN | RIGHT => '┌',
        mask if mask & (LEFT | RIGHT) != 0 && mask & (UP | DOWN) == 0 => '─',
        _ => '│',
    }
}

/// Returns the style for diagram borders, connectors, and arrow heads.
fn structure_style() -> Style {
    Style::default().fg(style::palette::text())
}

/// Returns the style for node and edge label text.
fn label_style() -> Style {
    Style::default().fg(style::palette::text())
}

#[cfg(test)]
#[path = "mermaid_test.rs"]
mod tests;
