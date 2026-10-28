# Phase 4 驗收紀錄

2026-10-28，Windows 本機；本分支尚未宣稱完整 aria2 功能相容。

- PASS：`python -X utf8 tools/verify.py --target-dir C:/Users/steven/AppData/Local/ariai-target`，包含 rustfmt、76 項 Rust 測試（另外有 2 項需指定外部 aria2 的測試在一般測試中略過）、Clippy、release 建置、版本檢查及 19 組 release CLI smoke。命令與 SHA-256 見 `evidence/verification.json`。
- PASS：另指定官方 aria2 1.37.0 Windows 執行檔，執行 `cargo test --locked --test engine -- --ignored --nocapture`，2 項通過：HTTP `.aria2` 控制檔雙向接續，以及官方 aria2 讀取本程式輸出的文字 session 並續傳。測試皆使用 loopback HTTP 與內容比對。官方程式僅作對照，不是下載後端。測試檔來源：<https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip>。
- PASS：HTTP＋FTP 並行下載的共享配額、HTTP Range 和 HTTP 修復；FTP＋SFTP 同時修復共 8 bytes、4 B/s 限速耗時約 2.10 秒。這是同一 manager 的應用層配額，不含部分 metadata、探測與 socket 緩衝。
- PASS：受控 SOCKS5 代理實際轉送 torrent metadata 和 peer 資料；合成 Firefox／Chromium SQLite 與已提交 WAL、Digest auth-int、XML-RPC、FTP/SFTP 分片修復及 Windows 重新命名失敗保留原檔。
- NOT_RUN：實際使用者瀏覽器資料庫並行寫入、Chromium 加密 cookie 的解密、外部 FTP／SFTP 伺服器、公開 BT swarm／UDP tracker／DHT、真實 Ctrl+C、Linux／macOS 本機執行及 Rust 1.89。PR 的三平台 CI 狀態以 GitHub 當次結果為準。

BT 代理限於 SOCKS5 和明確初始 peer，停用 tracker、DHT、入站連線，且不保證 DNS 隱私；BT 全域下載和非零全域上傳限速目前拒絕。`.aria2` 互通僅涵蓋 HTTP(S) 已知長度、Range 單檔和 v1 格式，控制檔不含來源 validator。完整限制及其餘功能差距見 `docs/FEATURES.md`。
