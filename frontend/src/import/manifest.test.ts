import { describe, expect, it } from 'vitest';
import { MAX_CAPTION_CHARS, MAX_COMMENT_CHARS, MAX_IMAGES_PER_POST, MAX_USERNAME_CHARS } from './limits';
import { parseCount, parseManifest, parseManifestText, parseTimestamp } from './manifest';
import { ImportError } from './types';

function errorCode(run: () => unknown): string | undefined {
  try {
    run();
  } catch (err) {
    return err instanceof ImportError ? err.code : `not an ImportError: ${String(err)}`;
  }
  return undefined;
}

describe('parseCount', () => {
  it.each([
    [0, 0],
    [1234, 1234],
    [12.6, 13],
    ['1234', 1234],
    ['1,234', 1234],
    ['1.234', 1234],
    ['1 234', 1234],
    ['1 234 567', 1234567],
    ['1,234,567', 1234567],
    ['12.3K', 12300],
    ['12,3k', 12300],
    ['1.2M', 1200000],
    ['2m', 2000000],
    ['1.5B', 1500000000],
    ['3 thousand', 3000],
    ['1,234 likes', 1234],
    [' 56 ', 56],
    [{ count: 77 }, 77],
  ])('parses %j as %d', (input, expected) => {
    expect(parseCount(input)).toBe(expected);
  });

  it.each([[null], [undefined], [-1], ['-5'], [NaN], [Infinity], [''], ['abc'], ['K'], [true], [[]], [{}], [{ count: {} }]])(
    'rejects %j',
    (input) => {
      expect(parseCount(input)).toBeNull();
    },
  );
});

describe('parseCount on hostile input', () => {
  // The pattern used to backtrack cubically on "digit, spaces, junk": 4000 spaces took seconds
  // and 20,000 froze the tab. SPACES is far beyond what the old pattern could survive.
  const SPACES = 100_000;

  it('answers at once however long the string is', () => {
    const started = performance.now();
    expect(parseCount(`1${' '.repeat(SPACES)}!`)).toBeNull();
    expect(parseCount(`1${' 1'.repeat(SPACES / 2)}!`)).toBeNull();
    expect(parseCount(`1${'\u00a0'.repeat(SPACES)}k${' '.repeat(SPACES)}!`)).toBeNull();
    expect(parseCount('1'.repeat(SPACES))).toBeNull();
    expect(parseCount({ count: `1${' '.repeat(SPACES)}!` })).toBeNull();
    expect(performance.now() - started).toBeLessThan(500);
  });

  it('still reads every sensible spelling', () => {
    expect(parseCount(`  1 234${' '.repeat(SPACES)}`)).toBe(1234); // surrounding whitespace is trimmed
    expect(parseCount('1\u00a0234\u202f567')).toBe(1234567);
    expect(parseCount("1'234")).toBe(1234);
    expect(parseCount('1’234')).toBe(1234);
    expect(parseCount('1_000')).toBe(1000);
    expect(parseCount('12 k')).toBe(12000);
    expect(parseCount('3  million')).toBe(3000000);
    expect(parseCount('1 234 comments')).toBe(1234);
    expect(parseCount('9,007,199,254,740,991 likes')).toBe(Number.MAX_SAFE_INTEGER);
    expect(parseCount('12 34 x!')).toBeNull();
    expect(parseCount('12 km')).toBe(12); // a unit word, not a magnitude
  });
});

describe('parseTimestamp', () => {
  it.each([
    ['2026-11-01T15:04:05Z', '2026-11-01T15:04:05.000Z'],
    ['2026-11-01T16:04:05+01:00', '2026-11-01T15:04:05.000Z'],
    ['2026-11-01', '2026-11-01T00:00:00.000Z'],
    [1793545445, '2026-11-01T15:04:05.000Z'],
    [1793545445000, '2026-11-01T15:04:05.000Z'],
    ['1793545445', '2026-11-01T15:04:05.000Z'],
    ['1793545445000', '2026-11-01T15:04:05.000Z'],
    [1793545445000000, '2026-11-01T15:04:05.000Z'],
  ])('normalises %j', (input, expected) => {
    expect(parseTimestamp(input)).toBe(expected);
  });

  it.each([[undefined], [null], [''], ['not a date'], ['-5'], ['2 days ago'], [0], [-10], [NaN], [{}], [true], ['2026-13-45T99:99:99Z']])(
    'returns undefined for %j',
    (input) => {
      expect(parseTimestamp(input)).toBeUndefined();
    },
  );
});

describe('parseManifestText', () => {
  it('reports invalid JSON', () => {
    expect(errorCode(() => parseManifestText('{ "posts": [ }'))).toBe('invalid-json');
    expect(errorCode(() => parseManifestText(''))).toBe('invalid-json');
  });
});

