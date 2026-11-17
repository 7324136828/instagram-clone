/**
 * The import pipeline shared by archives and folders:
 * index the source -> read post.json -> load the referenced pictures and videos -> assemble the result.
 */
import { openArchive } from './archive';
import { prepareHeic } from './heic';
import { DEFAULT_LIMITS, IMAGE_CONCURRENCY, formatBytes, type ImportLimits } from './limits';
import { parseManifest, parseManifestText, type ManifestMedia, type ManifestPost } from './manifest';
import { registerBlobUrl, revokeBlobUrl } from './registry';
import { SNIFF_BYTES, describeUnsupportedMedia, sniffImage, sniffMedia } from './sniff';
import {
  EntryIndex,
  entriesFromFiles,
  exportRootOf,
  noManifestError,
  type PathedFile,
  type SourceEntry,
} from './source';
import {
  ImportError,
  type ImportOptions,
  type ImportPhase,
  type ImportResult,
  type ImportSourceKind,
  type ImportWarning,
  type ImportWarningCode,
  type ImportedMedia,
  type ImportedMediaKind,
  type ImportedMediaType,
  type ImportedPost,
  type ImportedUser,
} from './types';
import {
  abortError,
  asBlobPart,
  decodeText,
  describeError,
  nextTask,
  readBytes,
  runPool,
  throwIfAborted,
} from './util';

/** Synchronous matching work is interrupted this often, so that Cancel and the page stay responsive. */
const YIELD_AFTER_MS = 50;

/** Bytes of pictures and videos taken so far; shared by the sources of one import action. */
export interface MediaBudget {
  used: number;
}

export interface CoreOptions extends ImportOptions {
  /** Overrides for the safety limits (tests only; the public API always uses the defaults). */
  limits?: Partial<ImportLimits>;
  /** Shared with other sources of the same import action; a fresh one per import otherwise. */
  budget?: MediaBudget;
}

type Progress = (phase: ImportPhase, done: number, total: number, message: string) => void;

function progressReporter(options: ImportOptions): Progress {
  const callback = options.onProgress;
  if (!callback) return () => undefined;
  return (phase, done, total, message) => {
    try {
      callback({ phase, done, total, message });
    } catch {
      // A broken progress listener must not break the import.
    }
  };
}

/** Outcome of loading one referenced file. */
type Loaded =
  | { ok: true; kind: ImportedMediaKind; url: string; mime: string; converted: boolean; bytes: number }
  | { ok: false; code: ImportWarningCode; message: string };

interface MediaJob {
  entry: SourceEntry;
  /** True while the file is only wanted as a picture (poster, avatar): it is then read with the image limit. */
  imageOnly: boolean;
  /** True when post.json calls the file a video somewhere; only used to word warnings. */
  declaredVideo: boolean;
  outcome?: Loaded;
}

interface PlannedMedia {
  item: ManifestMedia;
  job: MediaJob | undefined;
  poster: MediaJob | undefined;
}

interface Context {
  sourceName: string;
  sourceKind: ImportSourceKind;
  limits: ImportLimits;
  signal: AbortSignal | undefined;
  progress: Progress;
  budget: MediaBudget;
  /** Bytes this import added to the budget, given back when it fails. */
  charged: number;
  /** Object URLs created so far, so that a failed import can give them back. */
  createdUrls: string[];
}

async function readManifest(entry: SourceEntry, ctx: Context): Promise<unknown> {
  const { limits, signal } = ctx;
  const tooLarge = () =>
    new ImportError('too-large', `post.json is larger than ${formatBytes(limits.maxManifestBytes)}.`);
  if (entry.size > limits.maxManifestBytes) throw tooLarge();
  let blob: Blob;
  try {
    blob = await entry.open(limits.maxManifestBytes, signal);
  } catch (err) {
    if (err instanceof ImportError && err.code === 'too-large') throw tooLarge();
    throw err;
  }
  if (blob.size > limits.maxManifestBytes) throw tooLarge();
  const bytes = await readBytes(blob);
  throwIfAborted(signal);
  return parseManifestText(decodeText(bytes));
}

const IMAGE_FORMATS = 'JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF';
const VIDEO_FORMATS = 'MP4, MOV or WebM';

