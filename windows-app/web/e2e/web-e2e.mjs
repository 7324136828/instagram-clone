// End-to-end test of the browser pages: the real exe, a real browser (Microsoft Edge, headless,
// through Playwright), and the files the app writes. Not part of `cargo test`; see README.md in
// this folder.
//
//   node web\e2e\web-e2e.mjs [--exe <exe>] [--work <folder>] [--shots <folder>] [--clone <folder>]
//
// Playwright is imported from the Instagram clone's e2e\node_modules, read only (nothing is
// installed for this project). Without it the script prints SKIPPED and ends with exit code 0.
//
// Phase A opens core\tests\fixtures\export-basic and the clone's export-videos.zip in the app and
// checks in the browser: the banner, the order and captions of the posts, that every picture
// loads (naturalWidth > 0, also the carousel and the HEIC picture, which is converted), that a
// video plays (with Range requests); an edit of a description in the browser reaches
// descriptions.json and the app; an edit in the app (a scenario step that waits for a file this
// script creates) shows after a reload; "Clear library" is refused with the app's sentence; an
// import of the clone's export-basic.zip lands in the browser imports folder and in the app;
// "Download all" gives an archive the clone's validator accepts; raw requests: a foreign Host, a
// cross-site write, a path outside the open posts, the API address. Screenshots in the light and
// the dark appearance. Then the app is closed with WM_CLOSE (taskkill without /F) and must end
// within 5 seconds with both ports free.
// Phase B opens loose pictures and a loose MP4 (the clone's clip2.mp4) and checks them in the
// browser: the video is listed with a poster made from its first frame, and it plays (Range
// requests, its currentTime advances). An import is refused. Both phases check that the media
// come from the page address: Edge refuses them in a page of another port
// (Cross-Origin-Resource-Policy: same-site).
// Phase C takes screenshots of the app's About and Browser view sheets with the exe's
// --screenshot option.
//
// Every run of the exe has a --profile-dir of its own under the work folder; nothing of the
// user's profile is read or written.

import { spawn, spawnSync } from 'node:child_process';
import { createConnection, createServer } from 'node:net';
import { copyFileSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';

const PROJECT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

function option(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 && process.argv[index + 1] ? resolve(process.argv[index + 1]) : fallback;
}

const EXE = option('--exe', join(PROJECT, 'build', 'target', 'release', 'instagram-windows-app.exe'));
const WORK = option('--work', join(tmpdir(), `instagram-windows-app-e2e-${process.pid}`));
const SHOTS = option('--shots', join(WORK, 'shots'));
const CLONE = option('--clone', resolve(PROJECT, '..'));
const PLAYWRIGHT = join(CLONE, 'e2e', 'node_modules', 'playwright', 'index.mjs');
const FIXTURES = join(PROJECT, 'core', 'tests', 'fixtures');
const CLONE_FIXTURES = join(CLONE, 'e2e', 'fixtures');
const VALIDATOR = join(PROJECT, 'core', 'tests', 'validator', 'validate-export.mjs');
const APP_EDIT = 'Edited in the app window';
const BROWSER_EDIT = 'Edited in the browser 🌊\nSecond line.';

if (!existsSync(PLAYWRIGHT)) {
  console.log(`SKIPPED: Playwright was not found at ${PLAYWRIGHT} (the Instagram clone's e2e\\node_modules).`);
  process.exit(0);
}
if (!existsSync(EXE)) {
  console.error(`The exe ${EXE} does not exist. Build it first (cargo build --release -p instagram-windows-app).`);
  process.exit(1);
}
const { chromium } = await import(pathToFileURL(PLAYWRIGHT).href);

let failures = 0;
const results = [];
function pass(what) {
  results.push(`PASS ${what}`);
  console.log(`PASS ${what}`);
}
function check(condition, what, detail = '') {
  if (condition) {
    pass(what);
  } else {
    failures += 1;
    results.push(`FAIL ${what} ${detail}`);
    console.log(`FAIL ${what} ${detail}`);
  }
}
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

async function poll(probe, timeoutMs, everyMs = 200) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value) return value;
    if (Date.now() > deadline) return value;
    await sleep(everyMs);
  }
}

/** A port nobody listens on. */
function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
  });
}

