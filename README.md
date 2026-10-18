# Remote Desk

Remote Desk is a desktop SFTP browser and download manager for Windows and macOS. It connects directly over SSH and uses a bundled, checksum-pinned rclone engine for concurrent files and multithreaded transfers.

## Install

Get the Windows installer or the Apple Silicon/Intel Mac DMG at https://rd.killean.me/. Windows supports a per-user installation with Start menu integration. On Mac, drag the app into Applications. The bundled engine requires macOS Tahoe 26 or newer.

Current Windows installers have no publisher certificate. Mac bundles are ad-hoc signed and are not notarized. Follow your operating system's normal approval flow; do not disable its security controls. [Installation details](docs/install.md).

## Features

- Browse remote directories, filter and select files or folders.
- Bookmark remote folders under each connection, with local persistence and quick navigation.
- Fetch the SSH host-key fingerprint, verify it independently and pin it before authentication.
- Enter passwords in a native prompt and store them in Windows Credential Manager or macOS Keychain; SSH-agent authentication is also supported.
- Queue downloads with aggregate/per-file progress, cancellation, retry, readable ETA and persistent history.
- Edit/delete saved connections and clear completed history without deleting downloaded files.
- Reveal completed downloads in Explorer/Finder, or open a transfer's download folder.
- Check for signed application updates and install them with confirmation.
- Opt into private desktop alerts for background download completion or failure.

Each selected item receives its own local job directory. Browsing preserves case-distinct names; a folder copy refuses names that would collide in one local directory tree. Completed-file retries use size checks; partial multithreaded files may restart from zero. SFTP downloads do not promise preservation of macOS bundle metadata or permissions; use a remotely prepared archive when that matters.

## Develop

Use Node.js 22+, pnpm 11.25.0 and stable Rust. Windows builds also need Visual Studio C++ build tools and WebView2. Mac builds need Apple Command Line Tools and macOS 26+.

```sh
pnpm install --frozen-lockfile
node scripts/prepare-rclone.mjs
pnpm tauri dev
```

The engine download is checked against a pinned SHA256 before it is bundled. No server, account, credentials or downloaded content are included. Only connect to servers and download content you are authorized to access.

## Build and release

The manually dispatched Desktop builds workflow uses standard GitHub-hosted Windows and native Apple Silicon/Intel Mac runners. It runs the frontend, Rust tests, generated local transfer fixtures and installer checks. It does not use a personal machine or connect to a production SFTP server.

Release signing is restricted to the main-branch releases environment. The private signing key is never committed. All three signed packages must pass before the HTTPS update feed advances. See [release maintenance](docs/updates.md) and [architecture](docs/architecture.md).

## Version 0.6.1

In **Settings > Download notifications**, enable **Desktop notifications**. On Mac, approve the system permission request. Alerts use generic completion/failure text; filenames, server details and logs stay in the app. Only live attempts finishing in the background can alert, with no cancellation or startup-history alerts. An explicit retry may alert for its new outcome. Delivery problems never change a transfer's saved result.

Choose **Send test in 5 seconds**, then switch to another app. Remote Desk must stay open. **Check permission** refreshes the OS status without prompting; blocked states explain which system settings to check. Windows may initialize its per-app notification preferences with the first alert. Focus and system controls can silence alerts, so an accepted request does not guarantee a banner. Use the installed app so alerts carry Remote Desk's identity. No custom notification click action or service after app exit is added.

## Version 0.6.0

After opening a remote folder, choose **Bookmark this folder**. Bookmarks appear beneath their connection in the sidebar; click one to browse it, or choose **Edit** to rename or remove it. Root folders are supported. Bookmarks stay on this computer and saving them does not connect.

Changing a connection's server, port, username or root requires reviewing its bookmark targets before opening them. Renaming a connection or changing its password keeps bookmarks usable. Deleting a connection removes its bookmarks and preserves downloaded files and history.

## Version 0.5.3

Completed transfers offer **Show in Explorer/Finder**. **Open download folder** is also available for partial or failed transfers. These actions work from saved history even after a connection is deleted; missing locations show an explanation.

Remote browsing accepts server filenames that cannot be saved on both desktop platforms. Download preflight still rejects incompatible local names and identifies the item that needs attention.

## Earlier improvements

Fixes remote-folder browsing when sibling names differ only by capitalization or Unicode normalization. Download preflight continues to protect against local path collisions and identifies the conflicting names.

Folder rows offer **Calculate size**: recursively scan metadata without downloading contents, cancel large scans, and reuse totals while browsing during the current session. **Recalculate** refreshes a cached result. Only one size scan runs at a time; browsing and downloads remain available. Symlinks are excluded and incomplete scans do not display a total.
