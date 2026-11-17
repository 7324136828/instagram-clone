import { describe, expect, it } from 'vitest';
import {
  instagramPostUrl,
  instagramProfileUrl,
  isInstagramUrl,
  sanitizeInstagramUrl,
  sanitizePostUrl,
  sanitizeProfileUrl,
  sanitizeSourceUrl,
  sanitizeWebUrl,
  shortcodeFromUrl,
  usernameFromProfileUrl,
} from './urls';

describe('sanitizeInstagramUrl', () => {
  it.each([
    ['https://www.instagram.com/p/DBx1abcDEF/', 'https://www.instagram.com/p/DBx1abcDEF/'],
    ['https://instagram.com/natgeo/', 'https://instagram.com/natgeo/'],
    ['http://www.instagram.com/natgeo/', 'https://www.instagram.com/natgeo/'],
    ['  https://www.instagram.com/reel/Cabc123/  ', 'https://www.instagram.com/reel/Cabc123/'],
    ['HTTPS://WWW.INSTAGRAM.COM/p/AbC/', 'https://www.instagram.com/p/AbC/'],
    ['https://m.instagram.com/natgeo/', 'https://m.instagram.com/natgeo/'],
    ['https://instagr.am/p/abc/', 'https://instagr.am/p/abc/'],
    ['https://www.instagram.com/p/abc/?img_index=2#x', 'https://www.instagram.com/p/abc/?img_index=2#x'],
    // Credentials and ports are dropped rather than passed on.
    ['https://user:secret@www.instagram.com/p/abc/', 'https://www.instagram.com/p/abc/'],
    ['https://www.instagram.com:8443/p/abc/', 'https://www.instagram.com/p/abc/'],
    ['http://www.instagram.com:80/p/abc/', 'https://www.instagram.com/p/abc/'],
  ])('accepts %s', (input, expected) => {
    expect(sanitizeInstagramUrl(input)).toBe(expected);
  });

  it.each([
    ['javascript:alert(1)'],
    ['JaVaScRiPt:alert(document.cookie)'],
    ['javascript://www.instagram.com/%0aalert(1)'],
    ['data:text/html,<script>alert(1)</script>'],
    ['vbscript:msgbox(1)'],
    ['blob:https://www.instagram.com/123'],
    ['file:///etc/passwd'],
    ['ftp://www.instagram.com/p/abc/'],
    ['//www.instagram.com/p/abc/'],
    ['//evil.example/p/abc/'],
    ['/p/abc/'],
    ['www.instagram.com/p/abc/'],
    ['instagram.com'],
    ['https://instagram.com.evil.tld/p/abc/'],
    ['https://evil-instagram.com/p/abc/'],
    ['https://evilinstagram.com/'],
    ['https://instagram.com.evil.tld'],
    // Subdomains are other services: an open redirector, OAuth consent pages, the help centre.
    ['https://l.instagram.com/?u=https%3A%2F%2Fevil.example%2Fphish&e=AT0signature'],
    ['https://L.Instagram.com/?u=https%3A%2F%2Fevil.example'],
    ['https://api.instagram.com/oauth/authorize?client_id=1&redirect_uri=https://evil.example'],
    ['https://help.instagram.com/123'],
    ['https://evil.instagram.com/x'],
    ['https://www.www.instagram.com/p/abc/'],
    ['https://www.instagr.am/p/abc/'],
    ['https://www.instagram.com@evil.tld/p/abc/'],
    ['https://www.instagram.com:pass@evil.tld/'],
    ['https://evil.tld/www.instagram.com/p/abc/'],
    ['https://evil.tld/?next=https://www.instagram.com/'],
    ['https://evil.tld#https://www.instagram.com/'],
    ['https://www.instagram.com\\@evil.tld/'],
    ['https://evil.tld\\.instagram.com/'],
    ['https://instagram.com%2eevil.tld/'],
    ['https://www.instagram.com.'],
    ['https://instagr.am.evil.tld/'],
    ['https://xinstagr.am/'],
    ['https://www.instagr.am.evil/'],
    ['https://instagrаm.com/p/abc/'], // Cyrillic "а"
    ['https://www.instagram.com/p/abc/\njavascript:alert(1)'],
    ['java\tscript:alert(1)'],
    ['https://www.instagram.com/ p/abc/'],
    ['https://'],
    ['https:///p/abc'],
    ['https://[::1]/'],
    ['https://127.0.0.1/'],
    [''],
    ['   '],
    [`https://www.instagram.com/${'a'.repeat(3000)}`],
  ])('rejects %s', (input) => {
    expect(sanitizeInstagramUrl(input)).toBeUndefined();
  });

  it('rejects values that are not strings', () => {
    for (const value of [undefined, null, 42, true, {}, [], ['https://www.instagram.com/']]) {
      expect(sanitizeInstagramUrl(value)).toBeUndefined();
    }
  });

  it('always returns an https URL on an Instagram host', () => {
    const inputs = [
      'http://www.instagram.com/p/abc/',
      'https://a:b@m.instagram.com:444/x?y=1',
      'https://instagr.am/p/abc/',
    ];
    for (const input of inputs) {
      const url = new URL(sanitizeInstagramUrl(input) as string);
      expect(url.protocol).toBe('https:');
      expect(url.username).toBe('');
      expect(url.password).toBe('');
      expect(url.port).toBe('');
      expect(['instagram.com', 'www.instagram.com', 'm.instagram.com', 'instagr.am']).toContain(url.hostname);
    }
  });
});

