# Build and publish updates

The public source repository is https://github.com/killean-solvely/remote-desk-app. The manually dispatched build workflow runs only on main and standard GitHub-hosted Windows, Apple Silicon and Intel Mac runners. No local runner is needed.

## Signing and identity

The app's public update key and HTTPS feed remain unchanged, preserving compatibility with existing 0.5+ installations. Keep the private signing key outside Git and maintain an independent encrypted backup. Losing it prevents existing installations from trusting releases signed with another key.

Store TAURI_SIGNING_PRIVATE_KEY only as a secret in the releases environment. TAURI_SIGNING_PRIVATE_KEY_PASSWORD is optional for an encrypted key. Restrict that environment to main. Check-only builds use the checks environment and do not receive the signing key. Signing values are passed only to the packaging steps; never place them in command arguments, source or logs. OS publisher certificates are separate.

Only the publishing job receives id-token: write. The update service accepts the exact repository/owner IDs, workflow, main branch, release environment and HTTPS audience. No long-lived upload token is needed. Changing repository or workflow identity requires an explicit update of that allowlist.

## Release

Choose a stable numeric version newer than the current feed. Update the four version files together:

```sh
node scripts/set-version.mjs 0.5.2
git add package.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "Release 0.5.2"
git push origin main
```

In GitHub Actions, run Desktop builds on main with release_tag set to v plus that version. Leave release_tag empty for a complete check-only build. The tag input selects publication; the workflow does not create a Git tag.

All three native builds and their tests must pass. Windows CI verifies a silent installation in its disposable runner; Mac CI mounts each DMG read-only and checks its signature, architecture and bundled engine. The final job collects matching source/version records, uploads packages in chunks and asks the host to verify signatures, signed versions and hashes before atomically updating latest.json. Published versions are immutable. Temporary CI artifacts expire after one day.

## Installed-app validation

Test Settings > Check for updates on installed Windows and Mac apps, confirm installation/restart, and verify the new version with preserved connections/history. Test unavailable networking and installation blocked by queued work. Automated queue/fixture checks do not replace OS installation and relaunch tests.
