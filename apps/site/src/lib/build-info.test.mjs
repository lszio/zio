import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { test } from 'node:test';
import { getBuildInfo } from './build-info.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'zio-build-info-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.2.0"\n');
  return root;
}

test('only a matching clean tagged commit is a release', (t) => {
  const root = fixture(t);
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '-b', 'main');
  git('add', 'Cargo.toml');
  git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-m', 'fixture');
  git('tag', 'v0.1.0');
  assert.equal(getBuildInfo(root, {}).release, false);
  git('tag', '-d', 'v0.1.0');
  git('tag', 'v0.2.0');
  assert.equal(getBuildInfo(root, {}).release, true);
  writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "0.2.0"\n# modified\n');
  const modified = getBuildInfo(root, {});
  assert.equal(modified.release, false);
  assert.equal(modified.dirty, true);
});

test('archives expose missing provenance; CI metadata restores it without Git', (t) => {
  const root = fixture(t);
  const archive = getBuildInfo(root, {});
  assert.equal(archive.commit, null);
  assert.equal(archive.release, false);
  const commit = 'a'.repeat(40);
  const ci = getBuildInfo(root, { ZIO_BUILD_COMMIT: commit, ZIO_BUILD_BRANCH: 'release/0.2', ZIO_BUILD_TAG: 'v0.2.0', ZIO_BUILD_DIRTY: 'false' });
  assert.equal(ci.release, true);
  assert.equal(ci.commit, commit);
  assert.equal(ci.branch, 'release/0.2');
  assert.equal(getBuildInfo(root, { ZIO_BUILD_COMMIT: 'not-a-commit', ZIO_BUILD_TAG: 'v0.2.0', ZIO_BUILD_DIRTY: 'false' }).release, false);
});