describe('links by what they are shown as', () => {
  it.each([
    ['https://www.instagram.com/p/DBx1abcDEF/', 'https://www.instagram.com/p/DBx1abcDEF/'],
    ['http://instagram.com/p/DBx1abcDEF', 'https://instagram.com/p/DBx1abcDEF'],
    ['https://www.instagram.com/reel/Cxyz_-9/?igsh=1', 'https://www.instagram.com/reel/Cxyz_-9/?igsh=1'],
    ['https://www.instagram.com/reels/Cxyz/', 'https://www.instagram.com/reels/Cxyz/'],
    ['https://www.instagram.com/tv/Babc/', 'https://www.instagram.com/tv/Babc/'],
    ['https://www.instagram.com/natgeo/p/DBx1abcDEF/', 'https://www.instagram.com/natgeo/p/DBx1abcDEF/'],
    ['https://www.instagram.com/share/reel/BAxyz/', 'https://www.instagram.com/share/reel/BAxyz/'],
    ['https://www.instagram.com/p/DBx1abcDEF/?img_index=2', 'https://www.instagram.com/p/DBx1abcDEF/?img_index=2'],
    ['https://instagr.am/p/abc/', 'https://instagr.am/p/abc/'],
  ])('sanitizePostUrl accepts %s', (input, expected) => {
    expect(sanitizePostUrl(input)).toBe(expected);
  });

  it.each([
    ['https://www.instagram.com/'],
    ['https://www.instagram.com/natgeo/'],
    ['https://www.instagram.com/p/'],
    ['https://www.instagram.com/p/has%20space/'],
    ['https://www.instagram.com/accounts/logout/'],
    ['https://www.instagram.com/accounts/p/abc/'],
    ['https://www.instagram.com/accounts/login/?next=/p/abc/'],
    ['https://www.instagram.com/oauth/authorize?client_id=1'],
    ['https://www.instagram.com/explore/tags/p/abc/'],
    ['https://www.instagram.com/natgeo/../accounts/logout/'],
    ['javascript:alert(1)'],
    ['data:text/html,<script>alert(1)</script>'],
    ['//example.com/post/1'],
    ['not a link'],
    [undefined],
  ])('sanitizePostUrl rejects %s', (input) => {
    expect(sanitizePostUrl(input)).toBeUndefined();
  });

  it.each([
    ['https://example.com/posts/42', 'https://example.com/posts/42'],
    ['https://Mastodon.Social/@alice/1123?x=1#c', 'https://mastodon.social/@alice/1123?x=1#c'],
    ['http://blog.example.org:8080/2024/hello', 'http://blog.example.org:8080/2024/hello'],
    ['https://user:secret@example.com/p/abc/', 'https://example.com/p/abc/'],
    // Not Instagram's own pages, so the Instagram shape rules do not apply to them.
    ['https://l.instagram.com/p/abc/', 'https://l.instagram.com/p/abc/'],
    ['https://evil.tld/p/abc/', 'https://evil.tld/p/abc/'],
  ])('sanitizePostUrl accepts a link on another site: %s', (input, expected) => {
    expect(sanitizePostUrl(input)).toBe(expected);
  });

  it('sanitizeProfileUrl accepts profile pages only', () => {
    expect(sanitizeProfileUrl('http://instagram.com/alice.real/')).toBe('https://instagram.com/alice.real/');
    expect(sanitizeProfileUrl('https://www.instagram.com/natgeo')).toBe('https://www.instagram.com/natgeo');
    expect(sanitizeProfileUrl('https://www.instagram.com/natgeo/?hl=en')).toBe('https://www.instagram.com/natgeo/?hl=en');
    for (const input of [
      'https://www.instagram.com/',
      'https://www.instagram.com/p/abc/',
      'https://www.instagram.com/accounts/logout/',
      'https://www.instagram.com/accounts/',
      'https://www.instagram.com/logout/',
      'https://www.instagram.com/oauth/',
      'https://www.instagram.com/natgeo/saved/',
      'javascript:alert(1)',
      'natgeo',
    ]) {
      expect(sanitizeProfileUrl(input), input).toBeUndefined();
    }
  });

  it('sanitizeProfileUrl accepts a profile on another site', () => {
    expect(sanitizeProfileUrl('https://mastodon.social/@alice')).toBe('https://mastodon.social/@alice');
    expect(sanitizeProfileUrl('https://evil.tld/natgeo/')).toBe('https://evil.tld/natgeo/');
  });

  it('sanitizeSourceUrl accepts the pages an export is taken from', () => {
    for (const input of [
      'https://www.instagram.com/',
      'https://www.instagram.com/natgeo/',
      'https://www.instagram.com/natgeo/saved/',
      'https://www.instagram.com/natgeo/tagged/',
      'https://www.instagram.com/p/DBx1abcDEF/',
      'https://www.instagram.com/explore/tags/sourdough/',
      'https://www.instagram.com/explore/locations/213385402/paris-france/',
    ]) {
      expect(sanitizeSourceUrl(input), input).toBe(input);
    }
    for (const input of [
      'https://www.instagram.com/accounts/logout/',
      'https://www.instagram.com/accounts/edit/',
      'https://www.instagram.com/oauth/authorize?client_id=1',
      'https://www.instagram.com/linkshim/?u=https%3A%2F%2Fevil.example',
      'https://www.instagram.com/direct/inbox/',
      'javascript:alert(1)',
      'feed',
    ]) {
      expect(sanitizeSourceUrl(input), input).toBeUndefined();
    }
  });

  it('sanitizeSourceUrl accepts a page on another site', () => {
    for (const input of ['https://example.com/', 'https://www.reddit.com/r/pics/', 'http://localhost:3000/feed']) {
      expect(sanitizeSourceUrl(input), input).toBe(input);
    }
  });
});

