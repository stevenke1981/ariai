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

## Phase 3 integration - 2026-10-28

Workspace: `Z:\ariai`. Configured role: `gpt-6.1-sol/high`, native integration worker. Workflow: `workflow8d7bf6b60bcf497fb750ebd91112cd66`. Scope: one integration pass against `PLAN-PHASE3.md`, focusing on shared HTTP authentication/cookies, XML-RPC execution/authentication and multicall, measured BitTorrent statistics and seed stopping, manager snapshot propagation, and Metalink streaming repair/publication. No recursive agents or repeated whole-project review were used.

Concrete defect and fix:

- `src/engine.rs`: a fresh download from a truncated first mirror can be extended by successful Metalink piece repair. The engine previously returned the pre-repair assembly length. A deterministic 8-byte first mirror plus 10-byte complete mirror reproduced a 10-byte verified output with `DownloadResult.total_length == 8`. Read the assembly metadata after successful repair so the result and final progress report its actual verified length.
- `tests/repair.rs`: extended the loopback fixture with a truncated mirror. The regression verifies final bytes, result length, total/completed progress, and that the complete mirror supplies only the missing `bytes=8-9` piece. It failed before the fix (`left: 8`, `right: 10`) and passed afterward.
- Applied the required `cargo fmt --all` to the shared core. No additional behavior changes were needed in HTTP, XML-RPC, manager, or BitTorrent during this integration pass. Root-owned smoke/package/docs/model-test additions were not edited by this role.

Executed validation, using `C:\Users\steven\AppData\Local\ariai-target` throughout:

| Check | Result | Evidence |
|---|---|---|
| Initial combined targeted run: `cargo test --locked --test http_auth --test engine --test xmlrpc --test rpc_extended --test torrent --test repair` | PASS | 26 tests total: HTTP auth 8, engine 5, XML-RPC HTTP 1, extended RPC 4, torrent 7, repair 1. Includes actual loopback Digest/cookies, token/multicall HTTP and WebSocket behavior, controlled-peer upload/download throttling and statistics, tracker discovery, selective repair and malformed-range/cancellation checks. |
| New truncated-mirror regression before fix | EXPECTED FAIL | `cargo test --locked --test repair` failed at the final result-length assertion: reported 8 instead of 10. Output bytes had already verified as correct. |
| `cargo test --locked --test repair --test engine` after the fix | PASS | Repair 1 and engine 5, no failures. Includes the added truncated-source length/progress regression. |
| `cargo fmt --all -- --check` after the fix | PASS | Exit 0, no diagnostics. |
| `cargo clippy --locked --all-targets -- -D warnings` after the fix | PASS | Exit 0; no warnings or errors. |
| Final scoped diff whitespace check | PASS with Windows CRLF setting | `git -c safe.directory=Z:/ariai -c core.whitespace=cr-at-eol diff --check` exited 0. The plain check flagged CRLF as trailing whitespace in torrent source/tests and option coverage; no source changes were made for those line-ending-only reports. |
| Full locked suite, release build, executable smoke, portable package | NOT_RUN by this role | Reserved for the independent executable workflow verifier and root after this integration completes. |
| Public swarm, live Internet Digest/Cookie hosts, cross-platform runtime | NOT_RUN by this role | Loopback evidence does not establish these environments. |

Final scoped Git inspection is available with `git -c safe.directory=Z:/ariai ...`; the initial unscoped invocation did not recognize the SMB-mounted checkout. This corrects the earlier integration notes' unverified no-repository assumption for the current checkout. No commit, push, release, or deployment is claimed by this role. Core source and tests are stable for independent acceptance.
