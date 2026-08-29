use thought_store::{NewReviewerConnectionInput, ReviewerConnectionUpdateInput, Store, StoreError};

fn register_all(store: &Store, id: &str, label: &str, hash_byte: u8) {
    store
        .create_reviewer_connection(&NewReviewerConnectionInput {
            id,
            client: "codex",
            provider: "openai",
            display_label: label,
            document_scope: "all",
            can_edit: true,
            can_create: true,
            can_trash: false,
            credential_hash: &[hash_byte; 32],
            credential_expires_at: None,
            document_ids: &[],
            created_at: 100,
        })
        .unwrap();
}

#[test]
fn same_name_connections_keep_distinct_ids_and_scopes() {
    let store = Store::open_in_memory().unwrap();
    store.create_document("draft-a", "Draft A").unwrap();
    store.create_document("draft-b", "Draft B").unwrap();

    let selected = vec!["draft-b".to_string(), "draft-a".to_string()];
    let first = store
        .create_reviewer_connection(&NewReviewerConnectionInput {
            id: "reviewer-one",
            client: "claude_code",
            provider: "anthropic",
            display_label: "Grammar review",
            document_scope: "selected",
            can_edit: true,
            can_create: false,
            can_trash: false,
            credential_hash: &[1; 32],
            credential_expires_at: None,
            document_ids: &selected,
            created_at: 10,
        })
        .unwrap();
    register_all(&store, "reviewer-two", "Grammar review", 2);

    assert_eq!(first.document_ids, ["draft-a", "draft-b"]);
    assert_eq!(store.list_reviewer_connections(20).unwrap().len(), 2);
    assert_eq!(
        store
            .reviewer_connection_by_credential_hash(&[1; 32], 20)
            .unwrap()
            .unwrap()
            .id,
        "reviewer-one"
    );
    assert_eq!(
        store
            .reviewer_connection_by_credential_hash(&[2; 32], 20)
            .unwrap()
            .unwrap()
            .id,
        "reviewer-two"
    );
}

#[test]
fn management_updates_are_optimistic_and_no_ops_keep_the_revision() {
    let store = Store::open_in_memory().unwrap();
    register_all(&store, "reviewer", "First label", 3);

    let no_op = store
        .update_reviewer_connection(&ReviewerConnectionUpdateInput {
            id: "reviewer",
            expected_revision: 1,
            display_label: "First label",
            document_scope: "all",
            can_edit: true,
            can_create: true,
            can_trash: false,
            document_ids: &[],
            updated_at: 101,
        })
        .unwrap();
    assert_eq!(no_op.revision, 1);

    let updated = store
        .update_reviewer_connection(&ReviewerConnectionUpdateInput {
            id: "reviewer",
            expected_revision: 1,
            display_label: "Second label",
            document_scope: "all",
            can_edit: true,
            can_create: true,
            can_trash: true,
            document_ids: &[],
            updated_at: 102,
        })
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.display_label, "Second label");
    assert!(updated.can_trash);

    assert!(matches!(
        store.update_reviewer_connection(&ReviewerConnectionUpdateInput {
            id: "reviewer",
            expected_revision: 1,
            display_label: "Stale overwrite",
            document_scope: "all",
            can_edit: true,
            can_create: true,
            can_trash: true,
            document_ids: &[],
            updated_at: 103,
        }),
        Err(StoreError::ReviewerConnectionRevisionConflict {
            expected: 1,
            actual: 2,
            ..
        })
    ));
}

#[test]
fn credential_rotation_accepts_pending_hash_then_invalidates_the_old_hash() {
    let store = Store::open_in_memory().unwrap();
    register_all(&store, "reviewer", "Review", 4);

    store
        .begin_reviewer_credential_rotation("reviewer", 1, &[5; 32], 110)
        .unwrap();
    store
        .mark_reviewer_seen("reviewer", 110, 150, None)
        .unwrap();
    assert!(
        store
            .reviewer_connection_by_credential_hash(&[4; 32], 111)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .reviewer_connection_by_credential_hash(&[5; 32], 111)
            .unwrap()
            .is_some()
    );

    let rotated = store
        .finish_reviewer_credential_rotation("reviewer", 1, 112)
        .unwrap();
    assert_eq!(rotated.revision, 2);
    assert_eq!(rotated.credential_version, 2);
    assert_eq!(rotated.status, "disconnected");
    assert_eq!(rotated.lease_expires_at, None);
    assert!(
        store
            .reviewer_connection_by_credential_hash(&[4; 32], 113)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .reviewer_connection_by_credential_hash(&[5; 32], 113)
            .unwrap()
            .is_some()
    );
}

