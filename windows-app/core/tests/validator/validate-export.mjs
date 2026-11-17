// The export validator of the reference web project, copied verbatim from section 9 of its
// instagram.md (the code block that starts with "// igx:validate.mjs"). The integration tests
// run it with Node.js on an extracted archive, and skip that check when node is not installed.
// Do not edit below this header: an export must pass the validator as the clone ships it.
//
// igx:validate.mjs
// Usage: node validate-export.mjs [export-folder]   exit code 0 = valid, 1 = errors, 2 = post.json unreadable
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(process.argv[2] || './instagram-export');
const errors = [];
const warnings = [];
const err = (m) => { errors.push(m); };
const warn = (m) => { warnings.push(m); };
const q = (s) => JSON.stringify(s);
const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const isCount = (v) => Number.isInteger(v) && v >= 0;
const ISO = /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,9})?Z$/;
const isIso = (v) => typeof v === 'string' && ISO.test(v);
/* links:begin */
// The clone keeps a link only when it leads to what its label promises ("View on Instagram" of a post,
// of a profile, of the page the export came from), so the three kinds are checked separately.
const LINK = /^https:\/\/([A-Za-z0-9.-]+)([/?#][^\s\\\x00-\x1f\x7f-\x9f]*)?$/;
// Instagram's own pages (instagr.am is its short domain). Other subdomains (l.instagram.com forwards to any site) are not links to Instagram.
const IG_HOSTS = ['www.instagram.com', 'instagram.com', 'm.instagram.com', 'instagr.am'];
// First path segments of instagram.com that are not usernames.
const NOT_A_PROFILE = ['p', 'reel', 'reels', 'tv', 'explore', 'stories', 'accounts', 'direct', 'about', 'developer', 'legal', 'web', 'api', 'graphql', 'share', 'oauth', 'linkshim', 'challenge', 'login', 'logout', 'emails', 'session', 'ajax'];
// The path segments of an https link on instagram.com, or null for anything else.
const igPath = (v) => {
  const m = typeof v === 'string' && v.length <= 2048 ? LINK.exec(v) : null;
  if (!m || !IG_HOSTS.includes(m[1].toLowerCase())) return null;
  const segments = (m[2] || '').split(/[?#]/)[0].split('/').filter(Boolean);
  return segments.some((x) => /^(?:\.|%2e){1,2}$/i.test(x)) ? null : segments;
};
const isName = (x) => /^[A-Za-z0-9._]{1,30}$/.test(x) && !NOT_A_PROFILE.includes(x.toLowerCase());
// /p/<shortcode>/, /reel/<shortcode>/, /tv/<shortcode>/, also below /<username>/ or /share/
const isPostPath = (x) => {
  const at = x.length >= 3 && (isName(x[0]) || x[0].toLowerCase() === 'share') ? 1 : 0;
  return x.length >= at + 2 && ['p', 'reel', 'reels', 'tv'].includes(x[at].toLowerCase()) && /^[A-Za-z0-9_-]{1,64}$/.test(x[at + 1]);
};
const isPostUrl = (v) => { const x = igPath(v); return !!x && isPostPath(x); };
const isProfileUrl = (v) => { const x = igPath(v); return !!x && x.length === 1 && isName(x[0]); };
// The home feed, a profile or one of its tabs, a post, or an explore page (hashtag, location).
const isSourceUrl = (v) => { const x = igPath(v); return !!x && (x.length === 0 || isPostPath(x) || x[0].toLowerCase() === 'explore' || isName(x[0])); };
/* links:end */
const EXT = { jpg: 'jpg', jpeg: 'jpg', png: 'png', gif: 'gif', webp: 'webp', avif: 'avif', heic: 'heic', heif: 'heic', bmp: 'bmp', mp4: 'mp4', m4v: 'mp4', mov: 'mov', webm: 'webm' };
const VIDEO = ['mp4', 'mov', 'webm'];
const MiB = 1024 * 1024;

function sniff(b) {
  const s = (at, n) => b.subarray(at, at + n).toString('latin1');
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return 'jpg';
  if (s(0, 8) === '\x89PNG\r\n\x1a\n') return 'png';
  if (s(0, 6) === 'GIF87a' || s(0, 6) === 'GIF89a') return 'gif';
  if (s(0, 4) === 'RIFF' && s(8, 4) === 'WEBP') return 'webp';
  if (s(4, 4) === 'ftyp') {
    const size = b.readUInt32BE(0);
    const end = Math.min(b.length, size >= 16 ? size : b.length);
    const major = s(8, 4);
    const others = [];
    for (let at = 16; at + 4 <= end; at += 4) others.push(s(at, 4));
    const hevc = ['heic', 'heix', 'hevc', 'hevx', 'heim', 'heis', 'hevm', 'hevs'];
    if (major === 'avif' || major === 'avis') return 'avif';
    if (hevc.includes(major)) return 'heic';
    if (others.includes('avif') || others.includes('avis')) return 'avif';
    if (others.some((x) => hevc.includes(x))) return 'heic';
    if (major === 'mif1' || major === 'msf1') return 'heic';
    if (/^[\x20-\x7e]{4}$/.test(major) && !['M4A ', 'M4B ', 'M4P ', 'F4A ', 'F4B '].includes(major)) return major === 'qt  ' ? 'mov' : 'mp4';
  }
  if (s(0, 4) === '\x1a\x45\xdf\xa3') { // EBML: WebM when its DocType element (42 82) says so
    const head = s(0, 64);
    const at = head.indexOf('\x42\x82');
    if (at !== -1 && head.slice(at + 2, at + 12).includes('webm')) return 'webm';
  }
  if (s(0, 2) === 'BM' && b.length >= 18 && b.readUInt32LE(6) === 0) return 'bmp';
  return null;
}

const files = new Map(); // lower-case relative path -> path as stored on disk
(function scan(dir, rel) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const r = rel ? rel + '/' + e.name : e.name;
    if (e.isDirectory()) scan(path.join(dir, e.name), r);
    else files.set(r.toLowerCase(), r);
  }
})(root, '');

const used = new Map(); // relative path -> 'image' | 'video'
const named = new Set(); // every path post.json names, also one that failed its check
let totalBytes = 0;
// Checks one referenced file. want: 'image', 'video' or 'any'. Returns 'image' or 'video', or null after an error.
function checkFile(rel, where, want) {
  const fail = (m) => { err(m); return null; };
  if (typeof rel === 'string') named.add(rel);
  if (typeof rel !== 'string' || !rel.trim()) return fail(`${where}: a file path must be a non-empty string`);
  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(rel) || rel.startsWith('//')) return fail(`${where}: ${q(rel)} is a URL or an absolute path; paths must point to files inside the export`);
  if (rel.includes('\\') || rel.split('/').some((seg) => seg === '' || seg === '.' || seg === '..')) return fail(`${where}: ${q(rel)} must be a clean relative path with forward slashes`);
  const actual = files.get(rel.toLowerCase());
  if (actual === undefined) return fail(`${where}: file ${q(rel)} does not exist`);
  if (actual !== rel) return fail(`${where}: ${q(rel)} differs in letter case from the file on disk ${q(actual)}`);
  const full = path.join(root, ...rel.split('/'));
  const size = fs.statSync(full).size;
  if (size === 0) return fail(`${where}: file ${q(rel)} is empty`);
  const head = Buffer.alloc(Math.min(256, size));
  const fd = fs.openSync(full, 'r');
  fs.readSync(fd, head, 0, head.length, 0);
  fs.closeSync(fd);
  const format = sniff(head);
  if (!format) return fail(`${where}: file ${q(rel)} is not a supported picture or video (unknown signature)`);
  const kind = VIDEO.includes(format) ? 'video' : 'image';
  if (want !== 'any' && kind !== want) return fail(`${where}: ${q(rel)} is a video, but only a picture belongs here`);
  if (kind === 'video' && size > 1024 * MiB) err(`${where}: ${q(rel)} is larger than 1 GB; the clone skips it`);
  if (kind === 'image' && size > 64 * MiB) err(`${where}: ${q(rel)} is larger than 64 MB; the clone skips it`);
  const ext = EXT[rel.includes('.') ? rel.split('.').pop().toLowerCase() : ''];
  if (ext !== format) warn(`${where}: ${q(rel)} contains ${format} data; rename it to .${format} and update post.json`);
  if (!used.has(rel)) totalBytes += size;
  used.set(rel, kind);
  return kind;
}

let data;
try {
  const raw = fs.readFileSync(path.join(root, 'post.json'));
  if (raw[0] === 0xef && raw[1] === 0xbb && raw[2] === 0xbf) err('post.json starts with a UTF-8 BOM; save it as UTF-8 without BOM');
  data = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(raw));
} catch (e) {
  console.log('ERROR: post.json is missing, is not UTF-8, or is not valid JSON');
  process.exit(2);
}

