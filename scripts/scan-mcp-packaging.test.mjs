import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync, openSync, writeSync, closeSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
const scanner = fileURLToPath(new URL('../.github/scripts/scan-secrets-in-bundles.mjs', import.meta.url));
test('release scan detects secrets past 50MB in extracted native binaries', () => {
  const root = mkdtempSync(join(tmpdir(), 'Lexena scan été '));
  try {
    const executable = join(root, 'lexena.exe');
    const file = openSync(executable, 'w');
    writeSync(file, Buffer.alloc(1), 0, 1, 51 * 1024 * 1024);
    writeSync(file, Buffer.from('sb_secret_' + 'a'.repeat(24)), 0, 34, 51 * 1024 * 1024 + 1);
    closeSync(file);
    const result = spawnSync(process.execPath, [scanner, '--root', root], { cwd: tmpdir(), encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /supabase-service-role-key/);
    assert.doesNotMatch(result.stderr, /sb_secret_a/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
test('release scan succeeds for a portable manifest and clean native sidecar', () => {
  const root = mkdtempSync(join(tmpdir(), 'Lexena scan clean '));
  try {
    writeFileSync(join(root, 'lexena-mcp.exe'), 'synthetic clean executable');
    writeFileSync(join(root, 'mcp-version.json'), '{"schema_version":1}');
    assert.equal(spawnSync(process.execPath, [scanner, '--root', root], { cwd: tmpdir() }).status, 0);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