/** True when something accepts a connection on 127.0.0.1:port. */
function answers(port) {
  return new Promise((done) => {
    const socket = createConnection({ host: '127.0.0.1', port });
    socket.once('connect', () => { socket.destroy(); done(true); });
    socket.once('error', () => done(false));
    socket.setTimeout(1000, () => { socket.destroy(); done(false); });
  });
}

/** One raw HTTP request (headers a browser would not let a page set); status and body. */
function raw(port, head, body = '') {
  return new Promise((done, fail) => {
    const socket = createConnection({ host: '127.0.0.1', port });
    const chunks = [];
    socket.on('data', (chunk) => chunks.push(chunk));
    socket.on('end', () => {
      const text = Buffer.concat(chunks).toString('utf8');
      done({ status: Number(text.split(' ')[1]), body: text.slice(text.indexOf('\r\n\r\n') + 4) });
    });
    socket.on('error', fail);
    socket.write(`${head}\r\nConnection: close\r\n\r\n${body}`);
  });
}

/** The app with its own profile; resolves once app.log names the two addresses. */
async function startApp(name, paths, extra = []) {
  const profile = join(WORK, name);
  rmSync(profile, { recursive: true, force: true });
  mkdirSync(profile, { recursive: true });
  const frontendPort = await freePort();
  let backendPort = await freePort();
  while (backendPort === frontendPort) backendPort = await freePort();
  const args = ['--profile-dir', profile, '--watchdog', '600', '--frontend-port', String(frontendPort), '--backend-port', String(backendPort), ...extra, ...paths];
  const child = spawn(EXE, args, { stdio: 'ignore', windowsHide: false });
  const log = join(profile, 'instagram-windows-app', 'data', 'app.log');
  const addresses = await poll(() => {
    if (!existsSync(log)) return null;
    const match = /Browser pages: (http:\/\/[^;\s]+); API: (http:\/\/\S+)/.exec(readFileSync(log, 'utf8'));
    return match ? { page: match[1], api: match[2] } : null;
  }, 60_000);
  if (!addresses) throw new Error(`The app did not log its addresses in ${log}`);
  return { child, profile, log, page: addresses.page, api: addresses.api, pagePort: Number(new URL(addresses.page).port), apiPort: Number(new URL(addresses.api).port) };
}

/** Closes the app as a person does (WM_CLOSE); checks that it ends quickly and frees its ports. */
async function closeApp(app, label) {
  const exited = new Promise((done) => app.child.once('exit', (code) => done(code)));
  const started = Date.now();
  spawnSync('taskkill', ['/PID', String(app.child.pid)], { stdio: 'ignore' });
  const code = await Promise.race([exited, sleep(5000).then(() => 'timeout')]);
  check(code !== 'timeout', `${label}: the app ends within 5 s after WM_CLOSE (${Date.now() - started} ms, exit code ${code})`);
  if (code === 'timeout') spawnSync('taskkill', ['/F', '/PID', String(app.child.pid)], { stdio: 'ignore' });
  check(!(await answers(app.pagePort)) && !(await answers(app.apiPort)), `${label}: both ports are free afterwards`);
}

const library = async (app) => (await fetch(`${app.page}/api/library`)).json();

/** Scrolls every card into view and waits until its picture has pixels. */
async function picturesLoad(page, ids) {
  const failed = [];
  for (const id of ids) {
    const card = page.locator(`article.post[data-post-id="imp:${id}"]`);
    await card.scrollIntoViewIfNeeded();
    const loaded = await poll(async () => card.evaluate((element) => {
      const image = element.querySelector('img.photo__img');
      const video = element.querySelector('video');
      // A video the browser cannot decode (the fixture's broken .mov) shows the clone's fallback.
      if (element.querySelector('[data-testid="video-unplayable"]')) return true;
      if (image) return image.complete && image.naturalWidth > 0;
      return !!video && video.readyState >= 1;
    }).catch(() => false), 30_000);
    if (!loaded) failed.push(id);
  }
  return failed;
}

async function newPage(browser, theme) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, acceptDownloads: true, colorScheme: theme });
  await context.addInitScript((value) => localStorage.setItem('ig-theme', value), theme);
  const page = await context.newPage();
  return { context, page };
}