#[test]
fn heartbeat_lease_transitions_do_not_change_management_revision() {
    let store = Store::open_in_memory().unwrap();
    register_all(&store, "reviewer", "Review", 6);

    let connected = store
        .mark_reviewer_seen("reviewer", 120, 150, Some("gpt-reported"))
        .unwrap();
    assert_eq!(connected.status, "connected");
    assert_eq!(connected.revision, 1);
    assert_eq!(connected.reported_model.as_deref(), Some("gpt-reported"));

    assert_eq!(store.expire_reviewer_leases(149).unwrap(), 0);
    assert_eq!(store.expire_reviewer_leases(150).unwrap(), 1);
    let disconnected = store.reviewer_connection("reviewer").unwrap();
    assert_eq!(disconnected.status, "disconnected");
    assert_eq!(disconnected.revision, 1);
}

#[test]
fn revoke_is_terminal_and_immediately_disables_authentication() {
    let store = Store::open_in_memory().unwrap();
    register_all(&store, "reviewer", "Review", 7);
    store
        .mark_reviewer_seen("reviewer", 120, 150, None)
        .unwrap();

    let revoked = store
        .revoke_reviewer_connection("reviewer", 1, 130)
        .unwrap();
    assert_eq!(revoked.status, "revoked");
    assert_eq!(revoked.revision, 2);
    assert!(
        store
            .reviewer_connection_by_credential_hash(&[7; 32], 131)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.mark_reviewer_seen("reviewer", 131, 161, None),
        Err(StoreError::ReviewerConnectionRevoked(_))
    ));
    assert!(matches!(
        store.revoke_reviewer_connection("reviewer", 2, 132),
        Err(StoreError::ReviewerConnectionRevoked(_))
    ));
}

#[test]
fn inconsistent_permissions_and_provider_are_rejected() {
    let store = Store::open_in_memory().unwrap();
    let selected = vec!["missing-document".to_string()];

    for input in [
        NewReviewerConnectionInput {
            id: "wrong-provider",
            client: "codex",
            provider: "anthropic",
            display_label: "Review",
            document_scope: "all",
            can_edit: true,
            can_create: false,
            can_trash: false,
            credential_hash: &[8; 32],
            credential_expires_at: None,
            document_ids: &[],
            created_at: 100,
        },
        NewReviewerConnectionInput {
            id: "create-selected",
            client: "codex",
            provider: "openai",
            display_label: "Review",
            document_scope: "selected",
            can_edit: true,
            can_create: true,
            can_trash: false,
            credential_hash: &[9; 32],
            credential_expires_at: None,
            document_ids: &selected,
            created_at: 100,
        },
    ] {
        assert!(matches!(
            store.create_reviewer_connection(&input),
            Err(StoreError::InvalidReviewerConnectionTransition(_))
        ));
    }
}

#[test]
fn current_and_pending_credentials_cannot_collide_across_connections() {
    let store = Store::open_in_memory().unwrap();
    register_all(&store, "first", "First", 10);
    register_all(&store, "second", "Second", 11);

    assert!(
        store
            .begin_reviewer_credential_rotation("first", 1, &[10; 32], 110)
            .is_err(),
        "a pending credential cannot equal its own current credential"
    );
    assert!(
        store
            .begin_reviewer_credential_rotation("first", 1, &[11; 32], 111)
            .is_err(),
        "a pending credential cannot equal another connection's current credential"
    );
}

#[test]
fn lifecycle_events_freeze_each_selected_document_allowlist() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let store = Store::open(&path).unwrap();
    store.create_document("draft-a", "Draft A").unwrap();
    store.create_document("draft-b", "Draft B").unwrap();

    store
        .create_reviewer_connection(&NewReviewerConnectionInput {
            id: "reviewer",
            client: "codex",
            provider: "openai",
            display_label: "Review",
            document_scope: "selected",
            can_edit: true,
            can_create: false,
            can_trash: false,
            credential_hash: &[12; 32],
            credential_expires_at: None,
            document_ids: &["draft-b".to_string(), "draft-a".to_string()],
            created_at: 100,
        })
        .unwrap();
    store
        .update_reviewer_connection(&ReviewerConnectionUpdateInput {
            id: "reviewer",
            expected_revision: 1,
            display_label: "Review",
            document_scope: "selected",
            can_edit: true,
            can_create: false,
            can_trash: false,
            document_ids: &["draft-b".to_string()],
            updated_at: 101,
        })
        .unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(path).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT event_type, document_ids_json
             FROM reviewer_connection_events
             WHERE connection_id = 'reviewer'
             ORDER BY seq",
        )
        .unwrap();
    let snapshots = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        snapshots,
        [
            (
                "created".to_string(),
                r#"["draft-a","draft-b"]"#.to_string()
            ),
            (
                "permissions_changed".to_string(),
                r#"["draft-b"]"#.to_string()
            ),
        ]
    );

    assert!(
        connection
            .execute(
                "UPDATE reviewer_connection_events SET document_ids_json = '[]'",
                [],
            )
            .is_err()
    );
}
