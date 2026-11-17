/**
 * Import of an Instagram export ("instagram-export" format v1, see /instagram.md) from one or
 * several archive files or folders. Parsing runs in the browser; library.ts prepares the
 * upload of a parsed import to the backend library (%TEMP%/instagram_cache) and turns the
 * library back into an ImportResult.
 *
 * Module layout:
 *   types.ts     public contract            limits.ts   safety limits
 *   core.ts      the pipeline (one source)  manifest.ts post.json parsing
 *   multi.ts     several sources            merge.ts    merging results
 *   archive.ts   container detection        zip.ts / tar.ts  archive readers
 *   source.ts    file lookup                paths.ts    path normalisation
 *   sniff.ts     image/video magic bytes    heic.ts     HEIC/HEIF conversion
 *   urls.ts      Instagram link sanitising  registry.ts object URL bookkeeping
 *   dnd.ts       drag and drop              persist.ts  IndexedDB persistence
 *   library.ts   backend library: upload plan, library JSON -> result
 *   export.ts    posts -> "instagram-export" archive(s) for download
 */
import { looksLikeArchive } from './archive';
import { importArchive } from './core';
import { captureDrop, collectDroppedFiles, type CapturedDrop } from './dnd';
import { DEFAULT_LIMITS } from './limits';
import {
  archiveSource,
  droppedFolderSource,
  importMany as importInputs,
  importPathed,
  importSources,
  type ImportInput,
  type Source,
} from './multi';
import { manifestRank } from './paths';
import { collectResultUrls, revokeBlobUrl } from './registry';
import type { PathedFile } from './source';
import { ImportError, type ImportOptions, type ImportResult } from './types';
import { abortError, describeError } from './util';

export * from './types';
export {
  HEIC_CONCURRENCY,
  IMAGE_CONCURRENCY,
  MAX_CAPTION_CHARS,
  MAX_COMMENTS_PER_POST,
  MAX_COMMENT_CHARS,
  MAX_IMAGES_PER_POST,
  MAX_IMAGE_BYTES,
  MAX_MANIFEST_BYTES,
  MAX_POSTS,
  MAX_SOURCES,
  MAX_SOURCE_ENTRIES,
  MAX_TOTAL_IMAGE_BYTES,
  MAX_TOTAL_MEDIA_BYTES,
  MAX_USERNAME_CHARS,
  MAX_VIDEO_BYTES,
  LIBRARY_COMMIT_LIMITS,
  type LibraryCommitLimits,
} from './limits';
export { setHeicConverter, setHeicNativeProbe, type HeicConverter, type HeicNativeProbe } from './heic';
export { parseCount, parseTimestamp } from './manifest';
export {
  sniffImage,
  sniffMedia,
  type ImageFormat,
  type SniffedImage,
  type SniffedMedia,
  type SniffedVideo,
  type VideoFormat,
} from './sniff';
export {
  instagramPostUrl,
  instagramProfileUrl,
  isInstagramUrl,
  sanitizeInstagramUrl,
  sanitizePostUrl,
  sanitizeProfileUrl,
  sanitizeSourceUrl,
} from './urls';
export { IMPORT_DB_NAME, clearSavedImport, loadSavedImport, saveImport } from './persist';
export { mergeImportResults } from './merge';
export {
  EXPORT_PART_BYTES,
  EXPORT_PART_POSTS,
  exportPosts,
  loadExportFile,
  type ExportArchive,
  type ExportManifest,
  type ExportOptions,
  type ExportPostEntry,
  type ExportProgress,
  type ExportResult,
} from './export';
export type { ImportInput } from './multi';
export {
  libraryFileUrl,
  prepareLibraryUpload,
  prepareLibraryUploads,
  resultFromLibrary,
  type LibraryCommitBody,
  type LibraryCommitPost,
  type LibraryImport,
  type LibraryMediaEntry,
  type LibraryPostEntry,
  type LibraryResponse,
  type LibraryUploadFile,
  type LibraryUploadPlan,
} from './library';

/**
 * Value for the `accept` attribute of the archive file input. Browsers match extension tokens
 * against the last extension only, so `.gz` is what makes `name.tar.gz` selectable everywhere.
 */
export const ARCHIVE_ACCEPT =
  '.zip,.tar,.tgz,.tar.gz,.gz,application/zip,application/x-zip-compressed,application/x-tar,application/gzip,application/x-gzip';

/** True when the file looks like a supported archive (by name or MIME type). */
export function isArchiveFile(file: File): boolean {
  return looksLikeArchive(file.name ?? '', file.type ?? '');
}

