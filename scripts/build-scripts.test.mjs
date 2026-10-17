import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, cpSync, rmSync, symlinkSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {spawnSync} from 'node:child_process';

const source = resolve('scripts');
function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'remote-desk-build-test-'));
  t.after(() => rmSync(directory, {recursive:true, force:true}));
  for (const folder of ['src-tauri', 'docs']) mkdirSync(join(directory, folder));
  writeFileSync(join(directory, 'package.json'), JSON.stringify({name:'test', version:'0.5.0', dependencies:{example:'1'}}));
  writeFileSync(join(directory, 'src-tauri/tauri.conf.json'), '{"version":"0.5.0"}');
  writeFileSync(join(directory, 'src-tauri/Cargo.toml'), '[package]\nname = "remote-desk"\nversion = "0.5.0"\n');
  writeFileSync(join(directory, 'src-tauri/Cargo.lock'), '[[package]]\nname = "remote-desk"\nversion = "0.5.0"\n\n[[package]]\nname = "other"\nversion = "1.0.0"\n');
  return directory;
}
function run(script, args, cwd, env = {}) {
  return spawnSync(process.execPath, [join(source, script), ...args], {cwd, encoding:'utf8', env:{...process.env, REMOTE_DESK_RELEASE_TAG:'', ...env}});
}
test('version helper changes all app versions, preserves dependency versions, rejects invalid input', t => {
  const directory = fixture(t);
  assert.equal(run('set-version.mjs', ['0.5.1'], directory).status, 0);
  for (const file of ['package.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock']) assert.match(readFileSync(join(directory, file), 'utf8'), /0\.5\.1/);
  assert.match(readFileSync(join(directory, 'src-tauri/Cargo.lock'), 'utf8'), /name = "other"\nversion = "1\.0\.0"/);
  const before = readFileSync(join(directory, 'package.json'), 'utf8');
  assert.notEqual(run('set-version.mjs', ['0.5.2;bad'], directory).status, 0);
  assert.equal(readFileSync(join(directory, 'package.json'), 'utf8'), before);
});
test('persistent-runner cleanup rejects unknown targets and preserves compiler caches', t => {
  const directory = fixture(t);
  for (const folder of ['artifacts', 'src-tauri/target/x86_64-pc-windows-msvc/release/bundle', 'src-tauri/target/debug', 'src-tauri/resources/engine']) mkdirSync(join(directory, folder), {recursive:true});
  writeFileSync(join(directory, 'artifacts/old.exe'), 'old');
  writeFileSync(join(directory, 'src-tauri/target/debug/cache'), 'keep');
  assert.notEqual(run('prepare-build.mjs', ['../../..'], directory).status, 0);
  assert.ok(existsSync(join(directory, 'artifacts/old.exe')));
  assert.equal(run('prepare-build.mjs', ['x86_64-pc-windows-msvc'], directory).status, 0);
  assert.ok(!existsSync(join(directory, 'artifacts/old.exe')));
  assert.ok(!existsSync(join(directory, 'src-tauri/target/x86_64-pc-windows-msvc/release/bundle')));
  assert.ok(existsSync(join(directory, 'src-tauri/target/debug/cache')));
  assert.notEqual(run('prepare-build.mjs', ['--publish'], directory, {REMOTE_DESK_RELEASE_TAG:'v0.5.0', REMOTE_DESK_WINDOWS_ONLY:'true'}).status, 0);
});
test('personal Windows packaging checks never execute an installer', {skip:process.platform !== 'win32' || !process.env.RDESK_TEST_RCLONE}, t => {
  const directory = fixture(t);
  const target = 'x86_64-pc-windows-msvc';
  for (const folder of [`.tools/ci-engine`, `src-tauri/target/${target}/release/bundle/nsis`, 'src-tauri/resources/engine']) mkdirSync(join(directory, folder), {recursive:true});
  const engine = process.env.RDESK_TEST_RCLONE;
  const version = spawnSync(engine, ['version'], {encoding:'utf8'}).stdout.match(/^rclone v([^\s]+)/)[1];
  writeFileSync(join(directory, '.tools/ci-engine/engine.json'), JSON.stringify({version, sha256:'fixture'}));
  writeFileSync(join(directory, 'src-tauri/resources/engine/engine.json'), JSON.stringify({version, archiveSha256:'fixture'}));
  writeFileSync(join(directory, 'src-tauri/resources/engine/COPYING-rclone.txt'), 'fixture');
  cpSync(engine, join(directory, 'src-tauri/resources/engine/rclone.exe'));
  writeFileSync(join(directory, `src-tauri/target/${target}/release/remote-desk.exe`), 'MZ fixture');
  // This is deliberately not executable. Attempting a silent install would fail the test.
  writeFileSync(join(directory, `src-tauri/target/${target}/release/bundle/nsis/fixture-setup.exe`), 'MZ fixture');
  writeFileSync(join(directory, 'docs/install.md'), 'fixture');
  const result = run('package-installers.mjs', [target], directory, {RUNNER_ENVIRONMENT:'self-hosted', GITHUB_ACTIONS:'true'});
  assert.equal(result.status, 0, result.stderr);
  const metadata = JSON.parse(readFileSync(join(directory, `artifacts/BUILD-${target}.json`), 'utf8'));
  assert.match(metadata.verification, /installer not executed/);
});
test('build cleanup refuses a target directory linked outside its checkout', t => {
  const directory = fixture(t);
  const outside = mkdtempSync(join(tmpdir(), 'remote-desk-outside-test-'));
  t.after(() => rmSync(outside, {recursive:true, force:true}));
  const bundle = join(outside, 'x86_64-pc-windows-msvc/release/bundle');
  mkdirSync(bundle, {recursive:true});
  writeFileSync(join(bundle, 'keep'), 'keep');
  symlinkSync(outside, join(directory, 'src-tauri/target'), process.platform === 'win32' ? 'junction' : 'dir');
  const result = run('prepare-build.mjs', ['x86_64-pc-windows-msvc'], directory);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /link outside/);
  assert.ok(existsSync(join(bundle, 'keep')));
});
