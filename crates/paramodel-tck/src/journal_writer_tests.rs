// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for the executor-side
//! [`paramodel_executor::JournalWriter`].

use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::Fingerprint;
use paramodel_executor::{ExecutionId, JournalEvent, JournalEventKind, JournalSequence, JournalWriter};
use ulid::Ulid;

use crate::providers::JournalWriterProvider;

fn fresh_execution_id() -> ExecutionId {
    ExecutionId::from_ulid(Ulid::new())
}

fn make_event(execution: ExecutionId, seq: u64) -> JournalEvent {
    JournalEvent {
        sequence:     JournalSequence::new(seq),
        execution_id: execution,
        timestamp:    Timestamp::now(),
        kind:         JournalEventKind::ExecutionCompleted {
            success:  true,
            duration: Duration::from_millis(seq),
        },
    }
}

/// `write` + `since(None)` returns every appended event in order.
pub async fn tck_journal_writer_write_then_since<P: JournalWriterProvider>(
    provider: &P,
) {
    let w = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=4u64 {
        w.write(make_event(exec, i)).await.expect("write");
    }
    let events = w.since(None).await.expect("read");
    assert_eq!(events.len(), 4);
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.sequence.get(), u64::try_from(i + 1).unwrap());
    }
}

/// `since(Some(N))` returns only events with `sequence > N`.
pub async fn tck_journal_writer_since_gate<P: JournalWriterProvider>(
    provider: &P,
) {
    let w = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=5u64 {
        w.write(make_event(exec, i)).await.unwrap();
    }
    let later = w.since(Some(JournalSequence::new(2))).await.unwrap();
    assert!(later.iter().all(|e| e.sequence.get() > 2));
    assert_eq!(later.len(), 3);
}

/// `last_event` returns the most recent event for the execution.
pub async fn tck_journal_writer_last_event<P: JournalWriterProvider>(
    provider: &P,
) {
    let w = provider.fresh();
    let exec = fresh_execution_id();
    assert!(w.last_event(&exec).await.is_none());
    for i in 1..=3u64 {
        w.write(make_event(exec, i)).await.unwrap();
    }
    let last = w.last_event(&exec).await.expect("has last");
    assert_eq!(last.sequence.get(), 3);
    let _ = Fingerprint::of(b""); // quiet unused-import warnings.
}
