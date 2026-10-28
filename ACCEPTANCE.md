# ariai 0.2.0 原生協定擴充驗收

驗收日期：2026-10-28。平台：Windows x64，Rust／Cargo 1.99.0。使用者選擇所有下載協定原生 Rust，接受分階段對齊。本輪交付 FTP/SFTP、BitTorrent/Magnet、Metalink 3/4 與擴充 RPC；仍未宣告完整 aria2 相容。

| 項目 | 結果 | 當次證據 |
|---|---|---|
| cargo fmt --all -- --check | PASS | evidence/format.log |
| cargo test --locked | PASS | evidence/tests.log；32 項，0 failed／0 ignored |
| Clippy all-targets -D warnings | PASS | evidence/clippy.log |
| Windows release 建置 | PASS | evidence/release.log |
| release --version | PASS：ariai 0.2.0 | evidence/version.log |
| release CLI／RPC 端到端 | PASS：15 組 | evidence/cli-smoke.log |
| 真實 release HTTPS／預設 TLS／內容比對 | PASS：91 bytes | evidence/https-smoke.json；Rust 官方站台，與 Python 獨立取得內容一致 |
| 獨立 workflow verifier | PASS：exit code 0 | evidence/router-verification.json；job 3b06733f9fdc4b08b94922006b3ee76b |
| 一次獨立整合 | PASS | evidence/integration.md |

測試分類：CLI 8、HTTP engine 5、Metalink 5、model 3、FTP/SFTP 4、既有 RPC／manager 2、擴充 RPC 3、BitTorrent 2。各測試可包含多個實際連線情境。

- FTP：本機 Tokio FTP server 實測主動／被動、編碼路徑、帳密拒絕、取消後 REST 續傳、checksum 拒絕發布及校驗失敗後鏡像切換。
- SFTP：本機真實 russh server 實測內容傳輸、密碼／私鑰認證、SHA-256 pinned host key、known_hosts 接受與拒絕。未使用外部 ssh 或 aria2c 程序。
- BitTorrent：librqbit loopback peer 實際傳送檔案；本機 torrent、file URL、HTTP metadata、magnet、選取第 2 檔及逐檔進度；本實作下載後向另一個 peer 實際做種上傳。另驗證取消、listener 釋放、無效 metadata／索引／多來源明確拒絕。
- Metalink：3/4 namespace、相對 URL、重新導向後基準網址、鏡像順序、選檔、長度及整檔／分片 hash、DTD／路徑與格式錯誤拒絕；本機 HTTP 完整下載，校驗失敗不發布。
- RPC：WebSocket 認證前不發事件、token secret、batch、Start/Complete 事件、base64 metadata、新增工作與逐檔資訊、等待佇列排序／修改選項。活動工作修改選項及不支援的 web-seed 注入會回錯誤。

15 組 release smoke 涵蓋：分段＋SHA-256＋中文路徑、忽略 Range 降級、Basic＋header、鏡像故障切換、redirect、force-sequential、checksum 錯誤、零長檔案、錯誤 Content-Range、未知協定／旗標、CLI Metalink 本機與 HTTP 相對 URL、CLI Torrent 已有資料驗證、RPC Metalink 提交與檔案資訊、RPC 程序下載／session／shutdown。

完整命令、退出碼、輸出 SHA-256、Rust 測試來源 SHA-256 與執行檔 SHA-256 位於 evidence/verification.json。當次 release 執行檔 SHA-256：`96f4b28248271d5f0095f618efbac11488a26ad09e80540bddab101d9bbbb0cd`。

## 限制與 NOT_RUN

- NOT_RUN：公開網路 DHT／HTTP/UDP tracker、IPv6 專項、SFTP 中斷續傳、加密私鑰 passphrase、SHA-1／MD5 SSH 指紋、FTP IPv6 主動模式、實際 HTTP/SOCKS 代理、精確限速／長時間效能、實際 Ctrl+C 終端操作、Linux／macOS、Rust 1.89 最低版本、遠端 CI。
- 尚未實作或未逐項對齊：BT proxy／限速／seed ratio／完整 extensions、混合 BT+HTTP/FTP/SFTP 來源、Metalink HTTP discovery／分片修復、完整 cookies／Digest／TLS client certificate、XML-RPC／multicall／動態全域設定及其餘 CLI 語義。詳見 docs/FEATURES.md。
- FTP/SFTP 採單串流；FTP 需要 SIZE，MDTM 不可用時重啟下載；SSH host certificate 拒絕。FTP/SFTP/BT proxy 明確拒絕。
- BT 預設 seed-time=0、忽略 out、只接受單一來源；listen-port 僅單埠。RPC uploadLength／uploadSpeed／connections 暫回 0，不表示沒有實際做種或連線。
- ariai 控制檔與 JSON session 不與 aria2 `.aria2`／文字 session 互通。HTTP/FTP/SFTP 鏡像採故障切換，未同時跨協定拼接。
- 197 個上游長選項中，目前 CLI help 列出其中 44 個名稱；這是旗標受理盤點，不是功能完成比例。逐項表見 docs/OPTION-COVERAGE.md。

三位實作子代理依使用者指定採 gpt-6.1-terra/xhigh，Luna/medium 協調，配置的 Sol/high 整合。角色與派工見 evidence/agents.json；模型標籤為 Host 回報，非 provider wire-model 證明。完成事件、整合及真正執行的獨立驗收分開記錄。