describe('sanitizeWebUrl', () => {
  it('accepts http(s) links on any site and drops credentials', () => {
    expect(sanitizeWebUrl('https://example.com/a?b=1#c')).toBe('https://example.com/a?b=1#c');
    expect(sanitizeWebUrl('HTTP://Example.COM:81/x')).toBe('http://example.com:81/x');
    expect(sanitizeWebUrl('https://a:b@example.com/x')).toBe('https://example.com/x');
    expect(sanitizeWebUrl('https://127.0.0.1:5000/x')).toBe('https://127.0.0.1:5000/x');
    // Instagram's own hosts are still normalised exactly as before.
    expect(sanitizeWebUrl('http://www.instagram.com:80/p/abc/')).toBe('https://www.instagram.com/p/abc/');
  });

  it.each([
    ['javascript:alert(1)'],
    ['data:text/html,<script>alert(1)</script>'],
    ['file:///etc/passwd'],
    ['ftp://example.com/x'],
    ['//example.com/x'],
    ['/relative/path'],
    ['example.com/x'],
    ['https://'],
    ['https:///x'],
    ['https://example.com/a b'],
    ['https://example.com\\@evil.tld/'],
    ['https://example.com/a\nb'],
    [''],
    [`https://example.com/${'a'.repeat(3000)}`],
    [42],
    [undefined],
    [null],
  ])('rejects %s', (input) => {
    expect(sanitizeWebUrl(input)).toBeUndefined();
  });
});

