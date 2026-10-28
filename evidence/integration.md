# Integration evidence

Date: 2026-10-28. Workspace: `Z:\ariai`. Role: native integration worker, configured `gpt-6.1-sol/high`. Job: `job2d554898c1a2468f8456ef51eac6cbeb`.

## Scope and changes

Performed one lightweight source integration pass against PLAN.md, SPEC.md, README.md, and the first-phase HTTP/HTTPS engine, model, manager, CLI/config, RPC, and relevant tests. No further concrete lifecycle or integrity defect was established in this pass.

Fixed two actual Clippy diagnostics:

- `src/engine.rs`: replaced separate `start`/`end` parameters of `transfer_part` with a `(u64, u64)` range, reducing its parameter count from eight to seven. Call-site values and transfer behavior are unchanged.
- `tests/engine.rs`: replaced `is_some` followed by `unwrap` in the HTTP fixture with tuple pattern matching.

No unrelated source, documentation, tool, or dependency changes were made by this role. A final lightweight inspection confirmed these edits match their intended scope. This workspace has no `.git` repository, so a Git diff was unavailable.

## Executed validation

Target directory for all Cargo commands: `C:\Users\steven\AppData\Local\ariai-target`. No isolated project copy was used.

| Check | Result | Evidence |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | Completed without formatting diagnostics after the final edits. |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS | Finished dev profile; no warnings or errors after both diagnostic fixes. |
| `cargo test --locked --test engine` | PASS | 5 passed, 0 failed: segmented bytes/checksum, Range-ignore fallback, checksum rejection, mirror failover, cancellation/resume and changed-validator restart. |
| Full test suite | NOT_RUN by this role | Reserved for independent verifier. Parent-reported earlier results are not claimed as this role's execution. |
| Release build and release CLI smoke | NOT_RUN by this role | Reserved for independent verifier. |
| Actual Ctrl+C signal | NOT_RUN | Cancellation-token test is not an operating-system Ctrl+C test. |

## Delivery limits

This is first-phase native HTTP/HTTPS work. Segments use a compatible probed source with mirror failover; simultaneous multi-source aggregation is not established. FTP/SFTP, BitTorrent/magnet, Metalink, full aria2 parity, and cross-platform runtime acceptance remain outside this integration evidence. No commit, remote push, release, deployment, or current-conversation model switch is claimed.

## Phase 2 integration - 2026-10-28

Workspace: `Z:\ariai`. Configured role: `gpt-6.1-sol/high`, native final integrator. Scope: one necessary integration check of the new protocol dispatch, shared file-result contract, manager/RPC seam, and actual compiler diagnostics against `PLAN-PHASE2.md`. No exhaustive repository review or additional review chain was performed.

Concrete changes:

- `src/metalink.rs`: preserve descending Metalink 3 preference ordering using `sort_by_key(Reverse(...))`, resolving the actual `unnecessary_sort_by` diagnostic.
- `src/rpc.rs`: collapse two nested conditions into let chains, resolving actual `collapsible_if` diagnostics without changing RPC behavior.
- `src/torrent.rs`: reject multiple metadata sources explicitly. The implementation reads only the first source, so accepting mirror lists silently discarded the remaining sources. A single-source check now fails before opening a BitTorrent session.
- `tests/torrent.rs`: assert the unsupported torrent mirror-list error before metadata parsing or network activity.

Executed validation, using `C:\Users\steven\AppData\Local\ariai-target` throughout:

| Check | Result | Evidence |
|---|---|---|
| `cargo fmt --check` after all source edits | PASS | Exit 0, no diagnostics. |
| `cargo clippy --locked --all-targets -- -D warnings` after all source edits | PASS | Exit 0, dev profile completed with no warnings. Initial run found precisely the three diagnostics fixed above. |
| `cargo test --locked --test metalink --test rpc_extended` after lint fixes | PASS | Metalink: 5 passed; extended RPC: 3 passed; no failures. Includes manifest download/hash verification, queue/options, WebSocket authentication/events, and persisted addTorrent metadata/file results. |
| `cargo test --locked --test torrent` after mirror rejection | PASS | 2 passed, 0 failed. Includes invalid metadata/selection, explicit mirror rejection, cancellation/listener release, selected-file local/file URL/HTTP metadata/magnet transfer and second-peer reseeding. |
| Initial affected torrent test launch | TRANSIENT FAILURE, resolved | Windows OS error 5 replacing `debug\ariai.exe` while root's owned smoke process used it. After root reported normal shutdown and process inventory was empty, retry passed. |
| FTP/SFTP loopback tests | NOT_RUN by this role | Parent owns earlier protocol evidence; these modules were inspected but not edited here. |
| Full suite, release build, packaged executable acceptance | NOT_RUN by this role | Reserved for the independent verifier after integration. |

Final lightweight self-check: source edits remain limited to the concrete items listed above. The workspace has no `.git` directory, so Git diff is unavailable. RPC torrent files preserve per-file selected/length/completed values through the manager; RPC uploadLength/uploadSpeed/connections remain zero placeholders, and root was notified to document that limitation. This pass does not establish complete aria2 parity, public-network tracker/DHT interoperability, cross-platform runtime behavior, or release/publication status.
