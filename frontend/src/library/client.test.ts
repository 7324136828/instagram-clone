import { describe, expect, it } from 'vitest';
import type { LibraryResponse, LibraryUploadPlan } from '../import';
import { createLibraryClient, LibraryError, type LibraryTransport, type UploadRequest } from './client';

const LIBRARY: LibraryResponse = {
  directory: 'C:\\Temp\\instagram_cache',
  revision: 2,
  updated_at: '2026-11-10T09:30:00Z',
  imports: [],
  posts: [],
};

interface Call {
  method: string;
  url: string;
  body?: unknown;
}

type Reply = Response | (() => Response) | Error;

/** fetch + XMLHttpRequest stand-ins that record every request. */
function fakeTransport(replies: Record<string, Reply>, putStatus: (name: string) => number = () => 200) {
  const calls: Call[] = [];
  const puts: Array<{ url: string; type: string; size: number; progress: number[] }> = [];
  let pendingPut: FakeUpload | null = null;

  class FakeUpload implements UploadRequest {
    status = 0;
    responseText = '';
    upload: UploadRequest['upload'] = { onprogress: null };
    onload: UploadRequest['onload'] = null;
    onerror: UploadRequest['onerror'] = null;
    onabort: UploadRequest['onabort'] = null;
    private url = '';
    private type = '';
    open(method: string, url: string) {
      this.url = url;
      calls.push({ method, url });
    }
    setRequestHeader(name: string, value: string) {
      if (name === 'Content-Type') this.type = value;
    }
    send(body: Blob) {
      const record = { url: this.url, type: this.type, size: body.size, progress: [] as number[] };
      puts.push(record);
      const name = decodeURIComponent(this.url.slice(this.url.lastIndexOf('/') + 1));
      this.status = putStatus(name);
      if (this.status === 0) {
        // Held: the test aborts it.
        pendingPut = this;
        return;
      }
      queueMicrotask(() => {
        this.upload.onprogress?.({ loaded: Math.floor(body.size / 2) } as ProgressEvent);
        if (this.status >= 400) this.responseText = JSON.stringify({ detail: `Rejected ${name}` });
        this.onload?.(new Event('load') as ProgressEvent);
      });
    }
    abort() {
      queueMicrotask(() => this.onabort?.(new Event('abort') as ProgressEvent));
    }
  }

  const transport: LibraryTransport = {
    fetch: async (url, init = {}) => {
      const method = init.method ?? 'GET';
      calls.push({ method, url, body: typeof init.body === 'string' ? JSON.parse(init.body) : undefined });
      if (init.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
      const reply = replies[`${method} ${url}`];
      if (reply === undefined) return new Response(null, { status: method === 'DELETE' ? 204 : 404 });
      if (reply instanceof Error) throw reply;
      return typeof reply === 'function' ? reply() : reply.clone();
    },
    createUpload: () => new FakeUpload(),
  };
  return { transport, calls, puts, pending: () => pendingPut };
}

const json = (value: unknown, status = 200) => () => new Response(JSON.stringify(value), {
  status,
  headers: { 'Content-Type': 'application/json' },
});

function plan(): LibraryUploadPlan {
  return {
    files: [
      { name: 'm000001.jpg', blob: new Blob([new Uint8Array(10)], { type: 'image/jpeg' }), mime: 'image/jpeg' },
      { name: 'm000002.mp4', blob: new Blob([new Uint8Array(30)], { type: 'video/mp4' }), mime: 'video/mp4' },
    ],
    commit: { source_name: 'a.zip', source_kind: 'archive', posts: [] },
  };
}

const UPLOAD = '/api/library/uploads/abc123';

describe('reading the library', () => {
  it('returns the library JSON', async () => {
    const { transport, calls } = fakeTransport({ 'GET /api/library': json(LIBRARY) });
    await expect(createLibraryClient(transport).load()).resolves.toEqual(LIBRARY);
    expect(calls).toEqual([{ method: 'GET', url: '/api/library', body: undefined }]);
  });

  it.each([
    ['a 503 with the reason', json({ detail: 'The cache folder is not writable' }, 503), 'The library could not be opened: The cache folder is not writable.'],
    ['a 404 (an older backend)', json({ detail: 'Not Found' }, 404), 'The library could not be opened: Not Found.'],
    ['a 507 without a reason', () => new Response('', { status: 507 }), 'The library could not be opened: there is not enough free disk space on the computer running the backend.'],
    ['a 429 without a reason', () => new Response('', { status: 429 }), 'The library could not be opened: too many imports are being saved at once; try again in a moment.'],
    ['a 403 without a reason', () => new Response('', { status: 403 }), "The library could not be opened: the library only accepts changes from this app's own pages."],
    ['an HTML page', () => new Response('<html></html>', { status: 200 }), 'The library could not be opened: the server sent an unreadable answer.'],
    ['JSON of another shape', json({ posts: 'none' }), 'The library could not be opened: the server sent an unexpected answer.'],
    ['a network failure', new TypeError('Failed to fetch'), 'The library could not be opened: the server could not be reached.'],
  ])('fails with a readable LibraryError on %s', async (_, reply, message) => {
    const { transport } = fakeTransport({ 'GET /api/library': reply });
    const error = await createLibraryClient(transport).load().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(LibraryError);
    expect((error as Error).message).toBe(message);
  });
});

describe('saving an import in the library', () => {
  it('opens a session, sends every file with its type and progress, then commits', async () => {
    const { transport, calls, puts } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
      [`POST ${UPLOAD}/commit`]: json({ ...LIBRARY, revision: 3 }),
    });
    const progress: Array<[number, number, number, number]> = [];
    const library = await createLibraryClient(transport).upload(plan(), {
      onProgress: (p) => progress.push([p.loaded, p.total, p.file, p.files]),
    });

    expect(library.revision).toBe(3);
    expect(calls.map((call) => `${call.method} ${call.url}`)).toEqual([
      'POST /api/library/uploads',
      `PUT ${UPLOAD}/files/m000001.jpg`,
      `PUT ${UPLOAD}/files/m000002.mp4`,
      `POST ${UPLOAD}/commit`,
    ]);
    expect(puts.map((put) => [put.type, put.size])).toEqual([['image/jpeg', 10], ['video/mp4', 30]]);
    expect(calls[3].body).toEqual(plan().commit);
    expect(progress).toEqual([
      [0, 40, 1, 2], [5, 40, 1, 2], [10, 40, 1, 2],
      [10, 40, 2, 2], [25, 40, 2, 2], [40, 40, 2, 2],
    ]);
  });

  it('drops the session and reports the server reason when a file is refused', async () => {
    const { transport, calls } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
    }, (name) => (name === 'm000002.mp4' ? 413 : 200));
    const error = await createLibraryClient(transport).upload(plan()).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(LibraryError);
    expect((error as LibraryError).message).toBe('"m000002.mp4" was not accepted: Rejected m000002.mp4.');
    expect((error as LibraryError).status).toBe(413);
    expect(calls.at(-1)).toMatchObject({ method: 'DELETE', url: UPLOAD });
    expect(calls.some((call) => call.url.endsWith('/commit'))).toBe(false);
  });

  it('drops the session when the commit fails', async () => {
    const { transport, calls } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
      [`POST ${UPLOAD}/commit`]: json({ detail: 'Unknown staged file m000009.jpg' }, 422),
    });
    await expect(createLibraryClient(transport).upload(plan())).rejects.toThrow(
      'Saving to the library failed: Unknown staged file m000009.jpg.',
    );
    expect(calls.at(-1)).toMatchObject({ method: 'DELETE', url: UPLOAD });
  });

  it('aborts the running upload and drops the session when cancelled', async () => {
    const { transport, calls, pending } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
    }, () => 0);
    const controller = new AbortController();
    const running = createLibraryClient(transport).upload(plan(), { signal: controller.signal });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(pending()).not.toBeNull();
    controller.abort();
    const error = await running.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(LibraryError);
    expect((error as LibraryError).aborted).toBe(true);
    expect(calls.at(-1)).toMatchObject({ method: 'DELETE', url: UPLOAD });
  });

  it('cannot be cancelled once the commit is sent: it resolves with the library the server answers', async () => {
    let answerCommit: () => void = () => undefined;
    const committed = new Promise<void>((resolve) => { answerCommit = resolve; });
    const { transport, calls } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
    });
    const fetchBefore = transport.fetch;
    let commitSignal: AbortSignal | null | undefined;
    transport.fetch = async (url, init = {}) => {
      if (url.endsWith('/commit')) {
        commitSignal = init.signal;
        calls.push({ method: 'POST', url });
        await committed;
        return json({ ...LIBRARY, revision: 7 })();
      }
      return fetchBefore(url, init);
    };
    const controller = new AbortController();
    const events: string[] = [];
    const running = createLibraryClient(transport).upload(plan(), {
      signal: controller.signal,
      onCommit: () => events.push('commit'),
    });
    await expect.poll(() => calls.some((call) => call.url.endsWith('/commit'))).toBe(true);
    expect(events).toEqual(['commit']);
    expect(commitSignal ?? null).toBeNull();
    // Cancel while the server is merging: too late, the import is saved.
    controller.abort();
    answerCommit();
    await expect(running).resolves.toMatchObject({ revision: 7 });
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
  });

  it('does not commit when cancelled after the last file was sent', async () => {
    const controller = new AbortController();
    const { transport, calls } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: 'abc123' }, 201),
    });
    let onCommit = false;
    const running = createLibraryClient(transport).upload(plan(), {
      signal: controller.signal,
      onProgress: (p) => { if (p.loaded === p.total) controller.abort(); },
      onCommit: () => { onCommit = true; },
    });
    const error = await running.catch((e: unknown) => e);
    expect((error as LibraryError).aborted).toBe(true);
    expect(onCommit).toBe(false);
    expect(calls.some((call) => call.url.endsWith('/commit'))).toBe(false);
    expect(calls.at(-1)).toMatchObject({ method: 'DELETE', url: UPLOAD });
  });

  it('refuses an upload id that is not a plain token', async () => {
    const { transport, calls } = fakeTransport({
      'POST /api/library/uploads': json({ upload_id: '../../library' }, 201),
    });
    await expect(createLibraryClient(transport).upload(plan())).rejects.toThrow('unexpected answer');
    expect(calls).toHaveLength(1);
  });

  it('says so when the server cannot be reached at all', async () => {
    const { transport } = fakeTransport({ 'POST /api/library/uploads': new TypeError('Failed to fetch') });
    await expect(createLibraryClient(transport).upload(plan())).rejects.toThrow(
      'Saving to the library failed: the server could not be reached.',
    );
  });
});

