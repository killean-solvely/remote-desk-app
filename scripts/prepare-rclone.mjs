import {createHash} from 'node:crypto';
import {appendFileSync, chmodSync, cpSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import {spawnSync} from 'node:child_process';

const version = '1.75.2';
// Pinned against the publisher's versioned SHA256SUMS, not a mutable latest URL.
const releases = {
  'darwin-arm64': ['osx-arm64', '610ab7d48615135dfc60b763858b11a426a0d3d1160c1bb76fb0ae6011b02edd'],
  'darwin-x64': ['osx-amd64', '574e1f31cf1c464e5fb0325cb66fcbdeaf1efa1802bd9b17f13c256fd74b6561'],
  'win32-x64': ['windows-amd64', '8a9db2bad1dc79a65b3348fd857069598e2aab7cf4d118b77b1df23055aff818'],
};
const target = process.argv[2];
const targetPlatforms = {'aarch64-apple-darwin':'darwin-arm64', 'x86_64-apple-darwin':'darwin-x64', 'x86_64-pc-windows-msvc':'win32-x64'};
const platformKey = target ? targetPlatforms[target] : `${process.platform}-${process.arch}`;
if (!platformKey?.startsWith(process.platform + '-')) throw Error('Build the engine on the corresponding OS');
const release = releases[platformKey];
if (!release) throw new Error('This build script supports native Windows x64, Mac arm64 and Mac x64 runners.');
const [platform, expected] = release;
const directory = resolve('.tools/ci-engine');
mkdirSync(directory, {recursive: true});
const basename = `rclone-v${version}-${platform}`;
const response = await fetch(`https://downloads.rclone.org/v${version}/${basename}.zip`);
if (!response.ok) throw new Error(`Engine download failed: HTTP ${response.status}`);
const archive = Buffer.from(await response.arrayBuffer());
if (createHash('sha256').update(archive).digest('hex') !== expected) throw new Error('rclone checksum mismatch');
const archivePath = join(directory, `${basename}.zip`);
writeFileSync(archivePath, archive);
const extracted = process.platform === 'darwin'
  ? spawnSync('/usr/bin/ditto', ['-x', '-k', archivePath, directory], {stdio: 'inherit'})
  : spawnSync('tar.exe', ['-xf', archivePath, '-C', directory], {stdio: 'inherit'});
if (extracted.error) throw extracted.error;
if (extracted.status !== 0) throw new Error('Engine extraction failed');
const executable = join(directory, basename, process.platform === 'win32' ? 'rclone.exe' : 'rclone');
if (process.platform !== 'win32') chmodSync(executable, 0o755);
const license = await fetch(`https://raw.githubusercontent.com/rclone/rclone/v${version}/COPYING`);
if (!license.ok) throw new Error('Could not obtain rclone license');
writeFileSync(join(directory, 'COPYING-rclone.txt'), await license.text());
writeFileSync(join(directory, 'engine.json'), JSON.stringify({version, executable, sha256: expected}, null, 2));
if (process.env.GITHUB_ENV) appendFileSync(process.env.GITHUB_ENV, `RDESK_TEST_RCLONE=${executable}\n`);
console.log(`Verified rclone ${version} for ${platform}`);

// Stage only runtime files into Tauri resources; no archives or build tools.
const bundled = resolve('src-tauri/resources/engine');
mkdirSync(bundled, {recursive: true});
const bundledExecutable = join(bundled, process.platform === 'win32' ? 'rclone.exe' : 'rclone');
cpSync(executable, bundledExecutable);
cpSync(join(directory, 'COPYING-rclone.txt'), join(bundled, 'COPYING-rclone.txt'));
writeFileSync(join(bundled, 'engine.json'), JSON.stringify({version, archiveSha256: expected, platform}, null, 2));
if (process.platform === 'darwin') {
  chmodSync(bundledExecutable, 0o755);
  const signing = spawnSync('/usr/bin/codesign', ['--force', '--sign', process.env.APPLE_SIGNING_IDENTITY || '-', bundledExecutable], {stdio: 'inherit'});
  if (signing.error) throw signing.error;
  if (signing.status !== 0) throw new Error('Could not sign bundled engine');
}
