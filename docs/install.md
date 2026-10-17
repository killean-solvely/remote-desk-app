# Install and update Remote Desk

Download installers from https://rd.killean.me/. The application includes its rclone engine and license; no extra executable or manual known_hosts file is required.

## Windows

Quit Remote Desk, run the Windows x64 installer, and launch Remote Desk from Start. The current-user installation includes the transfer engine and standard uninstallation. Installing over an older version preserves saved application data and OS credentials. Current builds have no Windows publisher certificate.

## macOS

Use macOS Tahoe 26+. Select Apple Silicon or Intel to match your Mac. Quit the old app, open the DMG, drag Remote Desk into Applications, eject the DMG and launch the installed app. Current bundles are ad-hoc signed, not Developer ID signed or notarized. Use the normal Open Anyway approval flow if required; do not disable Gatekeeper. Do not run the app from a mounted read-only DMG.

## First connection

In Settings, add your SFTP host, port, username and optional root. Fetch the host key, compare the fingerprint through an independent trusted channel, explicitly confirm it, and save. Enter your password only in the native prompt. Saving never starts a connection or transfer. SSH-agent authentication is also supported.

## Updates and removal

Version 0.5+ checks at startup and every six hours while open, with a manual Settings check. An update requires your confirmation, verifies the signed package and version, installs and restarts. Finish or cancel queued downloads first. Saved connections, trusted keys, credentials, preferences and history remain in place. Update signatures are separate from OS publisher certificates.

Installers also work for manual upgrades. Version 0.4 needs one manual installation of 0.5+ to gain the updater. Uninstall through Windows Installed apps or remove the Mac app from Applications. Downloaded files are independent. Delete connections inside the app first if you want their OS-stored credentials removed.

The adjacent SHA256 covers each installer. CI artifacts include target-specific build metadata. Automated checks use only generated local files; interactive installed-app tests are still needed on real devices.
