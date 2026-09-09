use super::*;

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_provider_request_durably_interrupts_the_session_turn() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let request_started = Arc::new(Notify::new());
    let harness = Arc::new(
        Harness::new(PendingModel {
            started: Arc::clone(&request_started),
        })
        .database(&database_path),
    );
    let session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    drop(session);
    let turn = tokio::spawn(send_with_resumed_session(
        Arc::clone(&harness),
        "pending provider request",
    ));
    wait_for_fixture(&request_started, "the provider request").await;
    let database = Database::open(&database_path)
        .await
        .expect("database should reopen");
    let expected_state = ("interrupted".to_string(), Some("cancelled".to_string()));

    // Act
    let cancellation = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        turn.abort();

        turn.await.expect_err("turn should be cancelled")
    };
    let (state, cancellation) = tokio::join!(
        wait_for_stored_turn_state(&database, &expected_state),
        cancellation
    );

    // Assert
    assert!(cancellation.is_cancelled());
    assert_eq!(state, expected_state);
}

#[tokio::test]
async fn pending_tool_file_system_rejects_replace_requests() {
    // Arrange
    let file_system = PendingToolFileSystem {
        started: Arc::new(Notify::new()),
    };

    // Act
    let error = file_system
        .replace_beneath(Path::new("repo"), Path::new("Cargo.toml"), None, Vec::new())
        .await
        .expect_err("pending read fixture should reject replacement");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Other);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_tool_request_durably_interrupts_the_session_turn() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let tool_started = Arc::new(Notify::new());
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("pending-read"),
        )))
    });
    let harness = Arc::new(
        Harness::new(model)
            .database(&database_path)
            .repository(Repository::fixture("repo"))
            .allow(Tool::Read)
            .file_system(PendingToolFileSystem {
                started: Arc::clone(&tool_started),
            }),
    );
    let session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    drop(session);
    let turn = tokio::spawn(send_with_resumed_session(
        Arc::clone(&harness),
        "pending tool request",
    ));
    wait_for_fixture(&tool_started, "the tool request").await;

    // Act
    turn.abort();
    let cancellation = turn.await.expect_err("turn should be cancelled");
    let database = Database::open(&database_path)
        .await
        .expect("database should reopen");
    let expected_state = ("interrupted".to_string(), Some("cancelled".to_string()));
    let state = wait_for_stored_turn_state(&database, &expected_state).await;

    // Assert
    assert!(cancellation.is_cancelled());
    assert_eq!(state, expected_state);
}

