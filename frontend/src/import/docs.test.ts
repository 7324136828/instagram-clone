/**
 * The documentation is part of the contract. Whatever README.md, instagram.md and the import
 * dialog show as an export has to import without a single warning, the messages and labels they
 * quote have to be the real ones, and the validator that instagram.md hands to the exporting
 * agent must never call a link fine that the importer would drop.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import readme from '../../../README.md?raw';
import runbook from '../../../instagram.md?raw';
import appSource from '../App.tsx?raw';
import actionSheetSource from '../components/ActionSheet.tsx?raw';
import autoViewSource from '../components/AutoView.tsx?raw';
import feedSource from '../components/Feed.tsx?raw';
import importModalSource from '../components/ImportModal.tsx?raw';
import mediaSource from '../components/Media.tsx?raw';
import sidebarSource from '../components/Sidebar.tsx?raw';
import videoSource from '../components/Video.tsx?raw';
import importReportSource from '../importReport.ts?raw';
import guardSource from '../../../backend/app/guard.py?raw';
import librarySource from '../../../backend/app/library.py?raw';
import {
  ImportError,
  MAX_COMMENTS_PER_POST,
  MAX_IMAGES_PER_POST,
  MAX_IMAGE_BYTES,
  MAX_MANIFEST_BYTES,
  MAX_POSTS,
  MAX_SOURCES,
  MAX_SOURCE_ENTRIES,
  MAX_TOTAL_IMAGE_BYTES,
  MAX_TOTAL_MEDIA_BYTES,
  MAX_VIDEO_BYTES,
  importFromArchive,
  importFromFiles,
  importMany,
  releaseImport,
  sanitizePostUrl,
  sanitizeProfileUrl,
  sanitizeSourceUrl,
  setHeicConverter,
  setHeicNativeProbe,
  type ImportResult,
} from './index';
import { IMAGES, VIDEOS, folderFiles, json, makeFile, makeZip, type FileMap } from './testkit';

type Json = Record<string, unknown>;

const results: ImportResult[] = [];
const track = async (pending: Promise<ImportResult>): Promise<ImportResult> => {
  const result = await pending;
  results.push(result);
  return result;
};

beforeEach(() => {
  // Node has no HEIC decoder: the conversion itself is covered by the e2e suite.
  setHeicNativeProbe(async () => false);
  setHeicConverter(async () => new Blob([IMAGES.jpeg(9)], { type: 'image/jpeg' }));
});

afterEach(() => {
  for (const result of results.splice(0)) releaseImport(result);
  setHeicConverter(null);
  setHeicNativeProbe(null);
});

/** Every ```json block of a Markdown text that is an export manifest. */
function manifestsIn(markdown: string): Json[] {
  const found: Json[] = [];
  for (const match of markdown.replace(/\r\n/g, '\n').matchAll(/```json\n([\s\S]*?)\n```/g)) {
    const value = JSON.parse(match[1]) as Json;
    if (value.format === 'instagram-export') found.push(value);
  }
  return found;
}

function dialogExample(): Json {
  const match = /const FORMAT_JSON = `([\s\S]*?)`;/.exec(importModalSource);
  if (!match) throw new Error('ImportModal.tsx no longer has FORMAT_JSON');
  return JSON.parse(match[1]) as Json;
}

const BYTES: Record<string, (fill: number) => Uint8Array> = {
  jpg: IMAGES.jpeg,
  jpeg: IMAGES.jpeg,
  png: IMAGES.png,
  gif: IMAGES.gif,
  webp: IMAGES.webp,
  avif: IMAGES.avif,
  bmp: IMAGES.bmp,
  heic: IMAGES.heic,
  heif: IMAGES.heifMif1,
  mp4: VIDEOS.mp4,
  m4v: VIDEOS.m4v,
  mov: VIDEOS.mov,
  webm: VIDEOS.webm,
};

const list = (value: unknown): unknown[] => (Array.isArray(value) ? value : value === undefined ? [] : [value]);
const pathOf = (item: unknown): unknown => (typeof item === 'object' && item !== null ? (item as Json).path : item);

/** The media paths of an example entry, in the order the format defines (instagram.md section 8). */
function mediaPaths(entry: Json): unknown[] {
  if (Array.isArray(entry.media)) return entry.media.map(pathOf);
  const paths = list(entry.images);
  for (const cover of list(entry.image)) if (!paths.includes(cover)) paths.unshift(cover);
  return [...paths, ...list(entry.videos), ...list(entry.video)];
}

