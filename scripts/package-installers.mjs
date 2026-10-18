import {cpSync, mkdirSync, readFileSync, writeFileSync, readdirSync, existsSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';

const target = process.argv[2];
const targets = {'darwin-arm64':'aarch64-apple-darwin', 'darwin-x64':'x86_64-apple-darwin', 'win32-x64':'x86_64-pc-windows-msvc'};
const intelOnAppleSilicon = process.platform === 'darwin' && process.arch === 'arm64' && target === 'x86_64-apple-darwin';
if (target !== targets[`${process.platform}-${process.arch}`] && !intelOnAppleSilicon) throw Error('Run on the corresponding packaging OS.');
const version = JSON.parse(readFileSync('package.json', 'utf8')).version;
const identifier = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8')).identifier;
const engine = JSON.parse(readFileSync('.tools/ci-engine/engine.json', 'utf8'));
const release = resolve('src-tauri/target', target, 'release');
function run(executable, args, timeout = 120000, extra = {}) {
  const result = spawnSync(executable, args, {encoding:'utf8', timeout, windowsHide:true, ...extra});
  if (result.error) throw result.error;
  if (result.status !== 0) throw Error(`${executable} failed: ${result.stderr}`);
  return result.stdout;
}
function checkEngine(directory, executable) {
  if (!existsSync(join(directory, 'COPYING-rclone.txt')) || !existsSync(join(directory, 'engine.json'))) throw Error('Engine metadata/license missing from installation.');
  const metadata = JSON.parse(readFileSync(join(directory, 'engine.json'), 'utf8'));
  if (metadata.version !== engine.version || metadata.archiveSha256 !== engine.sha256) throw Error('Wrong bundled engine metadata.');
  if (!run(executable, ['--config', process.platform === 'win32' ? 'NUL' : '/dev/null', 'version']).startsWith(`rclone v${engine.version}`)) throw Error('Wrong installed engine version.');
  const flags = run(executable, ['--config', process.platform === 'win32' ? 'NUL' : '/dev/null', 'help', 'flags']);
  for (const flag of ['--sftp-known-hosts-file','--sftp-host-key-algorithms','--multi-thread-streams','--immutable']) {
    if (!flags.includes(flag)) throw Error(`Installed engine missing ${flag}`);
  }
}
let installer;
let extension;
let verification;
if (process.platform === 'darwin') {
  const directory = join(release, 'bundle/dmg');
  installer = join(directory, readdirSync(directory).find(f=>f.endsWith('.dmg')) ?? 'missing.dmg');
  const mount = resolve('.tools/dmg-install-smoke');
  mkdirSync(mount, {recursive:true});
  run('/usr/bin/hdiutil', ['attach', '-readonly', '-nobrowse', '-mountpoint', mount, installer]);
  try {
    const app = join(mount, 'Remote Desk.app');
    if (!existsSync(join(mount, 'Applications'))) throw Error('DMG missing Applications shortcut.');
    if (run('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', join(app, 'Contents/Info.plist')]).trim() !== identifier) throw Error('Mac notification bundle identity differs from the application.');
    run('/usr/bin/codesign', ['--verify', '--deep', '--strict', app]);
    const architecture = target === 'x86_64-apple-darwin' ? 'x86_64' : 'arm64';
    for (const binary of [join(app, 'Contents/MacOS/remote-desk'), join(app, 'Contents/Resources/engine/rclone')]) {
      if (!run('/usr/bin/lipo', ['-archs', binary]).trim().split(/\s+/).includes(architecture)) throw Error('Wrong packaged Mac architecture');
    }
    // Run the engine from the mounted, read-only installer: no test-kit paths.
    const bundled = join(app, 'Contents/Resources/engine');
    checkEngine(bundled, join(bundled, 'rclone'));
  } finally {
    run('/usr/bin/hdiutil', ['detach', mount]);
  }
  extension = 'dmg';
  verification = intelOnAppleSilicon ? 'read-only mounted installer, architecture/signature and engine execution under Rosetta; local files only' : 'read-only mounted installer, architecture/signature and engine execution; local files only';
} else {
  const directory = join(release, 'bundle/nsis');
  installer = join(directory, readdirSync(directory).find(f=>f.endsWith('-setup.exe')) ?? 'missing.exe');
  const install = resolve('.tools/installer-smoke', `Remote Desk ${version}`);
  // Never install into the owner's profile or alter their Start menu on a personal runner.
  if (process.env.RUNNER_ENVIRONMENT === 'github-hosted') {
  run(installer, ['/S', `/D=${install}`], 180000, {windowsVerbatimArguments:true});
  if (!existsSync(join(install, 'remote-desk.exe'))) throw Error('Application missing from installed folder.');
  if (!existsSync(join(install, 'uninstall.exe'))) throw Error('Installed uninstaller is missing.');
  const shortcut = join(process.env.APPDATA, 'Microsoft/Windows/Start Menu/Programs/Remote Desk/Remote Desk.lnk');
  if (!existsSync(shortcut)) throw Error('Start menu shortcut is missing.');
  const shortcutId = run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '$s=New-Object -ComObject Shell.Application; $f=$s.Namespace([IO.Path]::GetDirectoryName($env:RDESK_SHORTCUT)); $f.ParseName([IO.Path]::GetFileName($env:RDESK_SHORTCUT)).ExtendedProperty("System.AppUserModel.ID")'], 30000, {env:{...process.env,RDESK_SHORTCUT:shortcut}}).trim();
  if (shortcutId !== identifier) throw Error('Installed Windows notification identity differs from the application.');
  checkEngine(join(install, 'engine'), join(install, 'engine/rclone.exe'));
  verification = 'isolated hosted-runner installation and installed engine checks; local files only';
  } else {
    if (!existsSync(join(release, 'remote-desk.exe')) || readFileSync(installer).subarray(0, 2).toString() !== 'MZ') throw Error('Windows application or installer missing');
    checkEngine(resolve('src-tauri/resources/engine'), resolve('src-tauri/resources/engine/rclone.exe'));
    verification = 'built Windows app/installer and staged engine checks; installer not executed on personal builder';
  }
  extension = 'exe';
}
mkdirSync('artifacts', {recursive:true});
const name = `Remote-Desk-${version}-${target}.${extension}`;
const output = resolve('artifacts', name);
cpSync(installer, output);
writeFileSync(`${output}.sha256`, `${createHash('sha256').update(readFileSync(output)).digest('hex')}  ${name}\n`);
cpSync('docs/install.md', 'artifacts/INSTALL.md');
writeFileSync(`artifacts/BUILD-${target}.json`, JSON.stringify({version, target, commit:process.env.GITHUB_SHA ?? null, rclone:engine.version, rcloneArchiveSha256:engine.sha256, signing:process.platform==='darwin'?'ad-hoc; not notarized':'unsigned', verification}, null, 2));
console.log(`Verified self-contained installer: ${name}`);
