# 規格來源

2026-10-28 讀取的一手來源：

- [aria2 官方手冊](https://aria2.github.io/manual/en/html/aria2c.html)：功能、CLI、input file、JSON-RPC。當次 HTML SHA-256 `4f0c8f1b9ba11d872f066fc89506f468056c3bae0958f907ade4e8f99e6ba070`；197 個長選項名稱另存 aria2-options.txt。
- [aria2 原始專案功能列表](https://github.com/aria2/aria2#features)：協定、BT extensions、Metalink、cookies、RPC 等完整功能族。
- [reqwest 官方 crate 文件](https://docs.rs/reqwest/0.12.28/reqwest/)：本期 HTTP client；啟用 rustls-tls、stream、json、socks，停用 default features。
- [axum 官方 crate 文件](https://docs.rs/axum/0.8.9/axum/)：HTTP JSON-RPC transport。
- [Tokio cancellation token](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)：任務取消邊界。
- [Clap 官方 crate 文件](https://docs.rs/clap/latest/clap/)：CLI 型別化解析。
- [librqbit 9.0.1](https://docs.rs/librqbit/9.0.1/librqbit/)：原生 Rust BitTorrent Session、metadata、檔案選取及統計介面。
- [russh 0.65.0](https://docs.rs/russh/0.65.0/russh/) 與 [russh-sftp 3.0.1](https://docs.rs/russh-sftp/3.0.1/russh_sftp/)：SSH、主機金鑰、認證與 SFTP 檔案介面。
- [quick-xml 0.42.0](https://docs.rs/quick-xml/0.42.0/quick_xml/)：namespace-aware Metalink XML 解析。
- [cookie_store 0.22.0 原始專案](https://github.com/pfernie/cookie_store)：cookie scope／expiry，搭配 Netscape 格式輸入輸出。
- [httpdate 1.0.3](https://docs.rs/httpdate/1.0.3/httpdate/)：HTTP cookie 日期格式。

0.3.0 再查閱 aria2 官方手冊的 XML-RPC／multicall token、seed-ratio／seed-time、check-integrity 語義；librqbit 限速與統計亦對照 Cargo registry 內鎖定版本的原始碼。

依賴的實際解析版本以 Cargo.lock 為準；最新網站版本不代表此專案使用的版本。aria2 文件是相容性規格參考，專案不包含 aria2 原始碼或執行檔。
