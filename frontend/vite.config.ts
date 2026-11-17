import type { ClientRequest, IncomingMessage } from 'node:http';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const backendUrl = process.env.VITE_BACKEND_URL || 'http://127.0.0.1:8000';

/**
 * The backend (backend/app/guard.py) accepts a change (POST, PUT, DELETE) only from the app's own
 * pages. A browser sends no Sec-Fetch-Site to a plain-http LAN address, and the Origin of a page
 * opened at http://192.168.1.20:5173 is not the backend's address. So a request from this dev
 * server's own pages is forwarded the way the packaged frontend server does it: without Origin
 * and Sec-Fetch-Site, and with the browser's address in X-Forwarded-For. A request from another
 * site keeps its Origin, and the backend still refuses it. Vite itself refuses host names it does
 * not know (server.allowedHosts), so a DNS-rebound page never gets this far.
 */
export function forwardAsSameOrigin(proxyReq: ClientRequest, req: IncomingMessage): void {
  const origin = req.headers.origin?.toLowerCase();
  const host = req.headers.host?.toLowerCase();
  if (origin && host && (origin === `http://${host}` || origin === `https://${host}`)) {
    proxyReq.removeHeader('origin');
    proxyReq.removeHeader('sec-fetch-site');
  }
  const address = req.socket.remoteAddress;
  if (address) proxyReq.setHeader('x-forwarded-for', address.replace(/^::ffff:/i, ''));
}

export default defineConfig({
  plugins: [react()],
  server: {
    host: process.env.BACKEND_HOST || '0.0.0.0',
    port: 5173,
    proxy: {
      '/api': {
        target: backendUrl,
        changeOrigin: true,
        configure: (proxy) => {
          proxy.on('proxyReq', forwardAsSameOrigin);
        },
      },
    },
  },
});
