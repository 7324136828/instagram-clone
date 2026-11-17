/**
 * HEIC/HEIF handling ("Apple image format").
 *
 * Browsers that decode HEIC themselves (Safari) keep the original bytes. Everywhere else the
 * image is converted to JPEG by the `heic-to` package (libheif compiled to JavaScript/WASM,
 * running in its own worker). The package is ~3 MB, so it is imported lazily and only when
 * an export actually contains such an image.
 */
import { HEIC_CONCURRENCY, HEIC_JPEG_QUALITY, HEIC_TIMEOUT_MS } from './limits';
import { Semaphore, raceAbort, withTimeout } from './util';

/** Converts a HEIC/HEIF blob to a blob every browser can display (JPEG by default). */
export type HeicConverter = (source: Blob) => Promise<Blob>;
/** Resolves true when the current browser can render the HEIC/HEIF blob natively. */
export type HeicNativeProbe = (source: Blob) => Promise<boolean>;

let converterOverride: HeicConverter | null = null;
let probeOverride: HeicNativeProbe | null = null;
/** Set once native decoding failed: browsers either support HEIC or they do not. */
let nativeUnavailable = false;

/** Replaces the HEIC converter (tests, alternative decoders). Pass null to restore the default. */
export function setHeicConverter(converter: HeicConverter | null): void {
  converterOverride = converter;
}

/** Replaces the native-support probe. Pass null to restore the default (createImageBitmap). */
export function setHeicNativeProbe(probe: HeicNativeProbe | null): void {
  probeOverride = probe;
  nativeUnavailable = false;
}

async function convertWithLibheif(source: Blob): Promise<Blob> {
  // Dynamic import: keeps the decoder out of the main bundle.
  const { heicTo } = await import('heic-to');
  return heicTo({ blob: source, type: 'image/jpeg', quality: HEIC_JPEG_QUALITY });
}

async function probeWithImageBitmap(source: Blob): Promise<boolean> {
  if (typeof createImageBitmap !== 'function') return false;
  try {
    const bitmap = await createImageBitmap(source);
    const usable = bitmap.width > 0 && bitmap.height > 0;
    bitmap.close();
    return usable;
  } catch {
    return false;
  }
}

const conversions = new Semaphore(HEIC_CONCURRENCY);

export interface PreparedHeic {
  blob: Blob;
  /** False when the browser renders the original HEIC bytes itself. */
  converted: boolean;
}

/**
 * Makes a HEIC/HEIF blob displayable. `source` must already carry an image/heic or
 * image/heif MIME type. Rejects when the conversion fails, times out or is aborted.
 */
export async function prepareHeic(
  source: Blob,
  signal?: AbortSignal,
  onConvertStart?: () => void,
): Promise<PreparedHeic> {
  if (!nativeUnavailable) {
    const probe = probeOverride ?? probeWithImageBitmap;
    let native = false;
    try {
      native = await probe(source);
    } catch {
      native = false;
    }
    if (native) return { blob: source, converted: false };
    nativeUnavailable = true;
  }
  return { blob: await convertHeic(source, signal, onConvertStart), converted: true };
}

/**
 * Converts a HEIC/HEIF blob to JPEG (by default) whether or not the browser could show it, e.g.
 * for a server that only stores widely supported formats. Rejects like prepareHeic.
 */
export function convertHeic(source: Blob, signal?: AbortSignal, onConvertStart?: () => void): Promise<Blob> {
  return conversions.run(async () => {
    onConvertStart?.();
    const convert = converterOverride ?? convertWithLibheif;
    const blob = await raceAbort(
      withTimeout(convert(source), HEIC_TIMEOUT_MS, 'the conversion timed out'),
      signal,
    );
    if (!(blob instanceof Blob) || blob.size === 0) throw new Error('the converter returned no image');
    return blob;
  });
}