#[tokio::test]
async fn active_session_turn_renews_its_lease_during_a_long_model_request() {
    // Arrange
    let timestamp_origin = 10;
    let clock_origin = tokio::time::Instant::now();
    let timestamp_source: Arc<dyn crate::session::TimestampSource> =
        Arc::new(move || elapsed_timestamp(timestamp_origin, clock_origin));
    let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
        .await
        .expect("database should open");
    database
        .create_session(
            &NewSession::new("session-a", object_schema()),
            None,
            DEFAULT_MAX_HISTORY_BYTES,
        )
        .await
        .expect("session should be created");
    let first_started = Arc::new(Notify::new());
    let first_release = Arc::new(Notify::new());
    let harness = Harness::new(LeaseExpiryModel {
        call_count: AtomicUsize::new(0),
        release_first: Arc::clone(&first_release),
        started_first: Arc::clone(&first_started),
    });
    let mut first = Session {
        database: database.clone(),
        harness: &harness,
        history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
        id: "session-a".to_string(),
        provider_session_id: None,
        schema: object_schema(),
        system_prompt: None,
    };
    let mut second = Session {
        database,
        harness: &harness,
        history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
        id: "session-a".to_string(),
        provider_session_id: None,
        schema: object_schema(),
        system_prompt: None,
    };

    // Act
    let (first_result, second_result) = tokio::join!(first.send("first"), async {
        wait_for_fixture(&first_started, "the long model request").await;
        tokio::task::yield_now().await;
        let original_expiry = stored_lease_expiry(&second.database).await;
        let lease_duration = Duration::from_secs(
            u64::try_from(crate::session::TURN_LEASE_SECONDS)
                .expect("turn lease duration should be positive"),
        );
        let renewal_interval =
            Duration::from_secs(crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
        assert!(renewal_interval < lease_duration);
        tokio::time::pause();
        tokio::time::advance(renewal_interval).await;
        tokio::time::resume();
        let renewal_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
        assert!(renewal_timestamp < original_expiry);
        let first_renewed_expiry =
            wait_for_lease_extension(&second.database, original_expiry).await;
        let first_renewed_expiry =
            first_renewed_expiry.expect("active turn should renew its lease before expiry");
        tokio::time::pause();
        tokio::time::advance(renewal_interval).await;
        tokio::time::resume();
        let next_renewal_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
        assert!(next_renewal_timestamp < first_renewed_expiry);
        let next_renewed_expiry =
            wait_for_lease_extension(&second.database, first_renewed_expiry).await;
        let next_renewed_expiry = next_renewed_expiry
            .expect("active turn should keep renewing its lease during a long request");
        let current_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
        let recovery_advance = Duration::from_secs(
            u64::try_from(
                first_renewed_expiry
                    .saturating_add(1)
                    .saturating_sub(current_timestamp),
            )
            .expect("first renewed lease should expire in the future"),
        );
        tokio::time::pause();
        tokio::time::advance(recovery_advance).await;
        tokio::time::resume();
        let recovery_timestamp = elapsed_timestamp(timestamp_origin, clock_origin);
        assert!(recovery_timestamp > first_renewed_expiry);
        assert!(next_renewed_expiry > recovery_timestamp);
        let result = second.send("second").await;
        first_release.notify_one();

        result
    });

    // Assert
    first_result.expect("the lease owner should complete its turn");
    assert!(matches!(second_result, Err(SessionError::Busy { .. })));
}

#[tokio::test]
async fn recovered_lease_cancels_the_original_model_request() {
    // Arrange
    let now = Arc::new(AtomicI64::new(10));
    let timestamp_source: Arc<dyn crate::session::TimestampSource> = {
        let now = Arc::clone(&now);

        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let database = Database::open_in_memory_with_timestamp_source(timestamp_source)
        .await
        .expect("database should open");
    database
        .create_session(
            &NewSession::new("session-a", object_schema()),
            None,
            DEFAULT_MAX_HISTORY_BYTES,
        )
        .await
        .expect("session should be created");
    let first_started = Arc::new(Notify::new());
    let first_dropped = Arc::new(Notify::new());
    let harness = Harness::new(LeaseOwnershipModel {
        call_count: AtomicUsize::new(0),
        dropped_first: Arc::clone(&first_dropped),
        started_first: Arc::clone(&first_started),
    });
    let mut first = Session {
        database: database.clone(),
        harness: &harness,
        history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
        id: "session-a".to_string(),
        provider_session_id: None,
        schema: object_schema(),
        system_prompt: None,
    };
    let mut second = Session {
        database,
        harness: &harness,
        history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
        id: "session-a".to_string(),
        provider_session_id: None,
        schema: object_schema(),
        system_prompt: None,
    };

    // Act
    let (first_result, second_result) = tokio::join!(first.send("first"), async {
        wait_for_fixture(&first_started, "the original model request").await;
        let original_expiry = stored_lease_expiry(&second.database).await;
        now.store(original_expiry.saturating_add(1), Ordering::SeqCst);
        let result = second.send("second").await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(
            crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS,
        ))
        .await;
        tokio::time::resume();
        wait_for_fixture(&first_dropped, "the original model request cancellation").await;

        result
    });

    // Assert
    assert!(matches!(
        first_result,
        Err(SessionError::OwnershipLost {
            ref id,
            turn_position: 0,
        }) if id == "session-a"
    ));
    second_result.expect("the replacement turn should complete");
}

#[tokio::test]
async fn lease_renewal_failure_cancels_the_model_request() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(
            &NewSession::new("session-a", object_schema()),
            None,
            DEFAULT_MAX_HISTORY_BYTES,
        )
        .await
        .expect("session should be created");
    sqlx::query(
        r"
CREATE TRIGGER reject_turn_lease_renewal
BEFORE UPDATE OF lease_expires_at ON session_turn
WHEN OLD.status = 'running' AND NEW.status = 'running'
BEGIN
SELECT RAISE(ABORT, 'injected lease renewal failure');
END
",
    )
    .execute(database.pool())
    .await
    .expect("renewal failure trigger should be created");
    let request_started = Arc::new(Notify::new());
    let request_dropped = Arc::new(Notify::new());
    let harness = Harness::new(LeaseOwnershipModel {
        call_count: AtomicUsize::new(0),
        dropped_first: Arc::clone(&request_dropped),
        started_first: Arc::clone(&request_started),
    });
    let mut session = Session {
        database: database.clone(),
        harness: &harness,
        history: SessionHistory::new(DEFAULT_MAX_HISTORY_BYTES),
        id: "session-a".to_string(),
        provider_session_id: None,
        schema: object_schema(),
        system_prompt: None,
    };

    // Act
    let (result, ()) = tokio::join!(session.send("pending"), async {
        wait_for_fixture(&request_started, "the model request").await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(
            crate::session::TURN_LEASE_RENEWAL_INTERVAL_SECONDS,
        ))
        .await;
        tokio::time::resume();
        wait_for_fixture(&request_dropped, "the model request cancellation").await;
    });
    let expected_state = ("interrupted".to_string(), Some("interrupted".to_string()));
    let state = wait_for_stored_turn_state(&database, &expected_state).await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::QueryContext {
            operation: "renew persistent session turn lease",
            ..
        })
    ));
    assert_eq!(state, expected_state);
}

