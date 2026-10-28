# ariai

原生 Rust 下載器，目標逐步對齊 aria2。包含 XML-RPC／multicall、Digest、瀏覽器 SQLite cookies、BT 限速／做種比例及 Metalink 分片修復；完整 aria2 相容性仍在分階段補齊。不呼叫 aria2c，也沒有 libaria2 相容後端。

提供 HTTP/HTTPS、FTP/SFTP、BitTorrent/Magnet 與 Metalink 3/4，包含來源識別續傳、鏡像切換、校驗、佇列、CLI、HTTP/WebSocket JSON-RPC 與下載事件。FTP/SFTP 目前採單串流；BitTorrent 使用 librqbit 原生協定核心。完整功能差距見 [功能矩陣](docs/FEATURES.md)，197 個上游長選項盤點見 [aria2-options.txt](docs/aria2-options.txt)。

[選項受理對照](docs/OPTION-COVERAGE.md) 由本次執行檔 help 產生；受理旗標與完整行為相容分開追蹤。

## 建置與使用

專案宣告最低 Rust 1.89；目前實測工具鏈為 1.99，1.89 專項驗證為 NOT_RUN。以 Cargo.lock 鎖定相依版本。

Windows 免安裝包解壓縮後可直接執行 `ariai.exe --help`；不需要安裝 Rust。下列 Cargo 步驟供自行建置使用。

```powershell
cargo build --release --locked
.\target\release\ariai.exe --help
.\target\release\ariai.exe -d downloads -o file.zip -s 8 --min-split-size=1M https://example.com/file.zip
```

同一命令的多個 URI 預設為同一檔案的鏡像，與 aria2 相同；`--force-sequential` 才將它們當成不同工作。

```powershell
ariai -c -o file.zip https://mirror1.example/file.zip https://mirror2.example/file.zip
ariai --force-sequential -j 3 https://example.com/a.zip https://example.com/b.zip
ariai --checksum=sha-256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef https://example.com/file
ariai --conf-path examples/ariai.conf --input-file urls.txt
```

校驗範例的 digest 為佔位值，請換成來源提供的正確值。未知選項／協定會回報錯誤；不會假裝支援。

設定優先序為預設值、設定檔、CLI，再加 input file 的單檔選項。Input file 一行是一個下載，多個鏡像用 tab 分隔；縮排設定套用至上一筆：

```text
https://example.com/a.zip
  out=a.zip
  split=4
https://example.com/b.zip
  out=b.zip
```

使用 `-i -` 從 stdin 讀取。設定檔採 `key=value`，範例見 [ariai.conf](examples/ariai.conf)。所有已開放旗標以 `ariai --help` 為準。

## FTP、SFTP、Torrent 與 Metalink

```powershell
ariai -c --ftp-user=alice --ftp-passwd=secret ftp://example.com/file.zip
ariai -c --ssh-known-hosts=C:\Users\alice\.ssh\known_hosts sftp://alice@example.com/file.zip
ariai --ssh-private-key=C:\Users\alice\.ssh\id_ed25519 sftp://alice@example.com/file.zip
ariai -T example.torrent --select-file=1,3-5 --seed-time=10
ariai 'magnet:?xt=urn:btih:REPLACE_WITH_INFO_HASH'
ariai -M files.meta4 --select-file=2
```

SFTP 必須通過 known_hosts 或 `--ssh-host-key-md=sha-256=HEX` 主機金鑰驗證；可用 URI 密碼或 `--ftp-user`／`--ftp-passwd`，私鑰可配 `--ssh-passphrase`。FTP 使用 EPSV/PASV，被動模式預設開啟；`--ftp-pasv=false` 使用主動模式。FTP/SFTP 代理尚未支援。

Torrent 接受本機路徑、file URL、HTTP(S) `.torrent` 與 magnet；依 metadata 命名檔案，忽略 `--out`。`--seed-time` 單位為分鐘，未要求做種時下載驗證完成即停止，與 aria2 預設行為不同。`--seed-ratio=1` 做種至上傳量達選取資料量；比例與正值時間並用時先達者停止。比例 `0` 不限比例，明確指定 `--seed-time=0` 則停用做種。`--max-download-limit`／`--max-upload-limit` 支援每 torrent 限速，0 不限速，非零值目前限 16384 至 4294967295 bytes/s，含初始 token bucket burst。`--enable-dht=false` 可關閉 DHT；`--bt-tracker` 接受逗號分隔 URL，`--listen-port` 目前只接受單一埠。`--bt-initial-peer=IP:PORT` 是 ariai 擴充選項。BT 可用 `--all-proxy=socks5://host:port` 搭配明確指定的初始 peer；代理模式停用 tracker、DHT 與入站連線，明確指定 tracker 或監聽埠會報錯。外部 checksum 仍不支援；HTTP metadata 的 Digest／cookies 尚未接入 BT 引擎。

