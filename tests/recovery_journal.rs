#![cfg(feature = "native")]
use tt::{
    Timestamp,
    protocol::{Envelope, RecordedOutcome, RequestId},
    recovery::{Journal, JournalOutcome},
    workspace::{Authority, Operation},
};
fn envelope(nonce: u8) -> Envelope {
    Envelope::new(
        "actor-A".into(),
        Authority {
            workspace: "11111111-1111-4111-8111-111111111111".into(),
            incarnation: "22222222-2222-4222-8222-222222222222".into(),
        },
        RequestId::new(Timestamp::UNIX_EPOCH, [nonce; 16]),
        &Operation::AddClient {
            name: "Acme".into(),
        },
        None,
    )
    .unwrap()
}
#[test]
fn wrong_committed_kind_never_publishes_and_forged_terminals_fail_closed() {
    let mut workspace = tt::workspace::Workspace::memory().unwrap();
    let e = Envelope::new(
        "actor-A".into(),
        workspace
            .session("actor-A", Timestamp::UNIX_EPOCH)
            .unwrap()
            .stamp
            .authority,
        RequestId::new(Timestamp::UNIX_EPOCH, [44; 16]),
        &Operation::AddClient {
            name: "Acme".into(),
        },
        None,
    )
    .unwrap();
    let outcome = workspace
        .execute("actor-A", &e, Timestamp::UNIX_EPOCH)
        .unwrap();
    let RecordedOutcome::Committed(mut value) = outcome else {
        panic!()
    };
    value["value"] = serde_json::json!("Removed");
    let wrong = RecordedOutcome::Committed(value);
    tt::protocol::validate_outcome(&wrong).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(dir.path().to_owned()).unwrap();
    let pending = journal
        .before_send("https://example.com/work".into(), e.to_bytes().unwrap())
        .unwrap();
    assert!(journal.finish(&pending, wrong.clone()).is_err());
    assert!(journal.load(&e.id.key()).unwrap().outcome.is_none());
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut forged = pending;
    forged.outcome = Some(JournalOutcome::Recorded(wrong));
    std::fs::write(
        path.with_extension("terminal"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    assert!(journal.load(&e.id.key()).is_err());
    assert!(journal.pending_keys().is_err());
    assert!(path.exists());
}

#[test]
fn concurrent_install_and_terminal_preserve_original() {
    let dir = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
        for n in 0..16 {
            let path = dir.path().to_owned();
            scope.spawn(move || {
                let journal = Journal::open(path).unwrap();
                let e = envelope(n);
                let pending = journal
                    .before_send(
                        "https://example.com/v1/workspaces/work".into(),
                        e.to_bytes().unwrap(),
                    )
                    .unwrap();
                assert_eq!(
                    journal.load(&e.id.key()).unwrap().envelope,
                    pending.envelope
                );
                assert!(
                    journal
                        .before_send(pending.endpoint.clone(), pending.envelope.clone())
                        .is_err()
                );
                let outcome = RecordedOutcome::Rejected {
                    code: "ClientExists".into(),
                };
                journal.finish(&pending, outcome.clone()).unwrap();
                journal.finish(&pending, outcome.clone()).unwrap();
                let saved = journal.load(&e.id.key()).unwrap();
                assert_eq!(saved.envelope, e.to_bytes().unwrap());
                assert_eq!(saved.outcome, Some(JournalOutcome::Recorded(outcome)));
                assert_eq!(saved.request().unwrap().principal, "actor-A");
            });
        }
    });
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 16);
    for file in files {
        assert_eq!(file.extension().unwrap(), "terminal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    assert!(
        Journal::open(dir.path().to_owned())
            .unwrap()
            .load("../../escape")
            .is_err()
    );
}
#[test]
fn bad_directory_and_conflicting_outcomes_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    assert!(Journal::open(file).is_err());
    let journal = Journal::open(dir.path().join("journal")).unwrap();
    let e = envelope(20);
    let pending = journal
        .before_send("https://example.com/work".into(), e.to_bytes().unwrap())
        .unwrap();
    journal
        .finish(
            &pending,
            RecordedOutcome::Rejected {
                code: "ClientExists".into(),
            },
        )
        .unwrap();
    assert!(
        journal
            .finish(
                &pending,
                RecordedOutcome::Rejected {
                    code: "ProjectExists".into()
                }
            )
            .is_err()
    );
    assert_eq!(
        journal.load(&e.id.key()).unwrap().outcome,
        Some(JournalOutcome::Recorded(RecordedOutcome::Rejected {
            code: "ClientExists".into()
        }))
    );
}

