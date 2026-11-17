import { ImportError } from './types';

export function abortError(): ImportError {
  return new ImportError('aborted', 'The import was cancelled.');
}

export function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted) throw abortError();
}

export function describeError(err: unknown): string {
  const text = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
  // Some libraries (the HEIC decoder) reject with a pre-formatted "Error: ..." string.
  return text.replace(/^(?:Uncaught\s+)?Error:\s*/i, '').trim() || 'unknown error';
}

/** fflate and our readers hand out plain ArrayBuffer-backed views; this only narrows the type. */
export function asBlobPart(bytes: Uint8Array): BlobPart {
  return bytes as Uint8Array<ArrayBuffer>;
}

/** Decompressed bytes are handed to the browser's Blob storage in steps of this size. */
const BLOB_FLUSH_BYTES = 16 * 1024 * 1024;

/**
 * Collects decompressed chunks into one Blob. Every `flushBytes` the chunks gathered so far
 * become a Blob of their own, so a member of hundreds of megabytes (a video) does not sit in
 * the JavaScript heap until it is complete: the browser may keep Blob data elsewhere (on disk).
 */
export class BlobCollector {
  private readonly blobs: Blob[] = [];
  private parts: BlobPart[] = [];
  private pending = 0;
  private readonly flushBytes: number;

  constructor(flushBytes = BLOB_FLUSH_BYTES) {
    this.flushBytes = flushBytes;
  }

  add(bytes: Uint8Array): void {
    if (bytes.length === 0) return;
    this.parts.push(asBlobPart(bytes));
    this.pending += bytes.length;
    if (this.pending >= this.flushBytes) {
      this.blobs.push(new Blob(this.parts));
      this.parts = [];
      this.pending = 0;
    }
  }

  finish(): Blob {
    return new Blob([...this.blobs, ...this.parts]);
  }
}

/** Reads a byte range of a blob. Failures (file moved, permission lost) become ImportError. */
export async function readBytes(blob: Blob, start = 0, end = blob.size): Promise<Uint8Array> {
  try {
    const slice = start === 0 && end >= blob.size ? blob : blob.slice(start, end);
    return new Uint8Array(await slice.arrayBuffer());
  } catch (err) {
    throw new ImportError('read-failed', `The file could not be read (${describeError(err)}).`);
  }
}

/** Decodes JSON text: UTF-8 (with or without BOM) and, leniently, UTF-16 with a BOM. */
export function decodeText(bytes: Uint8Array): string {
  let encoding = 'utf-8';
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) encoding = 'utf-16le';
  else if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) encoding = 'utf-16be';
  try {
    return new TextDecoder(encoding).decode(bytes);
  } catch {
    return new TextDecoder().decode(bytes);
  }
}

/** Decodes an archive member name: UTF-8 when valid, otherwise Latin-1 so nothing is lost. */
export function decodeName(bytes: Uint8Array): string {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    let out = '';
    for (let i = 0; i < bytes.length; i++) out += String.fromCharCode(bytes[i]);
    return out;
  }
}

/** Code page 437, bytes 0x80 to 0xFF. */
const CP437_HIGH =
  'ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐' +
  '└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u00a0';

/**
 * Decodes the name of a zip member. Modern tools write UTF-8 (with or without saying so);
 * names that are not valid UTF-8 are in the DOS code page, which is what the zip format
 * prescribes and what 7-Zip and older Windows tools write ("é" as the single byte 0x82).
 */
export function decodeZipName(bytes: Uint8Array): string {
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    let out = '';
    for (let i = 0; i < bytes.length; i++) {
      out += bytes[i] < 0x80 ? String.fromCharCode(bytes[i]) : CP437_HIGH[bytes[i] - 0x80];
    }
    return out;
  }
}

/** Hands control back to the event loop, so that input, timers and rendering get their turn. */
export function nextTask(): Promise<void> {
  return new Promise<void>((resolve) => setTimeout(resolve, 0));
}

/** Runs `worker` over `items` with bounded concurrency; the first failure stops the pool. */
export async function runPool<T>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T) => Promise<void>,
): Promise<void> {
  let next = 0;
  let failed = false;
  let failure: unknown;
  const runners = Array.from({ length: Math.max(1, Math.min(concurrency, items.length)) }, async () => {
    while (!failed && next < items.length) {
      const item = items[next++];
      try {
        await worker(item);
      } catch (err) {
        if (!failed) {
          failed = true;
          failure = err;
        }
      }
    }
  });
  await Promise.all(runners);
  if (failed) throw failure;
}

export class Semaphore {
  private active = 0;
  private readonly waiters: Array<() => void> = [];
  private readonly max: number;

  constructor(max: number) {
    this.max = Math.max(1, max);
  }

  async run<T>(task: () => Promise<T>): Promise<T> {
    if (this.active < this.max) this.active++;
    else await new Promise<void>((resolve) => this.waiters.push(resolve));
    try {
      return await task();
    } finally {
      // Hand the slot straight to the next waiter so the count can never overshoot.
      const next = this.waiters.shift();
      if (next) next();
      else this.active--;
    }
  }
}

/** Rejects with ImportError('aborted') as soon as the signal fires, without leaking listeners. */
export function raceAbort<T>(promise: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) return Promise.reject(abortError());
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortError());
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (err) => {
        signal.removeEventListener('abort', onAbort);
        reject(err);
      },
    );
  });
}

export function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      },
    );
  });
}
