/**
 * Several archives and/or folders in one import action: each one is imported in turn and the
 * results are merged. A source that cannot be imported becomes a warning; only when every
 * source fails does the import fail.
 */
import { looksLikeArchive } from './archive';
import { folderName, importArchive, importPathedFiles, type CoreOptions } from './core';
import { collectDroppedFiles } from './dnd';
import { DEFAULT_LIMITS, MAX_SOURCES } from './limits';
import { describeSources, mergeResults, sourcesOf } from './merge';
import { manifestRank } from './paths';
import { collectResultUrls, revokeBlobUrl } from './registry';
import type { PathedFile } from './source';
import { ImportError, type ImportResult, type ImportSourceSummary, type ImportWarning } from './types';
import { abortError, describeError, throwIfAborted } from './util';

/** One archive file, or the files of one folder (as a folder picker delivers them). */
export type ImportInput = { kind: 'archive'; file: File } | { kind: 'folder'; files: File[]; name?: string };

/** One archive or folder of an import action. */
export interface Source {
  name: string;
  kind: 'archive' | 'folder';
  run(options: CoreOptions): Promise<ImportResult>;
}

export function archiveSource(file: File): Source {
  return { name: file.name || 'archive', kind: 'archive', run: (options) => importArchive(file, options) };
}

/**
 * Files of a folder, or loose files. A folder without post.json that holds archives (the
 * Downloads folder an export was saved to) imports those archives instead.
 */
export function importPathed(files: readonly PathedFile[], options: CoreOptions, name?: string): Promise<ImportResult> {
  if (!files.some(({ path }) => manifestRank(path) >= 0)) {
    const archives = files.filter(({ file }) => looksLikeArchive(file.name, file.type));
    if (archives.length === 1) return importArchive(archives[0].file, options);
    if (archives.length > 1) return importSources(archives.map(({ file }) => archiveSource(file)), options);
  }
  return importPathedFiles(files, options, name);
}

export function folderSource(files: readonly PathedFile[], name = folderName(files)): Source {
  return { name, kind: 'folder', run: (options) => importPathed(files, options, name) };
}

/** A folder that was dropped: it is only walked when its turn comes. */
export function droppedFolderSource(entry: FileSystemEntry): Source {
  const name = entry.name;
  return {
    name,
    kind: 'folder',
    async run(options) {
      let files: PathedFile[];
      try {
        ({ files } = await collectDroppedFiles(
          { entries: [entry], files: [] },
          options.limits?.maxSourceEntries ?? DEFAULT_LIMITS.maxSourceEntries,
          options.signal,
        ));
      } catch (err) {
        if (options.signal?.aborted) throw abortError();
        if (err instanceof ImportError) throw err;
        throw new ImportError('read-failed', `The dropped folder "${name}" could not be read: ${describeError(err)}`);
      }
      if (files.length === 0) throw new ImportError('no-manifest', `The dropped folder "${name}" is empty.`);
      return importPathed(files, options, name);
    },
  };
}

interface Outcome {
  source: Source;
  result?: ImportResult;
  error?: ImportError;
}

function release(result: ImportResult): void {
  for (const url of collectResultUrls(result)) revokeBlobUrl(url);
}

/**
 * Imports the sources one after the other (so that memory use stays that of one import) and
 * merges the results. Media of all sources share one size budget. A single source is
 * imported exactly as on its own.
 */
export async function importSources(all: readonly Source[], options: CoreOptions = {}): Promise<ImportResult> {
  if (all.length === 0) throw new ImportError('no-manifest', 'Nothing was selected.');
  const sources = all.slice(0, MAX_SOURCES);
  if (all.length === 1) return sources[0].run(options);

  const { signal, onProgress } = options;
  const emit: NonNullable<CoreOptions['onProgress']> = (progress) => {
    try {
      onProgress?.(progress);
    } catch {
      // A broken progress listener must not break the import.
    }
  };
  const budget = options.budget ?? { used: 0 };
  const total = sources.length;
  const outcomes: Outcome[] = [];
  try {
    for (const [position, source] of sources.entries()) {
      throwIfAborted(signal);
      const label = `${source.kind} ${position + 1} of ${total}`;
      emit({ phase: 'reading', done: position, total, message: `Reading ${label}: ${source.name}` });
      try {
        const result = await source.run({
          ...options,
          budget,
          onProgress: (progress) => {
            // The end of one source is not the end of the import.
            if (progress.phase === 'done') return;
            emit({ ...progress, message: `${source.name} (${label}): ${progress.message}` });
          },
        });
        outcomes.push({ source, result });
      } catch (err) {
        if (signal?.aborted || (err instanceof ImportError && err.code === 'aborted')) throw abortError();
        const error =
          err instanceof ImportError
            ? err
            : new ImportError('read-failed', `"${source.name}" could not be imported: ${describeError(err)}`);
        outcomes.push({ source, error });
      }
    }
    throwIfAborted(signal);
  } catch (err) {
    // Cancelled: whatever was imported so far is given back.
    for (const { result } of outcomes) if (result) release(result);
    throw err;
  }

  const results = outcomes.flatMap(({ result }) => (result ? [result] : []));
  const failures = outcomes.flatMap(({ source, error }) => (error ? [{ source, error }] : []));
  if (results.length === 0) {
    const reasons = failures.map(({ source, error }) => `${source.name}: ${error.message}`).join(' ');
    throw new ImportError(failures[0].error.code, `None of the ${total} selected archives or folders could be imported. ${reasons}`);
  }

  const merged = mergeResults(results);
  const notes: ImportWarning[] = [];
  if (all.length > MAX_SOURCES) {
    notes.push({
      code: 'truncated',
      message: `${all.length} archives or folders were selected; only the first ${MAX_SOURCES} were imported.`,
    });
  }
  for (const { source, error } of failures) {
    notes.push({ code: 'invalid-entry', message: `"${source.name}" could not be imported: ${error.message}` });
  }
  const summaries: ImportSourceSummary[] = outcomes.flatMap(({ source, result, error }) => {
    if (result) return sourcesOf(result);
    return error ? [{ name: source.name, kind: source.kind, posts: 0, warnings: 0, error: error.message }] : [];
  });
  const imported = merged.posts.length;
  emit({
    phase: 'done',
    done: total,
    total,
    message: `Imported ${imported} post${imported === 1 ? '' : 's'} from ${describeSources(summaries)}`,
  });
  return {
    ...merged,
    sourceName: describeSources(summaries),
    warnings: [...notes, ...merged.warnings],
    sources: summaries,
  };
}

/** Imports several archives and/or folders and merges them (see importSources). */
export function importMany(inputs: readonly ImportInput[], options: CoreOptions = {}): Promise<ImportResult> {
  let sources: Source[];
  try {
    sources = Array.from(inputs, (input) =>
      input.kind === 'archive'
        ? archiveSource(input.file)
        : folderSource(
            Array.from(input.files, (file) => ({ path: file.webkitRelativePath || file.name, file })),
            input.name,
          ),
    );
  } catch {
    sources = [];
  }
  return importSources(sources, options);
}
