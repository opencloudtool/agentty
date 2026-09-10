use super::*;

#[test]
fn focused_review_formats_structured_fields_as_markdown() {
    // Arrange
    let review = FocusedReview {
        project_impact: vec!["Improves review reliability.".to_string()],
        suggestions: vec![
            FocusedReviewSuggestion {
                details: "Fix the stale cache check.".to_string(),
                severity: FocusedReviewSeverity::High,
            },
            FocusedReviewSuggestion {
                details: "Deduplicate the parsing path.".to_string(),
                severity: FocusedReviewSeverity::Medium,
            },
        ],
    };

    // Act
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(
        markdown,
        "## Review\n\n### Project Impact\n\n- Improves review reliability.\n\n### \
         Suggestions\n\n- [High]: Fix the stale cache check.\n- [Medium]: Deduplicate the parsing \
         path."
    );
}

#[test]
fn focused_review_formats_empty_arrays_with_none_sentinels() {
    // Arrange
    let review = FocusedReview {
        project_impact: Vec::new(),
        suggestions: Vec::new(),
    };

    // Act
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(
        markdown,
        "## Review\n\n### Project Impact\n\n- None\n\n### Suggestions\n\n- None"
    );
}

#[test]
fn focused_review_severity_serializes_as_lowercase() {
    // Arrange
    let severity = FocusedReviewSeverity::High;

    // Act
    let serialized = serde_json::to_string(&severity).expect("severity should serialize");
    let deserialized = serde_json::from_str::<FocusedReviewSeverity>(&serialized)
        .expect("severity should deserialize");

    // Assert
    assert_eq!(serialized, "\"high\"");
    assert_eq!(deserialized, severity);
}
