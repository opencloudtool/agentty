//! Shared review-request helpers used by app workflows.

/// Parsed commit-message metadata used to populate a new review request.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestCommitMessage {
    /// Optional body copied from the commit description.
    pub(crate) body: Option<String>,
    /// Title copied from the first non-empty commit-message line.
    pub(crate) title: String,
}

/// Parses a session-branch commit message into review-request metadata.
pub(crate) fn parse_review_request_commit_message(
    commit_message: &str,
) -> Option<ReviewRequestCommitMessage> {
    let mut lines = commit_message.lines();
    let title = lines
        .find(|line| !line.trim().is_empty())?
        .trim()
        .to_string();
    let description = lines.collect::<Vec<_>>().join("\n");
    let description = description.trim();

    Some(ReviewRequestCommitMessage {
        body: (!description.is_empty()).then(|| description.to_string()),
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies parsing keeps the first non-empty line as the title and trims
    /// the remaining description into the PR body.
    #[test]
    fn parse_review_request_commit_message_returns_title_and_body() {
        // Arrange
        let commit_message = "Refine session commit message\n\n- Keep title in sync\n";

        // Act
        let parsed_commit_message = parse_review_request_commit_message(commit_message);

        // Assert
        assert_eq!(
            parsed_commit_message,
            Some(ReviewRequestCommitMessage {
                body: Some("- Keep title in sync".to_string()),
                title: "Refine session commit message".to_string(),
            })
        );
    }

    /// Verifies parsing omits the PR body when the commit message only has a
    /// title.
    #[test]
    fn parse_review_request_commit_message_omits_empty_body() {
        // Arrange
        let commit_message = "Refine session commit message\n\n";

        // Act
        let parsed_commit_message = parse_review_request_commit_message(commit_message);

        // Assert
        assert_eq!(
            parsed_commit_message,
            Some(ReviewRequestCommitMessage {
                body: None,
                title: "Refine session commit message".to_string(),
            })
        );
    }

    /// Verifies parsing rejects commit messages that do not contain a title.
    #[test]
    fn parse_review_request_commit_message_rejects_empty_title() {
        // Arrange
        let commit_message = "\n \n";

        // Act
        let parsed_commit_message = parse_review_request_commit_message(commit_message);

        // Assert
        assert_eq!(parsed_commit_message, None);
    }
}
