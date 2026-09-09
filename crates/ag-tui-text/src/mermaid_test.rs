use std::fmt::Write;

use super::*;

fn diagram_text(diagram: &MermaidDiagram) -> String {
    diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_render_mermaid_uses_text_color_for_structure() {
    // Arrange
    let source = "graph TD\n    A[Start] --> B[Finish]";

    // Act
    let diagram = render_mermaid(source).expect("chain should render");
    let structure_span = diagram
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains('┌'))
        .expect("structure span should render");

    // Assert
    assert_eq!(structure_span.style.fg, Some(style::palette::text()));
}

#[test]
fn test_render_mermaid_draws_top_down_chain() {
    // Arrange
    let source = "graph TD\n    A[Start] --> B[Finish]";

    // Act
    let diagram = render_mermaid(source).expect("chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains('┌'));
    assert!(text.contains('▼'));
    assert!(diagram.width > 0);
}

#[test]
fn test_render_mermaid_draws_branching_diamond() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A --> C\n    B --> D\n    C --> D";

    // Act
    let diagram = render_mermaid(source).expect("diamond should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 3);
}

#[test]
fn test_render_mermaid_draws_top_down_long_edge() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A[User starts session] --> B{Choose action}\n",
        "    B -->|Ask agent| C[Send prompt]\n",
        "    B -->|Review changes| D[Open diff view]\n",
        "    C --> E[Agent works in worktree]\n",
        "    E --> F[Run checks]\n",
        "    F --> G[Report result]\n",
        "    D --> G\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("long-edge flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User starts session"));
    assert!(text.contains("Open diff view"));
    assert!(text.contains("Report result"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_draws_left_right_direction() {
    // Arrange
    let source = "flowchart LR\n    A[In] --> B[Out]";

    // Act
    let diagram = render_mermaid(source).expect("LR chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('▶'));
    let first_box_line = text
        .lines()
        .find(|line| line.contains("In"))
        .expect("label row");
    assert!(first_box_line.contains("Out"));
}

#[test]
fn test_render_mermaid_draws_left_right_feedback_cycle() {
    // Arrange
    let source = concat!(
        "flowchart LR\n",
        "    A[\"App\"] -- \"commands:<br/>prompt · interrupt · permission answer\" --> \
         H[\"ag-harness\"]\n",
        "    H -- \"typed events:<br/>deltas · tool calls · diffs · usage\" --> A\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("two-node feedback graph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("App"));
    assert!(text.contains("ag-harness"));
    assert!(text.contains("commands:"));
    assert!(text.contains("typed events:"));
    assert!(text.contains('▶'));
    assert!(text.contains('◀'));
    assert!(!text.contains("flowchart LR"));
    assert!(!text.contains("<br/>"));
}

#[test]
fn test_render_mermaid_draws_multi_node_feedback_cycles() {
    // Arrange
    let source = concat!(
        "flowchart LR\n",
        "    U[User and TUI] --> C[Orchestrator controller]\n",
        "    M[Agent model] --> P[Typed command response]\n",
        "    P --> C\n",
        "    C --> S[ag-session service]\n",
        "    S --> A[Agentty host adapter]\n",
        "    A --> W[Session workers]\n",
        "    W --> E[Session events]\n",
        "    E --> C\n",
        "    C --> M\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("cyclic flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Orchestrator controller"));
    assert!(text.contains("Typed command response"));
    assert!(text.contains("Session events"));
    assert!(text.contains("Session events ───▶ Orchestrator controller"));
    assert!(text.contains("Orchestrator controller ───▶ Agent model"));
    assert!(!text.contains("flowchart LR"));
    assert!(diagram.width < 80);
}

#[test]
fn test_render_mermaid_keeps_independent_feedback_cycles_separate() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A[First start] --> B[First middle]\n",
        "    B --> C[First end]\n",
        "    C --> A\n",
        "    X[Second start] --> Y[Second middle]\n",
        "    Y --> Z[Second end]\n",
        "    Z --> X\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("independent cycles should render");
    let feedback_lines = diagram
        .lines
        .iter()
        .map(ToString::to_string)
        .filter(|line| line.contains("───▶"))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        feedback_lines,
        ["First end ───▶ First start", "Second end ───▶ Second start",]
    );
}

#[test]
fn test_render_mermaid_draws_labeled_top_down_feedback_edge() {
    // Arrange
    let source = "flowchart TD\n    A[Start] --> B[Work]\n    B <-->|retry| A";

    // Act
    let diagram = render_mermaid(source).expect("labeled cycle should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("retry"));
    assert!(text.contains('◀'));
    assert!(text.contains('▶'));
}

#[test]
fn test_render_mermaid_keeps_self_link_fallback() {
    // Arrange, Act, Assert
    assert!(render_mermaid("flowchart LR\n    A --> A").is_none());
}

#[test]
fn test_render_mermaid_uses_invisible_feedback_for_layout_only() {
    // Arrange
    let source = "flowchart TD\n    A[Start] --> B[Finish]\n    B ~~~ A";

    // Act
    let diagram = render_mermaid(source).expect("invisible feedback should not reject diagram");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert_eq!(text.matches('▼').count(), 1);
    assert!(!text.contains('◀'));
}

#[test]
fn test_render_mermaid_writes_edge_label_on_track() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -->|yes| C";

    // Act
    let diagram = render_mermaid(source).expect("labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
}

#[test]
fn test_render_mermaid_supports_rounded_and_chained_statements() {
    // Arrange
    let source = "graph TD; A(Begin) --> B{Choice}; B --> C((End))";

    // Act
    let diagram = render_mermaid(source).expect("chained statements should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('╭'));
    assert!(text.contains("Begin"));
    assert!(text.contains("Choice"));
    assert!(text.contains("End"));
}

#[test]
fn test_render_mermaid_maps_extended_node_shapes() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    A([Stadium]) --> B[[Subroutine]]\n",
        "    B --> C[(Cylinder)]\n",
        "    C --> D{{Hexagon}}\n",
        "    D --> E(((Core)))\n",
        "    E --> F>Flag]",
    );

    // Act
    let diagram = render_mermaid(source).expect("extended shapes should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Stadium"));
    assert!(text.contains("Subroutine"));
    assert!(text.contains("Cylinder"));
    assert!(text.contains("Hexagon"));
    assert!(text.contains("Core"));
    assert!(text.contains("Flag"));
    assert!(!text.contains('['));
}

#[test]
fn test_render_mermaid_expands_ampersand_groups() {
    // Arrange
    let source = "flowchart TD\n    A --> B & C\n    B & C --> D";

    // Act
    let diagram = render_mermaid(source).expect("ampersand groups should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 3);
}

#[test]
fn test_render_mermaid_accepts_extended_arrow_variants() {
    // Arrange
    let long_arrow = "flowchart TD\n    A ----> B";
    let source_arrow = "flowchart TD\n    A <-- B";
    let bidirectional = "flowchart TD\n    A <--> B";
    let circle_ends = "flowchart TD\n    A o--o B";
    let cross_ends = "flowchart TD\n    A x--x B";
    let long_arrow_label = "flowchart TD\n    A[Alpha stage] ---->|later| B[Beta stage]";

    // Act
    let long_arrow_diagram = render_mermaid(long_arrow).expect("long arrow should render");
    let source_arrow_diagram = render_mermaid(source_arrow).expect("source arrow should render");
    let bidirectional_diagram =
        render_mermaid(bidirectional).expect("bidirectional arrow should render");
    let labeled_diagram =
        render_mermaid(long_arrow_label).expect("labeled long arrow should render");

    // Assert
    assert!(diagram_text(&long_arrow_diagram).contains('▼'));
    let source_arrow_text = diagram_text(&source_arrow_diagram);
    assert!(source_arrow_text.contains('▼'));
    assert!(!source_arrow_text.contains('▲'));
    let source_position = source_arrow_text.find('B').expect("B should render");
    let target_position = source_arrow_text.find('A').expect("A should render");
    assert!(source_position < target_position);
    let bidirectional_text = diagram_text(&bidirectional_diagram);
    assert!(bidirectional_text.contains('▲'));
    assert!(bidirectional_text.contains('▼'));
    assert!(render_mermaid(circle_ends).is_some());
    assert!(render_mermaid(cross_ends).is_some());
    assert!(diagram_text(&labeled_diagram).contains("later"));
}

#[test]
fn test_render_mermaid_fans_source_arrow_chain_out_of_shared_source() {
    // Arrange
    let source = "flowchart TD\n    A <-- B --> C";

    // Act
    let diagram = render_mermaid(source).expect("source arrow chain should render");
    let text = diagram_text(&diagram);

    // Assert
    let source_position = text.find('B').expect("B should render");
    let first_target_position = text.find('A').expect("A should render");
    let second_target_position = text.find('C').expect("C should render");
    assert!(source_position < first_target_position);
    assert!(source_position < second_target_position);
    assert_eq!(text.matches('▼').count(), 2);
    assert!(!text.contains('▲'));
}

#[test]
fn test_render_mermaid_treats_reciprocal_source_arrow_as_cycle() {
    // Arrange
    let top_down = "flowchart TD\n    A --> B\n    A <-- B";
    let left_right = "flowchart LR\n    A --> B\n    A <-- B";

    // Act
    let top_down_diagram = render_mermaid(top_down).expect("top-down feedback loop should render");
    let top_down_text = diagram_text(&top_down_diagram);
    let left_right_diagram =
        render_mermaid(left_right).expect("two-node feedback loop should render");
    let left_right_text = diagram_text(&left_right_diagram);

    // Assert
    assert!(top_down_text.contains("B ───▶ A"));
    assert!(left_right_text.contains('▶'));
    assert!(left_right_text.contains('◀'));
}

#[test]
fn test_render_mermaid_hides_invisible_layout_link() {
    // Arrange
    let source = "flowchart TD\n    A[Source] ~~~ B[Target]";

    // Act
    let diagram = render_mermaid(source).expect("invisible layout link should render");
    let lines: Vec<String> = diagram.lines.iter().map(ToString::to_string).collect();
    let source_row = lines
        .iter()
        .position(|line| line.contains("Source"))
        .expect("source node should render");
    let target_row = lines
        .iter()
        .position(|line| line.contains("Target"))
        .expect("target node should render");

    // Assert
    assert!(source_row + 2 < target_row - 1);
    assert!(
        lines[source_row + 2..target_row - 1]
            .iter()
            .all(|line| line.trim().is_empty())
    );
}

#[test]
fn test_render_mermaid_keeps_line_operator_before_labeled_arrow_chain() {
    // Arrange
    let source = "flowchart TD\n    A --- B --> C";

    // Act
    let diagram = render_mermaid(source).expect("mixed chain should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('B'));
    assert!(text.contains('C'));
    assert_eq!(text.matches('▼').count(), 1);
}

#[test]
fn test_render_mermaid_renders_unspaced_inline_edge_label() {
    // Arrange
    let source = "flowchart TD\n    A[Alpha stage]--send-->B[Beta stage]";

    // Act
    let diagram = render_mermaid(source).expect("unspaced inline label should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("send"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_draws_er_diagram_with_cardinality_markers() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    CUSTOMER ||--o{ ORDER : places\n",
        "    CUSTOMER ||--|| ACCOUNT : owns\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("CUSTOMER"));
    assert!(text.contains("ORDER"));
    assert!(text.contains("ACCOUNT"));
    assert!(text.contains("places"));
    assert!(text.contains('1'));
    assert!(text.contains('*'));
    assert!(!text.contains('▼'));
}

#[test]
fn test_render_mermaid_er_omits_attribute_blocks() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    CUSTOMER {\n",
        "        string name\n",
        "    }\n",
        "    CUSTOMER ||--o{ ORDER : places\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("CUSTOMER"));
    assert!(text.contains("ORDER"));
    assert!(!text.contains("string"));
}

#[test]
fn test_render_mermaid_er_supports_hyphenated_entities_and_bare_links() {
    // Arrange
    let source = concat!(
        "erDiagram\n",
        "    ORDER ||--|{ LINE-ITEM : contains\n",
        "    LINE-ITEM }o..o| DISCOUNT\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("er diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("LINE-ITEM"));
    assert!(text.contains("DISCOUNT"));
    assert!(text.contains('+'));
    assert!(text.contains('?'));
}

#[test]
fn test_render_mermaid_er_rejects_unknown_relationship_operators() {
    // Arrange & Act & Assert
    assert!(render_mermaid("erDiagram\n    A |x--o{ B : bad").is_none());
    assert!(render_mermaid("erDiagram\n    A ||==o{ B : bad").is_none());
    assert!(render_mermaid("erDiagram").is_none());
}

#[test]
fn test_render_mermaid_draws_sequence_diagram() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    participant Agent\n",
        "    User->>Agentty: Start new session\n",
        "    Agentty->>Agent: Send prompt\n",
        "    Agent-->>Agentty: Stream result\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("sequence diagram should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start new session"));
    assert!(text.contains('▶'));
    assert!(!text.contains("sequenceDiagram"));
}

#[test]
fn test_render_mermaid_truncates_long_sequence_labels() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant A as agentty (client)\n",
        "    participant S as ag-harness (service)\n",
        "    A->>S: connect (WebSocket, JSON-RPC)\n",
        "    S-->>A: events seq 1..40 (deltas, diffs, usage)\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("long labels should truncate, not reject");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("agentty (client)"));
    assert!(text.contains("connect (WebSocket, JSON-RPC)"));
    assert!(text.contains("events seq 1..40 (deltas, diffs…"));
    assert!(!text.contains("diffs, usage)"));
}

#[test]
fn test_render_mermaid_draws_sequence_self_message() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant A as agentty (client)\n",
        "    participant S as ag-harness (service)\n",
        "    A->>S: disconnect (app closes)\n",
        "    S->>S: session keeps running, events journaled\n",
        "    S-->>A: replay 41..n, then live events\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("self message should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("session keeps running, events j…"));
    assert!(text.contains('┐'));
    assert!(text.contains('┘'));
    assert!(text.contains('◀'));
}

#[test]
fn test_render_mermaid_skips_sequence_notes_blocks_and_activations() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    autonumber\n",
        "    actor User\n",
        "    User->>+Agentty: Start\n",
        "    activate Agentty\n",
        "    Note over Agentty: thinking\n",
        "    alt success\n",
        "    Agentty-->>-User: Done\n",
        "    else failure\n",
        "    Agentty--xUser: Abort\n",
        "    end\n",
        "    deactivate Agentty\n",
        "    Agentty-)User: Async ping",
    );

    // Act
    let diagram = render_mermaid(source).expect("tolerant sequence should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("User"));
    assert!(text.contains("Agentty"));
    assert!(text.contains("Start"));
    assert!(text.contains("Done"));
    assert!(text.contains("Abort"));
    assert!(text.contains("Async ping"));
    assert!(!text.contains("thinking"));
    assert!(!text.contains("success"));
}