const CANONICAL = {
  format: 'instagram-export',
  version: 1,
  exported_at: '2026-11-10T09:30:00Z',
  source: { type: 'profile', url: 'https://www.instagram.com/natgeo/' },
  posts: [
    {
      id: 'DBx1abcDEF',
      url: 'https://www.instagram.com/p/DBx1abcDEF/',
      image: 'posts/post1.jpg',
      images: ['posts/post1.jpg', 'posts/post1_2.jpg'],
      user: {
        username: 'natgeo',
        full_name: 'National Geographic',
        verified: true,
        avatar: 'avatars/natgeo.jpg',
        profile_url: 'https://www.instagram.com/natgeo/',
      },
      caption: 'A caption',
      comment_count: 1234,
      like_count: 56789,
      taken_at: '2026-11-01T15:04:05Z',
      media_type: 'carousel',
      alt: 'accessibility text',
      location: 'Paris, France',
      comments: [{ username: 'someone', text: 'Wow', like_count: 3, taken_at: '2026-11-01T16:00:00Z' }],
    },
  ],
};

describe('parseManifest', () => {
  it('parses the canonical format', () => {
    const manifest = parseManifest(CANONICAL);
    expect(manifest.warnings).toEqual([]);
    expect(manifest.totalEntries).toBe(1);
    expect(manifest.exportedAt).toBe('2026-11-10T09:30:00.000Z');
    expect(manifest.sourceUrl).toBe('https://www.instagram.com/natgeo/');
    expect(manifest.posts).toEqual([
      {
        index: 0,
        id: 'DBx1abcDEF',
        permalink: 'https://www.instagram.com/p/DBx1abcDEF/',
        media: [{ path: 'posts/post1.jpg' }, { path: 'posts/post1_2.jpg' }],
        user: {
          username: 'natgeo',
          fullName: 'National Geographic',
          verified: true,
          avatarPath: 'avatars/natgeo.jpg',
          profileUrl: 'https://www.instagram.com/natgeo/',
        },
        caption: 'A caption',
        commentCount: 1234,
        likeCount: 56789,
        takenAt: '2026-11-01T15:04:05.000Z',
        mediaType: 'carousel',
        alt: 'accessibility text',
        location: 'Paris, France',
        comments: [
          { id: 'DBx1abcDEF-c1', username: 'someone', text: 'Wow', likes: 3, takenAt: '2026-11-01T16:00:00.000Z' },
        ],
      },
    ]);
  });

  it('accepts a bare array of posts', () => {
    const manifest = parseManifest([{ image: 'posts/a.jpg', user: 'alice' }]);
    expect(manifest.posts).toHaveLength(1);
    expect(manifest.posts[0].user).toEqual({ username: 'alice', profileUrl: 'https://www.instagram.com/alice/' });
    expect(manifest.posts[0].id).toMatch(/^post-[0-9a-z]+$/);
    expect(manifest.posts[0].caption).toBe('');
    expect(manifest.posts[0].commentCount).toBe(0);
    expect(manifest.posts[0].likeCount).toBeNull();
    expect(manifest.posts[0].mediaType).toBe('image');
  });

  it('rejects JSON that is not an export', () => {
    expect(errorCode(() => parseManifest('hello'))).toBe('invalid-manifest');
    expect(errorCode(() => parseManifest(42))).toBe('invalid-manifest');
    expect(errorCode(() => parseManifest(null))).toBe('invalid-manifest');
    expect(errorCode(() => parseManifest({ format: 'instagram-export' }))).toBe('invalid-manifest');
    expect(errorCode(() => parseManifest({ posts: 'nope' }))).toBe('invalid-manifest');
    expect(errorCode(() => parseManifest({ posts: [] }))).toBe('no-posts');
    expect(errorCode(() => parseManifest([]))).toBe('no-posts');
  });

  describe('field aliases', () => {
    const base = { user: 'alice', image: 'posts/a.jpg' };
    const first = (entry: Record<string, unknown>) => parseManifest([entry]).posts[0];

    it.each(['image', 'image_path', 'path', 'file', 'media', 'src'])('image: %s', (key) => {
      expect(first({ user: 'alice', [key]: 'posts/a.jpg' }).media.map((item) => item.path)).toEqual(['posts/a.jpg']);
    });

    it.each(['images', 'image_paths', 'media_paths'])('images: %s', (key) => {
      const post = first({ user: 'alice', [key]: ['posts/a.jpg', 'posts/b.jpg'] });
      expect(post.media.map((item) => item.path)).toEqual(['posts/a.jpg', 'posts/b.jpg']);
      expect(post.mediaType).toBe('carousel');
    });

    it.each(['user', 'from', 'owner', 'author', 'username'])('user: %s as a string', (key) => {
      expect(first({ image: 'posts/a.jpg', [key]: '@alice' }).user.username).toBe('alice');
    });

    it.each(['user', 'from', 'owner', 'author'])('user: %s as an object', (key) => {
      expect(first({ image: 'posts/a.jpg', [key]: { username: 'alice' } }).user.username).toBe('alice');
    });

    it.each(['username', 'name', 'handle'])('user object name: %s', (key) => {
      expect(first({ image: 'posts/a.jpg', user: { [key]: 'alice' } }).user.username).toBe('alice');
    });

    it.each(['full_name', 'fullName', 'display_name'])('user object full name: %s', (key) => {
      expect(first({ image: 'posts/a.jpg', user: { username: 'alice', [key]: 'Alice A.' } }).user.fullName).toBe('Alice A.');
    });

    it.each(['verified', 'is_verified'])('user object verified: %s', (key) => {
      expect(first({ image: 'posts/a.jpg', user: { username: 'alice', [key]: true } }).user.verified).toBe(true);
    });

    it.each(['avatar', 'avatar_path', 'profile_pic'])('user object avatar: %s', (key) => {
      const post = first({ image: 'posts/a.jpg', user: { username: 'alice', [key]: 'avatars\\alice.jpg' } });
      expect(post.user.avatarPath).toBe('avatars/alice.jpg');
    });

    it.each(['profile_url', 'url'])('user object profile link: %s', (key) => {
      const post = first({ image: 'posts/a.jpg', user: { username: 'alice', [key]: 'http://instagram.com/alice.real/' } });
      expect(post.user.profileUrl).toBe('https://instagram.com/alice.real/');
    });

    it.each(['caption', 'description', 'first_comment', 'text'])('caption: %s', (key) => {
      expect(first({ ...base, [key]: 'Hello there' }).caption).toBe('Hello there');
    });

    it.each(['comment_count', 'comments_count', 'comments_total', 'total_comments', 'commentCount'])(
      'comment count: %s',
      (key) => {
        expect(first({ ...base, [key]: '1,234' }).commentCount).toBe(1234);
      },
    );

    it.each(['like_count', 'likes', 'likes_count', 'likeCount'])('like count: %s', (key) => {
      expect(first({ ...base, [key]: '12.3K' }).likeCount).toBe(12300);
    });

    it.each(['url', 'permalink', 'link', 'post_url'])('permalink: %s', (key) => {
      const post = first({ ...base, [key]: 'https://www.instagram.com/p/Cabc123/' });
      expect(post.permalink).toBe('https://www.instagram.com/p/Cabc123/');
      expect(post.id).toBe('Cabc123');
    });

    it.each(['id', 'shortcode', 'code'])('id: %s', (key) => {
      expect(first({ ...base, [key]: 'Cabc123' }).id).toBe('Cabc123');
    });

    it.each(['taken_at', 'timestamp', 'date', 'posted_at', 'created_at'])('timestamp: %s', (key) => {
      expect(first({ ...base, [key]: 1793545445 }).takenAt).toBe('2026-11-01T15:04:05.000Z');
    });

    it.each(['media_type', 'type'])('media type: %s', (key) => {
      expect(first({ ...base, [key]: 'video' }).mediaType).toBe('video');
    });

    // Hand-written exports tend to follow the wording "post image path, post from (user), post
    // comments total count, post user's very first comment".
    it.each([
      'photo',
      'picture',
      'filename',
      'image_file',
      'file_path',
      'post_image_path',
      'post image path',
      'postImagePath',
      'Post-Image',
    ])('image, as people write it: %s', (key) => {
      expect(first({ user: 'alice', [key]: 'posts/a.jpg' }).media.map((item) => item.path)).toEqual(['posts/a.jpg']);
    });

    it.each(['photos', 'pictures', 'post_images'])('images, as people write it: %s', (key) => {
      const post = first({ user: 'alice', [key]: ['posts/a.jpg', 'posts/b.jpg'] });
      expect(post.media.map((item) => item.path)).toEqual(['posts/a.jpg', 'posts/b.jpg']);
    });

    it.each(['from_user', 'posted_by', 'post_from', 'post from (user)', 'post_user', 'postOwner'])(
      'user, as people write it: %s',
      (key) => {
        const manifest = parseManifest([{ image: 'posts/a.jpg', [key]: 'alice' }]);
        expect(manifest.posts[0].user.username).toBe('alice');
        expect(manifest.warnings).toEqual([]);
      },
    );

    it.each([
      'comments_total_count',
      'total_comment_count',
      'total_comments_count',
      'comment_total',
      'comment_total_count',
      'number_of_comments',
      'num_comments',
      'post_comments_total_count',
      'post comments total count',
      'postCommentCount',
      'post_comments',
    ])('comment count, as people write it: %s', (key) => {
      expect(first({ ...base, [key]: 1234 }).commentCount).toBe(1234);
    });

    it.each(['total_likes', 'likes_total', 'number_of_likes', 'post_likes', 'post like count'])(
      'like count, as people write it: %s',
      (key) => {
        expect(first({ ...base, [key]: 77 }).likeCount).toBe(77);
      },
    );

    it.each([
      'user_first_comment',
      'post_user_first_comment',
      "post user's very first comment",
      'post_description',
      'post caption',
      'postCaption',
    ])('caption, as people write it: %s', (key) => {
      expect(first({ ...base, [key]: 'Hello there' }).caption).toBe('Hello there');
    });

    it('reads a whole entry written with a "post" in front of every field', () => {
      const manifest = parseManifest([
        {
          post_id: 'Cabc123',
          post_image_path: 'posts\\post1.jpg',
          post_from: 'alice',
          post_comments_total_count: 12,
          post_user_first_comment: 'Hello there',
          post_link: 'https://www.instagram.com/p/Cabc123/',
          post_date: '2026-11-01T15:04:05Z',
        },
      ]);
      expect(manifest.warnings).toEqual([]);
      expect(manifest.posts[0]).toMatchObject({
        id: 'Cabc123',
        media: [{ path: 'posts/post1.jpg' }],
        user: { username: 'alice' },
        commentCount: 12,
        caption: 'Hello there',
        permalink: 'https://www.instagram.com/p/Cabc123/',
        takenAt: '2026-11-01T15:04:05.000Z',
      });
    });

    it('prefers the plain field name over the one with "post" in front, and leaves other words alone', () => {
      expect(first({ ...base, post_caption: 'second choice', caption: 'first choice' }).caption).toBe('first choice');
      expect(first({ ...base, post_id: 'second', id: 'first' }).id).toBe('first');
      // "postcode" and "poster" are words of their own, not "post" + "code" / "er".
      const post = first({ ...base, postcode: '90210', poster: 'bob', postscript: 'x' });
      expect(post.id).toBe(first(base).id);
      expect(post.permalink).toBeUndefined();
      expect(post.user.username).toBe('alice');
    });
  });

  it('normalises counts: invalid comments become 0, invalid or hidden likes become null', () => {
    const { posts } = parseManifest([
      { user: 'a', image: 'p/1.jpg', comment_count: '1,234', like_count: '1.2M' },
      { user: 'a', image: 'p/2.jpg', comment_count: -3, like_count: -1 },
      { user: 'a', image: 'p/3.jpg', comment_count: 'many', like_count: null },
      { user: 'a', image: 'p/4.jpg', comments: [{ username: 'b', text: 'x' }, { username: 'c', text: 'y' }] },
      { user: 'a', image: 'p/5.jpg', comments: 42 },
    ]);
    expect(posts.map((post) => post.commentCount)).toEqual([1234, 0, 0, 2, 42]);
    expect(posts.map((post) => post.likeCount)).toEqual([1200000, null, null, null, null]);
  });

  it('normalises image paths and rejects URLs and escapes', () => {
    const manifest = parseManifest([
      { user: 'a', image: 'posts\\sub\\a.jpg' },
      { user: 'a', image: './posts/b.jpg' },
      { user: 'a', image: '/posts/c.jpg' },
      { user: 'a', image: 'posts/x/../d.jpg' },
      { user: 'a', image: '../outside.jpg' },
      { user: 'a', image: 'posts/../../outside.jpg' },
      { user: 'a', image: '..\\..\\windows\\system.ini' },
      { user: 'a', image: 'https://cdn.example/e.jpg' },
      { user: 'a', image: '//cdn.example/e.jpg' },
      { user: 'a', image: 'data:image/png;base64,AAAA' },
    ]);
    expect(manifest.posts.map((post) => post.media[0].path)).toEqual([
      'posts/sub/a.jpg',
      'posts/b.jpg',
      'posts/c.jpg',
      'posts/d.jpg',
    ]);
    expect(manifest.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['invalid-entry', 4],
      ['invalid-entry', 5],
      ['invalid-entry', 6],
      ['missing-image', 7],
      ['missing-image', 8],
      ['missing-image', 9],
    ]);
  });

  it('puts the cover first when "image" is not part of "images"', () => {
    const { posts } = parseManifest([{ user: 'a', image: 'posts/cover.jpg', images: ['posts/2.jpg', 'posts/3.jpg'] }]);
    expect(posts[0].media.map((item) => item.path)).toEqual(['posts/cover.jpg', 'posts/2.jpg', 'posts/3.jpg']);
  });

  it('skips entries without an image or that are not objects', () => {
    const manifest = parseManifest([{ user: 'a' }, 'posts/a.jpg', null, { user: 'a', image: '' }, { user: 'a', image: 'p.jpg' }]);
    expect(manifest.totalEntries).toBe(5);
    expect(manifest.posts.map((post) => post.index)).toEqual([4]);
    expect(manifest.warnings.map((warning) => warning.code)).toEqual([
      'invalid-entry',
      'invalid-entry',
      'invalid-entry',
      'invalid-entry',
    ]);
  });

  it('derives ids and makes duplicates unique', () => {
    const manifest = parseManifest([
      { user: 'a', image: '1.jpg', id: 'same' },
      { user: 'a', image: '2.jpg', id: 'same' },
      { user: 'a', image: '3.jpg', shortcode: 'same' },
      { user: 'a', image: '4.jpg', url: 'https://www.instagram.com/reel/Rabc/' },
      { user: 'a', image: '5.jpg' },
      { user: 'a', image: '6.jpg', id: 'post-5' },
      { user: 'a', image: '7.jpg', id: 12345 },
      { user: 'a', image: '8.jpg', id: 'weird id/with <chars>' },
    ]);
    const derived = manifest.posts[4].id;
    expect(derived).toMatch(/^post-[0-9a-z]+$/);
    expect(manifest.posts.map((post) => post.id)).toEqual([
      'same',
      'same-2',
      'same-3',
      'Rabc',
      derived,
      'post-5',
      '12345',
      'weird_id_with_chars_',
    ]);
    expect(new Set(manifest.posts.map((post) => post.id)).size).toBe(8);
    expect(manifest.warnings.filter((warning) => warning.code === 'duplicate-id').map((warning) => warning.postIndex)).toEqual([
      1, 2,
    ]);
  });

  describe('ids of entries without an id or a link', () => {
    const idOf = (entry: Record<string, unknown>, before: unknown[] = []) =>
      parseManifest([...before, entry]).posts[before.length].id;
    const entry = { user: 'alice', images: ['posts/a.jpg', 'posts/b.jpg'], taken_at: '2026-11-01T15:04:05Z', caption: 'x' };

    it('depend on what the post is, not on where post.json lists it', () => {
      const id = idOf(entry);
      expect(idOf(entry, [{ user: 'bob', image: 'posts/z.jpg' }, { user: 'carol', image: 'posts/y.jpg' }])).toBe(id);
      // The caption and the counts may be edited between two exports of the same post.
      expect(idOf({ ...entry, caption: 'edited', like_count: 5 })).toBe(id);
      // The same moment written differently is the same moment.
      expect(idOf({ ...entry, taken_at: 1793545445 })).toBe(id);
      // A default user of the export counts like a user on the entry.
      expect(parseManifest({ user: 'alice', posts: [{ ...entry, user: undefined }] }).posts[0].id).toBe(id);
    });

    it('differ for another author, date or file', () => {
      const id = idOf(entry);
      const others = [
        idOf({ ...entry, user: 'bob' }),
        idOf({ ...entry, taken_at: '2026-11-02T15:04:05Z' }),
        idOf({ ...entry, images: ['posts/a.jpg'] }),
        idOf({ ...entry, images: ['posts/b.jpg', 'posts/a.jpg'] }),
        idOf({ ...entry, images: ['posts/c.jpg', 'posts/b.jpg'] }),
      ];
      expect(new Set([id, ...others]).size).toBe(6);
    });

    it('are made unique without a warning when the same post is listed twice', () => {
      const manifest = parseManifest([entry, entry, { ...entry, id: 'given' }]);
      const [first, second] = manifest.posts.map((post) => post.id);
      expect(second).toBe(`${first}-2`);
      expect(manifest.warnings).toEqual([]);
      // A given id that collides with a derived one is still reported.
      const clash = parseManifest([entry, { user: 'a', image: 'posts/q.jpg', id: first }]);
      expect(clash.posts[1].id).toBe(`${first}-2`);
      expect(clash.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([['duplicate-id', 1]]);
    });
  });

  it('derives the permalink from an explicit shortcode but not from a generic id', () => {
    const { posts } = parseManifest([
      { user: 'a', image: '1.jpg', shortcode: 'Cabc123' },
      { user: 'a', image: '2.jpg', id: 'my-own-id' },
    ]);
    expect(posts[0].permalink).toBe('https://www.instagram.com/p/Cabc123/');
    expect(posts[1].permalink).toBeUndefined();
  });

  it('drops unsafe links with a warning and falls back to a derived profile link', () => {
    const manifest = parseManifest({
      source: { url: 'javascript:alert(1)' },
      posts: [
        {
          image: '1.jpg',
          url: 'javascript:alert(1)',
          user: { username: 'alice', profile_url: 'ftp://example.com/alice' },
        },
        { image: '2.jpg', url: 'data:text/html,x', user: 'not a handle!' },
      ],
    });
    expect(manifest.sourceUrl).toBeUndefined();
    expect(manifest.posts[0].permalink).toBeUndefined();
    expect(manifest.posts[0].user.profileUrl).toBe('https://www.instagram.com/alice/');
    expect(manifest.posts[1].permalink).toBeUndefined();
    expect(manifest.posts[1].user).toEqual({ username: 'not a handle!' });
    expect(manifest.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['unsafe-url', undefined],
      ['unsafe-url', 0],
      ['unsafe-url', 0],
      ['unsafe-url', 1],
    ]);
  });

  it('keeps only links that lead to a post, a profile or the page the export came from', () => {
    const shim = 'https://www.instagram.com/linkshim/?u=https%3A%2F%2Fevil.example%2Fphish'; // not a post, profile or feed
    const manifest = parseManifest({
      source: { url: shim },
      posts: [
        { image: '1.jpg', url: shim, user: { username: 'alice', profile_url: 'https://www.instagram.com/accounts/logout/' } },
        { image: '2.jpg', url: 'https://www.instagram.com/accounts/logout/', user: 'alice' },
        { image: '3.jpg', url: 'https://www.instagram.com/oauth/authorize?client_id=1', user: 'alice' },
        {
          image: '4.jpg',
          url: 'https://www.instagram.com/alice/',
          user: { username: 'alice', url: 'https://www.instagram.com/p/Cabc123/' },
        },
        { image: '5.jpg', url: 'https://www.instagram.com/alice/reel/Cabc123/?igsh=1', user: 'alice' },
      ],
    });
    expect(manifest.sourceUrl).toBeUndefined();
    expect(manifest.posts.map((post) => post.permalink)).toEqual([
      undefined,
      undefined,
      undefined,
      undefined,
      'https://www.instagram.com/alice/reel/Cabc123/?igsh=1',
    ]);
    // The profile link falls back to the one derived from the username.
    expect(manifest.posts.map((post) => post.user.profileUrl)).toEqual(Array(5).fill('https://www.instagram.com/alice/'));
    expect(manifest.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['unsafe-url', undefined],
      ['unsafe-url', 0],
      ['unsafe-url', 0],
      ['unsafe-url', 1],
      ['unsafe-url', 2],
      ['unsafe-url', 3],
      ['unsafe-url', 3],
    ]);
    expect(manifest.warnings.every((warning) => warning.message.endsWith('was ignored.'))).toBe(true);
  });

  it('keeps links to posts from other sites and does not invent Instagram links for them', () => {
    const manifest = parseManifest({
      source: { type: 'feed', url: 'https://mastodon.social/@alice' },
      posts: [
        { image: '1.jpg', url: 'https://mastodon.social/@alice/1123', user: { username: 'alice', profile_url: 'https://mastodon.social/@alice' } },
        // No profile link: it is not made up from the username, because this is not an Instagram post.
        { image: '2.jpg', url: 'https://example.com/p/abc/', user: 'bob' },
        // The same path on Instagram is a shortcode and gives the post its id.
        { image: '3.jpg', url: 'https://www.instagram.com/p/Cabc123/', user: 'carol' },
      ],
    });
    expect(manifest.warnings).toEqual([]);
    expect(manifest.sourceUrl).toBe('https://mastodon.social/@alice');
    expect(manifest.posts.map((post) => post.permalink)).toEqual([
      'https://mastodon.social/@alice/1123',
      'https://example.com/p/abc/',
      'https://www.instagram.com/p/Cabc123/',
    ]);
    expect(manifest.posts.map((post) => post.user.profileUrl)).toEqual([
      'https://mastodon.social/@alice',
      undefined,
      'https://www.instagram.com/carol/',
    ]);
    expect(manifest.posts[1].id).not.toBe('abc');
    expect(manifest.posts[2].id).toBe('Cabc123');
  });

  it('does not take the default author from a profile link on another site', () => {
    const manifest = parseManifest({
      source: { url: 'https://example.com/alice' },
      posts: [{ image: '1.jpg' }],
    });
    expect(manifest.posts[0].user).toEqual({ username: 'unknown' });
  });

  it.each([
    'https://www.instagram.com/',
    'https://www.instagram.com/natgeo/',
    'https://www.instagram.com/natgeo/saved/',
    'https://www.instagram.com/natgeo/tagged/',
    'https://www.instagram.com/explore/tags/sourdough/',
    'https://www.instagram.com/p/Cabc123/',
  ])('accepts %s as the source page', (url) => {
    const manifest = parseManifest({ source: { url }, posts: [{ image: '1.jpg', user: 'a' }] });
    expect(manifest.sourceUrl).toBe(url);
    expect(manifest.warnings).toEqual([]);
  });

  it('uses the export-level profile for posts that do not name a user', () => {
    const fromSource = parseManifest({
      source: { type: 'profile', url: 'https://www.instagram.com/natgeo/' },
      posts: [{ image: '1.jpg' }],
    });
    expect(fromSource.posts[0].user.username).toBe('natgeo');
    expect(fromSource.warnings).toEqual([]);

    const unknown = parseManifest([{ image: '1.jpg' }]);
    expect(unknown.posts[0].user).toEqual({ username: 'unknown' });
    expect(unknown.warnings.map((warning) => warning.code)).toEqual(['invalid-entry']);
  });

  it('caps text lengths', () => {
    const { posts } = parseManifest([
      {
        user: 'u'.repeat(500),
        image: '1.jpg',
        caption: 'c'.repeat(MAX_CAPTION_CHARS + 500),
        comments: [{ username: 'v'.repeat(500), text: 't'.repeat(MAX_COMMENT_CHARS + 500) }],
      },
    ]);
    expect(posts[0].user.username).toHaveLength(MAX_USERNAME_CHARS);
    expect(posts[0].caption).toHaveLength(MAX_CAPTION_CHARS);
    expect(posts[0].comments[0].text).toHaveLength(MAX_COMMENT_CHARS);
    expect(posts[0].comments[0].username).toHaveLength(MAX_USERNAME_CHARS);
  });

  it('does not cut a surrogate pair in half when capping', () => {
    const caption = `${'a'.repeat(MAX_CAPTION_CHARS - 1)}😀`;
    const { posts } = parseManifest([{ user: 'a', image: '1.jpg', caption }]);
    expect(posts[0].caption).toBe('a'.repeat(MAX_CAPTION_CHARS - 1));
  });

  it('truncates long manifests and long carousels with a warning', () => {
    const entries = Array.from({ length: 7 }, (_, i) => ({ user: 'a', image: `posts/${i}.jpg` }));
    const manifest = parseManifest(entries, 5);
    expect(manifest.totalEntries).toBe(7);
    expect(manifest.posts).toHaveLength(5);
    expect(manifest.warnings).toEqual([expect.objectContaining({ code: 'truncated' })]);

    const slides = Array.from({ length: MAX_IMAGES_PER_POST + 3 }, (_, i) => `posts/s${i}.jpg`);
    const carousel = parseManifest([{ user: 'a', images: slides }]);
    expect(carousel.posts[0].media).toHaveLength(MAX_IMAGES_PER_POST);
    expect(carousel.warnings).toEqual([expect.objectContaining({ code: 'truncated', postIndex: 0 })]);
  });

  it('understands Instagram-style media type names and nested caption objects', () => {
    const { posts } = parseManifest([
      { user: 'a', image: '1.jpg', media_type: 'GraphSidecar', caption: { text: 'nested' } },
      { user: 'a', image: '2.jpg', type: 'reel' },
      { user: 'a', image: '3.jpg', media_type: 'something else' },
      { user: 'a', image: '4.jpg', location: { name: 'Tokyo' } },
    ]);
    expect(posts.map((post) => post.mediaType)).toEqual(['carousel', 'video', 'image', 'image']);
    expect(posts[0].caption).toBe('nested');
    expect(posts[3].location).toBe('Tokyo');
  });
});