/** post.json plus a file of the right type for every path the manifest mentions. */
function exportFiles(manifest: Json): FileMap {
  const files: FileMap = { 'post.json': json(manifest) };
  let fill = 10;
  const add = (path: unknown) => {
    if (typeof path !== 'string' || files[path]) return;
    const make = BYTES[path.slice(path.lastIndexOf('.') + 1).toLowerCase()];
    if (!make) throw new Error(`the example names a file of an unknown type: ${path}`);
    files[path] = make(fill++);
  };
  for (const post of manifest.posts as Json[]) {
    for (const path of mediaPaths(post)) add(path);
    for (const item of list(post.media)) add((item as Json | undefined)?.poster);
    add(post.poster);
    add((post.user as Json | undefined)?.avatar);
  }
  return files;
}

const EXAMPLES: Array<[string, Json]> = [
  ...manifestsIn(runbook).map((manifest, i): [string, Json] => [`instagram.md, example ${i + 1}`, manifest]),
  ...manifestsIn(readme).map((manifest, i): [string, Json] => [`README.md, example ${i + 1}`, manifest]),
  ['the import dialog, "Expected format"', dialogExample()],
];

describe('the post.json examples of the documentation', () => {
  it('are all found', () => {
    // Section 1 and section 8 of the runbook, "The format in brief" of the README, the dialog.
    expect(manifestsIn(runbook)).toHaveLength(2);
    expect(manifestsIn(readme)).toHaveLength(1);
    expect(EXAMPLES).toHaveLength(4);
  });

  it.each(EXAMPLES)('%s imports without a warning, as a folder and as a zip', async (_name, manifest) => {
    const entries = manifest.posts as Json[];
    const files = exportFiles(manifest);
    const imports = [
      await track(importFromFiles(folderFiles('instagram-export', files))),
      await track(importFromArchive(makeFile('instagram-export.zip', makeZip(files)))),
    ];
    for (const result of imports) {
      expect(result.warnings).toEqual([]);
      expect(result.totalEntries).toBe(entries.length);
      expect(result.posts).toHaveLength(entries.length);
      const source = manifest.source as Json | undefined;
      expect(result.sourceUrl).toBe(source?.url);

      result.posts.forEach((post, i) => {
        const entry = entries[i];
        const user = entry.user as Json;
        // Nothing the example states is dropped or changed on the way in.
        expect(post.permalink).toBe(entry.url);
        expect(post.user.username).toBe(user.username);
        expect(post.user.profileUrl).toBe(user.profile_url ?? `https://www.instagram.com/${String(user.username)}/`);
        expect(!!post.user.avatarUrl).toBe(user.avatar !== undefined);
        expect(post.caption).toBe(entry.caption);
        expect(post.commentCount).toBe(entry.comment_count);
        expect(post.likeCount).toBe(entry.like_count);
        expect(post.media.map((item) => item.path)).toEqual(mediaPaths(entry));
        expect(post.takenAt).toBe(new Date(entry.taken_at as string).toISOString());
        if (entry.id !== undefined) expect(post.id).toBe(entry.id);
        if (entry.media_type !== undefined) expect(post.mediaType).toBe(entry.media_type);
        expect(post.comments).toHaveLength(((entry.comments as unknown[] | undefined) ?? []).length);
      });
    }
  });
});

describe('what the README says about hand-made exports', () => {
  it('names real alias spellings, and they import without a warning', async () => {
    for (const alias of ['post_image_path', 'post_from', 'comments_total_count', 'first_comment', '"1,234"', '"12.3K"']) {
      expect(readme).toContain(alias);
    }
    const manifest = [{
      post_image_path: 'posts/post1.jpg',
      post_from: 'someone',
      comments_total_count: '1,234',
      first_comment: 'The description',
      likes: '12.3K',
      permalink: 'https://www.instagram.com/p/C9aKq2LtXy1/',
    }];
    const result = await track(importFromFiles(folderFiles('my-export', { 'post.json': json(manifest), 'posts/post1.jpg': IMAGES.jpeg() })));
    expect(result.warnings).toEqual([]);
    expect(result.posts[0]).toMatchObject({
      caption: 'The description',
      commentCount: 1234,
      likeCount: 12300,
      permalink: 'https://www.instagram.com/p/C9aKq2LtXy1/',
    });
    expect(result.posts[0].user.username).toBe('someone');
  });

  it('gives image paths written from outside the export that really work', async () => {
    const quoted = ['my-export/posts/post1.jpg', 'C:/Users/me/my-export/posts/post1.jpg'];
    for (const path of quoted) expect(readme).toContain(`\`${path}\``);
    const manifest = quoted.map((image) => ({ image, user: 'someone' }));
    const result = await track(importFromFiles(folderFiles('my-export', { 'post.json': json(manifest), 'posts/post1.jpg': IMAGES.jpeg() })));
    expect(result.warnings).toEqual([]);
    expect(result.posts).toHaveLength(2);
  });
});

