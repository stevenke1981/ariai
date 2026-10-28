# ariai 0.3.0 implementation plan

Goal: continue native Rust aria2 parity from 0.2.0. Completion of this phase does not mean complete aria2 compatibility.

## Ownership and acceptance

| Owner | Files | Required evidence |
|---|---|---|
| HTTP worker, Terra/xhigh | engine.rs, http.rs, tests/http_auth.rs, tests/engine.rs | Deterministic resume assertions; real Digest challenges and cookie load/save transfers |
| RPC worker, Terra/xhigh | rpc.rs, xmlrpc.rs, tests/xmlrpc.rs, tests/rpc_extended.rs | Actual XML-RPC HTTP calls sharing execution and token auth; multicall results and faults |
| BT worker, Terra/xhigh | torrent.rs, tests/torrent.rs | Controlled peer transfers, limits, seed stopping, actual upload/connection metrics |
| Root | model, manager, config, CLI, Cargo, integrity module, repair tests, documentation | Only damaged Metalink pieces fetched; good pieces retained; failure does not publish corrupt output |

All workers share the checkout, preserve other edits, and never spawn recursive coordinators. Coordinator: native Luna/medium; integration: configured Sol/high; independent acceptance: executable workflow verifier. Cargo artifacts use the local disk target directory.

## Shared contracts

DownloadOptions gains load_cookies/save_cookies: Option<PathBuf>, http_auth_challenge: bool (default false), max_upload_limit: u64 (default 0), seed_ratio: Option<f64> (default None), check_integrity: bool (default false), and seed_time_explicit: bool. The legacy immediate stop remains unless seeding is requested. Positive seed time and positive ratio stop when either condition is reached; zero seed ratio removes the ratio stopping condition. Explicit seed-time=0 disables seeding.

DownloadProgress and DownloadSnapshot gain upload_length/upload_speed/connections: u64, serde defaults. DownloadResult remains unchanged. Manager retains uploaded bytes when a task stops and resets current speeds/connections to zero. RPC reports these measured fields.

HTTP worker supplies a reusable native HttpSession, including credentials, Digest retries and cookies. Root and HTTP owner agree the exact API before repair integration. Engine calls integrity repair before final verification/publication and optionally repairs an existing target for check-integrity plus Metalink piece hashes. Repair initially supports HTTP(S) mirrors, validates exact Content-Range/length and each repaired hash, and never marks unsupported protocol repair as implemented.

## Gates

Fix observed Ubuntu scheduling-sensitive resume test without weakening validator-change coverage. Run targeted tests during work, then one integration pass and fmt, locked tests, Clippy, release build and actual release smoke through the workflow verifier. Update feature/option coverage and Windows portable package. Commit/push the validated phase under the existing authorization and report the actual remote SHA and CI state. Public swarm, live SFTP hosts and complete aria2 option parity remain separate from loopback evidence.