struct PendingModel {
    started: Arc<Notify>,
}

#[async_trait]
impl Model for PendingModel {
    async fn complete(&self, _request: ModelRequest) -> Result<crate::ModelCompletion, ModelError> {
        self.started.notify_one();
        std::future::pending().await
    }
}

struct PendingToolFileSystem {
    started: Arc<Notify>,
}

#[async_trait]
impl FileSystem for PendingToolFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if path == Path::new("repo") {
            Ok(PathBuf::from("/repo"))
        } else {
            Ok(PathBuf::from("/repo/Cargo.toml"))
        }
    }

    async fn open_beneath(
        &self,
        _root: &Path,
        _path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        self.started.notify_one();
        std::future::pending().await
    }

    async fn replace_beneath(
        &self,
        _root: &Path,
        _path: &Path,
        _expected: Option<Vec<u8>>,
        _content: Vec<u8>,
    ) -> io::Result<()> {
        Err(io::Error::other(
            "pending read fixture must not replace files",
        ))
    }
}

struct LeaseExpiryModel {
    call_count: AtomicUsize,
    release_first: Arc<Notify>,
    started_first: Arc<Notify>,
}

#[async_trait]
impl Model for LeaseExpiryModel {
    async fn complete(&self, _request: ModelRequest) -> Result<crate::ModelCompletion, ModelError> {
        if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            self.started_first.notify_one();
            self.release_first.notified().await;
        }

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

struct RequestDropNotifier {
    dropped: Arc<Notify>,
}

impl Drop for RequestDropNotifier {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}

struct LeaseOwnershipModel {
    call_count: AtomicUsize,
    dropped_first: Arc<Notify>,
    started_first: Arc<Notify>,
}

#[async_trait]
impl Model for LeaseOwnershipModel {
    async fn complete(&self, _request: ModelRequest) -> Result<crate::ModelCompletion, ModelError> {
        if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            let _drop_notifier = RequestDropNotifier {
                dropped: Arc::clone(&self.dropped_first),
            };
            self.started_first.notify_one();
            std::future::pending::<()>().await;
        }

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

async fn wait_for_fixture(notify: &Notify, description: &str) {
    let result = tokio::time::timeout(Duration::from_secs(5), notify.notified()).await;

    assert!(result.is_ok(), "timed out waiting for {description}");
}

async fn stored_lease_expiry(database: &Database) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT lease_expires_at FROM session_turn WHERE session_id = ?")
        .bind("session-a")
        .fetch_one(database.pool())
        .await
        .expect("active lease should load")
}

fn elapsed_timestamp(origin: i64, started_at: tokio::time::Instant) -> i64 {
    let elapsed_seconds = i64::try_from(started_at.elapsed().as_secs()).unwrap_or(i64::MAX);

    origin.saturating_add(elapsed_seconds)
}

async fn wait_for_lease_extension(database: &Database, previous_expiry: i64) -> Option<i64> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let lease_expiry = stored_lease_expiry(database).await;
            if lease_expiry > previous_expiry {
                return lease_expiry;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
}