describe('the messages quoted in the README troubleshooting table', () => {
  const user = 'someone';
  const failure = async (pending: Promise<ImportResult>): Promise<string> => {
    try {
      results.push(await pending);
    } catch (err) {
      if (err instanceof ImportError) return err.message;
      throw err;
    }
    throw new Error('the import was expected to fail');
  };
  const warningsOf = async (posts: Json[], files: FileMap = {}): Promise<string[]> => {
    const result = await track(importFromArchive(makeFile('x.zip', makeZip({
      'post.json': json(posts),
      'posts/ok.jpg': IMAGES.jpeg(),
      ...files,
    }))));
    return result.warnings.map((warning) => warning.message);
  };

  // What the README prints (without the code marks), what the real message contains when that
  // differs (a number where the README has N), and how to get the message.
  const cases: Array<[string, string, () => Promise<string[]>]> = [
    ['No post.json was found', '', async () => [await failure(importFromArchive(makeFile('x.zip', makeZip({ 'posts/a.jpg': IMAGES.jpeg() }))))]],
    ['The zip file is damaged or incomplete', '', async () => [await failure(importFromArchive(makeFile('x.zip', 'PK\u0003\u0004 this is not a zip')))]],
    ['the image "..." is not in the export', 'the image "posts/missing.jpg" is not in the export', () =>
      warningsOf([{ image: 'posts/ok.jpg', user }, { image: 'posts/missing.jpg', user }])],
    ['is not a supported image', '', () => warningsOf([{ image: 'posts/ok.jpg', user }, { image: 'posts/a.svg', user }], { 'posts/a.svg': IMAGES.svg() })],
    ['HEIC/HEIF image that could not be converted', '', () => {
      setHeicConverter(async () => { throw new Error('unsupported variant'); });
      return warningsOf([{ image: 'posts/ok.jpg', user }, { image: 'posts/a.heic', user }], { 'posts/a.heic': IMAGES.heic() });
    }],
    ['is not a valid http(s) link', 'is not a valid http(s) link and was ignored', () =>
      warningsOf([{ image: 'posts/ok.jpg', user, url: 'javascript:alert(1)' }])],
    ['and was ignored', 'is an instagram.com link, but not to an Instagram post, and was ignored', () =>
      warningsOf([{ image: 'posts/ok.jpg', user, url: 'https://www.instagram.com/accounts/login/' }])],
    ['None of the N posts in post.json could be imported', 'None of the 2 posts in post.json could be imported', async () => [
      await failure(importFromArchive(makeFile('x.zip', makeZip({ 'post.json': json([{ image: 'posts/a.jpg', user }, { image: 'posts/b.jpg', user }]) })))),
    ]],
    ['the video', 'the video "posts/missing.mp4" is not in the export', () =>
      warningsOf([{ image: 'posts/ok.jpg', user }, { video: 'posts/missing.mp4', user }])],
    ['which is not supported. Convert it to MP4 (H.264) or WebM.', '', () =>
      warningsOf([{ image: 'posts/ok.jpg', user }, { video: 'posts/a.mkv', user }], { 'posts/a.mkv': VIDEOS.mkv() })],
    ['replaces the one with the same id', '', async () => {
      const part = (caption: string) => ({
        kind: 'archive' as const,
        file: makeFile(`${caption}.zip`, makeZip({ 'post.json': json([{ id: 'same', image: 'posts/ok.jpg', user, caption }]), 'posts/ok.jpg': IMAGES.jpeg() })),
      });
      const result = await track(importMany([part('first'), part('second')]));
      return result.warnings.map((warning) => warning.message);
    }],
  ];

  it.each(cases)('"%s" is what the importer really says', async (quoted, actual, produce) => {
    expect(readme.replace(/`/g, '')).toContain(quoted);
    const messages = await produce();
    expect(messages.some((message) => message.includes(actual || quoted)), messages.join(' | ')).toBe(true);
  });

  it('says when an archive is refused because its files overlap', async () => {
    expect(readme).toContain('(files overlap)');
    // Two directory entries for one stretch of data: the second local header is the first one.
    const zip = makeZip({ 'post.json': json([{ image: 'a.jpg', user }]), 'a.jpg': IMAGES.jpeg(1, 4000), 'b.jpg': IMAGES.jpeg(2, 4000) }, true);
    const view = new DataView(zip.buffer, zip.byteOffset, zip.byteLength);
    const offsets: number[] = [];
    for (let at = 0; at + 46 <= zip.length; at++) {
      if (view.getUint32(at, true) === 0x02014b50) offsets.push(at);
    }
    expect(offsets).toHaveLength(3);
    view.setUint32(offsets[2] + 42, view.getUint32(offsets[1] + 42, true), true);
    expect(await failure(importFromArchive(makeFile('x.zip', zip)))).toBe('The zip file is damaged or incomplete (files overlap).');
  });
});

describe('the limits table of the README', () => {
  it('states the limits the importer enforces', () => {
    const MiB = 1024 * 1024;
    const rows: Array<[string, string, number, number]> = [
      ['Posts per export', '5,000', MAX_POSTS, 5000],
      ['Pictures and videos per post', '30', MAX_IMAGES_PER_POST, 30],
      ['Sample comments per post', '200', MAX_COMMENTS_PER_POST, 200],
      ['`post.json`', '32 MB', MAX_MANIFEST_BYTES, 32 * MiB],
      ['One image', '64 MB', MAX_IMAGE_BYTES, 64 * MiB],
      ['One video', '1 GB', MAX_VIDEO_BYTES, 1024 * MiB],
      ['All pictures and videos together', '4 GB', MAX_TOTAL_MEDIA_BYTES, 4096 * MiB],
      ['Files in the folder or archive', '100,000', MAX_SOURCE_ENTRIES, 100_000],
      ['Archives or folders per import', '50', MAX_SOURCES, 50],
    ];
    // The old name of the total, still exported for older callers, means the same.
    expect(MAX_TOTAL_IMAGE_BYTES).toBe(MAX_TOTAL_MEDIA_BYTES);
    for (const [label, printed, actual, expected] of rows) {
      expect(actual, label).toBe(expected);
      expect(readme, label).toMatch(new RegExp(`\\| ${label.replace(/[.`]/g, '\\$&')} \\| ${printed}[ |]`));
    }
  });
});

describe('the labels the documentation tells people to click', () => {
  const ui = [
    appSource, importModalSource, sidebarSource, actionSheetSource, mediaSource, feedSource, videoSource, autoViewSource,
  ].join('\n');
  const section = (markdown: string, heading: string): string => {
    const text = markdown.replace(/\r\n/g, '\n');
    const start = text.indexOf(`\n## ${heading}`);
    if (start < 0) throw new Error(`no section "${heading}"`);
    const end = text.indexOf('\n## ', start + 1);
    return text.slice(start, end < 0 ? undefined : end);
  };

  const documents: Array<[string, string, string[]]> = [
    ['README.md', section(readme, 'Importing an Instagram export'), [
      'Select archive files', 'Select folder', 'View feed', 'Import another', 'Clear import', 'View on Instagram',
      'View source on Instagram', 'More options', 'Import Instagram export', 'Discard', 'Show', 'Cancel',
      'Import more', 'Clear library', 'Auto view', 'Auto view from here', 'Add to current import',
      'Replace current import', 'Download video', 'Restart', 'Save to library',
    ]],
    ['instagram.md', section(runbook, '11 Importing into the clone'), [
      'Select archive file', 'Select folder', 'View feed', 'Clear import', 'View on Instagram', 'View source on Instagram',
    ]],
  ];

  it.each(documents)('%s: every label exists in the UI', (_file, text, labels) => {
    for (const label of labels) {
      expect(text, `the documentation should mention "${label}"`).toContain(label);
      // The name of the site in a link label is the host the link leads to ("Instagram" for Instagram's own pages).
      const inUi = [label, label.replace(/ on Instagram$/, ' on {siteName('), label.replace(/ on Instagram$/, ' on ${siteName(')]
        .some((variant) => ui.includes(variant));
      expect(inUi, `the UI should have "${label}"`).toBe(true);
    }
    // The status line and the warning buttons have variable parts: N and M in the documents.
    expect(ui).toMatch(/`Imported \$\{[^`]*\} of \$\{[^`]*'post'\)\} from /);
    expect(text).toContain('Imported N of M posts');
    expect(ui).toMatch(/`Show all \$\{[^`]*'warning'\)\}`/);
    expect(text).toContain('Show all N warnings');
    expect(ui).toMatch(/\{countLabel\(warningCount, 'warning'\)\}/);
    expect(text).toContain('**N warnings**');
  });

  it('the runbook names the dialog by its real title', () => {
    expect(section(runbook, '11 Importing into the clone')).toContain('"Import from Instagram export"');
    expect(importModalSource).toContain('>Import from Instagram export<');
  });

  it('the messages the README quotes from the screen are the real ones', () => {
    const shown = readme.replace(/`/g, '');
    expect(shown).toContain("This video can't be played in this browser");
    expect(videoSource).toContain('This video can&apos;t be played in this browser.');
    expect(shown).toContain('Not saved in the library');
    expect(importModalSource).toContain('Not saved in the library.');
    expect(shown).toContain('Showing N posts from your library (M imports) · saved in <folder>');
    expect(appSource).toMatch(/Showing \{countLabel\([^)]*'post'\)\} from your library \(\{countLabel\([^)]*'import'\)\}\)/);
    expect(appSource).toContain('· saved in');
    expect(shown).toContain('"End of posts"');
    expect(autoViewSource).toContain('>End of posts<');
  });

  it('the library messages the README quotes are the real ones', () => {
    const shown = readme.replace(/`/g, '').replace(/\s+/g, ' ');
    // Backend refusals (backend/app/library.py and guard.py), quoted in the troubleshooting table.
    for (const quoted of ['already holds files this app did not create', 'is in use by another running copy of this app']) {
      expect(shown).toContain(quoted);
      // Python joins adjacent string literals: "...this " / f'...' / "app did not create".
      expect(librarySource.replace(/["']\s*\n\s*f?["']/g, '')).toContain(quoted);
    }
    for (const quoted of ['This host name is not allowed', "Changes are only accepted from the app's own pages"]) {
      expect(shown).toContain(quoted);
      expect(guardSource).toContain(quoted);
    }
    // The import dialog while and after saving.
    expect(shown).toContain('Adding the posts to your library…');
    expect(importReportSource).toContain("'Adding the posts to your library…'");
    expect(shown).toContain('part 1 of 2');
    expect(importReportSource).toContain('(part ${part.part} of ${part.parts})');
    expect(shown).toContain('Saved N of M posts; the rest could not be saved');
    expect(importReportSource).toMatch(/`Saved \$\{[^`]*\} of \$\{countLabel\(unsaved\.posts, 'post'\)\}; the rest could not be saved: `/);
    expect(shown).toContain('Imported N of M posts from 3 of 5 archives');
    expect(shown).toContain('N posts from <name> are kept in this browser only');
    expect(appSource).toContain('kept in this browser only: they are not in your library.');
    expect(shown).toContain('Reload the page to save to the library again.');
    expect(importModalSource).toContain('Reload the page to save to the library again.');
  });

  it('no document still uses a label that was renamed', () => {
    for (const text of [readme, runbook]) expect(text).not.toContain('Open source page');
  });
});

describe('the link checks of the validator in instagram.md', () => {
  interface LinkRules {
    isPostUrl: (value: unknown) => boolean;
    isProfileUrl: (value: unknown) => boolean;
    isSourceUrl: (value: unknown) => boolean;
  }
  const code = /\/\* links:begin \*\/([\s\S]*?)\/\* links:end \*\//.exec(runbook);
  if (!code) throw new Error('instagram.md: the validator no longer has its links:begin / links:end block');
  // The validator's own code, exactly as the runbook prints it.
  const rules = new Function(`${code[1]}; return { isPostUrl, isProfileUrl, isSourceUrl };`)() as LinkRules;

  const hosts = [
    'https://www.instagram.com', 'https://instagram.com', 'https://m.instagram.com', 'https://instagr.am',
    'http://www.instagram.com', 'HTTPS://WWW.INSTAGRAM.COM', 'https://WWW.Instagram.com',
    'https://l.instagram.com', 'https://help.instagram.com', 'https://api.instagram.com', 'https://evil.instagram.com',
    'https://www.instagram.com.evil.example', 'https://evil.example', 'https://www.instagram.com:8443',
    'https://user:pw@www.instagram.com', 'https://www.instagram.com.', '//www.instagram.com', 'javascript:alert(1)//',
  ];
  const paths = [
    '', '/', '/p/C9aKq2LtXy1/', '/p/C9aKq2LtXy1', '/reel/C7TtW0YhBs3/', '/reels/C7TtW0YhBs3/', '/tv/C7TtW0YhBs3/',
    '/P/C9aKq2LtXy1/', '/lumen.atlas/', '/lumen.atlas', '/lumen.atlas/p/C9aKq2LtXy1/', '/share/reel/C7TtW0YhBs3/',
    '/share/p/C9aKq2LtXy1/', '/lumen.atlas/saved/', '/lumen.atlas/tagged/', '/explore/', '/explore/tags/sunrise/',
    '/explore/locations/123/paris/', '/accounts/login/', '/accounts/login/?next=%2Fp%2FC9aKq2LtXy1%2F', '/oauth/authorize',
    '/linkshim/?u=https%3A%2F%2Fevil.example', '/direct/inbox/', '/stories/lumen.atlas/1/', '/p/', '/p//', '/p/a b/',
    '/p/../accounts/login/', '/lumen.atlas/../accounts/login/', '/%2e%2e/accounts/login/', '/lumen.atlas/%2E%2E/accounts/',
    '/./accounts/', '/p/C9aKq2LtXy1/?img_index=2', '/p/C9aKq2LtXy1/#comments', '/?u=https://evil.example',
    '/lumen.atlas/?hl=en', '/p\\C9aKq2LtXy1/', '/\\evil.example/', '/p/C9aKq2LtXy1/\u0001', '/a-b/', '/@lumen.atlas/',
    `/${'a'.repeat(31)}/`, `/p/${'a'.repeat(65)}/`, '/login', '/about/', '/api/v1/', '/graphql/query/',
  ];
  const links = hosts.flatMap((host) => paths.map((path) => host + path));

  it('accept what the exporter writes', () => {
    expect(rules.isPostUrl('https://www.instagram.com/p/C9aKq2LtXy1/')).toBe(true);
    expect(rules.isPostUrl('https://www.instagram.com/reel/C7TtW0YhBs3/')).toBe(true);
    expect(rules.isPostUrl('https://www.instagram.com/tv/C7TtW0YhBs3/')).toBe(true);
    expect(rules.isProfileUrl('https://www.instagram.com/lumen.atlas/')).toBe(true);
    for (const source of ['https://www.instagram.com/', 'https://www.instagram.com/lumen.atlas/', 'https://www.instagram.com/p/C9aKq2LtXy1/']) {
      expect(rules.isSourceUrl(source)).toBe(true);
    }
    for (const manifest of [...manifestsIn(runbook), ...manifestsIn(readme)]) {
      const source = manifest.source as Json | undefined;
      if (source?.url !== undefined) expect(rules.isSourceUrl(source.url)).toBe(true);
      for (const post of manifest.posts as Json[]) {
        expect(rules.isPostUrl(post.url)).toBe(true);
        const profile = (post.user as Json).profile_url;
        if (profile !== undefined) expect(rules.isProfileUrl(profile)).toBe(true);
      }
    }
  });

  it('reject forwarding hosts and pages that are not what the label says', () => {
    for (const bad of [
      'https://l.instagram.com/?u=https%3A%2F%2Fevil.example', 'https://www.instagram.com/accounts/login/',
      'https://www.instagram.com/lumen.atlas/', 'http://www.instagram.com/p/C9aKq2LtXy1/', 42, null, undefined,
    ]) {
      expect(rules.isPostUrl(bad), String(bad)).toBe(false);
    }
    expect(rules.isProfileUrl('https://www.instagram.com/p/C9aKq2LtXy1/')).toBe(false);
    expect(rules.isProfileUrl('https://www.instagram.com/explore/')).toBe(false);
    expect(rules.isSourceUrl('https://www.instagram.com/accounts/login/')).toBe(false);
    expect(rules.isSourceUrl('https://www.instagram.com/lumen.atlas/../accounts/login/')).toBe(false);
  });

  it('never pass a link that the importer would drop', () => {
    const pairs: Array<[keyof LinkRules, (value: unknown) => string | undefined]> = [
      ['isPostUrl', sanitizePostUrl],
      ['isProfileUrl', sanitizeProfileUrl],
      ['isSourceUrl', sanitizeSourceUrl],
    ];
    let accepted = 0;
    for (const link of links) {
      for (const [rule, sanitize] of pairs) {
        if (!rules[rule](link)) continue;
        accepted++;
        expect(sanitize(link), `${rule} accepts ${link}`).toBeDefined();
      }
    }
    // The matrix is not vacuous: plenty of links pass, and plenty do not.
    expect(accepted).toBeGreaterThan(40);
    expect(accepted).toBeLessThan(links.length);
  });
});