let posts = [];
if (Array.isArray(data)) {
  posts = data;
  warn('the top level is a bare list; the canonical form is an object with "format", "version" and "posts"');
} else if (isObj(data) && Array.isArray(data.posts)) {
  posts = data.posts;
  if (data.format !== 'instagram-export') warn('"format" should be "instagram-export"');
  if (data.version !== 1) warn('"version" should be 1');
  if (data.exported_at !== undefined && !isIso(data.exported_at)) err('"exported_at" must be an ISO 8601 UTC timestamp such as 2026-11-10T09:30:00Z');
  if (isObj(data.source) && data.source.url !== undefined && !isSourceUrl(data.source.url)) err('"source.url" must be the https://www.instagram.com/ address of the home feed, a profile, a post or an explore page');
} else {
  err('post.json must be an object with a "posts" list');
}
if (!posts.length) err('post.json lists no posts');

// "image", "images", "video", "videos": one path or a list of paths.
const pathsOf = (v, at, name) => {
  if (v === undefined) return [];
  if (typeof v === 'string') return [v];
  if (Array.isArray(v)) return v;
  err(`${at}: ${q(name)} must be a path or a list of paths`);
  return [];
};
const OLDER = ['image', 'images', 'video', 'videos'];
const POSTER = ['poster', 'thumbnail', 'cover']; // of the first video, next to the older fields

