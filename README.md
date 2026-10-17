# Remote Desk

Remote Desk is a desktop SFTP browser and download manager for Windows and macOS. It connects directly over SSH and uses a bundled, checksum-pinned rclone engine for concurrent files and multithreaded transfers.

## Install

Get the Windows installer or the Apple Silicon/Intel Mac DMG at https://rd.killean.me/. Windows supports a per-user installation with Start menu integration. On Mac, drag the app into Applications. The bundled engine requires macOS Tahoe 26 or newer.

Current Windows installers have no publisher certificate. Mac bundles are ad-hoc signed and are not notarized. Follow your operating system's normal approval flow; do not disable its security controls. [Installation details](docs/install.md).

## Features

- Browse remote directories, filter and select files or folders.
- Fetch the SSH host-key fingerprint, verify it independently and pin it before authentication.
- Enter passwords in a native prompt and store them in Windows Credential Manager or macOS Keychain; SSH-agent authentication is also supported.
- Queue downloads with aggregate/per-file progress, cancellation, retry, readable ETA and persistent history.
- Edit/delete saved connections and clear completed history without deleting downloaded files.
- Check for signed application updates and install them with confirmation.

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

## Version 0.5.1

Fixes remote-folder browsing when sibling names differ only by capitalization or Unicode normalization. Download preflight continues to protect against local path collisions and identifies the conflicting names.
