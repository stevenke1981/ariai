# ariai 實作計畫

使用者決策（2026-10-28）：所有下載協定用原生 Rust，接受分階段補齊。禁止以 aria2c 子程序冒充原生實作。

## 第一階段交付

1. 原生 HTTP/HTTPS：串流、Range 分段、續傳、鏡像重試、SHA-256/SHA-1/MD5、請求標頭、代理、認證、限速。
2. 下載管理：並行佇列、狀態／進度、暫停／繼續／移除、JSON session 儲存及載入、正常中止。
3. CLI：aria2 常用參數、設定檔、input file、stdin、JSON 輸出；HTTP JSON-RPC 核心方法。
4. 本機伺服器整合測試、Windows release 可執行檔、使用說明與功能差距清單。

## 所有權與執行

| Owner | 模型 | 可寫入範圍 |
|---|---|---|
| engine | gpt-6.1-terra/xhigh | src/engine.rs, tests/engine.rs |
| manager | gpt-6.1-terra/xhigh | src/manager.rs, src/rpc.rs, tests/rpc.rs |
| cli | gpt-6.1-terra/xhigh | src/cli.rs, src/config.rs, tests/cli.rs |
| root | 目前 Host | 共享 model、Cargo、文件、整合／打包腳本 |

共享工作目錄以互斥檔案 ownership 保護；整合者在所有作者完成後才改動其檔案。Luna 原生協調，並記錄 dispatch 與獨立 verifier。

## 驗收門檻

- cargo fmt --check、cargo test --locked、cargo clippy --locked --all-targets -- -D warnings。
- 實測本機 HTTP 分段、無 Range 降級、校驗失敗、續傳、鏡像故障切換及 JSON-RPC 狀態。
- CLI 程序 smoke test：下載後與來源內容雜湊一致、參數錯誤非零退出。
- cargo build --release --locked，對產物執行 --help／--version 及實際下載。
- FTP/SFTP、BT、Metalink 等未實作功能明列未完成，不能因 HTTP 測試通過而宣稱 aria2 完整相容。

## 後續里程碑

- 第二階段：FTP/FTPS/SFTP 與驗證、代理、遠端時間／續傳。
- 第三階段：torrent／magnet、tracker、DHT、PEX、MSE、web seed、做種、選檔與 peer 管理。
- 第四階段：Metalink 3/4、分塊校驗／修復、多協定來源協作。
- 第五階段：完整 aria2 CLI／設定／RPC／事件相容性、跨平台與長時間壓力驗收。