#[test]
fn test_render_mermaid_skips_sequence_critical_option_branches() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant Agentty\n",
        "    participant Forge\n",
        "    critical Open review request\n",
        "    Agentty->>Forge: Push branch\n",
        "    option Network timeout\n",
        "    Agentty->>Agentty: Retry push\n",
        "    option Auth rejected\n",
        "    Agentty->>Agentty: Report failure\n",
        "    end\n",
        "    Forge-->>Agentty: Review URL",
    );

    // Act
    let diagram = render_mermaid(source).expect("critical block should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Agentty"));
    assert!(text.contains("Forge"));
    assert!(text.contains("Push branch"));
    assert!(text.contains("Retry push"));
    assert!(text.contains("Report failure"));
    assert!(text.contains("Review URL"));
    assert!(!text.contains("Network timeout"));
    assert!(!text.contains("Auth rejected"));
}

#[test]
fn test_render_mermaid_narrows_sequence_gap_for_short_labels() {
    // Arrange
    let source = concat!(
        "sequenceDiagram\n",
        "    participant User\n",
        "    participant Agentty\n",
        "    participant Git\n",
        "    User->>Agentty: Start\n",
        "    Agentty->>Git: Commit\n",
        "    Git-->>Agentty: Ok\n",
        "    Agentty-->>User: Done",
    );

    // Act
    let diagram = render_mermaid(source).expect("sequence should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(diagram.width <= 50);
    assert!(text.contains("Commit"));
}

#[test]
fn test_render_mermaid_rejects_double_width_edge_labels() {
    // Arrange
    let pipe_label = "flowchart TD\n    A -->|你好| B";
    let inline_label = "flowchart TD\n    A -- 你好 --> B";
    let er_label = concat!(
        "erDiagram\n",
        "    PROJECT ||--o{ SESSION : 你好\n",
        "    SESSION ||--|| WORKTREE : owns",
    );

    // Act & Assert
    assert!(render_mermaid(pipe_label).is_none());
    assert!(render_mermaid(inline_label).is_none());
    assert!(render_mermaid(er_label).is_none());
}

#[test]
fn test_render_mermaid_rejects_unsupported_diagram_types() {
    // Arrange & Act & Assert
    assert!(render_mermaid("graph RL\n    A --> B").is_none());
    assert!(render_mermaid("").is_none());
}

#[test]
fn test_render_mermaid_rejects_source_over_preview_limits() {
    // Arrange
    let mut long_source = String::from("graph TD");
    for node_index in 0..MAX_SOURCE_LINE_COUNT {
        write!(&mut long_source, "\n    N{node_index}").expect("writing to String should succeed");
    }

    let wide_source = format!("graph TD\n    A[{}]", "x".repeat(MAX_SOURCE_BYTE_COUNT));

    // Act & Assert
    assert!(render_mermaid(&long_source).is_none());
    assert!(render_mermaid(&wide_source).is_none());
}

#[test]
fn test_render_mermaid_rejects_node_and_edge_over_preview_limits() {
    // Arrange
    let mut too_many_nodes = String::from("graph TD");
    for node_index in 0..=MAX_NODE_COUNT {
        write!(&mut too_many_nodes, "\n    N{node_index}")
            .expect("writing to String should succeed");
    }

    let mut too_many_edges = String::from("graph TD");
    for _ in 0..=MAX_EDGE_COUNT {
        too_many_edges.push_str("\n    A --> B");
    }

    // Act & Assert
    assert!(render_mermaid(&too_many_nodes).is_none());
    assert!(render_mermaid(&too_many_edges).is_none());
}

#[test]
fn test_render_mermaid_renders_small_cycles() {
    // Arrange
    let cyclic = "graph TD\n    A --> B\n    B --> A";
    let three_node_cycle = "graph LR\n    A --> B\n    B --> C\n    C --> A";

    // Act
    let top_down_diagram = render_mermaid(cyclic).expect("top-down cycle should render");
    let left_right_diagram =
        render_mermaid(three_node_cycle).expect("left-right cycle should render");

    // Assert
    assert!(diagram_text(&top_down_diagram).contains("B ───▶ A"));
    assert!(diagram_text(&left_right_diagram).contains("C ───▶ A"));
}

#[test]
fn test_render_mermaid_flattens_subgraph_statements() {
    // Arrange
    let source = concat!(
        "graph TD\n",
        "    subgraph Group\n",
        "    direction LR\n",
        "    A --> B\n",
        "    end\n",
        "    B --> C",
    );

    // Act
    let diagram = render_mermaid(source).expect("flattened subgraph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('C'));
    assert!(!text.contains("Group"));
}

#[test]
fn test_render_mermaid_skips_styling_statements() {
    // Arrange
    let source = concat!(
        "flowchart TD\n",
        "    classDef terminal stroke-width: 1.5px;\n",
        "    A:::terminal --> B\n",
        "    style A fill:#f9f\n",
        "    linkStyle 0 stroke:#f00\n",
        "    class B terminal\n",
        "    click A href \"https://example.com\"",
    );

    // Act
    let diagram = render_mermaid(source).expect("styled flowchart should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('B'));
    assert!(!text.contains("terminal"));
}

#[test]
fn test_render_mermaid_rejects_wide_character_labels() {
    // Arrange
    let source = "graph TD\n    A[你好] --> B";

    // Act & Assert
    assert!(render_mermaid(source).is_none());
}

#[test]
fn test_render_mermaid_truncates_over_long_node_labels() {
    // Arrange
    let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
    let long_bare = format!("graph TD\n    {long_identifier} --> B");
    let long_labeled =
        "graph TD\n    A[This label is much longer than thirty-two characters] --> B";
    let wide_bare = "graph TD\n    你好 --> B";

    // Act
    let bare_diagram = render_mermaid(&long_bare).expect("long bare id should render");
    let labeled_diagram = render_mermaid(long_labeled).expect("long label should render");

    // Assert
    assert!(diagram_text(&bare_diagram).contains('…'));
    assert!(diagram_text(&labeled_diagram).contains("This label is much longer than …"));
    assert!(render_mermaid(wide_bare).is_none());
}

#[test]
fn test_render_mermaid_accepts_long_bare_identifier_with_short_label() {
    // Arrange
    let long_identifier = "N".repeat(MAX_LABEL_WIDTH + 1);
    let source = format!("graph TD\n    {long_identifier}[Short] --> B");

    // Act
    let diagram = render_mermaid(&source).expect("labeled node should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Short"));
    assert!(!text.contains(&long_identifier));
}

#[test]
fn test_render_mermaid_uses_first_node_label_line() {
    // Arrange
    let source = concat!(
        "flowchart TB\n",
        "    APP[\"App - owns orchestration:<br/>spawning, coordination, aggregation\"]\n",
        "    S1[\"session 1\"]\n",
        "    S2[\"session 2\"]\n",
        "    S3[\"session 3\"]\n",
        "    APP --> S1\n",
        "    APP --> S2\n",
        "    APP --> S3\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("node label with line break should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("App - owns orchestration:"));
    assert!(text.contains("session 1"));
    assert!(text.contains("session 2"));
    assert!(text.contains("session 3"));
    assert!(text.contains('▼'));
    assert!(!text.contains("<br/>"));
    assert!(!text.contains("spawning, coordination, aggregation"));
}

#[test]
fn test_render_mermaid_skips_comments_and_inline_label_form() {
    // Arrange
    let source = "graph LR\n    %% comment line\n    A -- ok --> B";

    // Act
    let diagram = render_mermaid(source).expect("inline label form should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('▶'));
    assert!(!text.contains("comment"));
}

#[test]
fn test_render_mermaid_renders_dotted_edge_with_embedded_label() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -.yes.-> C";

    // Act
    let diagram = render_mermaid(source).expect("dotted labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_renders_spaced_dotted_edge_label_without_arrow() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A -. off .- C";

    // Act
    let diagram = render_mermaid(source).expect("dotted open labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("off"));
    assert_eq!(text.matches('▼').count(), 1);
}

#[test]
fn test_render_mermaid_renders_thick_edge_with_embedded_label() {
    // Arrange
    let source = "graph TD\n    A --> B\n    A ==big==> C";

    // Act
    let diagram = render_mermaid(source).expect("thick labeled edge should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("big"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_keeps_plain_dotted_and_thick_arrows() {
    // Arrange
    let source = "graph TD\n    A -.-> B\n    A ==>|yes| C";

    // Act
    let diagram = render_mermaid(source).expect("plain dotted and thick arrows should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("yes"));
    assert_eq!(text.matches('▼').count(), 2);
}

#[test]
fn test_render_mermaid_renders_graph_mixing_solid_and_dotted_labeled_edges() {
    // Arrange
    let source = concat!(
        "graph TD\n",
        "    T[Turn command] --> C[Auto-commit]\n",
        "    C --> P[Auto-push]\n",
        "    C --> R[Rebase]\n",
        "    P -.race.-> R\n",
    );

    // Act
    let diagram = render_mermaid(source).expect("mixed edge graph should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains("Turn command"));
    assert!(text.contains("Auto-push"));
    assert!(text.contains("Rebase"));
    assert!(text.contains('▼'));
}

#[test]
fn test_render_mermaid_renders_plain_dotted_and_thick_open_links() {
    // Arrange
    let source = "graph TD\n    A -.- B\n    B === C";

    // Act
    let diagram = render_mermaid(source).expect("open dotted and thick links should render");
    let text = diagram_text(&diagram);

    // Assert
    assert!(text.contains('A'));
    assert!(text.contains('C'));
    assert!(!text.contains('▼'));
}

#[test]
fn test_parsed_cache_reuses_source_across_widths_and_palettes() {
    // Arrange
    let source = "graph LR\nParse[Parse once] --> Paint[Paint twice]";
    let first = parsed_mermaid(source).expect("parsed graph");
    let mut settings = TextRenderSettings::default();
    settings.palette.text = ratatui::style::Color::Red;

    // Act
    let wide = render_mermaid_for_width(source, 80).expect("wide diagram");
    let narrow = render_mermaid_for_width(source, 20).expect("stacked diagram");
    let themed = render_mermaid_with_settings(source, settings).expect("themed diagram");
    let repeated = parsed_mermaid(source).expect("cached graph");

    // Assert
    assert!(Arc::ptr_eq(&first, &repeated));
    assert!(wide.width > narrow.width);
    assert!(narrow.lines.len() > wide.lines.len());
    assert!(
        themed
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg == Some(ratatui::style::Color::Red))
    );
}

#[test]
fn test_sequence_preview_rejects_narrow_width_and_reuses_parse_when_widened() {
    // Arrange
    let source = "sequenceDiagram\nAlice->>Bob: Hello";
    let original = render_mermaid_for_width(source, 80).expect("sequence preview");

    // Act
    let too_narrow = render_mermaid_for_width(source, original.width - 1);
    let widened = render_mermaid_for_width(source, original.width).expect("restored preview");

    // Assert
    assert!(too_narrow.is_none());
    assert_eq!(widened.lines, original.lines);
    assert_eq!(widened.width, original.width);
}

#[test]
fn test_parsed_cache_is_bounded_and_promotes_hits() {
    // Arrange
    PARSED_MERMAID_CACHE.with(|cache| cache.borrow_mut().clear());
    let source = "graph TD\nKeep --> Cached";
    let first = parsed_mermaid(source).expect("first diagram");
    for index in 0..PARSED_MERMAID_CACHE_ENTRY_LIMIT - 1 {
        parsed_mermaid(&format!("graph TD\nNode{index} --> End"));
    }

    // Act
    let promoted = parsed_mermaid(source).expect("promoted diagram");
    parsed_mermaid("graph TD\nExtra --> End");
    let retained = parsed_mermaid(source).expect("retained diagram");

    // Assert
    assert!(Arc::ptr_eq(&first, &promoted));
    assert!(Arc::ptr_eq(&first, &retained));
    PARSED_MERMAID_CACHE.with(|cache| {
        let entries = cache.borrow();
        assert_eq!(entries.len(), PARSED_MERMAID_CACHE_ENTRY_LIMIT);
        assert!(entries.iter().all(|entry| !entry.source.contains("Node0 ")));
    });
}

#[test]
fn test_parsed_cache_retains_unsupported_source_but_rejects_oversized_input() {
    // Arrange
    PARSED_MERMAID_CACHE.with(|cache| cache.borrow_mut().clear());
    let unsupported = "classDiagram\nA <|-- B";

    // Act
    let first = parsed_mermaid(unsupported);
    let repeated = parsed_mermaid(unsupported);
    let oversized = parsed_mermaid(&"x".repeat(MAX_SOURCE_BYTE_COUNT + 1));

    // Assert
    assert!(first.is_none());
    assert!(repeated.is_none());
    assert!(oversized.is_none());
    PARSED_MERMAID_CACHE.with(|cache| assert_eq!(cache.borrow().len(), 1));
}
