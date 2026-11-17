/**
 * Reading what the app hands out as a download: a ZIP extractor that does not share code with the
 * app's writer (fflate), and the export validator of instagram.md (section 9).
 */
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { crc32, inflateRawSync } from 'node:zlib';
import { E2E_ROOT } from './paths';

/**
 * Extracts a ZIP archive into `target` (emptied first) through its central directory, checking
 * every member's size and CRC-32. Returns the member names, sorted.
 */
export function extractZip(zipPath: string, target: string): string[] {
  const data = readFileSync(zipPath);
  let end = -1;
  for (let at = data.length - 22; at >= 0; at--) {
    if (data.readUInt32LE(at) === 0x06054b50) {
      end = at;
      break;
    }
  }
  if (end < 0) throw new Error(`${zipPath}: no end-of-archive record`);
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  const count = data.readUInt16LE(end + 10);
  let offset = data.readUInt32LE(end + 16);
  const names: string[] = [];
  for (let n = 0; n < count; n++) {
    if (data.readUInt32LE(offset) !== 0x02014b50) throw new Error(`${zipPath}: damaged central directory`);
    const method = data.readUInt16LE(offset + 10);
    const crc = data.readUInt32LE(offset + 16);
    const compressedSize = data.readUInt32LE(offset + 20);
    const size = data.readUInt32LE(offset + 24);
    const nameLength = data.readUInt16LE(offset + 28);
    const extraLength = data.readUInt16LE(offset + 30);
    const commentLength = data.readUInt16LE(offset + 32);
    const local = data.readUInt32LE(offset + 42);
    const name = data.toString('utf8', offset + 46, offset + 46 + nameLength);
    offset += 46 + nameLength + extraLength + commentLength;

    if (name.startsWith('/') || name.includes('\\') || name.split('/').includes('..')) throw new Error(`unsafe member name ${name}`);
    if (data.readUInt32LE(local) !== 0x04034b50) throw new Error(`${name}: no local header`);
    const start = local + 30 + data.readUInt16LE(local + 26) + data.readUInt16LE(local + 28);
    const stored = data.subarray(start, start + compressedSize);
    let content: Buffer;
    if (method === 0) content = stored;
    else if (method === 8) content = inflateRawSync(stored);
    else throw new Error(`${name}: unsupported compression method ${method}`);
    if (content.length !== size || crc32(content) !== crc) throw new Error(`${name}: size or CRC does not match`);

    const path = join(target, ...name.split('/'));
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, content);
    names.push(name);
  }
  return names.sort();
}

/** Runs the export validator of instagram.md (section 9, the igx:validate.mjs block) on a folder. */
export function validateExport(folder: string): { status: number | null; output: string } {
  const runbook = readFileSync(join(E2E_ROOT, '..', 'instagram.md'), 'utf8');
  const code = /```js\r?\n(\/\/ igx:validate\.mjs[\s\S]*?)\r?\n```/.exec(runbook);
  if (!code) throw new Error('instagram.md: section 9 no longer has the igx:validate.mjs block');
  const script = join(E2E_ROOT, 'test-results', 'validate-export.mjs');
  mkdirSync(dirname(script), { recursive: true });
  writeFileSync(script, code[1]);
  const run = spawnSync(process.execPath, [script, folder], { encoding: 'utf8' });
  return { status: run.status, output: `${run.stdout}${run.stderr}` };
}

/** Where a test keeps the archives it downloaded and their extracted contents. */
export const DOWNLOADS = join(E2E_ROOT, 'test-results', 'downloads');
