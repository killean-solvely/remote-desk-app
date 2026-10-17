import {mkdirSync, rmSync, readFileSync, existsSync, appendFileSync, realpathSync} from 'node:fs';
import {resolve, sep, dirname} from 'node:path';
import {spawnSync} from 'node:child_process';

const target = process.argv[2];
if (process.env.REMOTE_DESK_WINDOWS_ONLY === 'true' && process.env.REMOTE_DESK_RELEASE_TAG) throw Error('Windows-only checks cannot publish a release');
const targets = ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin', 'x86_64-apple-darwin'];
if (!targets.includes(target) && target !== '--publish') throw Error('Unsupported build target');
if (Number(process.versions.node.split('.')[0]) < 22) throw Error('Node.js 22 or newer is required');
const root = resolve('.');
const physicalRoot = realpathSync(root);
if (process.platform === 'win32' && process.env.GITHUB_PATH) {
  const git = spawnSync('where.exe', ['git'], {encoding:'utf8'}).stdout?.trim().split(/\r?\n/)[0];
  const bash = git && resolve(dirname(git), '../bin');
  if (!bash || !existsSync(resolve(bash, 'bash.exe'))) throw Error('Git for Windows Bash is required for build actions');
  // Otherwise Windows may select its WSL launcher for actions requesting shell: bash.
  appendFileSync(process.env.GITHUB_PATH, bash + '\n');
}
function clear(relative) {
  const path = resolve(root, relative);
  if (!path.startsWith(root + sep)) throw Error('Build cleanup escaped the checkout');
  let ancestor = path;
  while (!existsSync(ancestor)) ancestor = dirname(ancestor);
  const physicalAncestor = realpathSync(ancestor);
  if (physicalAncestor !== physicalRoot && !physicalAncestor.startsWith(physicalRoot + sep)) throw Error('Build cleanup followed a link outside the checkout');
  rmSync(path, {recursive:true, force:true});
}
// Keep compiler caches, but never mix packages from previous jobs or versions.
clear('artifacts');
mkdirSync('artifacts');
if (target !== '--publish') {
  clear(`src-tauri/target/${target}/release/bundle`);
  clear('src-tauri/resources/engine');
  if (process.platform === 'darwin') {
    if (Number(spawnSync('/usr/bin/sw_vers', ['-productVersion'], {encoding:'utf8'}).stdout.trim().split('.')[0]) < 26) throw Error('The bundled engine requires macOS 26 or newer');
    if (target === 'x86_64-apple-darwin' && process.arch === 'arm64') {
      const result = spawnSync('/usr/bin/arch', ['-x86_64', '/usr/bin/true']);
      if (result.status !== 0) throw Error('Install Rosetta before building/testing Intel packages on Apple Silicon');
    }
  }
}
const version = JSON.parse(readFileSync('package.json', 'utf8')).version;
const tauriVersion = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8')).version;
const cargoVersion = readFileSync('src-tauri/Cargo.toml', 'utf8').match(/\[package\][\s\S]*?\nversion = "([^"]+)"/)?.[1];
const lockVersion = readFileSync('src-tauri/Cargo.lock', 'utf8').match(/\[\[package\]\]\s*\nname = "remote-desk"\s*\nversion = "([^"]+)"/)?.[1];
if ([tauriVersion, cargoVersion, lockVersion].some(v => v !== version)) throw Error('App version files differ; run scripts/set-version.mjs before committing');
if (process.env.REMOTE_DESK_RELEASE_TAG && process.env.REMOTE_DESK_RELEASE_TAG !== `v${version}`) throw Error('Release tag differs from the checked-out version');
console.log(`Prepared clean package output for ${target}; compiler caches retained`);
