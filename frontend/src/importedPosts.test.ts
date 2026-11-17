import { describe, expect, it } from 'vitest';
import type { ImportResult, ImportedMedia, ImportedPost } from './import';
import { importToPosts, safeImageSrc, safeLinks, safeVideoSrc, siteName } from './importedPosts';

function post(overrides: Partial<ImportedPost> = {}): ImportedPost {
  return {
    id: 'a',
    media: [{ kind: 'image', url: 'blob:http://localhost/1', path: 'posts/a.jpg', mime: 'image/jpeg' }],
    user: { username: 'alice' },
    caption: '',
    commentCount: 0,
    likeCount: null,
    mediaType: 'image',
    comments: [],
    ...overrides,
  };
}

function result(posts: ImportedPost[]): ImportResult {
  return { sourceName: 'x.zip', sourceKind: 'archive', posts, warnings: [], totalEntries: posts.length };
}

// The UI is the second line of defence: a result handed to it (for example one restored from
// IndexedDB by older code) is not trusted to have gone through the importer's link rules.
describe('links of an import that reach the UI', () => {
  it('keeps the links the importer produces', () => {
    const [shown] = importToPosts(result([post({
      permalink: 'https://www.instagram.com/p/DBx1abcDEF/',
      user: { username: 'alice', profileUrl: 'https://www.instagram.com/alice/' },
    })]));
    expect(shown.permalink).toBe('https://www.instagram.com/p/DBx1abcDEF/');
    expect(shown.profileUrl).toBe('https://www.instagram.com/alice/');
  });

  it.each([
    'javascript:alert(1)',
    'data:text/html,<script>alert(1)</script>',
    '//www.instagram.com/p/DBx1abcDEF/',
    '/p/DBx1abcDEF/',
    // On Instagram's own hosts a link must lead to what its label says.
    'https://www.instagram.com/accounts/logout/',
  ])('never renders %s as a post or profile link', (link) => {
    const [shown] = importToPosts(result([post({ permalink: link, user: { username: 'alice', profileUrl: link } })]));
    expect(shown.permalink).toBeUndefined();
    expect(shown.profileUrl).toBeUndefined();
  });

  it.each([
    'https://example.com/posts/42',
    'https://mastodon.social/@alice/1123',
    'https://l.instagram.com/?u=https%3A%2F%2Fexample.com%2F',
    'http://localhost:3000/post/1',
  ])('renders %s, a post from another site, as a link', (link) => {
    const [shown] = importToPosts(result([post({ permalink: link, user: { username: 'alice', profileUrl: link } })]));
    expect(shown.permalink).toBe(link);
    expect(shown.profileUrl).toBe(link);
    expect(safeLinks.source(link)).toBe(link);
  });

  it('names the site a link leads to', () => {
    expect(siteName('https://www.instagram.com/p/abc/')).toBe('Instagram');
    expect(siteName('https://instagr.am/p/abc/')).toBe('Instagram');
    expect(siteName('https://www.example.com/posts/1')).toBe('example.com');
    expect(siteName('https://mastodon.social/@alice')).toBe('mastodon.social');
    expect(siteName('http://localhost:3000/')).toBe('localhost');
    expect(siteName(undefined)).toBe('the original');
    expect(siteName('nonsense')).toBe('the original');
  });

  it('only lets a link through under the label that fits it', () => {
    const profile = 'https://www.instagram.com/alice/';
    const permalink = 'https://www.instagram.com/reel/DBx1abcDEF/';
    const login = 'https://www.instagram.com/accounts/login/?next=%2Fp%2FDBx1abcDEF%2F';

    const [swapped] = importToPosts(result([post({ permalink: profile, user: { username: 'alice', profileUrl: permalink } })]));
    expect(swapped.permalink).toBeUndefined();
    expect(swapped.profileUrl).toBeUndefined();

    expect(safeLinks.post(permalink)).toBe(permalink);
    expect(safeLinks.profile(profile)).toBe(profile);
    for (const ok of ['https://www.instagram.com/', profile, permalink, 'https://www.instagram.com/explore/tags/sunset/']) {
      expect(safeLinks.source(ok)).toBe(ok);
    }
    expect(safeLinks.post(login)).toBeUndefined();
    expect(safeLinks.profile(login)).toBeUndefined();
    expect(safeLinks.source(login)).toBeUndefined();
  });

  it('upgrades http and drops credentials and ports instead of rendering them', () => {
    expect(safeLinks.post('http://user:pw@www.instagram.com:8443/p/DBx1abcDEF/')).toBe('https://www.instagram.com/p/DBx1abcDEF/');
  });
});

