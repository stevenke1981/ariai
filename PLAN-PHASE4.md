# Phase 4: aria2 compatibility expansion

Base: 700af398b6939a56c36470bfdcc8743ee77277c2. Branch: feat/aria2-compat-phase4. User requests native Rust implementations, tested integration, commit, push and PR.

| Owner | Scope | Acceptance |
|---|---|---|
| HTTP | src/http.rs, tests/http_auth.rs | SQLite Firefox/Chromium cookies and Digest auth-int real HTTP challenge fixtures; state encrypted-cookie limitations |
| RPC | src/rpc.rs, src/xmlrpc.rs, tests/xmlrpc.rs, tests/rpc_extended.rs | Additional XML-RPC value types and aria2 RPC calls, authentication, real clients and negative inputs |
| Remote | src/protocols/ftp.rs, src/protocols/sftp.rs, tests/remote.rs | Selective FTP/SFTP repair of existing and downloaded files, fixtures establish good pieces preserved and failure behavior |
| Root | BT proxy, aggregate limits, aria2 session/control compatibility, shared options, integration, docs | Real proxy, concurrent bandwidth and bidirectional aria2 interoperability where feasible; unsupported cases fail explicitly |

One author per worktree; common Cargo/model/API changes are serialized by root. Workers provide patch commits and exact dependency/interface requirements; root integrates sequentially. Each worktree has its own target directory. Luna medium selects worker model/effort; Sol/high integration then actual independent fmt/test/Clippy/release/smoke verifier. Real aria2 fixture interop may use aria2 as a test comparator only, never as a downloader backend. Final delivery includes evidence, remaining gaps, commit, pushed branch and PR.
