import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { buildBridge, readCompatibility, resolveTarget, createPortableManifest, verifyPortableManifest } from './build-mcp-bridge.mjs';

const target = 'x86_64-pc-windows-msvc';
function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'Lexena été 研究 '));
  mkdirSync(join(root, 'src-tauri/crates/mcp-bridge'), { recursive: true });
  mkdirSync(join(root, 'src-tauri/crates/mcp-ipc/src'), { recursive: true });
  writeFileSync(join(root, 'package.json'), JSON.stringify({ version: '3.2.2-beta.1' }));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '3.2.2-beta.1' }));
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nname="lexena"\nversion="3.2.2-beta.1"\n');
  writeFileSync(join(root, 'src-tauri/crates/mcp-bridge/Cargo.toml'), '[package]\nname="lexena-mcp"\nversion="3.2.2-beta.1"\n');
  writeFileSync(join(root, 'src-tauri/crates/mcp-bridge/Cargo.lock'), 'version = 4\n\n[[package]]\nname = "lexena-mcp"\nversion = "3.2.2-beta.1"\n');
  writeFileSync(join(root, 'src-tauri/crates/mcp-ipc/src/lib.rs'), 'pub const IPC_VERSION: u32 = 1;\n');
  return root;
}

test('explicit Tauri target is honored and unsupported/conflicting targets fail closed', () => {
  assert.equal(resolveTarget(undefined, { TAURI_ENV_TARGET_TRIPLE: target }, () => { throw Error('host queried'); }), target);
  assert.equal(resolveTarget(target, {}, () => { throw Error('host queried'); }), target);
  assert.equal(resolveTarget(undefined, {}, () => 'rustc 1.93.1\nhost: x86_64-pc-windows-msvc\n'), target);
  assert.throws(() => resolveTarget('aarch64-pc-windows-msvc', {}, () => ''), /Unsupported/);
  assert.throws(() => resolveTarget(target, { TAURI_ENV_TARGET_TRIPLE: 'aarch64-pc-windows-msvc' }, () => ''), /disagree/);
  assert.throws(() => resolveTarget('x86_64-pc-windows-msvc; touch injected', {}, () => ''), /Unsupported/);
});

test('version drift and incompatible IPC prevent staging', () => {
  const root = fixture();
  try {
    assert.deepEqual(readCompatibility(root), { app_version: '3.2.2-beta.1', bridge_version: '3.2.2-beta.1', ipc_version: 1 });
    writeFileSync(join(root, 'src-tauri/crates/mcp-bridge/Cargo.toml'), '[package]\nversion="0.1.0"\n');
    assert.throws(() => readCompatibility(root), /version/);
    writeFileSync(join(root, 'src-tauri/crates/mcp-bridge/Cargo.toml'), '[package]\nversion="3.2.2-beta.1"\n');
    writeFileSync(join(root, 'src-tauri/crates/mcp-ipc/src/lib.rs'), 'pub const IPC_VERSION: u32 = 2;');
    assert.throws(() => readCompatibility(root), /IPC/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('build stages exact bytes in Unicode paths using locked Cargo and an argument array', () => {
  const root = fixture();
  try {
    const result = buildBridge({ root, target, env: {}, cargo: 'C:/Tool chain/cargo.exe', run(command, args, options) {
      assert.equal(command, 'C:/Tool chain/cargo.exe');
      assert.equal(options.shell, false);
      assert.equal(options.cwd, root);
      assert.ok(args.includes('--locked'));
      assert.equal(args[args.indexOf('--target') + 1], target);
      const output = join(args[args.indexOf('--target-dir') + 1], target, 'release/lexena-mcp.exe');
      mkdirSync(resolve(output, '..'), { recursive: true });
      writeFileSync(output, 'synthetic EXE');
    } });
    assert.equal(result, join(root, `src-tauri/binaries/lexena-mcp-${target}.exe`));
    assert.equal(readFileSync(result, 'utf8'), 'synthetic EXE');
    assert.throws(() => buildBridge({ root, target, env: {}, run() { throw Error('cargo failed'); } }), /cargo failed/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('portable manifest hashes both exact filenames and rejects tampering/version drift', () => {
  const root = fixture();
  try {
    writeFileSync(join(root, 'lexena.exe'), 'main');
    writeFileSync(join(root, 'lexena-mcp.exe'), 'bridge');
    const manifest = createPortableManifest(root, readCompatibility(root), target);
    assert.equal(manifest.schema_version, 1);
    assert.equal(manifest.sha256['lexena.exe'], createHash('sha256').update('main').digest('hex'));
    assert.equal(manifest.sha256['lexena-mcp.exe'], createHash('sha256').update('bridge').digest('hex'));
    assert.doesNotThrow(() => verifyPortableManifest(root, manifest));
    assert.throws(() => verifyPortableManifest(root, { ...manifest, bridge_version: '0.1.0' }), /version/);
    assert.throws(() => verifyPortableManifest(root, { ...manifest, ipc_version: 2 }), /IPC/);
    assert.throws(() => verifyPortableManifest(root, { ...manifest, target: 'aarch64-pc-windows-msvc' }), /Unsupported/);
    writeFileSync(join(root, 'lexena-mcp.exe'), 'tampered');
    assert.throws(() => verifyPortableManifest(root, manifest), /hash/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('portable packager produces a flat ZIP with two executable hashes from another cwd', { skip: process.platform !== 'win32' }, () => {
  const root = fixture();
  try {
    writeFileSync(join(root, 'app input.exe'), 'main');
    writeFileSync(join(root, 'relay input.exe'), 'bridge');
    const destination = join(root, 'Lexena portable été 研究.zip');
    execFileSync('pwsh.exe', ['-NoProfile', '-File', fileURLToPath(new URL('./package-mcp-portable.ps1', import.meta.url)), '-AppPath', join(root, 'app input.exe'), '-BridgePath', join(root, 'relay input.exe'), '-Destination', destination], { cwd: tmpdir(), encoding: 'utf8' });
    // Decode ZIP via the actual Windows shipping API, then validate observable contents.
    execFileSync('pwsh.exe', ['-NoProfile', '-Command', 'Expand-Archive -LiteralPath $env:LEXENA_TEST_ZIP -DestinationPath $env:LEXENA_TEST_EXTRACT'], { env: { ...process.env, LEXENA_TEST_ZIP: destination, LEXENA_TEST_EXTRACT: join(root, 'extracted') }, encoding: 'utf8' });
    const directory = join(root, 'extracted');
    const manifest = JSON.parse(readFileSync(join(directory, 'mcp-version.json'), 'utf8'));
    assert.equal(manifest.app_version, JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version);
    assert.equal(readFileSync(join(directory, 'lexena.exe'), 'utf8'), 'main');
    assert.equal(readFileSync(join(directory, 'lexena-mcp.exe'), 'utf8'), 'bridge');
    verifyPortableManifest(directory, manifest);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
