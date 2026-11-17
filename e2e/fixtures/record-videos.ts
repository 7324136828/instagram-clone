// The callback passed to page.evaluate() runs in the browser: it needs the DOM types.
/// <reference lib="dom" />
/**
 * Records the small, synthetic test videos used by the import fixtures (see generate-fixtures.py)
 * with nothing but the browser the e2e suite already uses: a canvas animation is captured with
 * MediaRecorder in Microsoft Edge (Playwright, channel "msedge").
 *
 * The output is committed, so the suite needs neither this script nor any video tool. Re-run it
 * only when the videos have to change, then re-run generate-fixtures.py:
 *
 *     cd e2e
 *     node fixtures/record-videos.ts          (Node 23.6+ runs TypeScript directly)
 *
 * Output (fixtures/media/):
 *     clip-orange.webm   VP8 WebM, 2 s, 240 x 300, orange with a moving white disc
 *     clip-teal.webm     VP8 WebM, 3 s, 300 x 240, teal with a moving white disc
 *     clip-violet.mp4    H.264 MP4, 2 s, 240 x 240, violet (only when Edge records MP4)
 *
 * MediaRecorder writes WebM without a duration (the browser would report Infinity until the end
 * was reached), so the Duration element is added to the WebM Segment Info here.
 */
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

interface Clip {
  name: string;
  mime: string;
  width: number;
  height: number;
  seconds: number;
  color: string;
}

const CLIPS: Clip[] = [
  { name: 'clip-orange.webm', mime: 'video/webm;codecs=vp8', width: 240, height: 300, seconds: 2, color: '#f08c1e' },
  { name: 'clip-teal.webm', mime: 'video/webm;codecs=vp8', width: 300, height: 240, seconds: 3, color: '#139a8c' },
  { name: 'clip-violet.mp4', mime: 'video/mp4;codecs=avc1.42E01E', width: 240, height: 240, seconds: 2, color: '#7a3cc8' },
];

const output = join(dirname(fileURLToPath(import.meta.url)), 'media');

// --- WebM: add the Duration element MediaRecorder leaves out ------------------------------------

const SEGMENT = 0x18538067;
const INFO = 0x1549a966;
const TIMECODE_SCALE = 0x2ad7b1;
const DURATION = 0x4489;

/** Reads an EBML variable-length integer; `marker` keeps the length bits (element IDs). */
function readVint(data: Uint8Array, at: number, marker: boolean): { value: number; length: number } {
  const first = data[at];
  let length = 1;
  while (length <= 8 && !(first & (0x80 >> (length - 1)))) length += 1;
  if (length > 8) throw new Error(`Bad EBML length at ${at}`);
  let value = marker ? first : first & (0xff >> length);
  for (let i = 1; i < length; i++) value = value * 256 + data[at + i];
  return { value, length };
}

