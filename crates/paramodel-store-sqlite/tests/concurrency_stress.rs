// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Concurrent-writer stress test. With WAL mode enabled, many tasks
//! appending to distinct executions should complete without
//! serialisation deadlock or corruption.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use paramodel_executor::{ExecutionId, JournalEvent, JournalEventKind, JournalSequence};
use paramodel_persistence::JournalStore;
use paramodel_store_sqlite::SqliteStore;
use ulid::Ulid;

fn ev(seq: u64, exec: ExecutionId, ts: i64) -> JournalEvent {
    JournalEvent {
        sequence:     JournalSequence::new(seq),
        execution_id: exec,
        timestamp:    Timestamp::from_second(ts).unwrap(),
        kind:         JournalEventKind::ExecutionCompleted {
            success:  true,
            duration: Duration::from_secs(1),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_concurrent_appends_to_distinct_executions() {
    let store = Arc::new(SqliteStore::in_memory().await.unwrap());
    const N_EXECS: u64 = 32;
    const EVENTS_PER_EXEC: u64 = 32;

    let mut handles = Vec::with_capacity(N_EXECS as usize);
    for exec_n in 1..=N_EXECS {
        let store = store.clone();
        let exec = ExecutionId::from_ulid(Ulid::from_parts(exec_n, 1));
        handles.push(tokio::spawn(async move {
            for seq in 1..=EVENTS_PER_EXEC {
                store
                    .append(&ev(seq, exec, 1_700_000_000 + seq as i64))
                    .await
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Verify each execution received the full sequence.
    for exec_n in 1..=N_EXECS {
        let exec = ExecutionId::from_ulid(Ulid::from_parts(exec_n, 1));
        let events: Vec<_> = store
            .replay(&exec, None)
            .await
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            events.len(),
            EVENTS_PER_EXEC as usize,
            "execution {exec_n} received {} events, expected {EVENTS_PER_EXEC}",
            events.len(),
        );
    }
}