/** Turns a HEIC/HEIF picture into one the browser can show; null when it already can. */
async function displayableHeic(
  display: Blob,
  signal: AbortSignal | undefined,
  onConvertStart: () => void,
): Promise<{ blob: Blob; mime: string } | null> {
  const prepared = await prepareHeic(display, signal, onConvertStart);
  if (!prepared.converted) return null;
  // Trust the bytes, not the label, of whatever the converter produced.
  const outHead = await readBytes(prepared.blob, 0, Math.min(prepared.blob.size, SNIFF_BYTES));
  const out = sniffImage(outHead);
  if (!out || out.format === 'heic') throw new Error('the converter did not produce a displayable image');
  const blob = prepared.blob.type === out.mime ? prepared.blob : prepared.blob.slice(0, prepared.blob.size, out.mime);
  return { blob, mime: out.mime };
}

async function loadMedia(job: MediaJob, ctx: Context, onConvertStart: () => void): Promise<Loaded> {
  const { limits, signal } = ctx;
  const { entry } = job;
  const failed = (message: string): Loaded => ({
    ok: false,
    code: job.declaredVideo ? 'unsupported-media' : 'unsupported-image',
    message,
  });
  const maxBytes = job.imageOnly ? limits.maxImageBytes : Math.max(limits.maxImageBytes, limits.maxVideoBytes);
  const tooLarge = (max: number) => failed(`"${entry.path}" is larger than ${formatBytes(max)} and was skipped.`);
  if (entry.size > maxBytes) return tooLarge(maxBytes);

  let blob: Blob;
  try {
    blob = await entry.open(maxBytes, signal);
  } catch (err) {
    if (err instanceof ImportError && err.code === 'aborted') throw err;
    if (err instanceof ImportError && err.code === 'too-large') return tooLarge(maxBytes);
    return failed(`"${entry.path}" could not be read: ${describeError(err)}`);
  }
  throwIfAborted(signal);
  if (blob.size > maxBytes) return tooLarge(maxBytes);
  if (blob.size === 0) return failed(`"${entry.path}" is an empty file.`);

  let head: Uint8Array;
  try {
    head = await readBytes(blob, 0, Math.min(blob.size, SNIFF_BYTES));
  } catch (err) {
    return failed(`"${entry.path}" could not be read: ${describeError(err)}`);
  }
  throwIfAborted(signal);
  const sniffed = sniffMedia(head);
  if (!sniffed) {
    const known = describeUnsupportedMedia(head);
    if (known) {
      return {
        ok: false,
        code: 'unsupported-media',
        message: `"${entry.path}" is ${known}, which is not supported. Convert it to MP4 (H.264) or WebM.`,
      };
    }
    return job.declaredVideo
      ? failed(`"${entry.path}" is not a supported video (${VIDEO_FORMATS}).`)
      : failed(`"${entry.path}" is not a supported image (${IMAGE_FORMATS}) or video (${VIDEO_FORMATS}).`);
  }

  let display: Blob;
  let mime = sniffed.mime;
  let converted = false;
  if (sniffed.kind === 'video') {
    if (job.imageOnly) {
      return { ok: false, code: 'unsupported-image', message: `"${entry.path}" is a video, not a picture.` };
    }
    if (blob.size > limits.maxVideoBytes) return tooLarge(limits.maxVideoBytes);
    // Videos are not copied (they can be a gigabyte): they play from the archive or folder,
    // so they stop working if that is deleted while the import is shown.
    display = blob.type === mime ? blob : blob.slice(0, blob.size, mime);
  } else {
    if (blob.size > limits.maxImageBytes) {
      return { ok: false, code: 'unsupported-image', message: `"${entry.path}" is larger than ${formatBytes(limits.maxImageBytes)} and was skipped.` };
    }
    if (entry.inMemory) {
      display = blob.slice(0, blob.size, mime);
    } else {
      // A folder file, a stored zip member or a tar member is only a window onto the user's
      // file. The picture is shown (and saved) long after the import has finished, so it gets
      // its own copy now: afterwards the source may be deleted, moved or overwritten.
      try {
        display = new Blob([asBlobPart(await readBytes(blob))], { type: mime });
      } catch (err) {
        return failed(`"${entry.path}" could not be read: ${describeError(err)}`);
      }
      throwIfAborted(signal);
    }
    if (sniffed.format === 'heic') {
      try {
        const heic = await displayableHeic(display, signal, onConvertStart);
        if (heic) {
          display = heic.blob;
          mime = heic.mime;
          converted = true;
        }
      } catch (err) {
        if (err instanceof ImportError && err.code === 'aborted') throw err;
        throwIfAborted(signal);
        return {
          ok: false,
          code: 'conversion-failed',
          message: `"${entry.path}" is a HEIC/HEIF image that could not be converted: ${describeError(err)}`,
        };
      }
    }
  }

  ctx.budget.used += display.size;
  ctx.charged += display.size;
  if (ctx.budget.used > limits.maxTotalMediaBytes) {
    throw new ImportError(
      'too-large',
      `The pictures and videos of this import add up to more than ${formatBytes(limits.maxTotalMediaBytes)}. Import the export in smaller parts.`,
    );
  }
  const url = registerBlobUrl(display);
  ctx.createdUrls.push(url);
  return { ok: true, kind: sniffed.kind, url, mime, converted, bytes: display.size };
}