describe('isInstagramUrl', () => {
  it('is true only for the hosts of Instagram itself', () => {
    expect(isInstagramUrl('https://www.instagram.com/p/abc/')).toBe(true);
    expect(isInstagramUrl('https://instagr.am/p/abc/')).toBe(true);
    expect(isInstagramUrl('https://l.instagram.com/?u=x')).toBe(false);
    expect(isInstagramUrl('https://example.com/')).toBe(false);
    expect(isInstagramUrl('nonsense')).toBe(false);
    expect(isInstagramUrl(undefined)).toBe(false);
  });
});

describe('derived Instagram links', () => {
  it('derives a profile URL only for plausible usernames', () => {
    expect(instagramProfileUrl('natgeo')).toBe('https://www.instagram.com/natgeo/');
    expect(instagramProfileUrl('a.b_c9')).toBe('https://www.instagram.com/a.b_c9/');
    expect(instagramProfileUrl('x'.repeat(30))).toBeDefined();
    expect(instagramProfileUrl('x'.repeat(31))).toBeUndefined();
    expect(instagramProfileUrl('')).toBeUndefined();
    expect(instagramProfileUrl('has space')).toBeUndefined();
    expect(instagramProfileUrl('../../evil')).toBeUndefined();
    expect(instagramProfileUrl('evil.tld/x')).toBeUndefined();
    expect(instagramProfileUrl('name?x=1')).toBeUndefined();
    expect(instagramProfileUrl('ünïcode')).toBeUndefined();
  });

  it('derives a post URL only for well-formed shortcodes', () => {
    expect(instagramPostUrl('DBx1abc-_F')).toBe('https://www.instagram.com/p/DBx1abc-_F/');
    expect(instagramPostUrl('a/b')).toBeUndefined();
    expect(instagramPostUrl('')).toBeUndefined();
  });

  it('extracts the shortcode from post permalinks', () => {
    expect(shortcodeFromUrl('https://www.instagram.com/p/DBx1abcDEF/')).toBe('DBx1abcDEF');
    expect(shortcodeFromUrl('https://www.instagram.com/reel/Cxyz_-9/?igsh=1')).toBe('Cxyz_-9');
    expect(shortcodeFromUrl('https://www.instagram.com/tv/Babc/')).toBe('Babc');
    expect(shortcodeFromUrl('https://www.instagram.com/natgeo/p/DBx1abcDEF/')).toBe('DBx1abcDEF');
    expect(shortcodeFromUrl('https://www.instagram.com/natgeo/')).toBeUndefined();
    expect(shortcodeFromUrl(undefined)).toBeUndefined();
  });

  it('extracts the username from profile URLs only', () => {
    expect(usernameFromProfileUrl('https://www.instagram.com/natgeo/')).toBe('natgeo');
    expect(usernameFromProfileUrl('https://www.instagram.com/explore/')).toBeUndefined();
    expect(usernameFromProfileUrl('https://www.instagram.com/p/abc/')).toBeUndefined();
    expect(usernameFromProfileUrl('https://www.instagram.com/')).toBeUndefined();
  });
});
