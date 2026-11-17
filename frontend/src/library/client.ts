/**
 * Talks to the backend library (%TEMP%/instagram_cache, see backend/app/library.py): reading it,
 * saving a parsed import into it (upload session -> one PUT per file with progress -> commit) and
 * clearing it. Errors are LibraryError with a message that can be shown to the user.
 */
import type { LibraryResponse, LibraryUploadPlan } from '../import';

export interface LibraryUploadProgress {
  /** Bytes sent so far, over all files. */
  loaded: number;
  /** Bytes of all files of the upload. */
  total: number;
  /** One-based number of the file being sent. */
  file: number;
  files: number;
}

export interface LibraryUploadOptions {
  /** Cancels the upload while files are being sent. Once the commit is sent it is ignored (see onCommit). */
  signal?: AbortSignal;
  onProgress?: (progress: LibraryUploadProgress) => void;
  /**
   * Called right before the commit is sent. From then on the upload cannot be cancelled: the server
   * may merge the import whatever the browser does, so the upload resolves with the library it
   * answers (or fails with its error) even when `signal` aborts.
   */
  onCommit?: () => void;
}

export class LibraryError extends Error {
  /** HTTP status of the failed request; undefined for a network failure. */
  readonly status?: number;
  /** The request was aborted by the caller. */
  readonly aborted: boolean;

  constructor(message: string, options: { status?: number; aborted?: boolean } = {}) {
    super(message);
    this.name = 'LibraryError';
    this.status = options.status;
    this.aborted = options.aborted ?? false;
  }
}

/** The subset of XMLHttpRequest the upload uses (so tests can provide their own). */
export interface UploadRequest {
  open(method: string, url: string): void;
  setRequestHeader(name: string, value: string): void;
  send(body: Blob): void;
  abort(): void;
  readonly status: number;
  readonly responseText: string;
  readonly upload: { onprogress: ((event: ProgressEvent) => void) | null };
  onload: ((event: ProgressEvent) => void) | null;
  onerror: ((event: ProgressEvent) => void) | null;
  onabort: ((event: ProgressEvent) => void) | null;
}

export interface LibraryTransport {
  fetch: (input: string, init?: RequestInit) => Promise<Response>;
  /** A new XMLHttpRequest: fetch() cannot report upload progress. */
  createUpload: () => UploadRequest;
}

export interface LibraryClient {
  /** GET /api/library. Throws LibraryError when the library cannot be used (503, network, bad JSON). */
  load(signal?: AbortSignal): Promise<LibraryResponse>;
  /**
   * Saves a parsed import; resolves with the library after the commit. Aborts the session on
   * failure. Cancelling works until the commit is sent (see LibraryUploadOptions.onCommit).
   */
  upload(plan: LibraryUploadPlan, options?: LibraryUploadOptions): Promise<LibraryResponse>;
  /** DELETE /api/library: removes every imported post and file from the cache folder. */
  clear(): Promise<void>;
  /**
   * PATCH /api/library/posts/{id}: replaces the post's description (its caption) in post.json.
   * Resolves with the library after the change.
   */
  updateCaption(postId: string, caption: string): Promise<LibraryResponse>;
}

const API = '/api/library';

function isLibraryResponse(value: unknown): value is LibraryResponse {
  if (typeof value !== 'object' || value === null) return false;
  const library = value as Partial<LibraryResponse>;
  return typeof library.directory === 'string'
    && typeof library.revision === 'number'
    && Array.isArray(library.posts)
    && Array.isArray(library.imports);
}

/** FastAPI errors are `{detail: string}` or `{detail: [{msg}]}` (validation). */
function detailOf(text: string): string | undefined {
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return undefined;
  }
  const detail = typeof body === 'object' && body !== null ? (body as { detail?: unknown }).detail : undefined;
  if (typeof detail === 'string' && detail.trim()) return detail.trim();
  if (Array.isArray(detail)) {
    const messages = detail
      .map((item) => (typeof item === 'object' && item !== null ? (item as { msg?: unknown }).msg : undefined))
      .filter((msg): msg is string => typeof msg === 'string' && msg.length > 0);
    if (messages.length > 0) return messages.join('; ');
  }
  return undefined;
}

function statusError(status: number, text: string, what: string): LibraryError {
  const detail = detailOf(text);
  const reason = detail
    ?? (status === 413 ? 'the files are larger than the library accepts'
      : status === 415 ? 'a file has a type the library does not store'
        : status === 503 ? 'the library folder cannot be used'
          : status === 507 ? 'there is not enough free disk space on the computer running the backend'
            : status === 429 ? 'too many imports are being saved at once; try again in a moment'
              : status === 403 ? "the library only accepts changes from this app's own pages"
                : `the server answered ${status}`);
  return new LibraryError(`${what}: ${reason.replace(/[.\s]+$/, '')}.`, { status });
}

function abortedError(): LibraryError {
  return new LibraryError('Saving to the library was cancelled.', { aborted: true });
}

function networkError(what: string, error: unknown): LibraryError {
  if (error instanceof DOMException && error.name === 'AbortError') return abortedError();
  return new LibraryError(`${what}: the server could not be reached.`);
}