/** The post-level type once the files are known: an undeclared single file may be a video. */
export function finalMediaType(declared: ImportedMediaType, media: readonly ImportedMedia[]): ImportedMediaType {
  if (declared !== 'image') return declared;
  if (media.length > 1) return 'carousel';
  return media[0].kind === 'video' ? 'video' : 'image';
}

async function buildResult(entries: readonly SourceEntry[], ctx: Context): Promise<ImportResult> {
  const { limits, signal, progress } = ctx;
  if (entries.length > limits.maxSourceEntries) {
    throw new ImportError('too-large', `"${ctx.sourceName}" contains more than ${limits.maxSourceEntries} files.`);
  }
  const index = new EntryIndex(entries);
  const manifestEntry = index.findManifest();
  if (!manifestEntry) throw noManifestError(ctx.sourceName);

  progress('manifest', 0, 1, 'Reading post.json');
  const manifest = parseManifest(await readManifest(manifestEntry, ctx), limits.maxPosts);
  progress('manifest', 1, 1, `Found ${manifest.posts.length} posts in post.json`);
  throwIfAborted(signal);

  // One job per distinct file, however many posts reference it.
  const root = exportRootOf(manifestEntry);
  const jobs = new Map<string, MediaJob>();
  const jobFor = (relativePath: string, use: 'media' | 'video' | 'picture'): MediaJob | undefined => {
    const entry = index.resolve(root, relativePath);
    if (!entry || entry === manifestEntry) return undefined;
    let job = jobs.get(entry.path);
    if (!job) {
      job = { entry, imageOnly: true, declaredVideo: false };
      jobs.set(entry.path, job);
    }
    if (use !== 'picture') job.imageOnly = false;
    if (use === 'video') job.declaredVideo = true;
    return job;
  };
  const planned = new Map<ManifestPost, PlannedMedia[]>();
  const avatarJobs = new Map<string, MediaJob | undefined>();
  // Matching paths to files is synchronous; a huge post.json must not lock up the page (and
  // the Cancel button) while it runs.
  let sliceStart = Date.now();
  const breathe = async (): Promise<void> => {
    if (Date.now() - sliceStart < YIELD_AFTER_MS) return;
    await nextTask();
    throwIfAborted(signal);
    sliceStart = Date.now();
  };
  for (const post of manifest.posts) {
    planned.set(
      post,
      post.media.map((item) => ({
        item,
        job: jobFor(item.path, item.type === 'video' ? 'video' : 'media'),
        poster: undefined,
      })),
    );
    await breathe();
  }
  // Posters and avatars are queued after the post media so that they never delay or starve them.
  for (const post of manifest.posts) {
    for (const plan of planned.get(post) ?? []) {
      if (plan.item.posterPath) plan.poster = jobFor(plan.item.posterPath, 'picture');
    }
    await breathe();
  }
  for (const post of manifest.posts) {
    const avatarPath = post.user.avatarPath;
    if (avatarPath && !avatarJobs.has(avatarPath)) avatarJobs.set(avatarPath, jobFor(avatarPath, 'picture'));
    await breathe();
  }

  const queue = [...jobs.values()];
  const total = queue.length;
  let done = 0;
  const files = (count: number) => `${count} file${count === 1 ? '' : 's'}`;
  if (total > 0) progress('images', 0, total, `Loading ${files(total)}`);
  await runPool(queue, IMAGE_CONCURRENCY, async (job) => {
    throwIfAborted(signal);
    job.outcome = await loadMedia(job, ctx, () => {
      progress('images', done, total, `Converting HEIC image ${Math.min(done + 1, total)} of ${total}`);
    });
    done++;
    progress('images', done, total, `Loaded file ${done} of ${total}`);
  });
  throwIfAborted(signal);

  const warnings: ImportWarning[] = [...manifest.warnings];
  // The warnings that say why a post is missing, as opposed to notes about a post that was
  // imported (unknown user, renamed id, dropped link, missing poster): those of entries the
  // parser rejected and, added below, those about media that are missing or unusable.
  const parsed = new Set(manifest.posts.map((post) => post.index));
  const skipReasons = warnings.filter((warning) => warning.postIndex !== undefined && !parsed.has(warning.postIndex));
  const posts: ImportedPost[] = [];
  const reportedAvatars = new Set<string>();
  for (const source of manifest.posts) {
    const media: ImportedMedia[] = [];
    const posterWarnings: ImportWarning[] = [];
    for (const { item, job, poster } of planned.get(source) ?? []) {
      const outcome = job?.outcome;
      const noun = item.type === 'video' ? 'video' : 'image';
      if (!outcome?.ok) {
        const warning: ImportWarning = {
          code: outcome ? outcome.code : 'missing-image',
          message: outcome
            ? `Post ${source.index + 1}: ${outcome.message}`
            : `Post ${source.index + 1}: the ${noun} "${item.path}" is not in the export.`,
          postIndex: source.index,
          path: item.path,
        };
        warnings.push(warning);
        skipReasons.push(warning);
        continue;
      }
      const loaded: ImportedMedia = {
        kind: outcome.kind,
        url: outcome.url,
        path: item.path,
        mime: outcome.mime,
        bytes: outcome.bytes,
      };
      if (outcome.converted) loaded.converted = true;
      if (item.alt) loaded.alt = item.alt;
      if (outcome.kind === 'video' && item.posterPath) {
        const posterOutcome = poster?.outcome;
        if (posterOutcome?.ok && posterOutcome.kind === 'image') {
          loaded.posterUrl = posterOutcome.url;
          loaded.posterPath = item.posterPath;
        } else {
          const where = `Post ${source.index + 1}: the video "${item.path}" is shown without its poster`;
          posterWarnings.push({
            code: posterOutcome ? (posterOutcome.ok ? 'unsupported-image' : posterOutcome.code) : 'missing-image',
            message: !posterOutcome
              ? `${where}: "${item.posterPath}" is not in the export.`
              : posterOutcome.ok
                ? `${where}: "${item.posterPath}" is a video, not a picture.`
                : `${where}: ${posterOutcome.message}`,
            postIndex: source.index,
            path: item.posterPath,
          });
        }
      }
      media.push(loaded);
    }
    if (media.length === 0) continue; // skipped; the warnings above say why
    warnings.push(...posterWarnings);

    const user: ImportedUser = { username: source.user.username };
    if (source.user.fullName) user.fullName = source.user.fullName;
    if (source.user.verified !== undefined) user.verified = source.user.verified;
    if (source.user.profileUrl) user.profileUrl = source.user.profileUrl;
    const avatarPath = source.user.avatarPath;
    if (avatarPath) {
      const outcome = avatarJobs.get(avatarPath)?.outcome;
      if (outcome?.ok && outcome.kind === 'image') {
        user.avatarUrl = outcome.url;
      } else if (!reportedAvatars.has(avatarPath)) {
        // Once per avatar file, not once per post of that user.
        reportedAvatars.add(avatarPath);
        warnings.push({
          code: outcome ? (outcome.ok ? 'unsupported-image' : outcome.code) : 'missing-image',
          message: !outcome
            ? `The profile picture "${avatarPath}" of ${source.user.username} is not in the export.`
            : outcome.ok
              ? `Profile picture of ${source.user.username}: "${avatarPath}" is a video, not a picture.`
              : `Profile picture of ${source.user.username}: ${outcome.message}`,
          path: avatarPath,
        });
      }
    }

    const post: ImportedPost = {
      id: source.id,
      media,
      user,
      caption: source.caption,
      commentCount: source.commentCount,
      likeCount: source.likeCount,
      mediaType: finalMediaType(source.mediaType, media),
      comments: source.comments,
    };
    if (source.permalink) post.permalink = source.permalink;
    if (source.takenAt) post.takenAt = source.takenAt;
    if (source.alt) post.alt = source.alt;
    if (source.location) post.location = source.location;
    posts.push(post);
  }

  if (posts.length === 0) {
    // The earliest entry of post.json that failed, with the problem that made it fail.
    const reason = skipReasons.reduce<ImportWarning | undefined>(
      (first, warning) => (first && (first.postIndex ?? 0) <= (warning.postIndex ?? 0) ? first : warning),
      undefined,
    );
    const summary =
      manifest.totalEntries === 1
        ? 'The only post in post.json could not be imported.'
        : `None of the ${manifest.totalEntries} posts in post.json could be imported.`;
    throw new ImportError('no-posts', `${summary}${reason ? ` First problem: ${reason.message}` : ''}`);
  }

  // Export-level warnings first, then per post in manifest order.
  const order = (warning: ImportWarning) => warning.postIndex ?? -1;
  warnings.sort((a, b) => order(a) - order(b));

  const result: ImportResult = {
    sourceName: ctx.sourceName,
    sourceKind: ctx.sourceKind,
    posts,
    warnings,
    totalEntries: manifest.totalEntries,
  };
  if (manifest.exportedAt) result.exportedAt = manifest.exportedAt;
  if (manifest.sourceUrl) result.sourceUrl = manifest.sourceUrl;

  // A URL that the result does not reference (the avatar of a skipped post, the poster of a
  // picture) would never be released by releaseImport: revoke it now.
  const referenced = new Set<string>();
  for (const post of posts) {
    for (const item of post.media) {
      referenced.add(item.url);
      if (item.posterUrl) referenced.add(item.posterUrl);
    }
    if (post.user.avatarUrl) referenced.add(post.user.avatarUrl);
  }
  ctx.createdUrls = ctx.createdUrls.filter((url) => referenced.has(url) || !revokeBlobUrl(url));

  progress('done', total, total, `Imported ${posts.length} post${posts.length === 1 ? '' : 's'}`);
  return result;
}

