import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export function getBuildInfo(root, env = process.env) {
  const cargo = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  const workspace = cargo.match(/^\[workspace\.package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  const version = workspace?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error('Missing workspace package version in Cargo.toml');
  const git = (...args) => {
    try {
      return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    } catch {
      return null;
    }
  };
  const rawCommit = env.ZIO_BUILD_COMMIT ?? git('rev-parse', 'HEAD');
  const commit = /^(?:[a-f\d]{40}|[a-f\d]{64})$/i.test(rawCommit ?? '') ? rawCommit : null;
  const branch = (env.ZIO_BUILD_BRANCH ?? git('branch', '--show-current')) || null;
  const tag = (env.ZIO_BUILD_TAG ?? git('tag', '--points-at', 'HEAD')) || null;
  const status = env.ZIO_BUILD_DIRTY ?? git('status', '--porcelain', '--untracked-files=normal');
  const dirty = env.ZIO_BUILD_DIRTY !== undefined
    ? status === 'true' ? true : status === 'false' ? false : null
    : status === null ? null : status !== '';
  const release = !!commit && dirty === false && !!tag?.split('\n').includes(`v${version}`);
  return { version, commit, branch, tag, dirty, release };
}

