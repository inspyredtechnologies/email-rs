# Outlook RS

A clean-room re-creation of **Outlook Classic's mail experience** in Rust (egui/eframe).
It copies how Outlook looks and behaves; it does not contain or decompile any Microsoft code.

## Run

```bash
cargo run --release              # your accounts (first run opens Add Account)
cargo run --release -- --demo    # offline sample mailbox, no account needed
```

## What works

- **Layout:** ribbon (File / Home / Send / Receive / Folder / View), folder pane with Favorites and unread counts, a three-line message list grouped by date (Today, Yesterday, weekday, Last Week…), reading pane, navigation bar, and status bar.
- **Mail over IMAP/SMTP:** folder sync with SPECIAL-USE detection (Inbox, Sent Items, Deleted Items, Junk Email…), a local cache of the newest 500 messages per folder, previews for the newest 25, read/unread, flags, Move, Archive, and Delete (to Deleted Items, or permanent when already there).
- **HTML mail:** rendered with WebView2 (built into Windows 10/11) in the reading pane. Scripts are disabled, and a Content-Security-Policy blocks remote pictures and tracking pixels until you click "Click here to download pictures". Embedded (`cid:`) images display inline. Links open in your browser, and `mailto:` links open a new message.
- **Compose:** opens in its own window with From / To / Cc / Bcc / Subject and attachments. HTML is the default format, with bold, italic, underline (Ctrl+B / I / U), strikethrough, font size, font color, highlight, bullets and numbering; URLs become links. Mail is sent as multipart/alternative (plain text + HTML); switch Format to Plain Text to send text only. Reply, Reply All and Forward quote the original Outlook-style, including the original's HTML. Sent mail is saved to Sent Items, except on Gmail and Microsoft 365, which do that themselves.
- **Accounts:** Add Account with presets for Outlook.com, Gmail, Yahoo, iCloud, Fastmail and AOL, plus a live connection test. Three sign-in methods: password, **Microsoft** (Outlook.com, Hotmail, Microsoft 365 / Exchange Online) and **Google**. The last two use OAuth 2.0 in your browser (XOAUTH2), so Outlook RS never sees your password. Passwords and refresh tokens are stored in Windows Credential Manager.
- **Shortcuts:**

  | Keys | Action |
  | --- | --- |
  | Ctrl+N | New email |
  | Ctrl+R / Ctrl+Shift+R | Reply / Reply all |
  | Ctrl+F | Forward |
  | Ctrl+Q / Ctrl+U | Mark read / unread |
  | Del | Delete |
  | Insert | Flag |
  | F9 | Send/Receive all folders |
  | Ctrl+E | Search |
  | Ctrl+Enter or Alt+S | Send (in compose) |
  | ↑ / ↓ | Move through the list |

## Setting up Microsoft or Google sign-in

Microsoft and Google only allow OAuth sign-in through an *app registration*. Create one (free, about 5 minutes) and paste its client ID into Add Account. Outlook RS saves it in `%APPDATA%\OutlookRs\data\oauth.json`, so you only enter it once.

**Microsoft (Outlook.com, Hotmail, Microsoft 365 / Exchange Online)**

1. Go to [entra.microsoft.com](https://entra.microsoft.com) > **App registrations** > **New registration**.
2. For supported account types, choose **Accounts in any organizational directory and personal Microsoft accounts**.
3. Under Redirect URI, choose platform **Public client/native (mobile & desktop)** with value `http://localhost`, then click **Register**.
4. Copy the **Application (client) ID** into the Client ID field.
5. For work or school accounts: under **API permissions**, add *Office 365 Exchange Online* > Delegated > `IMAP.AccessAsUser.All` and `SMTP.Send`. Your admin may need to grant consent, and must have IMAP and Authenticated SMTP enabled for the mailbox.

**Google (Gmail, Google Workspace)**

1. In [console.cloud.google.com](https://console.cloud.google.com), create a project and configure the **OAuth consent screen** (External, Testing). Add yourself as a test user.
2. Go to **Credentials** > **Create credentials** > **OAuth client ID** > **Desktop app**.
3. Copy the client ID and client secret into Add Account.

While the Google project is in Testing, Google expires its sign-ins after 7 days. Without OAuth, Gmail still works with an app password.

To build with your client IDs baked in, set `OUTLOOK_RS_MS_CLIENT_ID`, `OUTLOOK_RS_GOOGLE_CLIENT_ID` and `OUTLOOK_RS_GOOGLE_CLIENT_SECRET` when running `cargo build`.

## Limitations

- In replies and forwards the quoted original can't be edited (it's shown below your text and sent as-is). There's no font-face picker, inline images, tables or signatures.
- Exchange is reached over IMAP/SMTP only, with no EWS or MAPI. On-premises Exchange works with the Password method if its admin has enabled IMAP.
- Calendar, People and Tasks are placeholders.
- No PST/OST import, drafts saving, rules, or IMAP IDLE (it polls every 10 minutes and on F9).

## Layout

| Path | Purpose |
| --- | --- |
| `src/worker.rs` | One background thread per account; all IMAP/SMTP I/O and cache writes |
| `src/app.rs` | UI state, action queue, keyboard shortcuts |
| `src/ui/` | Ribbon, folder pane, list, reading pane, compose, account dialogs |
| `src/mime.rs` | Message parsing, reply/forward building, outgoing MIME |
| `src/html.rs`, `src/webview.rs` | HTML mail sanitizing (CSP, cid: images) and the WebView2 reading-pane view |
| `src/oauth.rs` | Microsoft/Google OAuth: PKCE, loopback redirect, token refresh, XOAUTH2 |
| `src/store.rs` | Cache in `%APPDATA%\OutlookRs\data`, Credential Manager passwords |

`cargo test` runs the unit tests (`cargo test -- --ignored` adds a check against Microsoft's live token endpoint). To capture the window to a PNG and exit, run `outlook-rs --demo --screenshot out.png`.
