# ntpuvpn-rs

臺北大學（NTPU）校園 VPN 的命令列連線工具，支援 Windows、Linux、macOS。

連上之後，**只有連往校內網路（預設 `10.0.0.0/8`）的流量會走 VPN**，其他上網流量照舊走你原本的網路，不會因為開 VPN 而變慢。斷線或 session 過期時會自動重新登入。

> 底層用的是 OpenConnect 的 Juniper（`nc`）協定，連線伺服器為 `ntpu.twaren.net`。

---

## 快速開始

先到 [Releases](https://github.com/billy948787/ntpuvpn-rs/releases) 下載對應系統的檔案並解壓縮。

### Linux / macOS

```bash
sudo ./ntpuvpn-rs
```

需要 `sudo` 是因為要建立虛擬網卡和修改路由。Linux 若連不上，先確認有 TUN 裝置：`test -c /dev/net/tun && echo ok`。

### Windows 10 / 11

Windows 版需要另外裝 OpenConnect：

1. 安裝原生 Windows 版 OpenConnect（[說明在這](https://www.infradead.org/openconnect/building.html)），要支援 `--protocol=nc`，且 `wintun.dll` 要跟 `openconnect.exe` 放在同一個資料夾。**WSL 裡的 openconnect 不能用。**
2. 開啟 PowerShell 或 Windows Terminal。
3. 執行程式，在 Windows UAC 提示中按「是」，接著在新開啟的管理員視窗輸入帳密：

   ```powershell
   .\ntpuvpn-rs.exe
   ```

若視窗本來已有管理員權限，程式會直接在原視窗執行。拒絕 UAC 時會結束啟動。目前資料夾和 VPN 環境變數會傳給提升權限後的程式，因此 `.env` 和自訂 OpenConnect 路徑仍然適用。

程式會依序在這些地方找 `openconnect.exe`：程式所在資料夾 → `C:\Program Files\OpenConnect` → `C:\Program Files (x86)\OpenConnect` → `PATH`。裝在別的地方的話，用環境變數指定：

```powershell
$env:NTPUVPN_OPENCONNECT = 'D:\tools\OpenConnect\openconnect.exe'
.\ntpuvpn-rs.exe
```

### 啟動之後

請使用本校 **Webmail 電子郵件帳號與密碼**登入。學生帳號是小寫 `s` 加學號，例如 `s411234567`；不要加 `@webmail.ntpu.edu.tw`。教職員使用郵件帳號，也不要加 `@mail.ntpu.edu.tw`。帳密說明可參考[資訊中心 VPN 操作手冊](https://drive.google.com/file/d/1lqXKjwYhHoWn6fF7L9bRgL_Pi2ezOLuI/view)。

```
Username [s411234567]:   ← 直接按 Enter 沿用上次的帳號，或輸入新的
Password:               ← 輸入 Webmail 密碼，不會被存起來
```

看到連線成功後讓視窗開著就好。要斷線按 **Ctrl+C**。

---

## 不想每次輸入密碼？用 `.env`

在你執行程式的那個資料夾放一個 `.env` 檔：

```bash
cp .env.example .env     # Windows: Copy-Item .env.example .env
chmod 600 .env           # 只讓自己讀得到
```

內容長這樣：

```ini
NTPUVPN_USERNAME=s411234567
NTPUVPN_PASSWORD='你的Webmail密碼'
```

之後在**同一個資料夾**執行 `sudo ./ntpuvpn-rs`（或 Windows 的 `.\ntpuvpn-rs.exe`）就會直接連線，不用輸入任何東西。

注意事項：

- 密碼裡有 `$`、`#` 或空白的話，請用**單引號**包起來。
- `.env` 是明文存密碼，請自己保護好（Windows 請用檔案權限限制存取）。它已經在 `.gitignore` 裡，**不要 commit 出去**。
- 只會讀「目前資料夾」的 `.env`，不會往上層找。

### 所有可用的設定

| 變數 | 用途 | 預設值 |
| --- | --- | --- |
| `NTPUVPN_USERNAME` | Webmail 帳號，學生為小寫 `s` 加學號，不含郵件網域 | 上次輸入的帳號 |
| `NTPUVPN_PASSWORD` | Webmail 密碼 | 每次詢問 |
| `NTPUVPN_VPN_NETWORK` | 要走 VPN 的網段 | `10.0.0.0` |
| `NTPUVPN_VPN_MASK` | 網段遮罩 | `255.0.0.0` |
| `NTPUVPN_OPENCONNECT` | （僅 Windows）`openconnect.exe` 的完整路徑，只能用環境變數設，不能寫在 `.env` | 自動尋找 |

優先順序：**環境變數 > `.env` > 上次存的設定 > 執行時詢問**。設了但是空白或格式錯誤會直接報錯，不會默默跳過。

### 用環境變數 + sudo（Linux）

`sudo` 預設會清掉環境變數，要用 `--preserve-env` 帶進去。這樣密碼不會出現在指令列或 shell history：

```bash
export NTPUVPN_USERNAME='s411234567'
read -rs -p 'VPN password: ' NTPUVPN_PASSWORD; export NTPUVPN_PASSWORD
sudo --preserve-env=NTPUVPN_USERNAME,NTPUVPN_PASSWORD ./ntpuvpn-rs
unset NTPUVPN_PASSWORD
```

---

## 自動重連

第一次登入成功後，如果連線中斷（包括 session 過期被踢掉），程式會用你一開始輸入的帳密自動重新登入：

- 失敗時等待 5 秒再試，之後逐次拉長，最多每 60 秒試一次。
- 連線撐過一分鐘就會把等待時間重設回 5 秒。
- 第一次登入就失敗的話會直接結束，不會重試（通常是帳密打錯）。

有幾個情況需要自己重開程式：

- 執行中改了密碼
- 學校伺服器要求互動式 MFA（自動重連不支援）

---

## 常見問題

**Q：登入後出現 `welcome.cgi?p=failed`、`User input required in non-interactive mode`？**
`p=failed` 表示伺服器沒有接受這次登入，之後 OpenConnect 再次要求輸入帳密，程式便因非互動模式而結束。先用同一組帳密在 [VPN 網頁](https://ntpu.twaren.net/) 確認可以登入。學生請確認帳號為 `s` 加學號，密碼使用 Webmail 那一組；若程式顯示舊帳號，直接輸入正確帳號取代。若使用環境變數或 `.env`，也要更新其中的帳密。

**Q：連上之後其他網站變慢 / 連不上？**
不應該發生。本程式只把 `10.0.0.0/8` 導向 VPN，不會改預設路由，也不會改 DNS。如果你需要其他網段走 VPN，用 `NTPUVPN_VPN_NETWORK` / `NTPUVPN_VPN_MASK` 調整。

**Q：Windows 說找不到 openconnect？**
確認 `openconnect.exe` 在上面列的位置之一，或設定 `NTPUVPN_OPENCONNECT`。OpenConnect 附帶的 DLL（包括 `wintun.dll`）要跟 exe 放在一起。

**Q：Windows 出現 script 相關錯誤？**
程式需要 Windows Script Host（`cscript.exe`）來讀取伺服器給的網路設定，請確認它沒有被停用。

**Q：Windows 出現 `Access denied creating Wintun adapter` 或 `0x00000005`？**
建立虛擬網卡需要管理員權限。互動啟動時程式會要求 UAC 核准，核准後請在新開啟的管理員視窗繼續操作。若已核准仍出現這個錯誤，請檢查電腦的驅動程式安裝限制或組織管理政策。

**Q：設定存在哪？**
只存帳號和網段，不存密碼：

- Windows：`%APPDATA%\ntpuvpn-rs\config.json`
- Linux：`~/.config/ntpuvpn-rs/config.json`
- macOS：`~/Library/Application Support/ntpuvpn-rs/config.json`

用 `sudo` 執行時，`~` 會是 root 的家目錄。透過環境變數或 `.env` 給的值不會寫進這個檔案。

**Q：可以在沒有終端機的環境跑（例如 systemd、排程）嗎？**
可以，只要帳號密碼都用環境變數或 `.env` 提供。缺任何一個會直接報錯，不會卡住等輸入。Windows 無終端機執行時，請在排程設定中勾選「以最高權限執行」；這種執行方式不會彈出 UAC。

---

## 自己編譯

需要 Rust（[rustup](https://rustup.rs/)）。編譯結果在 `target/release/ntpuvpn-rs`（Windows 是 `.exe`）。

### Windows

安裝 Rust 的 MSVC toolchain 和 Visual Studio C++ Build Tools，然後：

```powershell
cargo build --release
```

不需要 OpenSSL 或 OpenConnect 的開發套件；OpenConnect 和 Wintun 只在執行時需要。

### Linux / macOS

OpenConnect 9.20 會從原始碼一起編進執行檔，所以**不用**另外裝 `openconnect` 套件，但需要它的編譯依賴：

```bash
# Debian / Ubuntu
sudo apt install build-essential clang libdbus-1-dev libssl-dev libxml2-dev pkg-config zlib1g-dev

# Fedora
sudo dnf install clang dbus-devel gcc libxml2-devel make openssl-devel pkgconf-pkg-config zlib-devel

# Arch Linux
sudo pacman -S base-devel clang dbus libxml2 openssl pkgconf zlib

# macOS
brew install openssl@3 pkgconf
```

然後：

```bash
cargo build --release
```

### 測試

```bash
cargo test
```

一般測試不會連 VPN、也不會動到路由。

Linux 上另有會實際建立 TUN 和路由的測試，必須在隔離的 network namespace 裡跑，避免影響本機網路：

```bash
NTPUVPN_TEST_HOST_NETNS="$(readlink /proc/self/ns/net)" \
  unshare --user --map-root-user --net \
  cargo test -- --ignored
```

---

## 技術細節

- **Linux / macOS**：透過 `anyconnect` crate 把 OpenConnect 直接嵌入程式。重連時沿用同一張 TUN 網卡，介面編號不變。
- **Windows**：呼叫外部的 `openconnect.exe` + Wintun。OpenConnect 只負責回報伺服器給的網卡設定，實際的 IP 和路由由本程式用 `netsh` 設定，不套用 OpenConnect 預設的路由和 DNS。每次重新登入會啟動新的 OpenConnect 程序和網卡，並更新路由。
- 密碼透過 stdin 傳給 OpenConnect，不會出現在命令列參數中。

## 授權

Linux / macOS 執行檔靜態連結了 OpenConnect（LGPL-2.1-only），再散布時請遵守其原始碼提供與重新連結的要求。本專案授權見 [LICENSE](LICENSE)。
