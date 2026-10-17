# Architecture

The Tauri 2 desktop app embeds a React/TypeScript interface. Typed IPC connects it to a Rust core that stores settings, remotes and job state in SQLite, uses native OS credential storage, and starts a bundled architecture-specific rclone process with an argument array. File bytes travel directly between the desktop and SFTP server over SSH.

Every SFTP process requires an app-managed known-hosts pin and restricts negotiation to algorithms represented by the verified pin. Host-key lookup rejects the initial handshake before authentication, presents a fingerprint and requires independent verification. Passwords never reach the webview or a generated configuration file.

Each download receives an isolated local job directory. Remote paths, platform-unsafe names, traversal and destination links/reparse points are checked. Browsing retains distinct case/Unicode spellings. Folder-copy manifests reject local path collisions, including differently cased parent directories. Existing different files are protected from overwrite. Success verifies local types and sizes; remote checksum or byte-range resume guarantees are not made.

rclone emits machine-readable JSON progress. One job runs at a time with bounded file concurrency and per-file streams. Canceled/interrupted jobs require explicit retry. No startup connection or queue resume occurs automatically.

Updates use the existing application identifier, HTTPS feed and embedded public verification key. Installation is confirmed by the user and blocked while work is queued or active. The backend blocks new work during installation and handles macOS instance-lock release before relaunch. Publishing authenticates a short-lived GitHub workflow identity and verifies complete signed releases before switching the feed.