describe('pictures and videos of an import', () => {
  const video = (url: string, extra: Partial<ImportedMedia> = {}): ImportedMedia => ({
    kind: 'video', url, path: 'posts/v.mp4', mime: 'video/mp4', ...extra,
  });

  it('turns a video into a playable slide with its poster and type', () => {
    const [shown] = importToPosts(result([post({
      mediaType: 'video',
      media: [video('blob:http://localhost/v', { posterUrl: 'blob:http://localhost/p', posterPath: 'posts/p.jpg' })],
    })]));
    expect(shown.photo).toMatchObject({
      video: 'blob:http://localhost/v', videoMime: 'video/mp4', src: 'blob:http://localhost/p', alt: 'Video by alice',
    });
    expect(shown.isVideo).toBe(true);
    expect(shown.photos).toBeUndefined();
  });

  it('marks a carousel that starts with a video as a video, and labels every slide', () => {
    const [shown] = importToPosts(result([post({
      mediaType: 'carousel',
      media: [
        video('blob:http://localhost/v'),
        { kind: 'image', url: 'blob:http://localhost/i', path: 'posts/i.jpg', mime: 'image/jpeg', alt: 'A lemon tart' },
      ],
    })]));
    expect(shown.isVideo).toBe(true);
    expect(shown.photos?.map((photo) => [photo.video, photo.src, photo.alt])).toEqual([
      ['blob:http://localhost/v', undefined, 'Video by alice'],
      [undefined, 'blob:http://localhost/i', 'A lemon tart'],
    ]);
  });

  it('keeps a carousel that starts with a picture a picture post', () => {
    const [shown] = importToPosts(result([post({
      mediaType: 'carousel',
      media: [{ kind: 'image', url: 'blob:http://localhost/i', path: 'posts/i.jpg', mime: 'image/jpeg' }, video('blob:http://localhost/v')],
    })]));
    expect(shown.isVideo).toBeUndefined();
    expect(shown.photos?.[1]).toMatchObject({ video: 'blob:http://localhost/v', alt: 'Video 2 of 2 by alice' });
  });

  it("renders the files of the app's own library", () => {
    const url = '/api/library/files/posts/C5xYz_1.mp4?r=3';
    const poster = '/api/library/files/posts/C5xYz_1_poster.jpg?r=3';
    const [shown] = importToPosts(result([post({
      media: [video(url, { posterUrl: poster })],
      user: { username: 'alice', avatarUrl: '/api/library/files/avatars/alice.jpg?r=3' },
    })]));
    expect(shown.photo.video).toBe(url);
    expect(shown.photo.src).toBe(poster);
    expect(shown.avatarSrc).toBe('/api/library/files/avatars/alice.jpg?r=3');
  });

  it.each([
    'https://cdninstagram.com/v/t51/video.mp4',
    'http://127.0.0.1:8000/api/library/files/posts/a.mp4',
    '//evil.example/api/library/files/posts/a.mp4',
    '/api/library/files/../../health',
    '/api/library/files/posts/../../../health',
    '/api/library/files/',
    '/api/counter',
    'data:video/mp4;base64,AAAA',
    'javascript:alert(1)',
  ])('never renders %s as a picture or video', (url) => {
    expect(safeVideoSrc(url)).toBeUndefined();
    expect(safeImageSrc(url)).toBeUndefined();
    const [shown] = importToPosts(result([post({ media: [video(url, { posterUrl: url })] })]));
    expect(shown.photo.video).toBeUndefined();
    expect(shown.photo.src).toBeUndefined();
    // Still a video post: it shows the placeholder instead of a picture.
    expect(shown.isVideo).toBe(true);
  });
});
