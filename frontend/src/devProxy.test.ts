/// <reference types="node" />
/**
 * The dev server's /api proxy (vite.config.ts) and the backend's write check (backend/app/guard.py):
 * a change from the app's own page, opened at a LAN address (no Sec-Fetch-Site over plain http),
 * must reach the backend, and one from another site must still carry its Origin there.
 */
import type { ClientRequest, IncomingMessage } from 'node:http';
import { describe, expect, it } from 'vitest';
import { forwardAsSameOrigin } from '../vite.config';

function forward(headers: Record<string, string>, remoteAddress = '192.168.1.30') {
  const sent = new Map<string, string>(Object.entries(headers));
  const proxyReq = {
    removeHeader: (name: string) => { sent.delete(name); },
    setHeader: (name: string, value: string) => { sent.set(name, value); },
  } as unknown as ClientRequest;
  const req = { headers, socket: { remoteAddress } } as unknown as IncomingMessage;
  forwardAsSameOrigin(proxyReq, req);
  return Object.fromEntries(sent);
}

describe('the dev server proxy', () => {
  it('forwards a write from its own page on a LAN address without Origin, with the browser address', () => {
    expect(forward({ host: '192.168.1.20:5173', origin: 'http://192.168.1.20:5173' })).toEqual({
      host: '192.168.1.20:5173', 'x-forwarded-for': '192.168.1.30',
    });
    expect(forward({ host: 'localhost:5173', origin: 'http://localhost:5173', 'sec-fetch-site': 'same-origin' }, '::ffff:127.0.0.1'))
      .toEqual({ host: 'localhost:5173', 'x-forwarded-for': '127.0.0.1' });
  });

  it('keeps the Origin of another site, so the backend refuses it', () => {
    expect(forward({ host: '192.168.1.20:5173', origin: 'http://evil.example', 'sec-fetch-site': 'cross-site' })).toEqual({
      host: '192.168.1.20:5173', origin: 'http://evil.example', 'sec-fetch-site': 'cross-site', 'x-forwarded-for': '192.168.1.30',
    });
    expect(forward({ host: '192.168.1.20:5173', origin: 'null' })).toMatchObject({ origin: 'null' });
    expect(forward({ host: '192.168.1.20:5173', origin: 'http://192.168.1.20:5174' })).toMatchObject({ origin: 'http://192.168.1.20:5174' });
  });

  it('replaces an X-Forwarded-For sent by the browser', () => {
    expect(forward({ host: 'h:5173', 'x-forwarded-for': '127.0.0.1' })['x-forwarded-for']).toBe('192.168.1.30');
  });
});
