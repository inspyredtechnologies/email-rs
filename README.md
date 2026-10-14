# E-Mail RS

A desktop mail and calendar client in Rust (egui/eframe), with the familiar layout of classic desktop email clients such as Outlook Classic.

E-Mail RS was previously called Outlook RS. It isn't affiliated with or endorsed by Microsoft.
It copies how Outlook looks and behaves; it does not contain or decompile any Microsoft code.

## Run

```bash
cargo run --release              # your accounts (first run opens Add Account)
cargo run --release -- --demo    # offline sample mailbox, no account needed
```

## Build an installer (MSI)

You need the WiX Toolset v5 (`dotnet tool install --global wix --version 5.0.2`). Then run:

```bash
powershell -ExecutionPolicy Bypass -File installer/build-msi.ps1
```

This builds a release exe and writes `target\wix\EmailRS-<version>-x64.msi`. The MSI installs per-machine to `Program Files\E-Mail RS`, adds a Start menu shortcut and an Apps & features entry, and upgrades older versions in place, including installs from when the app was called Outlook RS. Your accounts move over automatically from `%APPDATA%\OutlookRs` on first start. It's unsigned, so Windows SmartScreen will warn on first run. Set the `EMAIL_RS_GOOGLE_*` client variables before building to build in Google sign-in. Microsoft sign-in is already built in.

## What works

- **Layout:** ribbon (File / Home / Send / Receive / Folder / View), folder pane with Favorites and unread counts, a three-line message list grouped by date (Today, Yesterday, weekday, Last Week…), reading pane, navigation bar, and status bar.
- **Three kinds of account**, with the type detected from your email domain:
  - **Microsoft 365 / Outlook.com:** mail and calendar through Microsoft Graph with one Microsoft sign-in. IMAP and SMTP don't need to be enabled.
  - **Exchange Server (on-premises):** mail and calendar through Exchange Web Services (EWS), with Autodiscover to find the server. Sign-in uses Windows' own HTTP stack (WinHTTP), so Kerberos/NTLM, Extended Protection, proxies and company certificates work.
  - **IMAP / SMTP:** any provider, signing in with a password, Microsoft or Google.
- **Mail:** folder sync with SPECIAL-USE detection (Inbox, Sent Items, Deleted Items, Junk Email…), a local cache of the newest 500 messages per folder, previews for the newest 25, read/unread, flags, Move, Archive, and Delete (to Deleted Items, or permanent when already there).
- **HTML mail:** rendered with WebView2 (built into Windows 10/11) in the reading pane. Scripts are disabled, and a Content-Security-Policy blocks remote pictures and tracking pixels until you click "Click here to download pictures". Embedded (`cid:`) images display inline. Links open in your browser, and `mailto:` links open a new message.
- **Compose:** opens in its own window with From / To / Cc / Bcc / Subject and attachments. HTML is the default format, with bold, italic, underline (Ctrl+B / I / U), strikethrough, font size, font color, highlight, bullets and numbering; URLs become links. Mail is sent as multipart/alternative (plain text + HTML); switch Format to Plain Text to send text only. Reply, Reply All and Forward quote the original Outlook-style, including the original's HTML. Sent mail is saved to Sent Items, except on Gmail and Microsoft 365, which do that themselves.
- **Calendar:**
  - **Views:** Day, Work Week, Week and Month, with a date navigator and "My Calendars".
  - **Appointments and meetings:** created, edited and deleted in their own window (Ctrl+N; Ctrl+Shift+Q for a new meeting).
  - **Calendars:** a local calendar stored on this PC, plus two-way sync with the calendar of each Microsoft account (Outlook.com / Microsoft 365) through Microsoft Graph. Recurring Graph meetings are expanded into their occurrences.
- **Meeting invites:** invites in your mail show When / Where / Organizer with Accept, Tentative and Decline.
  - On a Microsoft calendar, the response goes through Graph.
  - Otherwise it's emailed to the organizer as a standard iCalendar reply (iMIP) and the meeting is added to the local calendar. Cancellations offer "Remove from Calendar".
  - Meetings you organize on the local calendar email iCalendar invitations, updates and cancellations to attendees.
- **Accounts:** Add Account picks the account type from your email domain. It has presets for Gmail, Yahoo, iCloud, Fastmail and AOL, and tests the connection before saving. Microsoft and Google sign-in use OAuth 2.0 in your browser, so E-Mail RS never sees those passwords. Passwords and refresh tokens are encrypted with Windows DPAPI so only your Windows account can read them, and kept in `%APPDATA%\EmailRs\data\secrets`.
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

## Microsoft and Google sign-in