`.meta4`／`.metalink` URI 可自動展開，`-M -` 從 stdin 讀取 XML。Metalink 支援檔案選取、相對 URL、鏡像順序、尺寸、整檔與分片 hash。HTTP(S)、FTP 和 SFTP 下載後若有分片 hash，會修復錯誤分片；`ariai -M files.meta4 --check-integrity` 可驗證並修復既有檔案，保留正確分片。截短檔案需 manifest size；只有整檔 hash 的既有檔案修復與 Metalink HTTP discovery 待補。目前整檔與分片 hash 都會驗證，與 aria2 同時提供兩者時的選擇語義不同。

HTTP 檔案與遠端 Metalink metadata 共用 Digest MD5／SHA-256（含 sess）及 qop=auth／auth-int；netrc 尚未支援。`--http-auth-challenge` 等待伺服器 challenge 再認證。`--load-cookies` 可讀 Netscape 或 Firefox／Chromium SQLite（含已提交 WAL），`--save-cookies` 輸出 Netscape 格式。Chromium 加密值不支援，請使用已關閉瀏覽器的穩定資料庫副本；多工作共同儲存同一 cookie 檔案的合併待補。

## HTTP／WebSocket JSON-RPC 與 XML-RPC

以前景程序啟動常駐服務（預設 loopback）：

```powershell
ariai --enable-rpc --rpc-listen-port=6800 --rpc-secret=change-me --save-session=queue.json
```

```powershell
$body = @{jsonrpc='2.0'; id='1'; method='aria2.addUri'; params=@('token:change-me', @('https://example.com/file.zip'), @{dir='downloads'; split='4'})} | ConvertTo-Json -Depth 5
Invoke-RestMethod http://127.0.0.1:6800/jsonrpc -Method Post -ContentType application/json -Body $body
```

完整方法清單由 `system.listMethods` 回傳。新增 `addMetalink`／`addTorrent`（base64 metadata）、`getUris`／`getFiles`／`getOption`、`getGlobalOption`、`changePosition`、`changeOption` 與 force 操作別名。設定僅可修改等待或已停止傳輸的暫停工作，活動中的修改會回錯誤。`addTorrent` 將 metadata 存於輸出目錄的 `.ariai-metadata`，以便 session 恢復；web-seed URI 注入尚未支援。

同一路徑 `ws://127.0.0.1:6800/jsonrpc` 支援 WebSocket JSON-RPC、batch 與五種下載事件：Start／Pause／Stop／Complete／Error。設定 secret 時，每次下載方法呼叫仍須帶 token；連線首次認證前不傳送下載事件。事件佇列溢出會關閉連線，需重新連線並查詢狀態。`http://127.0.0.1:6800/rpc` 提供 XML-RPC POST，共用方法與認證，支援 string/base64/int/i4/i8/double/boolean/array/struct/nil。dateTime 可解析並保留型別，但目前方法沒有日期參數，傳入會回參數錯誤。`system.multicall` 每個子呼叫各自帶 token，成功回單元素陣列、失敗回 fault struct；不接受巢狀 multicall。XML GET、全域動態設定及其他未列方法尚未實作。

RPC 的 uploadLength／uploadSpeed／connections 回報 BT 引擎實際統計，停止後保留上傳量、速率與連線數歸零；其他協定連線統計尚未接入。

## 續傳與 session

下載期間使用 `.ariai` 控制檔與分段暫存，`-c` 嘗試復原已有進度。來源 validator 不相容時重新下載，避免拼接不同版本。控制格式與 aria2 `.aria2` 不互通。

`--save-session=queue.json` 儲存未完成工作；`--load-session=queue.json` 載入。這是 ariai JSON 格式，不能當成 aria2 文字 input file。Ctrl+C 的設計行為是停止下載、保留可復原進度並以非零退出；本次實際送出 Ctrl+C 的操作驗收為 NOT_RUN。Session 會保存工作選項，包含設定的認證或標頭，請放在合適的本機位置。

## 驗證

```powershell
python tools/verify.py
```

執行格式檢查、測試、Clippy、release 建置與真實 CLI HTTP smoke，將命令、退出碼、輸出雜湊與產物雜湊存入 `evidence/verification.json`。測試使用本機伺服器，不需外部服務。`CARGO_TARGET_DIR` 可指定本機建置目錄以避開網路磁碟延遲。

CI 配置包含 Windows／Linux／macOS；只有實際執行結果能證明各平台通過。當次交付的已執行與 NOT_RUN 項目見 [ACCEPTANCE.md](ACCEPTANCE.md)。

[PLAN.md](PLAN.md)／[SPEC.md](SPEC.md) 保留第一階段規格；[PLAN-PHASE2.md](PLAN-PHASE2.md)／[PLAN-PHASE3.md](PLAN-PHASE3.md) 記錄後續分工與驗收。