const ids = new Map();
posts.forEach((p, i) => {
  const at = `posts[${i}]`;
  if (!isObj(p)) return err(`${at}: must be an object`);
  const slides = []; // { path, type, poster, where }
  if (p.media !== undefined) {
    if (!Array.isArray(p.media)) err(`${at}: "media" must be a list`);
    else p.media.forEach((m, k) => {
      const where = `${at} media[${k}]`;
      if (typeof m === 'string') return slides.push({ path: m, where });
      if (!isObj(m)) return err(`${where}: must be a path or an object with "path"`);
      if (m.type !== undefined && m.type !== 'image' && m.type !== 'video') err(`${where}: "type" must be image or video`);
      slides.push({ path: m.path, type: m.type, poster: m.poster, where });
    });
    // The older fields are ignored next to "media". Copies of paths that "media" holds lose nothing
    // (the clone's library folder writes the pictures and posters there for older readers).
    const listed = new Set(slides.flatMap((s) => [s.path, s.poster]));
    const ignored = OLDER.filter((k) => p[k] !== undefined && (Array.isArray(p[k]) ? p[k] : [p[k]]).some((x) => !listed.has(x)));
    if (ignored.length) warn(`${at}: "media" is given, so the clone ignores ${ignored.map(q).join(' and ')}`);
  } else {
    // The older form, in the clone's order: the cover "image", the other "images", then "videos" and "video".
    const images = pathsOf(p.images, at, 'images');
    const pictures = [...pathsOf(p.image, at, 'image').filter((x) => !images.includes(x)), ...images];
    const clips = [...pathsOf(p.videos, at, 'videos'), ...pathsOf(p.video, at, 'video')];
    const poster = POSTER.find((k) => p[k] !== undefined);
    for (const x of pictures) slides.push({ path: x, type: 'image', where: at });
    clips.forEach((x, k) => slides.push({ path: x, type: 'video', poster: k === 0 && poster ? p[poster] : undefined, where: at }));
    if (poster && !clips.length) warn(`${at}: ${q(poster)} is only used for a video ("video" or "videos")`);
    if (typeof p.image === 'string' && images.length && images[0] !== p.image) warn(`${at}: "image" should equal images[0], the cover`);
  }
  if (!slides.length) err(`${at}: no picture or video ("media")`);
  if (slides.length > 30) warn(`${at}: ${slides.length} slides; the clone shows the first 30`);
  for (const slide of slides) {
    const kind = checkFile(slide.path, slide.where, 'any');
    if (kind && slide.type && kind !== slide.type) warn(`${slide.where}: declared as ${slide.type}, but the file is ${kind === 'video' ? 'a video' : 'a picture'}; the clone goes by the file`);
    if (slide.poster === undefined) continue;
    if (kind === 'image') warn(`${slide.where}: "poster" is only used for a video, and ${q(slide.path)} is a picture`);
    checkFile(slide.poster, `${slide.where} poster`, 'image');
  }

  const u = p.user;
  const name = typeof u === 'string' ? u : isObj(u) ? u.username : undefined;
  if (typeof name !== 'string' || !name.trim()) err(`${at}: no user ("user.username")`);
  else if (!/^[A-Za-z0-9._]{1,30}$/.test(name)) warn(`${at}: username ${q(name)} does not look like an Instagram username`);
  if (isObj(u)) {
    if (u.avatar !== undefined) checkFile(u.avatar, `${at} avatar`, 'image');
    if (u.profile_url !== undefined && !isProfileUrl(u.profile_url)) err(`${at}: "user.profile_url" must be https://www.instagram.com/<username>/`);
  }

  if (p.url === undefined) warn(`${at}: no "url"; the clone cannot link back to Instagram`);
  else if (!isPostUrl(p.url)) err(`${at}: "url" must be the permalink of the post, https://www.instagram.com/p/<shortcode>/ (or /reel/<shortcode>/)`);
  if (p.id !== undefined) {
    if (typeof p.id !== 'string' || !p.id) err(`${at}: "id" must be a non-empty string`);
    else if (ids.has(p.id)) err(`${at}: duplicate id ${q(p.id)} (also used by posts[${ids.get(p.id)}])`);
    else ids.set(p.id, i);
  }
  if (p.caption === undefined) warn(`${at}: no "caption"; use "" when the post has none`);
  else if (typeof p.caption !== 'string') err(`${at}: "caption" must be a string`);
  if (p.comment_count === undefined || p.comment_count === null) warn(`${at}: "comment_count" is unknown; the clone shows 0`);
  else if (!isCount(p.comment_count)) err(`${at}: "comment_count" must be a non-negative integer or null`);
  if (p.like_count !== undefined && p.like_count !== null && !isCount(p.like_count)) err(`${at}: "like_count" must be a non-negative integer or null`);
  if (p.taken_at !== undefined && !isIso(p.taken_at)) err(`${at}: "taken_at" must be an ISO 8601 UTC timestamp such as 2026-11-01T15:04:05Z`);
  if (p.media_type !== undefined && !['image', 'carousel', 'video'].includes(p.media_type)) err(`${at}: "media_type" must be image, carousel or video`);
  if (p.comments !== undefined && !Array.isArray(p.comments)) err(`${at}: "comments" must be a list`);
});

for (const key of [...files.keys()].sort()) {
  const rel = files.get(key);
  if (/^(posts|avatars)\//.test(key) && !named.has(rel)) warn(`file ${q(rel)} is not referenced by post.json`);
}
if (totalBytes > 4096 * MiB) warn('the pictures and videos add up to more than 4 GB, more than the clone takes in one import; import the export in parts');

const videos = [...used.values()].filter((kind) => kind === 'video').length;
for (const m of errors) console.log('ERROR: ' + m);
for (const m of warnings) console.log('WARN: ' + m);
if (errors.length) console.log(`FAILED: ${errors.length} error(s), ${warnings.length} warning(s)`);
else console.log(`OK: ${posts.length} post(s), ${used.size - videos} image file(s), ${videos} video file(s), ${warnings.length} warning(s)`);
process.exit(errors.length ? 1 : 0);