async function run(
  sourceName: string,
  sourceKind: ImportSourceKind,
  options: CoreOptions,
  open: (ctx: Context) => Promise<SourceEntry[]>,
): Promise<ImportResult> {
  const ctx: Context = {
    sourceName,
    sourceKind,
    limits: { ...DEFAULT_LIMITS, ...options.limits },
    signal: options.signal,
    progress: progressReporter(options),
    budget: options.budget ?? { used: 0 },
    charged: 0,
    createdUrls: [],
  };
  try {
    throwIfAborted(ctx.signal);
    ctx.progress('reading', 0, 1, sourceKind === 'archive' ? `Opening ${sourceName}` : `Reading ${sourceName}`);
    const entries = await open(ctx);
    throwIfAborted(ctx.signal);
    return await buildResult(entries, ctx);
  } catch (err) {
    for (const url of ctx.createdUrls) revokeBlobUrl(url);
    ctx.budget.used -= ctx.charged;
    if (ctx.signal?.aborted) throw abortError();
    if (err instanceof ImportError) throw err;
    throw new ImportError('read-failed', `"${sourceName}" could not be imported: ${describeError(err)}`);
  }
}

/** Import from one archive file. */
export function importArchive(file: File, options: CoreOptions = {}): Promise<ImportResult> {
  return run(file.name || 'archive', 'archive', options, async (ctx) => {
    let lastPercent = -1;
    const entries = await openArchive(file, {
      limits: ctx.limits,
      signal: ctx.signal,
      onProgress: (done, total) => {
        // Byte-accurate, but only reported when the percentage moves.
        const percent = total > 0 ? Math.floor((done / total) * 100) : 100;
        if (percent === lastPercent) return;
        lastPercent = percent;
        ctx.progress('reading', done, total, `Reading ${file.name} (${percent}%)`);
      },
    });
    ctx.progress('reading', 1, 1, `Opened ${file.name}`);
    return entries;
  });
}

/** "dropped-export" for one folder, the file name for one loose file, "Selected files" otherwise. */
export function folderName(files: readonly PathedFile[]): string {
  const roots = new Set<string>();
  for (const { path } of files) {
    const normalized = path.replace(/\\/g, '/').replace(/^\/+/, '');
    const cut = normalized.indexOf('/');
    if (cut <= 0) return files.length === 1 ? normalized : 'Selected files';
    roots.add(normalized.slice(0, cut));
    if (roots.size > 1) return 'Selected files';
  }
  return roots.values().next().value ?? 'Selected files';
}

/**
 * Import from files that are already on hand: a picked or dropped folder, or loose files.
 * (A folder that only holds archives is handled by the caller; see multi.ts.)
 */
export function importPathedFiles(
  files: readonly PathedFile[],
  options: CoreOptions = {},
  sourceName?: string,
): Promise<ImportResult> {
  return run(sourceName ?? folderName(files), 'folder', options, async (ctx) => {
    const entries = entriesFromFiles(files);
    ctx.progress('reading', 1, 1, `Found ${entries.length} file${entries.length === 1 ? '' : 's'}`);
    return entries;
  });
}
