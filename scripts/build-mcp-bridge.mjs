#!/usr/bin/env node
// Build-time tooling only: the distributed relay is a standalone Rust executable.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const WINDOWS_TARGET = 'x86_64-pc-windows-msvc';
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const read = (root, path) => readFileSync(join(root, path), 'utf8');
const json = (root, path) => JSON.parse(read(root, path));
const packageVersion = (text) => text.match(/\[package\]([^]*?)(?=\n\[|$)/)?.[1].match(/^version\s*=\s*"([^"]+)"/m)?.[1];

export function resolveTarget(explicit, env = process.env, host = () => execFileSync('rustc', ['-vV'], { encoding: 'utf8', shell: false })) {
  if (explicit && env.TAURI_ENV_TARGET_TRIPLE && explicit !== env.TAURI_ENV_TARGET_TRIPLE) throw Error('Explicit and Tauri targets disagree');
  const target = explicit || env.TAURI_ENV_TARGET_TRIPLE || host().match(/^host: (.+)$/m)?.[1].trim();
  if (target !== WINDOWS_TARGET) throw Error(`Unsupported MCP target: ${target ?? 'unknown'}; Windows x86_64 MSVC is required`);
  return target;
}

export function readCompatibility(root = ROOT) {
  const app_version = json(root, 'package.json').version;
  const bridge_version = packageVersion(read(root, 'src-tauri/crates/mcp-bridge/Cargo.toml'));
  const versions = [app_version, bridge_version, json(root, 'src-tauri/tauri.conf.json').version, packageVersion(read(root, 'src-tauri/Cargo.toml'))];
  if (!app_version || versions.some((version) => version !== app_version)) throw Error('Application/bridge version mismatch; update all package manifests together');
  const lock = read(root, 'src-tauri/crates/mcp-bridge/Cargo.lock');
  const lockedBridge = lock.split('[[package]]').find((entry) => /^name\s*=\s*"lexena-mcp"$/m.test(entry));
  if (lockedBridge?.match(/^version\s*=\s*"([^"]+)"/m)?.[1] !== app_version) throw Error('Bridge lockfile version mismatch');
  const ipc_version = Number(read(root, 'src-tauri/crates/mcp-ipc/src/lib.rs').match(/pub const IPC_VERSION:\s*u32\s*=\s*(\d+)/)?.[1]);
  if (ipc_version !== 1) throw Error('Unsupported IPC version; review packaging compatibility before changing protocol');
  return { app_version, bridge_version, ipc_version };
}

export function buildBridge({ root = ROOT, target: explicit, env = process.env, cargo = 'cargo', debug = false, run = execFileSync } = {}) {
  readCompatibility(root);
  const target = resolveTarget(explicit, env);
  const outputDir = join(root, 'src-tauri/target/mcp-bridge');
  const args = ['build', '--locked', '--manifest-path', join(root, 'src-tauri/crates/mcp-bridge/Cargo.toml'), '--target', target, '--target-dir', outputDir];
  if (!debug) args.push('--release');
  run(cargo, args, { cwd: root, env, stdio: 'inherit', shell: false, windowsHide: true });
  const destination = join(root, `src-tauri/binaries/lexena-mcp-${target}.exe`);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(join(outputDir, target, debug ? 'debug' : 'release', 'lexena-mcp.exe'), destination);
  return destination;
}

const hash = (directory, name) => createHash('sha256').update(readFileSync(join(directory, name))).digest('hex');
export function createPortableManifest(directory, compatibility, target = WINDOWS_TARGET) {
  resolveTarget(target, {});
  const manifest = { schema_version: 1, ...compatibility, target, sha256: { 'lexena.exe': hash(directory, 'lexena.exe'), 'lexena-mcp.exe': hash(directory, 'lexena-mcp.exe') } };
  verifyPortableManifest(directory, manifest);
  return manifest;
}

export function verifyPortableManifest(directory, manifest) {
  if (manifest.schema_version !== 1) throw Error('Unsupported MCP manifest schema');
  if (!manifest.app_version || manifest.app_version !== manifest.bridge_version) throw Error('MCP manifest version mismatch');
  if (manifest.ipc_version !== 1) throw Error('Unsupported IPC version');
  resolveTarget(manifest.target, {});
  if (!manifest.sha256 || Object.keys(manifest.sha256).sort().join(',') !== 'lexena-mcp.exe,lexena.exe') throw Error('MCP manifest must hash exactly both executables');
  for (const name of ['lexena.exe', 'lexena-mcp.exe']) {
    if (manifest.sha256[name] !== hash(directory, name)) throw Error(`MCP executable hash mismatch: ${name}`);
  }
}

function main(args) {
  let target, cargo = 'cargo', debug = process.env.TAURI_ENV_DEBUG === 'true', manifestDir;
  for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === '--debug') debug = true;
    else if (['--target', '--cargo', '--portable-manifest'].includes(argument)) {
      const value = args[++index];
      if (!value || value.startsWith('--')) throw Error(`Missing value for ${argument}`);
      if (argument === '--target') target = value;
      if (argument === '--cargo') cargo = value;
      if (argument === '--portable-manifest') manifestDir = resolve(value);
    } else if (argument !== '--check') throw Error(`Unknown argument: ${argument}`);
  }
  const compatibility = readCompatibility();
  if (manifestDir) {
    const manifest = createPortableManifest(manifestDir, compatibility, target || WINDOWS_TARGET);
    writeFileSync(join(manifestDir, 'mcp-version.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  } else if (!args.includes('--check')) {
    console.log(`MCP bridge staged: ${buildBridge({ target, cargo, debug })}`);
  } else console.log(`MCP packaging versions coherent: ${JSON.stringify(compatibility)}`);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(`MCP packaging: ${error.message}`); process.exitCode = 1; }
}
