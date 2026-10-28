# aria2 功能對照與剩餘差距

版本：ariai 0.2.0，2026-10-28。依 [aria2 手冊](https://aria2.github.io/manual/en/html/aria2c.html) 與 [功能列表](https://github.com/aria2/aria2#features) 盤點。採原生 Rust 協定核心，無 aria2c/libaria2 後端。這是持續對齊的功能矩陣；「實作」與「已實測」分開記錄。

| 功能族 | 目前實作 | 驗收／剩餘差距 |
|---|---|---|
| HTTP／HTTPS 串流、Range 分段 | 已實作 | 驗證 206、Content-Range、鏡像尺寸；不支援 Range 時降級 |
| HTTP 續傳、validator | 已實作 | 中斷後續傳、來源版本改變重下載；控制檔與 aria2 不互通 |
| HTTP／HTTPS／FTP／SFTP 鏡像 | 已實作故障切換 | 同一協定可重試；混合協定依群組切換，尚無同時跨來源拼接 |
| FTP binary、EPSV/PASV、EPRT/PORT | 已實作 | 本機主動／被動、帳密、URL 編碼路徑實測；需要 SIZE；連線重用、FTP 代理待補 |
| FTP REST 續傳 | 已實作 | 取消後續傳實測；URI、SIZE、MDTM 相容才使用暫存；缺 MDTM 時重啟下載 |
| SFTP／SSH 認證 | 已實作 | 本機真實 russh server 密碼與私鑰實測；加密私鑰 passphrase 專項 NOT_RUN |
| SSH host key／known_hosts | 已實作嚴格驗證 | SHA-256 pinned key、known_hosts 接受與拒絕實測；SHA-1／MD5 專項 NOT_RUN；host certificate 不支援 |
| SFTP 續傳 | 已實作來源識別與 seek | 專項中斷續傳 NOT_RUN；SFTP 代理不支援；FTP/SFTP 目前單串流 |
| 本機／file／HTTP(S) torrent | 已實作 librqbit | 本機與 HTTP metadata 從受控 peer 實際下載及內容比對 |
| Magnet metadata exchange | 已實作 librqbit | 受控 peer metadata＋資料傳輸、取消實測 |
| Torrent 選檔、檔案進度、完整性 | 已實作 | 多檔 metadata 選第 2 檔、per-file path／length／selection、錯誤索引及 metadata 實測 |
| Torrent 做種／停止 | 已實作 | 本實作完成後向另一個 peer 實際上傳，取消後 listener 釋放；預設 seed-time=0，與 aria2 不同 |
| DHT、HTTP／UDP tracker | 透過 librqbit 接入 | 公開網路 DHT／tracker 專項 NOT_RUN；不宣告 aria2 extensions 全相容 |
| DHT6、PEX、LPD、MSE/PSE、Fast extension | 尚未逐項對齊 | 本程式關閉 LSD，listener 目前 IPv4；逐項設定、互通驗收待補 |
| BT＋HTTP(S)/FTP/SFTP 混合來源、web seed | 尚未對齊 | addTorrent 額外 web-seed URI 明確拒絕；不宣告混合來源支援 |
| BT proxy、下載／上傳限速、seed ratio | 尚未實作 | BT proxy、max-download-limit、外部 checksum／Metalink piece 設定明確拒絕 |
| Metalink 3／4 XML | 已實作 | namespace、相對 URL、檔名、尺寸、鏡像順序、選檔、DTD／錯誤格式拒絕實測 |
| Metalink 整檔／分片 hash | 已實作下載完成驗證 | SHA-256／SHA-1／MD5，選可用最強 hash；錯誤時不發布；局部分片修復待補 |
| Metalink HTTP discovery、P2P resource | 尚未實作 | 目前以 -M 或 .meta4/.metalink 路徑展開 HTTP(S)/FTP/SFTP 來源 |
| 佇列／並行／暫停／恢復／移除 | 已實作 | 等待及暫停工作可排序；暫停會等傳輸終止才回應 |
| checksum、retry、timeout、redirect | 已實作基本版本 | 全檔 SHA-256／SHA-1／MD5；aria2 完整 retry 分類與語義待對齊 |
| 每工作限速 | HTTP／FTP／SFTP 已實作 | 全域、上傳與 BT 限速待補；精確限速效能量測 NOT_RUN |
| Header／User-Agent／Referer／Basic | HTTP 已實作 | Digest、netrc、cookies SQLite／Netscape、gzip／deflate 待補 |
| HTTP／SOCKS proxy | HTTP 透過 reqwest | 真實代理專項 NOT_RUN；FTP/SFTP/BT 明確拒絕 |
| TLS client certificate、自訂 CA | 尚未實作 | 目前使用 rustls 預設驗證 |
| CLI／config／input／stdin | 已實作選項子集 | 未支援旗標會報錯；197 個上游長選項名稱見 aria2-options.txt |
| Session 儲存／恢復 | 已實作 ariai JSON | 與 aria2 文字 session／.aria2 控制檔不互通；Torrent RPC metadata 持續保存在 .ariai-metadata |
| HTTP JSON-RPC、batch、token secret | 已實作子集 | system.listMethods 列出實際方法；未列方法回錯誤 |
| WebSocket JSON-RPC、下載事件 | 已實作 | Start/Pause/Stop/Complete/Error；首次認證前無事件；溢出則斷線重新查詢 |
| addUri／addTorrent／addMetalink | 已實作 | base64 metadata；等待佇列 position；不支援 Torrent web-seed URI 注入 |
| getFiles／getUris／getOption／changeOption | 已實作子集 | Torrent 真實逐檔資料；changeOption 僅等待／已停止的暫停工作；getOption 僅公開目前實作的部分設定 |
| changePosition／force 操作 | 已實作 | POS_SET/CUR/END；force 與一般操作使用同一取消／收尾路徑 |
| RPC 上傳速率／上傳總量／連線數 | 尚未接入真實統計 | 回傳 0；目前不能用這些欄位判斷 Torrent 做種狀態 |
| XML-RPC、multicall、changeUri、changeGlobalOption | 尚未實作 | 第三方 aria2 前端完整相容性未宣告 |
| Timestamp／Content-Disposition／URI expansion | 尚未實作 | 檔名目前依 URI／out／metadata 決定 |
| server-stat／URI selector、hooks、daemon/service | 尚未實作 | CLI RPC 可前景常駐，沒有背景服務安裝 |
| allocation／disk-cache／mmap／piece policy | 尚未逐項對齊 | 目前採各原生引擎的 I/O，未重現全部 aria2 選項 |
| IPv6／Async DNS／Happy Eyeballs | 依各引擎能力 | IPv6 專項 NOT_RUN；BT listener 目前 IPv4 |
| Windows／Linux／macOS／MSRV | Windows 本機驗收 | 其他平台、Rust 1.89 與遠端 CI 均 NOT_RUN，詳見 ACCEPTANCE.md |

所有 PASS 以 evidence/verification.json 和 ACCEPTANCE.md 的當次命令結果為準；依賴提供某能力不代表已驗證 aria2 行為相容。