#[test]
fn discovery_validates_names_and_prefers_durable_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("new/ancestor/journal");
    let journal = Journal::open(root.clone()).unwrap();
    let e = envelope(30);
    let pending = journal
        .before_send("https://example.com/work".into(), e.to_bytes().unwrap())
        .unwrap();
    assert_eq!(journal.pending_keys().unwrap(), vec![e.id.key()]);
    let path = std::fs::read_dir(root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = std::fs::read(&path).unwrap();
    journal
        .finish(
            &pending,
            RecordedOutcome::Rejected {
                code: "ClientExists".into(),
            },
        )
        .unwrap();
    std::fs::write(&path, &bytes).unwrap(); // crash window: terminal durable, pending not unlinked
    assert!(journal.pending_keys().unwrap().is_empty());
    std::fs::write(path.with_file_name("wrong.pending"), &bytes).unwrap();
    assert!(journal.pending_keys().is_err());
    std::fs::remove_file(path.with_file_name("wrong.pending")).unwrap();
    std::fs::write(&path, b"{").unwrap();
    assert!(journal.pending_keys().is_err());
}
#[test]
fn not_applied_is_terminal_and_survives_publication_crash() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(dir.path().to_owned()).unwrap();
    let e = envelope(31);
    let pending = journal
        .before_send("https://example.com/work".into(), e.to_bytes().unwrap())
        .unwrap();
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original = std::fs::read(&path).unwrap();
    journal.finish_not_applied(&pending).unwrap();
    journal.finish_not_applied(&pending).unwrap();
    std::fs::write(&path, original).unwrap(); // published terminal, interrupted unlink
    assert!(journal.pending_keys().unwrap().is_empty());
    let saved = journal.load(&e.id.key()).unwrap();
    assert_eq!(saved.outcome, Some(JournalOutcome::NotApplied));
    assert_eq!(saved.endpoint, pending.endpoint);
    assert_eq!(saved.envelope, pending.envelope);
    assert!(
        journal
            .finish(
                &pending,
                RecordedOutcome::Rejected {
                    code: "ClientExists".into(),
                }
            )
            .is_err()
    );
}

#[test]
fn legacy_recorded_outcome_keeps_exact_envelope_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let journal = Journal::open(dir.path().to_owned()).unwrap();
    let e = envelope(32);
    let pending = journal
        .before_send("https://example.com/work".into(), e.to_bytes().unwrap())
        .unwrap();
    let outcome = RecordedOutcome::Rejected {
        code: "ClientExists".into(),
    };
    journal.finish(&pending, outcome.clone()).unwrap();
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut legacy = serde_json::to_value(&pending).unwrap();
    legacy["outcome"] = serde_json::to_value(&outcome).unwrap();
    std::fs::write(path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let saved = journal.load(&e.id.key()).unwrap();
    assert_eq!(
        saved.outcome,
        Some(JournalOutcome::Recorded(outcome.clone()))
    );
    assert_eq!(saved.envelope, pending.envelope);
    journal.finish(&pending, outcome).unwrap();
}

#[cfg(unix)]
#[test]
fn existing_permissions_are_private_and_symlink_leaf_is_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    Journal::open(dir.path().to_owned()).unwrap();
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let link = dir.path().join("link");
    symlink(dir.path(), &link).unwrap();
    assert!(Journal::open(link).is_err());
}