mkdirSync(WORK, { recursive: true });
mkdirSync(SHOTS, { recursive: true });
console.log(`exe ${EXE}\nwork ${WORK}\nshots ${SHOTS}`);
const browser = await chromium.launch({ channel: 'msedge', headless: true });
const apps = [];
try {
  // --- Phase A: exports -------------------------------------------------------------------------
  const trigger = join(WORK, 'go-edit-in-app.txt');
  rmSync(trigger, { force: true });
  const scenario = `wait-file:${trigger},edit:0,edit-text:${APP_EDIT},edit-save`;
  const app = await startApp('profile-exports', [join(FIXTURES, 'export-basic'), join(CLONE_FIXTURES, 'export-videos.zip')], ['--scenario', scenario]);
  apps.push(app);
  console.log(`page ${app.page}  api ${app.api}`);
  const before = await library(app);
  const ids = before.posts.map((post) => post.id);
  check(ids.length === 11, `the API lists the 11 posts of export-basic and export-videos.zip (${ids.length})`);

  const { context: lightContext, page } = await newPage(browser, 'light');
  const ranges = [];
  page.on('request', (request) => {
    if (request.url().includes('/api/library/files/') && request.headers().range) ranges.push(`${request.headers().range} ${new URL(request.url()).pathname}`);
  });
  await page.goto(app.page);
  const summary = page.getByTestId('library-summary');
  await summary.waitFor({ timeout: 30_000 });
  const summaryText = (await summary.textContent()) ?? '';
  check(summaryText.includes(`Showing ${ids.length} posts from your library (${before.imports.length} imports)`), 'the banner names the posts and the sources', JSON.stringify(summaryText));
  // "saved in" names where imports go (the browser imports folder), never the user's own export.
  const importsFolder = join(app.profile, 'Local', 'instagram-windows-app', 'library');
  check(summaryText.includes(`saved in ${importsFolder}`) && !summaryText.includes(join(FIXTURES, 'export-basic')), 'the banner says "saved in" the browser imports folder', JSON.stringify(summaryText));
  const firstCaption = (await page.locator(`article.post[data-post-id="imp:${ids[0]}"] .post__caption`).textContent()) ?? '';
  check(firstCaption.includes(before.posts[0].caption.split('\n')[0].slice(0, 30)), 'the first caption is the app\'s');
  const notLoaded = await picturesLoad(page, ids);
  check(notLoaded.length === 0, 'every picture of the feed loads (naturalWidth > 0; a video the browser cannot play shows the clone fallback)', JSON.stringify(notLoaded));
  // Media carry Cross-Origin-Resource-Policy: same-site, and Edge takes another port of the same
  // address for another site: the pages must fetch pictures, posters and videos from their own
  // address. A frontend that used the API address would fail here, not only on screen.
  const mediaUrls = await page.locator('article.post img.photo__img, article.post video').evaluateAll((elements) => elements.flatMap((element) => [element.currentSrc || element.src, element.poster]).filter(Boolean));
  const mediaOrigins = [...new Set(mediaUrls.map((url) => new URL(url, app.page).origin))];
  check(mediaUrls.length > 0 && mediaOrigins.length === 1 && mediaOrigins[0] === new URL(app.page).origin, `every picture, poster and video comes from the page address (${mediaOrigins.join(', ')})`);
  // The feed adds cards while it is scrolled; all of them are there now.
  const shown = await page.locator('article.post[data-post-id^="imp:"]').evaluateAll((cards) => cards.map((card) => card.getAttribute('data-post-id').slice(4)));
  check(JSON.stringify(shown) === JSON.stringify(ids), 'the feed shows the posts in the order of the app', JSON.stringify(shown));
  const heic = page.locator('article.post[data-post-id="imp:FXTbasic004"] img.photo__img');
  check((await heic.evaluate((image) => image.naturalWidth)) > 0, 'the HEIC picture is shown (converted to JPEG)');
  const slides = await page.locator('article.post[data-post-id="imp:FXTbasic005"] img.photo__img').count();
  check(slides >= 2, `the carousel has its two slides (${slides})`);
  const videoId = before.posts.find((post) => post.media.some((media) => media.type === 'video' && media.mime === 'video/mp4'))?.id
    ?? before.posts.find((post) => post.media.some((media) => media.type === 'video'))?.id;
  const video = page.locator(`article.post[data-post-id="imp:${videoId}"] video`).first();
  await video.scrollIntoViewIfNeeded();
  await video.evaluate((element) => element.play().catch(() => undefined));
  const played = await poll(() => video.evaluate((element) => element.readyState >= 2 && element.videoWidth > 0 && element.currentTime > 0).catch(() => false), 30_000);
  check(played, `a video plays (${videoId})`);
  check(ranges.length > 0, `the video was fetched with Range requests (${ranges.slice(0, 2).join(' | ')})`);
  await page.evaluate(() => window.scrollTo(0, 0));
  await sleep(500);
  await page.screenshot({ path: join(SHOTS, 'browser-feed-light.png') });

  // The post view.
  await page.locator('article.post[data-post-id="imp:FXTbasic005"]').getByRole('button', { name: 'Comment', exact: true }).click();
  const detail = page.getByRole('dialog', { name: /^Post by / });
  await detail.waitFor();
  await sleep(500);
  await page.screenshot({ path: join(SHOTS, 'browser-detail-light.png') });
  await detail.getByRole('button', { name: 'Close' }).first().click();

  // An edit in the browser: a post of the archive.
  const target = before.posts.find((post) => post.id.startsWith('FXTvideo') && post.id !== ids[0])?.id ?? 'FXTbasic002';
  const card = page.locator(`article.post[data-post-id="imp:${target}"]`);
  await card.scrollIntoViewIfNeeded();
  await card.getByRole('button', { name: 'More options' }).click();
  await page.getByRole('dialog', { name: 'Post options' }).getByRole('button', { name: 'Edit description' }).click();
  const editor = page.getByRole('dialog', { name: 'Edit description' });
  const field = editor.getByRole('textbox', { name: 'Description' });
  await field.fill(BROWSER_EDIT);
  await sleep(300);
  await page.screenshot({ path: join(SHOTS, 'browser-edit-light.png') });
  await field.press('Control+Enter');
  await page.getByTestId('toast').filter({ hasText: 'Description saved in your library.' }).waitFor({ timeout: 15_000 });
  pass('the browser says the description was saved');
  const descriptionsFile = join(app.profile, 'instagram-windows-app', 'data', 'descriptions.json');
  const stored = await poll(() => existsSync(descriptionsFile) && JSON.parse(readFileSync(descriptionsFile, 'utf8')).descriptions, 10_000);
  const entry = Object.entries(stored || {}).find(([key]) => key.endsWith(`|${target}`));
  check(entry && entry[1].caption === BROWSER_EDIT, `descriptions.json holds the edit of ${target}`, JSON.stringify(entry));
  check(entry && entry[0].startsWith('archive|'), `with the archive's key (${entry?.[0]?.split('|')[0]})`);
  check(readFileSync(app.log, 'utf8').includes(`The description of post ${target} was changed in a browser.`), 'the app logged the edit from the browser');
  const afterEdit = await library(app);
  check(afterEdit.posts.find((post) => post.id === target)?.caption === BROWSER_EDIT, 'the app\'s state has the new caption (GET /api/library is made from it)');
  check(afterEdit.revision > before.revision, `the revision grew (${before.revision} -> ${afterEdit.revision})`);
  await page.reload();
  await page.getByTestId('library-summary').waitFor();
  const reloaded = (await page.locator(`article.post[data-post-id="imp:${target}"] .post__caption`).textContent()) ?? '';
  check(reloaded.includes('Edited in the browser'), 'the edit survives a reload');

  // An edit in the app (the scenario waits for the trigger file), then a reload.
  writeFileSync(trigger, 'go');
  const appEdit = await poll(() => existsSync(descriptionsFile) && Object.values(JSON.parse(readFileSync(descriptionsFile, 'utf8')).descriptions).some((value) => value.caption === APP_EDIT), 60_000);
  check(appEdit, 'the app saved its own edit (scenario edit:0) in descriptions.json');
  await page.reload();
  await page.getByTestId('library-summary').waitFor();
  const firstAfter = (await page.locator(`article.post[data-post-id="imp:${ids[0]}"] .post__caption`).textContent()) ?? '';
  check(firstAfter.includes(APP_EDIT), `a reload shows the app's edit of ${ids[0]}`);

  // "Clear library" is refused while the app shows its own folders and archives.
  await page.getByTestId('import-banner').getByRole('button', { name: 'Clear library' }).click();
  await page.getByRole('alertdialog', { name: 'Clear library?' }).getByRole('button', { name: 'Clear library' }).click();
  const refusal = page.getByTestId('toast').filter({ hasText: 'The posts come from your own pictures, folders or archives' });
  await refusal.waitFor({ timeout: 15_000 });
  pass(`"Clear library" is refused: ${(await refusal.textContent())?.trim()}`);

  // An import in the browser: the clone's export-basic.zip.
  await page.getByTestId('import-open').click();
  await page.getByTestId('import-archive-input').setInputFiles(join(CLONE_FIXTURES, 'export-basic.zip'));
  const status = page.getByTestId('import-status');
  await poll(async () => (await status.getAttribute('data-state')) === 'success', 120_000);
  check((await status.getAttribute('data-state')) === 'success', `the import is saved (${(await status.textContent())?.trim()})`);
  await page.screenshot({ path: join(SHOTS, 'browser-import-light.png') });
  await page.getByRole('button', { name: 'View feed' }).click();
  const libraryManifest = join(app.profile, 'Local', 'instagram-windows-app', 'library', 'post.json');
  check(existsSync(libraryManifest), 'the browser imports folder holds a post.json (in the profile\'s Local folder)');
  const imported = await poll(() => readFileSync(app.log, 'utf8').match(/(\d+) posts were imported in a browser and are shown\./), 15_000);
  check(imported && imported[1] === '6', `the app shows the imported posts (log: ${imported?.[0]})`);
  const afterImport = await library(app);
  check(afterImport.imports.some((record) => record.source_name === 'export-basic.zip'), 'the API lists the import from the browser');
  const libraryPost = JSON.parse(readFileSync(libraryManifest, 'utf8'));
  check(libraryPost.posts.length === 6 && libraryPost.source?.type === 'library', `the folder is a library export with 6 posts (${libraryPost.posts.length})`);

  // Download all in the browser; the clone's validator checks the archive.
  await page.getByTestId('library-summary').waitFor();
  const downloads = join(WORK, 'downloads');
  mkdirSync(downloads, { recursive: true });
  const waiting = page.waitForEvent('download', { timeout: 120_000 });
  await page.getByTestId('import-banner').getByTestId('download-all').click();
  const download = await waiting;
  const zip = join(downloads, download.suggestedFilename());
  await download.saveAs(zip);
  const extracted = join(downloads, 'extracted');
  rmSync(extracted, { recursive: true, force: true });
  mkdirSync(extracted, { recursive: true });
  // Windows' own tar (bsdtar) reads zip archives; a tar of another toolset on PATH may not.
  const untar = spawnSync(join(process.env.SystemRoot ?? 'C:\Windows', 'System32', 'tar.exe'), ['-xf', zip, '-C', extracted], { encoding: 'utf8' });
  check(untar.status === 0, `the download ${download.suggestedFilename()} can be extracted`);
  const verdict = spawnSync(process.execPath, [VALIDATOR, extracted], { encoding: 'utf8' });
  const verdictText = `${verdict.stdout}${verdict.stderr}`.trim().split('\n').pop();
  check(verdict.status === 0, `the clone's validator accepts the download: ${verdictText}`);

  // Raw requests.
  const evil = await raw(app.pagePort, 'GET /api/health HTTP/1.1\r\nHost: evil.example');
  check(evil.status === 400, `Host: evil.example -> ${evil.status} ${evil.body}`);
  const crossBody = JSON.stringify({ caption: 'from another site' });
  const cross = await raw(app.pagePort, `PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1:${app.pagePort}\r\nOrigin: http://evil.example\r\nSec-Fetch-Site: cross-site\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(crossBody)}`, crossBody);
  check(cross.status === 403, `a cross-site PATCH -> ${cross.status} ${cross.body}`);
  const traversal = await raw(app.pagePort, `GET /api/library/files/..%2f..%2fWindows%2fwin.ini HTTP/1.1\r\nHost: 127.0.0.1:${app.pagePort}`);
  check(traversal.status === 400 || traversal.status === 404, `/api/library/files/..%2f..%2fWindows%2fwin.ini -> ${traversal.status} ${traversal.body}`);
  const health = await raw(app.apiPort, `GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:${app.apiPort}`);
  check(health.status === 200 && health.body.includes('"ok"'), `API address /api/health -> ${health.status} ${health.body}`);
  const docs = await raw(app.apiPort, `GET /docs HTTP/1.1\r\nHost: 127.0.0.1:${app.apiPort}`);
  check(docs.status === 200 && docs.body.includes('/api/library/uploads'), `API address /docs -> ${docs.status}`);
  await lightContext.close();

  // The same pages in the dark appearance.
  const { context: darkContext, page: dark } = await newPage(browser, 'dark');
  await dark.goto(app.page);
  await dark.getByTestId('library-summary').waitFor();
  // The feed adds cards while it is scrolled: scroll through all of them once.
  await picturesLoad(dark, (await library(app)).posts.map((post) => post.id));
  await dark.evaluate(() => window.scrollTo(0, 0));
  await sleep(500);
  await dark.screenshot({ path: join(SHOTS, 'browser-feed-dark.png') });
  await dark.locator('article.post[data-post-id="imp:FXTbasic005"]').getByRole('button', { name: 'Comment', exact: true }).click();
  await dark.getByRole('dialog', { name: /^Post by / }).waitFor();
  await sleep(500);
  await dark.screenshot({ path: join(SHOTS, 'browser-detail-dark.png') });
  await dark.keyboard.press('Escape');
  const darkCard = dark.locator('article.post[data-post-id="imp:FXTbasic001"]');
  await darkCard.scrollIntoViewIfNeeded();
  await darkCard.getByRole('button', { name: 'More options' }).click();
  await dark.getByRole('dialog', { name: 'Post options' }).getByRole('button', { name: 'Edit description' }).click();
  await dark.getByRole('dialog', { name: 'Edit description' }).waitFor();
  await sleep(300);
  await dark.screenshot({ path: join(SHOTS, 'browser-edit-dark.png') });
  await darkContext.close();
  await closeApp(app, 'exports');

  // --- Phase B: loose pictures and a loose video --------------------------------------------------
  const looseFolder = join(WORK, 'loose');
  rmSync(looseFolder, { recursive: true, force: true });
  mkdirSync(looseFolder, { recursive: true });
  for (const name of readdirSync(join(FIXTURES, 'loose'))) copyFileSync(join(FIXTURES, 'loose', name), join(looseFolder, name));
  const unpacked = join(WORK, 'clone-videos');
  mkdirSync(unpacked, { recursive: true });
  const clip = spawnSync(join(process.env.SystemRoot ?? 'C:\Windows', 'System32', 'tar.exe'), ['-xf', join(CLONE_FIXTURES, 'export-videos.zip'), '-C', unpacked, 'posts/clip2.mp4'], { encoding: 'utf8' });
  check(clip.status === 0, 'the clone\'s clip2.mp4 is taken from export-videos.zip');
  copyFileSync(join(unpacked, 'posts', 'clip2.mp4'), join(looseFolder, 'clip.mp4'));
  const looseBefore = readdirSync(looseFolder).sort().join(',');
  const loose = await startApp('profile-loose', [looseFolder]);
  apps.push(loose);
  const looseLibrary = await library(loose);
  const looseIds = looseLibrary.posts.map((post) => post.id);
  const loosePost = looseLibrary.posts.find((post) => post.media_type === 'video');
  const looseMedia = loosePost?.media?.[0];
  check(looseMedia?.type === 'video' && looseMedia?.mime === 'video/mp4' && /_1_poster\.jpg$/.test(looseMedia?.poster ?? ''), `the loose MP4 is listed as a video with a poster (${JSON.stringify(looseMedia)})`);
  check(loosePost?.image === looseMedia?.poster && loosePost?.images?.length === 1, 'its poster is its picture in the grids');
  const posterAnswer = await fetch(`${loose.page}/api/library/files/${looseMedia?.poster}`);
  check(posterAnswer.status === 200 && posterAnswer.headers.get('content-type') === 'image/jpeg', `its poster is served (${posterAnswer.status} ${posterAnswer.headers.get('content-type')})`);
  const { context: looseContext, page: loosePage } = await newPage(browser, 'light');
  const looseRanges = [];
  loosePage.on('request', (request) => {
    if (request.url().includes('/api/library/files/') && request.headers().range) looseRanges.push(`${request.headers().range} ${new URL(request.url()).pathname}`);
  });
  await loosePage.goto(loose.page);
  const looseSummary = loosePage.getByTestId('library-summary');
  await looseSummary.waitFor();
  check(((await looseSummary.textContent()) ?? '').includes(`Showing ${looseIds.length} posts from your library (1 import)`), `loose pictures: the banner names ${looseIds.length} posts`);
  const looseFailed = await picturesLoad(loosePage, looseIds);
  check(looseFailed.length === 0, 'loose pictures: every picture loads, the HEIC one converted', JSON.stringify(looseFailed));
  // The video's poster as a picture of this page: from the page address it loads; the same file
  // from the API address is refused (Cross-Origin-Resource-Policy: same-site, another port).
  const pictureWidth = (src) => loosePage.evaluate((url) => new Promise((done) => {
    const picture = new Image();
    picture.onload = () => done(picture.naturalWidth);
    picture.onerror = () => done(0);
    picture.src = url;
  }), src);
  const posterHere = await pictureWidth(`/api/library/files/${looseMedia?.poster}`);
  const posterFromApi = await pictureWidth(new URL(`/api/library/files/${looseMedia?.poster}`, loose.api).href);
  check(posterHere > 0 && posterFromApi === 0, `the poster loads in a page from the page address (${posterHere} px wide) and not from the API address (${posterFromApi})`);
  check(looseLibrary.posts.every((post) => post.like_count === null && !post.url), 'loose pictures: no link and no like count');
  const looseVideo = loosePage.locator(`article.post[data-post-id="imp:${loosePost?.id}"] video`).first();
  await looseVideo.scrollIntoViewIfNeeded();
  await looseVideo.evaluate((element) => element.play().catch(() => undefined));
  const firstTime = await poll(() => looseVideo.evaluate((element) => (element.readyState >= 2 && element.videoWidth > 0 && element.currentTime > 0 ? element.currentTime : 0)).catch(() => 0), 30_000);
  await sleep(700);
  const laterTime = await looseVideo.evaluate((element) => element.currentTime).catch(() => 0);
  check(firstTime > 0 && laterTime !== firstTime, `the loose MP4 plays in the browser: currentTime ${firstTime} -> ${laterTime}`);
  check(looseRanges.some((line) => line.includes('.mp4')), `it was fetched with Range requests (${looseRanges.filter((line) => line.includes('.mp4')).slice(0, 2).join(' | ')})`);
  check(readdirSync(looseFolder).sort().join(',') === looseBefore, 'nothing was written next to the pictures and the video');
  check(existsSync(join(loose.profile, 'Local', 'instagram-windows-app', 'posters')), 'the poster went into the profile\'s poster cache');
  await loosePage.evaluate(() => window.scrollTo(0, 0));
  await loosePage.screenshot({ path: join(SHOTS, 'browser-loose-light.png') });
  await loosePage.getByTestId('import-open').click();
  await loosePage.getByTestId('import-archive-input').setInputFiles(join(CLONE_FIXTURES, 'export-basic.zip'));
  const refusedImport = loosePage.getByText('Instagram Windows App is showing loose pictures', { exact: false }).first();
  const refusedShown = await poll(() => refusedImport.isVisible().catch(() => false), 60_000);
  check(refusedShown, 'loose pictures: an import from the browser is refused with the app\'s sentence');
  check(!existsSync(join(loose.profile, 'Local', 'instagram-windows-app', 'library', 'post.json')), 'loose pictures: nothing was saved in the browser imports folder');
  await looseContext.close();
  await closeApp(loose, 'loose');

  // --- Phase C: the app's own sheets --------------------------------------------------------------
  for (const theme of ['light', 'dark']) {
    for (const [step, name] of [['about', 'app-about'], ['browser-view', 'app-browser-view']]) {
      const profile = join(WORK, `profile-shot-${name}-${theme}`);
      rmSync(profile, { recursive: true, force: true });
      const port = await freePort();
      let api = await freePort();
      while (api === port) api = await freePort();
      const shot = join(SHOTS, `${name}-${theme}.png`);
      const run = spawnSync(EXE, ['--profile-dir', profile, '--screenshot', shot, '--size', '1100x860', '--scale', '1.0', '--theme', theme, '--frontend-port', String(port), '--backend-port', String(api), '--scenario', step, join(FIXTURES, 'export-basic')], { encoding: 'utf8', timeout: 180_000 });
      check(run.status === 0 && existsSync(shot), `screenshot ${name}-${theme}.png (${(run.stderr || '').trim().split('\n').pop()})`);
    }
  }
} catch (error) {
  failures += 1;
  console.log(`FAIL ${error.stack ?? error}`);
} finally {
  await browser.close();
  for (const app of apps) {
    if (app.child.exitCode === null) spawnSync('taskkill', ['/F', '/PID', String(app.child.pid)], { stdio: 'ignore' });
  }
}
writeFileSync(join(WORK, 'e2e-results.txt'), `${results.join('\n')}\n`);
console.log(failures === 0 ? `ALL PASSED (${results.length} checks)` : `${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
