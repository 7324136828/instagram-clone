/// <reference types="node" />
/**
 * The validator that instagram.md (section 9) hands to the exporting agent, run exactly as printed
 * with Node.js. It has to pass what the clone imports without a warning: the documented older
 * fields (`video`, `videos`, a post-level poster), the project's own test fixtures, and the
 * library folder the backend writes (README: "The folder is itself a valid export").
 */
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { unzipSync } from 'fflate';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import runbook from '../../../instagram.md?raw';
import { importFromFiles, releaseImport, sanitizePostUrl, type ImportResult } from './index';
import { IMAGES, VIDEOS, folderFiles, json, type FileMap } from './testkit';

type Json = Record<string, unknown>;

const FIXTURES = fileURLToPath(new URL('../../../e2e/fixtures/', import.meta.url));
let work = '';
let script = '';
let folders = 0;

beforeAll(() => {
  const code = /```js\r?\n(\/\/ igx:validate\.mjs[\s\S]*?)\r?\n```/.exec(runbook);
  if (!code) throw new Error('instagram.md: section 9 no longer has the igx:validate.mjs block');
  work = mkdtempSync(join(tmpdir(), 'igx-validate-'));
  script = join(work, 'validate-export.mjs');
  writeFileSync(script, code[1]);
});

afterAll(() => {
  if (work) rmSync(work, { recursive: true, force: true });
});

const results: ImportResult[] = [];
afterEach(() => {
  for (const result of results.splice(0)) releaseImport(result);
});

interface Verdict {
  status: number | null;
  lines: string[];
}

/** Writes the files into a new folder and runs the validator on it. */
function validate(files: FileMap): Verdict {
  const folder = join(work, `export-${++folders}`);
  for (const [path, content] of Object.entries(files)) {
    const full = join(folder, ...path.split('/'));
    mkdirSync(dirname(full), { recursive: true });
    writeFileSync(full, content);
  }
  const run = spawnSync(process.execPath, [script, folder], { encoding: 'utf8' });
  return { status: run.status, lines: run.stdout.split(/\r?\n/).filter(Boolean) };
}

function fixture(name: string): FileMap {
  const files: FileMap = {};
  for (const [path, content] of Object.entries(unzipSync(readFileSync(join(FIXTURES, `${name}.zip`))))) {
    if (!path.endsWith('/')) files[path] = content;
  }
  return files;
}

async function importWarnings(files: FileMap): Promise<{ result: ImportResult; warnings: string[] }> {
  const result = await importFromFiles(folderFiles('instagram-export', files));
  results.push(result);
  return { result, warnings: result.warnings.map((warning) => warning.message) };
}

const user = { username: 'lumen.atlas', profile_url: 'https://www.instagram.com/lumen.atlas/' };
const details = (id: string): Json => ({
  id,
  url: `https://www.instagram.com/p/${id}/`,
  user,
  caption: `Post ${id}`,
  comment_count: 0,
  taken_at: '2026-11-01T15:04:05Z',
});
const manifest = (posts: Json[], extra: Json = {}): string =>
  json({ format: 'instagram-export', version: 1, ...extra, posts });