/** Import from one archive file (.zip, .tar, .tar.gz/.tgz). */
export async function importFromArchive(file: File, options?: ImportOptions): Promise<ImportResult> {
  return importArchive(file, options);
}

/**
 * Import from a list of files: a folder picked with <input webkitdirectory> (each File carries
 * webkitRelativePath), loose files (post.json + images), one archive file, or several archive
 * files (imported one by one and merged, see importMany).
 */
export async function importFromFiles(
  files: Iterable<File> | ArrayLike<File>,
  options?: ImportOptions,
): Promise<ImportResult> {
  let list: File[];
  try {
    list = Array.from(files).filter((file) => file instanceof Blob);
  } catch {
    list = [];
  }
  if (list.length === 0) throw new ImportError('no-manifest', 'No files were selected.');
  // One file that did not come out of a folder picker can only be an archive.
  if (list.length === 1 && (isArchiveFile(list[0]) || !list[0].webkitRelativePath)) {
    if (list[0].name.toLowerCase().endsWith('.json')) {
      throw new ImportError(
        'no-posts',
        'post.json was selected on its own. Select the whole export folder (post.json together with the posts folder) or an archive of it.',
      );
    }
    return importArchive(list[0], options);
  }
  const pathed: PathedFile[] = list.map((file) => ({ path: file.webkitRelativePath || file.name, file }));
  return importPathed(pathed, options ?? {});
}

/**
 * Import several archives and/or folders in one go. They are imported one after the other and
 * merged (see mergeImportResults: later sources win on the same post id). A source that cannot
 * be imported becomes an 'invalid-entry' warning naming it, and `sources` lists it with its
 * error; the import only fails when every source fails. Progress messages name the source
 * ("Reading archive 2 of 3: part2.zip"). More than MAX_SOURCES sources: the rest is ignored
 * with a 'truncated' warning.
 */
export function importMany(inputs: readonly ImportInput[], options?: ImportOptions): Promise<ImportResult> {
  return importInputs(inputs, options);
}

async function importCapturedDrop(captured: CapturedDrop, options: ImportOptions): Promise<ImportResult> {
  if (captured.entries.length === 0) {
    if (captured.files.length === 0) {
      throw new ImportError('no-manifest', 'Nothing was dropped. Drop an export folder or an archive file.');
    }
    return importFromFiles(captured.files, options);
  }
  // Several folders and/or archives: each one is an export of its own. Loose files that are
  // not archives only make sense next to a loose post.json (post.json + posts folder).
  const looseManifest = captured.files.some((file) => manifestRank(file.name) >= 0);
  if (!looseManifest && captured.entries.length + captured.files.length > 1) {
    const sources: Source[] = [
      ...captured.files.filter((file) => looksLikeArchive(file.name, file.type)).map(archiveSource),
      ...captured.entries.map(droppedFolderSource),
    ];
    return importSources(sources, options);
  }
  let dropped;
  try {
    dropped = await collectDroppedFiles(captured, DEFAULT_LIMITS.maxSourceEntries, options.signal);
  } catch (err) {
    if (options.signal?.aborted) throw abortError();
    if (err instanceof ImportError) throw err;
    throw new ImportError('read-failed', `The dropped items could not be read: ${describeError(err)}`);
  }
  if (dropped.files.length === 0) {
    throw new ImportError('no-manifest', 'The dropped folder is empty.');
  }
  return importPathed(dropped.files, options, dropped.folderName);
}

/**
 * Import from a drag-and-drop DataTransfer: a dropped archive or folder, loose files, or several
 * archives and folders at once (imported one by one and merged, see importMany).
 * Entries are captured synchronously, so call this directly inside the drop event handler.
 */
export function importFromDataTransfer(
  dataTransfer: DataTransfer,
  options?: ImportOptions,
): Promise<ImportResult> {
  let captured: CapturedDrop;
  try {
    // Must happen before the first await: the items die when the drop handler returns.
    captured = captureDrop(dataTransfer);
  } catch (err) {
    return Promise.reject(new ImportError('read-failed', `The dropped items could not be read: ${describeError(err)}`));
  }
  return importCapturedDrop(captured, options ?? {});
}

/**
 * Revoke every object URL owned by an import result. The result must not be rendered afterwards.
 * Server URLs (a library result) are not object URLs and are left alone.
 */
export function releaseImport(result: ImportResult): void {
  // The registry forgets a URL when it is revoked, so repeated calls are harmless.
  for (const url of collectResultUrls(result)) revokeBlobUrl(url);
}