describe('clearing the library', () => {
  it('sends DELETE /api/library', async () => {
    const { transport, calls } = fakeTransport({ 'DELETE /api/library': () => new Response(null, { status: 204 }) });
    await createLibraryClient(transport).clear();
    expect(calls).toEqual([{ method: 'DELETE', url: '/api/library', body: undefined }]);
  });

  it('reports why it could not be cleared', async () => {
    const { transport } = fakeTransport({ 'DELETE /api/library': json({ detail: 'The cache folder is in use' }, 409) });
    await expect(createLibraryClient(transport).clear()).rejects.toThrow(
      'The library could not be cleared: The cache folder is in use.',
    );
  });
});

describe('editing a post description', () => {
  it('sends the caption with PATCH and returns the library', async () => {
    const edited = { ...LIBRARY, revision: 3 };
    const { transport, calls } = fakeTransport({ 'PATCH /api/library/posts/C9a.K_q-2': json(edited) });
    const library = await createLibraryClient(transport).updateCaption('C9a.K_q-2', 'New text 🍋');
    expect(library.revision).toBe(3);
    expect(calls).toEqual([{ method: 'PATCH', url: '/api/library/posts/C9a.K_q-2', body: { caption: 'New text 🍋' } }]);
  });

  it('encodes the post id into one path segment', async () => {
    const { transport, calls } = fakeTransport({ 'PATCH /api/library/posts/a%2Fb%3F': json(LIBRARY) });
    await createLibraryClient(transport).updateCaption('a/b?', '');
    expect(calls[0].url).toBe('/api/library/posts/a%2Fb%3F');
  });

  it('reports why the description was not saved', async () => {
    const { transport } = fakeTransport({
      'PATCH /api/library/posts/a': json({ detail: 'The library has no post with this id.' }, 404),
    });
    await expect(createLibraryClient(transport).updateCaption('a', 'x')).rejects.toThrow(
      'The description could not be saved: The library has no post with this id.',
    );
  });
});
