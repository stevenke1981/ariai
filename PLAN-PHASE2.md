# ariai 0.2.0 協定擴充計畫

2026-10-28：延續原生 Rust 全協定方向。本輪新增 FTP/SFTP、torrent/magnet、Metalink 3/4 與 RPC 操作。完整 aria2 細節相容性依功能矩陣逐項處理，不把單一成功傳輸當作全部 extensions 通過。

| Owner | 模型 | 寫入範圍 |
|---|---|---|
| remote | gpt-6.1-terra/xhigh | src/protocols/ftp.rs, src/protocols/sftp.rs, tests/remote.rs |
| metalink | gpt-6.1-terra/xhigh | src/metalink.rs, tests/metalink.rs |
| torrent | gpt-6.1-terra/xhigh | src/torrent.rs, tests/torrent.rs |
| root | Host | model、engine dispatch、共用 protocol I/O、CLI/config、manager/RPC、Cargo、文件及驗收腳本 |

原生 FTP 使用 Tokio TCP；SSH/SFTP 採 russh + russh-sftp，BT 使用 librqbit 的 Rust 協定核心；不啟動 aria2c、curl、ssh 或其他傳輸子程序。新增 russh 將最低 Rust 宣告從 1.88 提高至 1.89；本機工具鏈實測仍以 1.99 為準。

公開介面：FTP/SFTP/torrent 模組各提供與 engine 相同的 `pub async fn download(DownloadRequest, CancellationToken, watch::Sender<DownloadProgress>) -> Result<DownloadResult>`。Remote 使用 `protocols::target(&request)`、`protocols::side_path(path,suffix)`、`protocols::publish(partial,target,&options)`；publish 做長度／checksum／piece-hash 驗證與最後 rename。取消必須結束傳輸後才讓管理器恢復工作。

Metalink：`pub fn parse(bytes: &[u8], base: Option<&url::Url>, defaults: &DownloadOptions) -> Result<Vec<DownloadRequest>>`。支援 namespace v3/v4、選檔、相對 URL、priority、檔名／尺寸／hash／piece hash；拒絕路徑穿越、DTD／外部 entity、無來源及不支援的必需資訊，不能忽略驗證資料假成功。共用 `model::selected_indices(spec: Option<&str>, count: Option<usize>) -> Result<Vec<usize>>`，回傳 0-based indices。

新選項型別以 src/model.rs 為準：FTP credentials/passive；SSH private key/passphrase/known_hosts/host-key digest；select_file；BT trackers/initial peers/DHT/listen port/seed seconds/follow_torrent；Metalink expected_length/piece_hashes。

驗收：真實 loopback FTP 與 SFTP fixtures（含認證失敗、取消／續傳、host key 拒絕）；離線受控 torrent peer 完成內容雜湊比對；Metalink 3/4 解析與透過管理器下載；既有 HTTP/CLI/RPC 回歸；fmt/test/Clippy/release 與產物 smoke；獨立整合一次與真正 workflow verifier。

RPC 介面補充：`/jsonrpc` 同時提供 HTTP POST 與 WebSocket GET upgrade，維持逐次 token secret 驗證；WebSocket 首次認證後可收到 Start/Pause/Stop/Complete/Error 事件。事件來自管理器 broadcast，暫停／移除事件在傳輸停止後送出；事件佇列溢出會斷線。`addTorrent` base64 metadata 寫入 `.ariai-metadata/<sha256>.torrent` 供 session 持續使用；不接受額外 web-seed URI。`addMetalink` 展開各選取檔案並回傳 gid 陣列。`changeOption` 僅接受 waiting 或傳輸已停止的 paused 工作；`changePosition` 操作等待／暫停工作，可用 POS_SET/CUR/END。force 操作使用相同取消收尾機制。

Torrent 每檔資訊透過 DownloadFile 回報 1-based index、path、length、completed_length、selected；HTTP/FTP/SFTP 仍是單檔。RPC uploadLength/uploadSpeed/connections 尚未接入真實統計，固定 0。多來源 Torrent 明確拒絕，避免只讀第一個 URI。做種預設 0 分鐘；全域設定、XML-RPC、multicall 與其餘 aria2 細節留待後續。