async function request(
  transport: LibraryTransport,
  what: string,
  url: string,
  init: RequestInit,
): Promise<Response> {
  let response: Response;
  try {
    response = await transport.fetch(url, init);
  } catch (error) {
    throw networkError(what, error);
  }
  if (!response.ok) throw statusError(response.status, await response.text().catch(() => ''), what);
  return response;
}

async function libraryFrom(response: Response, what: string): Promise<LibraryResponse> {
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    throw new LibraryError(`${what}: the server sent an unreadable answer.`, { status: response.status });
  }
  if (!isLibraryResponse(body)) {
    throw new LibraryError(`${what}: the server sent an unexpected answer.`, { status: response.status });
  }
  return body;
}

function putFile(
  transport: LibraryTransport,
  url: string,
  file: LibraryUploadPlan['files'][number],
  signal: AbortSignal | undefined,
  onProgress: (loaded: number) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(abortedError());
      return;
    }
    const xhr = transport.createUpload();
    const onAbort = () => xhr.abort();
    const done = (error?: LibraryError) => {
      signal?.removeEventListener('abort', onAbort);
      if (error) reject(error);
      else resolve();
    };
    xhr.upload.onprogress = (event) => onProgress(Math.min(event.loaded, file.blob.size));
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        onProgress(file.blob.size);
        done();
      } else {
        done(statusError(xhr.status, xhr.responseText, `"${file.name}" was not accepted`));
      }
    };
    xhr.onerror = () => done(new LibraryError('Saving to the library failed: the connection to the server was lost.'));
    xhr.onabort = () => done(abortedError());
    signal?.addEventListener('abort', onAbort, { once: true });
    xhr.open('PUT', url);
    xhr.setRequestHeader('Content-Type', file.mime || 'application/octet-stream');
    xhr.send(file.blob);
  });
}

export function createLibraryClient(transport: LibraryTransport): LibraryClient {
  const load = async (signal?: AbortSignal): Promise<LibraryResponse> => {
    const what = 'The library could not be opened';
    const response = await request(transport, what, API, { cache: 'no-store', signal });
    return libraryFrom(response, what);
  };

  const dropSession = (session: string) => {
    transport.fetch(session, { method: 'DELETE', keepalive: true }).catch(() => undefined);
  };

  const upload = async (plan: LibraryUploadPlan, options: LibraryUploadOptions = {}): Promise<LibraryResponse> => {
    const { signal, onProgress, onCommit } = options;
    if (signal?.aborted) throw abortedError();
    const what = 'Saving to the library failed';
    const created = await request(transport, what, `${API}/uploads`, { method: 'POST', signal });
    let uploadId: unknown;
    try {
      uploadId = ((await created.json()) as { upload_id?: unknown }).upload_id;
    } catch {
      uploadId = undefined;
    }
    if (typeof uploadId !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(uploadId)) {
      throw new LibraryError(`${what}: the server sent an unexpected answer.`);
    }
    const session = `${API}/uploads/${uploadId}`;

    try {
      const total = plan.files.reduce((sum, file) => sum + file.blob.size, 0);
      let finished = 0;
      for (let i = 0; i < plan.files.length; i++) {
        const file = plan.files[i];
        const report = (loaded: number) => onProgress?.({ loaded: finished + loaded, total, file: i + 1, files: plan.files.length });
        report(0);
        await putFile(transport, `${session}/files/${encodeURIComponent(file.name)}`, file, signal, report);
        finished += file.blob.size;
      }
      if (signal?.aborted) throw abortedError();
    } catch (error) {
      // The staged files are useless now: drop the session. Best effort, and never with the
      // caller's (possibly aborted) signal; the server also removes stale sessions itself.
      dropSession(session);
      if (signal?.aborted) throw abortedError();
      throw error instanceof LibraryError ? error : new LibraryError(`${what}: ${String(error)}`);
    }

    // The commit is not abortable: aborting the request would not undo a merge the server already
    // made, and the user would be told "cancelled" about posts that are in the library.
    onCommit?.();
    let response: Response;
    try {
      response = await request(transport, what, `${session}/commit`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(plan.commit),
      });
    } catch (error) {
      dropSession(session);
      throw error instanceof LibraryError ? error : new LibraryError(`${what}: ${String(error)}`);
    }
    return libraryFrom(response, what);
  };

  const clear = async (): Promise<void> => {
    await request(transport, 'The library could not be cleared', API, { method: 'DELETE' });
  };

  const updateCaption = async (postId: string, caption: string): Promise<LibraryResponse> => {
    const what = 'The description could not be saved';
    const response = await request(transport, what, `${API}/posts/${encodeURIComponent(postId)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ caption }),
    });
    return libraryFrom(response, what);
  };

  return { load, upload, clear, updateCaption };
}

/** The client of the running app: the page's own origin (Vite and the packaged server proxy /api). */
export const libraryClient: LibraryClient = createLibraryClient({
  fetch: (input, init) => fetch(input, init),
  createUpload: () => new XMLHttpRequest(),
});