function sizeVint(size: number): Uint8Array {
  // 8-byte size: always enough, and what MediaRecorder itself writes for big elements.
  const out = new Uint8Array(8);
  out[0] = 0x01;
  let rest = size;
  for (let i = 7; i >= 1; i--) {
    out[i] = rest % 256;
    rest = Math.floor(rest / 256);
  }
  return out;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** Returns the WebM with Segment > Info > Duration set to `seconds` (in TimecodeScale units). */
function withWebmDuration(data: Uint8Array, seconds: number): Uint8Array {
  let at = 0;
  // EBML header, then the Segment (unknown size), whose first children include Info.
  const header = readVint(data, at, true);
  const headerSize = readVint(data, at + header.length, false);
  at += header.length + headerSize.length + headerSize.value;
  const segment = readVint(data, at, true);
  if (segment.value !== SEGMENT) throw new Error('No WebM Segment after the EBML header');
  at += segment.length + readVint(data, at + segment.length, false).length;
  while (at < data.length) {
    const id = readVint(data, at, true);
    const size = readVint(data, at + id.length, false);
    const bodyStart = at + id.length + size.length;
    if (id.value !== INFO) {
      at = bodyStart + size.value;
      continue;
    }
    let scale = 1_000_000;
    let child = bodyStart;
    const kept: Uint8Array[] = [];
    while (child < bodyStart + size.value) {
      const childId = readVint(data, child, true);
      const childSize = readVint(data, child + childId.length, false);
      const end = child + childId.length + childSize.length + childSize.value;
      if (childId.value === TIMECODE_SCALE) {
        scale = 0;
        for (let i = child + childId.length + childSize.length; i < end; i++) scale = scale * 256 + data[i];
      }
      if (childId.value !== DURATION) kept.push(data.subarray(child, end));
      child = end;
    }
    const value = new Uint8Array(8);
    new DataView(value.buffer).setFloat64(0, (seconds * 1e9) / scale);
    const duration = concat(new Uint8Array([0x44, 0x89, 0x88]), value);
    const body = concat(...kept, duration);
    const info = concat(new Uint8Array([0x15, 0x49, 0xa9, 0x66]), sizeVint(body.length), body);
    return concat(data.subarray(0, at), info, data.subarray(bodyStart + size.value));
  }
  throw new Error('No Info element in the WebM Segment');
}

// --- Recording -----------------------------------------------------------------------------------

async function main(): Promise<void> {
  mkdirSync(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const page = await browser.newPage();
    await page.setContent('<!doctype html><title>record</title>');
    for (const clip of CLIPS) {
      const supported = await page.evaluate((mime) => MediaRecorder.isTypeSupported(mime), clip.mime);
      if (!supported) {
        console.log(`skipped ${clip.name}: this Edge cannot record ${clip.mime}`);
        continue;
      }
      const base64 = await page.evaluate(async (c) => {
        const canvas = document.createElement('canvas');
        canvas.width = c.width;
        canvas.height = c.height;
        document.body.append(canvas);
        const context = canvas.getContext('2d');
        if (!context) throw new Error('No 2D canvas');
        let started = performance.now();
        const paint = (t: number) => {
          context.fillStyle = c.color;
          context.fillRect(0, 0, c.width, c.height);
          const radius = Math.min(c.width, c.height) / 6;
          const x = radius + ((c.width - 2 * radius) * (t / c.seconds));
          context.fillStyle = '#ffffff';
          context.beginPath();
          context.arc(x, c.height / 2, radius, 0, Math.PI * 2);
          context.fill();
        };
        // The first captured frame must already be painted (the first frame is the grid preview).
        paint(0);
        await new Promise((resolve) => requestAnimationFrame(resolve));
        const stream = canvas.captureStream(30);
        const recorder = new MediaRecorder(stream, { mimeType: c.mime, videoBitsPerSecond: 250_000 });
        const chunks: Blob[] = [];
        recorder.ondataavailable = (event) => { if (event.data.size > 0) chunks.push(event.data); };
        const stopped = new Promise<void>((resolve) => { recorder.onstop = () => resolve(); });
        const draw = () => {
          const t = (performance.now() - started) / 1000;
          paint(Math.min(t, c.seconds));
          if (t < c.seconds) requestAnimationFrame(draw);
          else recorder.stop();
        };
        recorder.start(250);
        started = performance.now();
        draw();
        await stopped;
        stream.getTracks().forEach((track) => track.stop());
        canvas.remove();
        const bytes = new Uint8Array(await new Blob(chunks).arrayBuffer());
        let binary = '';
        for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
        return btoa(binary);
      }, clip);
      let data: Uint8Array = new Uint8Array(Buffer.from(base64, 'base64'));
      if (clip.name.endsWith('.webm')) data = withWebmDuration(data, clip.seconds);
      writeFileSync(join(output, clip.name), data);
      console.log(`wrote ${clip.name} (${data.length} bytes)`);
    }
  } finally {
    await browser.close();
  }
}

await main();
