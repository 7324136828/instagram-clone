/**
 * Drag and drop support. DataTransfer items are only valid while the drop handler runs, so
 * capture() must be called synchronously; the (asynchronous) directory walk happens later.
 */
import { isNoisePath } from './paths';
import type { PathedFile } from './source';
import { ImportError } from './types';
import { throwIfAborted } from './util';

const MAX_DEPTH = 32;

export interface CapturedDrop {
  /** File-system entries (Chromium, Firefox, Safari), which is the only way to see folders. */
  entries: FileSystemEntry[];
  /** Plain files, for items without an entry and for browsers without the entries API. */
  files: File[];
}

/** Synchronously takes everything of interest out of a DataTransfer. */
export function captureDrop(dataTransfer: DataTransfer): CapturedDrop {
  const captured: CapturedDrop = { entries: [], files: [] };
  const items = dataTransfer.items;
  if (items && items.length > 0) {
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.kind !== 'file') continue;
      let entry: FileSystemEntry | null = null;
      try {
        entry = typeof item.webkitGetAsEntry === 'function' ? item.webkitGetAsEntry() : null;
      } catch {
        entry = null;
      }
      // A top-level file is taken as a File right away; only folders need the entry.
      if (entry && entry.isDirectory) {
        captured.entries.push(entry);
        continue;
      }
      const file = item.getAsFile();
      if (file) captured.files.push(file);
      else if (entry) captured.entries.push(entry);
    }
  }
  if (captured.entries.length === 0 && captured.files.length === 0 && dataTransfer.files) {
    for (let i = 0; i < dataTransfer.files.length; i++) captured.files.push(dataTransfer.files[i]);
  }
  return captured;
}

function readBatch(reader: FileSystemDirectoryReader): Promise<FileSystemEntry[]> {
  return new Promise((resolve, reject) => reader.readEntries(resolve, reject));
}

function entryFile(entry: FileSystemFileEntry): Promise<File> {
  return new Promise((resolve, reject) => entry.file(resolve, reject));
}

interface WalkState {
  out: PathedFile[];
  maxFiles: number;
  signal: AbortSignal | undefined;
}

async function walk(entry: FileSystemEntry, path: string, depth: number, state: WalkState): Promise<void> {
  throwIfAborted(state.signal);
  if (isNoisePath(path)) return;
  if (entry.isFile) {
    try {
      state.out.push({ path, file: await entryFile(entry as FileSystemFileEntry) });
    } catch {
      // Unreadable file: treated like a file that is not there.
      return;
    }
    if (state.out.length > state.maxFiles) {
      throw new ImportError('too-large', `The dropped folder contains more than ${state.maxFiles} files.`);
    }
    return;
  }
  if (!entry.isDirectory || depth >= MAX_DEPTH) return;
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  // readEntries returns the listing in batches (100 at a time in Chromium) and signals the
  // end with an empty batch.
  for (;;) {
    throwIfAborted(state.signal);
    let batch: FileSystemEntry[];
    try {
      batch = await readBatch(reader);
    } catch {
      break;
    }
    if (batch.length === 0) break;
    for (const child of batch) await walk(child, `${path}/${child.name}`, depth + 1, state);
  }
}

export interface DroppedFiles {
  files: PathedFile[];
  /** Name of the dropped folder when exactly one folder (and nothing else) was dropped. */
  folderName?: string;
}

/** Expands a captured drop into a flat list of files with their relative paths. */
export async function collectDroppedFiles(
  captured: CapturedDrop,
  maxFiles: number,
  signal?: AbortSignal,
): Promise<DroppedFiles> {
  const state: WalkState = { out: [], maxFiles, signal };
  for (const file of captured.files) state.out.push({ path: file.name, file });
  for (const entry of captured.entries) await walk(entry, entry.name, 0, state);
  const single = captured.files.length === 0 && captured.entries.length === 1 ? captured.entries[0] : undefined;
  return single?.isDirectory ? { files: state.out, folderName: single.name } : { files: state.out };
}