describe('parseManifest: videos and the "media" list', () => {
  const first = (entry: Record<string, unknown>) => parseManifest([{ user: 'alice', ...entry }]);

  it('reads the canonical video post of instagram.md', () => {
    const manifest = parseManifest([
      {
        id: 'C5xYz',
        url: 'https://www.instagram.com/reel/C5xYz/',
        media_type: 'video',
        media: [{ type: 'video', path: 'posts/post5.mp4', poster: 'posts/post5_poster.jpg' }],
        user: 'someone',
        caption: '...',
        comment_count: 12,
        like_count: 340,
        taken_at: '2026-10-01T12:00:00Z',
      },
    ]);
    expect(manifest.warnings).toEqual([]);
    expect(manifest.posts[0]).toMatchObject({
      id: 'C5xYz',
      permalink: 'https://www.instagram.com/reel/C5xYz/',
      mediaType: 'video',
      media: [{ path: 'posts/post5.mp4', type: 'video', posterPath: 'posts/post5_poster.jpg' }],
    });
  });

  it('accepts strings and objects with path aliases, kind aliases, poster aliases and alt texts', () => {
    const { posts, warnings } = first({
      media: [
        'posts/1.jpg',
        { path: 'posts/2.jpg', alt: 'second' },
        { file: 'posts\\3.mp4', type: 'video', thumbnail: './posts/3_poster.jpg' },
        { src: 'posts/4.webm', kind: 'reel', cover: 'posts/4.jpg' },
        { url: 'posts/5.png', media_type: 'photo' },
        { video: 'posts/6.mov' },
        { image: 'posts/7.jpg', type: 'carousel' },
      ],
    });
    expect(warnings).toEqual([]);
    expect(posts[0].media).toEqual([
      { path: 'posts/1.jpg' },
      { path: 'posts/2.jpg', alt: 'second' },
      { path: 'posts/3.mp4', type: 'video', posterPath: 'posts/3_poster.jpg' },
      { path: 'posts/4.webm', type: 'video', posterPath: 'posts/4.jpg' },
      { path: 'posts/5.png', type: 'image' },
      { path: 'posts/6.mov', type: 'video' },
      { path: 'posts/7.jpg' },
    ]);
    expect(posts[0].mediaType).toBe('carousel');
  });

  it('lets "media" win over image, images, video and videos', () => {
    const { posts } = first({
      image: 'posts/cover.jpg',
      images: ['posts/a.jpg'],
      video: 'posts/v.mp4',
      media: ['posts/m.jpg'],
    });
    expect(posts[0].media).toEqual([{ path: 'posts/m.jpg' }]);
    expect(posts[0].mediaType).toBe('image');
    // A single object is a list of one.
    expect(first({ media: { path: 'posts/v.mp4', type: 'video' } }).posts[0].media).toEqual([{ path: 'posts/v.mp4', type: 'video' }]);
  });

  it('reads video and videos next to the older image fields, with a post-level poster for the first video', () => {
    expect(first({ video: 'posts/v.mp4' }).posts[0].media).toEqual([{ path: 'posts/v.mp4', type: 'video' }]);
    expect(first({ video_path: 'posts/v.mp4', poster: 'posts/p.jpg' }).posts[0].media).toEqual([
      { path: 'posts/v.mp4', type: 'video', posterPath: 'posts/p.jpg' },
    ]);
    const mixed = first({
      image: 'posts/cover.jpg',
      images: ['posts/a.jpg'],
      videos: ['posts/v1.mp4', 'posts/v2.webm'],
      thumbnail: 'posts/v1.jpg',
    }).posts[0];
    expect(mixed.media).toEqual([
      { path: 'posts/cover.jpg' },
      { path: 'posts/a.jpg' },
      { path: 'posts/v1.mp4', type: 'video', posterPath: 'posts/v1.jpg' },
      { path: 'posts/v2.webm', type: 'video' },
    ]);
    expect(mixed.mediaType).toBe('carousel');
    // A poster without any video is simply not used.
    expect(first({ image: 'posts/a.jpg', cover: 'posts/c.jpg' }).posts[0].media).toEqual([{ path: 'posts/a.jpg' }]);
  });

  it('keeps the post-level type as declared, or derived from the number of files', () => {
    expect(first({ media: ['posts/a.mp4'] }).posts[0].mediaType).toBe('image');
    expect(first({ media: ['posts/a.jpg'], media_type: 'video' }).posts[0].mediaType).toBe('video');
    expect(first({ media: ['posts/a.jpg', 'posts/b.mp4'], media_type: 'video' }).posts[0].mediaType).toBe('video');
  });

  it('warns about remote and escaping media paths in words that fit, and drops bad posters silently', () => {
    const { posts, warnings } = first({
      media: [
        { type: 'video', url: 'https://cdn.example/v.mp4' },
        { type: 'video', path: '../outside.mp4' },
        'https://cdn.example/a.jpg',
        { type: 'video', path: 'posts/ok.mp4', poster: 'https://cdn.example/p.jpg' },
        { type: 'video', path: 'posts/ok2.mp4', poster: '../../p.jpg' },
      ],
    });
    expect(posts[0].media).toEqual([
      { path: 'posts/ok.mp4', type: 'video' },
      { path: 'posts/ok2.mp4', type: 'video' },
    ]);
    expect(warnings.map((warning) => [warning.code, warning.message])).toEqual([
      ['missing-image', expect.stringContaining('Videos are not downloaded')],
      ['invalid-entry', expect.stringContaining('the video path "../outside.mp4" points outside the export')],
      ['missing-image', expect.stringContaining('Images are not downloaded')],
    ]);
  });

  it('skips media lists without a usable path, and de-duplicates paths', () => {
    const empty = parseManifest([{ user: 'a', media: [null, 3, { alt: 'no path' }, ''] }, { user: 'a', image: 'posts/ok.jpg' }]);
    expect(empty.posts).toHaveLength(1);
    expect(empty.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([['invalid-entry', 0]]);
    const twice = first({ media: ['posts/a.jpg', { path: './posts/a.jpg', type: 'image' }, 'posts/b.jpg'] });
    expect(twice.posts[0].media).toEqual([{ path: 'posts/a.jpg' }, { path: 'posts/b.jpg' }]);
  });

  it('truncates long media lists like long carousels', () => {
    const media = Array.from({ length: MAX_IMAGES_PER_POST + 2 }, (_, i) => ({ type: 'video', path: `posts/v${i}.mp4` }));
    const manifest = first({ media });
    expect(manifest.posts[0].media).toHaveLength(MAX_IMAGES_PER_POST);
    expect(manifest.warnings).toEqual([
      expect.objectContaining({ code: 'truncated', message: expect.stringContaining('pictures and videos') }),
    ]);
  });
});
