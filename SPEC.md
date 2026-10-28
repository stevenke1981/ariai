# 第一階段規格

程式名稱 ariai；Rust 2024；Tokio async I/O；reqwest + rustls；axum HTTP JSON-RPC。無 aria2/libaria2 依賴。

公開型別位於 src/model.rs。Engine 的介面為：

```rust
pub async fn download(request: DownloadRequest, cancel: CancellationToken,
    progress: tokio::sync::watch::Sender<DownloadProgress>) -> anyhow::Result<DownloadResult>;
```

暫停／移除以 cancellation token 停止網路與寫入，engine 取消以 Err 回傳，manager 根據自己的請求狀態判斷 paused/removed，禁止直接 abort 後立刻重啟同一輸出。Range 回應必須驗證 Content-Range 與預期位移；伺服器忽略 Range 時重新串流，不能把 200 當作分段。續傳使用 `.ariai` 自有控制格式，與 `.aria2` 不互通；只在來源 identity／長度／validator 相容時使用已有分段，否則安全重建。只有完成且校驗成功後才公布最終結果。

Manager 是可 Clone 的 handle，公共 async 介面：

```rust
pub fn new(options: ManagerOptions) -> anyhow::Result<Self>;
pub async fn add(&self, request: DownloadRequest) -> anyhow::Result<String>;
pub async fn get(&self, gid: &str) -> Option<DownloadSnapshot>;
pub async fn list(&self) -> Vec<DownloadSnapshot>;
pub async fn pause(&self, gid: &str) -> anyhow::Result<()>;
pub async fn unpause(&self, gid: &str) -> anyhow::Result<()>;
pub async fn remove(&self, gid: &str) -> anyhow::Result<()>;
pub async fn wait_idle(&self);
pub async fn shutdown(&self) -> anyhow::Result<()>;
pub async fn save_session(&self, path: &Path) -> anyhow::Result<()>;
pub async fn load_session(&self, path: &Path) -> anyhow::Result<usize>;
```

RPC 介面 `rpc::serve(listener: TcpListener, manager: DownloadManager, secret: Option<String>, shutdown: CancellationToken) -> Result<()>`。預設 loopback；配置 secret 時 aria2 token:SECRET 為第一個 params。實作 addUri/tellStatus/tellActive/tellWaiting/tellStopped/pause/unpause/remove/getGlobalStat/getVersion/getSessionInfo/saveSession/shutdown 與可行的 all variants；未實作方法必須回 method-not-found，不能假成功。返回 aria2 欄位字串長度／速度及 gid 狀態。XML-RPC、WS／通知本期尚未實作。

Config 公共 API `apply_option(options: &mut DownloadOptions, key: &str, value: &str) -> Result<()>` 供 RPC 及 CLI 共用；未知設定必須明確拒絕。CLI 公共 `pub async fn run() -> Result<()>`。設定優先序 default < config < CLI < input per-item。CLI positional URIs 預設同一檔鏡像，`--force-sequential` 分開工作。JSON session 是 ariai 格式，不能當作 aria2 的 save-session 文字格式。

錯誤不偽裝完成；校驗失敗不回 Complete；下載輸出避免無意覆寫。不可透過 --check-certificate=false 預設關閉 TLS 驗證。完整功能對照以 docs/FEATURES.md 為準。