**Microsoft (Outlook.com, Hotmail, Microsoft 365 / Exchange Online)** works out of the box. E-Mail RS ships with its own multi-tenant Entra app registration (client ID `3d431ce5-1cc0-4e61-8696-7e88a14e62ef`), so users just click *Sign in with Microsoft*. An organization that restricts user consent needs its admin to approve E-Mail RS once, at `https://login.microsoftonline.com/<tenant-domain>/adminconsent?client_id=3d431ce5-1cc0-4e61-8696-7e88a14e62ef`.

To use your own registration instead (for example in a fork), build with `EMAIL_RS_MS_CLIENT_ID` set. The registration must be set up like this:

1. Go to [entra.microsoft.com](https://entra.microsoft.com) > **App registrations** > **New registration**.
2. For supported account types, choose **Accounts in any organizational directory and personal Microsoft accounts**.
3. Under Redirect URI, choose platform **Public client/native (mobile & desktop)** with value `http://localhost`, then click **Register**.
4. Copy the **Application (client) ID**.
5. Under **API permissions**, add *Microsoft Graph* > Delegated > `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite` and `offline_access`. That's all the "Microsoft 365 / Outlook.com" account type needs. Your organization's admin may need to grant consent once.
6. Only for Microsoft accounts set up as "IMAP / SMTP": also add `IMAP.AccessAsUser.All` and `SMTP.Send`, and have the admin enable IMAP and Authenticated SMTP for the mailbox. For the calendar on such an account, "Connect this calendar..." under My Calendars runs a one-time extra sign-in if needed.

**Google (Gmail, Google Workspace)** needs your own OAuth client: there isn't one built in. In builds without a built-in client ID, Add Account shows a Client ID field and saves the value in `%APPDATA%\EmailRs\data\oauth.json`.

1. In [console.cloud.google.com](https://console.cloud.google.com), create a project and configure the **OAuth consent screen** (External, Testing). Add yourself as a test user.
2. Go to **Credentials** > **Create credentials** > **OAuth client ID** > **Desktop app**.
3. Copy the client ID and client secret into Add Account.

While the Google project is in Testing, Google expires its sign-ins after 7 days. Without OAuth, Gmail still works with an app password.

To build client IDs in, set `EMAIL_RS_MS_CLIENT_ID`, `EMAIL_RS_GOOGLE_CLIENT_ID` and `EMAIL_RS_GOOGLE_CLIENT_SECRET` when running `cargo build`. Users of such a build never see a Client ID field.

## Limitations

- In replies and forwards the quoted original can't be edited (it's shown below your text and sent as-is). There's no font-face picker, inline images, tables or signatures.
- Exchange: no contacts / address book, shared mailboxes, server-side rules or online archive yet. Exchange Server needs EWS on the Exchange 2013 or later schema.
- Microsoft 365: sending through Graph is limited to about 3 MB of attachments per message.
- Building currently requires Windows.
- People and Tasks are placeholders.
- Calendar: no reminders, no recurring events of your own, and no Google or CalDAV calendar sync. For invites handled on the local calendar, only the first occurrence of a recurring meeting is added.
- No PST/OST import, drafts saving, rules, or IMAP IDLE (it polls every 10 minutes and on F9).

## Layout

| Path | Purpose |
| --- | --- |
| `src/worker.rs` | One background thread per account: cache, previews and UI events, independent of protocol |
| `src/backend/` | `imap.rs` (IMAP/SMTP), `graph_mail.rs` (Microsoft Graph mail), `ews.rs` (Exchange Web Services and Autodiscover) |
| `src/winhttp.rs` | WinHTTP client used for Exchange Server (Negotiate/NTLM/Basic) |
| `src/app.rs` | UI state, action queue, keyboard shortcuts |
| `src/ui/` | Ribbon, folder pane, list, reading pane, compose, account dialogs |
| `src/mime.rs` | Message parsing, reply/forward building, outgoing MIME |
| `src/html.rs`, `src/webview.rs` | HTML mail sanitizing (CSP, cid: images) and the WebView2 reading-pane view |
| `src/calendar.rs`, `src/ics.rs`, `src/graph.rs` | Calendar events and storage, iCalendar invites (with VTIMEZONE), Microsoft Graph calendar API |
| `src/ui/calendar.rs`, `src/ui/appointment.rs`, `src/app/calendar_actions.rs` | Calendar views, the appointment window, and invite / sync logic |
| `src/oauth.rs` | Microsoft/Google OAuth: PKCE, loopback redirect, token refresh, XOAUTH2 |
| `src/store.rs`, `src/dpapi.rs` | Cache in `%APPDATA%\EmailRs\data`; DPAPI-encrypted passwords and tokens |
| `assets/`, `build.rs` | App icon (`make-icon.py` regenerates `icon.ico` / `icon.png` with Pillow), embedded into the exe at build time |

`cargo test` runs the unit tests (`cargo test -- --ignored` adds checks against Microsoft's live sign-in, Graph and Exchange endpoints). To capture the window to a PNG and exit, run `email-rs --demo --screenshot out.png`.
