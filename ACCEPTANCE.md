# ariai 0.3.0 驗收

驗收日期 2026-10-28，Windows x64，Rust／Cargo 1.99。以原生 Rust 協定實作分階段對齊 aria2；本版本尚未完成全部相容性。

| 項目 | 結果 | 證據 |
|---|---|---|
| 格式、完整 Rust 測試、all-target Clippy | PASS | evidence/verification.json；50 測試、0 failed |
| Windows release 建置與版本 | PASS | evidence/verification.json；ariai 0.3.0 |
| release 執行檔本機 CLI／RPC smoke | PASS | evidence/cli-smoke.log；19 組 |
| release 真實 HTTPS、TLS 驗證及內容比對 | PASS | evidence/https-smoke.json；91 bytes |
| 獨立 workflow verifier | PASS | evidence/router-verification.json；exit 0，job 8d7bf6b60bcf497fb750ebd91112cd66 |
| 一次整合與缺陷修復 | PASS | evidence/integration.md Phase 3 |
| Linux／macOS 本版本 CI | NOT_RUN，待推送後的實際結果 | 前版 0.2.0 Windows/macOS PASS、Ubuntu 測試失敗；本版修正排程敏感斷言 |

本機回環測試涵蓋 HTTP Digest MD5／SHA-256 與 sess、認證失敗、redirect 和 Netscape cookies；XML-RPC 認證、base64、struct、multicall 及 fault；HTTP tracker 和真實 BT peer 傳輸、上下載限速、做種條件、統計及取消；Metalink 損壞／缺失分片、短檔修復、Range 拒絕與正確分片保留。整合過程重現並修正修復後長度回報錯誤。

19 組 release smoke 包含真實 CLI Digest、cookie save/load、選擇性分片修復及 XML-RPC multicall，另保留分段、續傳相關下載、FTP/SFTP 以外的原有 HTTP／Metalink／Torrent／RPC 基本流程。其他協定細節主要以本機 Rust 測試驗證。

執行檔 SHA-256：`35113abbe7f9408351a77675bef5a72496f55ab2fdb6a2c2013e6d2870dca1d7`。測試、來源與產物雜湊見 evidence/verification.json。CLI 接受上游 197 個選項名稱中的 50 個；旗標數不代表語義或功能完成比例。

剩餘差距包括 BT proxy、全域限速及其他 extensions、混合 BT／HTTP 來源、SQLite cookies、Digest auth-int、FTP/SFTP 分片修復、Metalink HTTP discovery、部分 XML-RPC 類型與 RPC 方法，以及 aria2 控制檔／session 相容。BT 限速非零範圍為 16384..4294967295 bytes/s，並有初始 burst；未指定做種時即停止，與 aria2 預設不同。完整範圍見 docs/FEATURES.md。

NOT_RUN：本版 Linux／macOS runtime、Rust 1.89、UDP tracker／公開 swarm／DHT、真實 HTTP/SOCKS 代理、SFTP 中斷續傳、加密私鑰 passphrase、實際 Ctrl+C 及長時間精確效能。Luna／Terra／Sol 的角色與工作紀錄見 evidence/agents.json；這些是 Host 回報，非 provider wire-model 證明。
