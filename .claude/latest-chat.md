# Hand-off (end of session, 2026-11-21)

`.claude/progress.md` is the source of truth (item 74 is the newest). This is a short summary.

## Done this session
- Removed the telemetry queue from `Ev` (only `log` remains). Telemetry and subscribers will be bus work (events as a bus topic, as in cFS).
- File service is strictly one request in flight: no `N`, no tags, `file::Response = Result<Data, Error>`. `FileService` has `start/submit/poll/wait/handle`. `FileDevice` is a 3-state value (Idle, Waiting{deadline}, TimedOut) with `request/poll/wait/busy`. `file_client.rs` deleted.
- OSAL `Receiver::recv_timeout`; `Platform::Files: Clone + Send + 'static`.
- New `core/src/log_writer.rs` (`LogWriter`, `EVENT_LOG_TIMEOUT_US`); `event_log::batch` replaces `append`. `Core::tick` no longer blocks; `Core::stop` flushes (blocking, with timeout); `Core::flush_event_log` is public for tests.
- 369 tests pass, clippy and fmt clean. Docs (design.md, progress.md) updated.

## Open
- Sample run (8 s, release) still shows 2 overruns (at 1.97 s and 2.93 s); cause unknown. Before the change: one per event.
- `Core::tick` not yet asserted allocation-free; `write_file` uses `format!` (allocates on the worker thread, will trip the alloc guard).
- Next planned: software bus (designed in item 58, not written), then FILE and CFDP modules.

## Uncommitted
Everything after `741858c` is uncommitted (the user commits). Includes all of the above plus earlier file-service work (`core-api/src/file.rs`, `core/src/file_*.rs`, `log_writer.rs`, `osal/src/channel.rs`, `thread.rs`, tests/common).