describe('the validator of instagram.md', () => {
  it.each(['export-basic', 'export-part1', 'export-part2', 'export-videos'])(
    'passes the fixture %s without an error or a warning',
    (name) => {
      const verdict = validate(fixture(name));
      expect(verdict.lines.filter((line) => /^(ERROR|WARN):/.test(line))).toEqual([]);
      expect(verdict.lines.at(-1)).toMatch(/^OK: /);
      expect(verdict.status).toBe(0);
    },
  );

  it('fails the fixture with problems, without calling a file it names unreferenced', () => {
    const verdict = validate(fixture('export-problems'));
    expect(verdict.status).toBe(1);
    expect(verdict.lines).toContain('ERROR: posts[5]: file "posts/problem6.svg" is not a supported picture or video (unknown signature)');
    expect(verdict.lines.filter((line) => line.includes('is not referenced'))).toEqual([]);
  });

  it('reads the older video, videos and post-level poster fields the way the clone does', async () => {
    const files: FileMap = {
      'post.json': manifest([
        { ...details('V1'), video: 'posts/v1.mp4' },
        { ...details('V2'), videos: ['posts/v2.webm'], poster: 'posts/v2_poster.jpg' },
        {
          ...details('V3'),
          image: 'posts/v3_1.jpg',
          images: ['posts/v3_1.jpg', 'posts/v3_2.png'],
          video: 'posts/v3_3.mov',
          thumbnail: 'posts/v3_3_thumb.jpg',
        },
      ]),
      'posts/v1.mp4': VIDEOS.mp4(1),
      'posts/v2.webm': VIDEOS.webm(2),
      'posts/v2_poster.jpg': IMAGES.jpeg(3),
      'posts/v3_1.jpg': IMAGES.jpeg(4),
      'posts/v3_2.png': IMAGES.png(5),
      'posts/v3_3.mov': VIDEOS.mov(6),
      'posts/v3_3_thumb.jpg': IMAGES.jpeg(7),
    };
    const verdict = validate(files);
    expect(verdict.lines).toEqual(['OK: 3 post(s), 4 image file(s), 3 video file(s), 0 warning(s)']);
    expect(verdict.status).toBe(0);

    // The clone agrees: every file is used, nothing is warned about.
    const { result, warnings } = await importWarnings(files);
    expect(warnings).toEqual([]);
    expect(result.posts.map((post) => post.media.map((item) => item.path))).toEqual([
      ['posts/v1.mp4'], ['posts/v2.webm'], ['posts/v3_1.jpg', 'posts/v3_2.png', 'posts/v3_3.mov'],
    ]);
    expect(result.posts[1].media[0].posterPath).toBe('posts/v2_poster.jpg');
    expect(result.posts[2].media[2].posterPath).toBe('posts/v3_3_thumb.jpg');
  });

  it('accepts a poster on a media object without "type" when the file is a video', () => {
    const files: FileMap = {
      'post.json': manifest([{ ...details('P1'), media: [{ path: 'posts/p1.mp4', poster: 'posts/p1_poster.jpg' }] }]),
      'posts/p1.mp4': VIDEOS.mp4(),
      'posts/p1_poster.jpg': IMAGES.jpeg(),
    };
    expect(validate(files).lines).toEqual(['OK: 1 post(s), 1 image file(s), 1 video file(s), 0 warning(s)']);

    // ...and still says so when the file is a picture, which has no use for a poster.
    const picture: FileMap = {
      'post.json': manifest([{ ...details('P2'), media: [{ path: 'posts/p2.jpg', poster: 'posts/p2_poster.jpg' }] }]),
      'posts/p2.jpg': IMAGES.jpeg(),
      'posts/p2_poster.jpg': IMAGES.jpeg(2),
    };
    expect(validate(picture).lines).toContain(
      'WARN: posts[0] media[0]: "poster" is only used for a video, and "posts/p2.jpg" is a picture',
    );
  });

  it('passes the library folder the backend writes: image/images mirrors and instagr.am links', () => {
    // The shape backend/app/library.py writes: "media" plus image/images copies of its pictures
    // and posters, a "library" object, and the links as the clone kept them.
    const post = (id: string, url: string, media: Json[]): Json => {
      const pictures = media.map((item) => (item.type === 'image' ? item.path : item.poster)).filter(Boolean);
      return {
        ...details(id),
        url,
        media_type: media.length > 1 ? 'carousel' : media[0].type,
        media,
        ...(pictures.length ? { image: pictures[0], images: pictures } : {}),
      };
    };
    expect(sanitizePostUrl('https://instagr.am/p/LIB2/')).toBe('https://instagr.am/p/LIB2/');
    const files: FileMap = {
      'post.json': manifest([
        post('LIB1', 'https://www.instagram.com/p/LIB1/', [
          { type: 'image', path: 'posts/LIB1_1.jpg', mime: 'image/jpeg', bytes: 64 },
          { type: 'video', path: 'posts/LIB1_2.mp4', poster: 'posts/LIB1_2_poster.jpg', mime: 'video/mp4', bytes: 64 },
        ]),
        post('LIB2', 'https://instagr.am/p/LIB2/', [{ type: 'image', path: 'posts/LIB2_1.png', mime: 'image/png', bytes: 64 }]),
        post('LIB3', 'https://www.instagram.com/reel/LIB3/', [{ type: 'video', path: 'posts/LIB3_1.webm', mime: 'video/webm', bytes: 64 }]),
      ], {
        exported_at: '2026-11-10T09:30:00.000Z',
        source: { type: 'library' },
        library: { revision: 3, updated_at: '2026-11-10T09:30:00.000Z', imports: [] },
      }),
      'posts/LIB1_1.jpg': IMAGES.jpeg(1),
      'posts/LIB1_2.mp4': VIDEOS.mp4(2),
      'posts/LIB1_2_poster.jpg': IMAGES.jpeg(3),
      'posts/LIB2_1.png': IMAGES.png(4),
      'posts/LIB3_1.webm': VIDEOS.webm(5),
    };
    const verdict = validate(files);
    expect(verdict.lines).toEqual(['OK: 3 post(s), 3 image file(s), 2 video file(s), 0 warning(s)']);
    expect(verdict.status).toBe(0);
  });

  it('still warns when "media" makes the clone ignore paths that are not copies of it', () => {
    const files: FileMap = {
      'post.json': manifest([{
        ...details('M1'),
        media: ['posts/m1.jpg'],
        image: 'posts/m1.jpg',
        images: ['posts/m1.jpg', 'posts/other.jpg'],
        video: 'posts/m1.mp4',
      }]),
      'posts/m1.jpg': IMAGES.jpeg(),
    };
    expect(validate(files).lines).toContain('WARN: posts[0]: "media" is given, so the clone ignores "images" and "video"');
  });
});
