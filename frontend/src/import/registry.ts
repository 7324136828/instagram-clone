/**
 * Every object URL handed out by the import library, with the Blob behind it.
 * saveImport needs the Blobs; releaseImport must revoke each URL exactly once.
 */
import type { ImportResult, ImportedPost } from './types';

const blobsByUrl = new Map<string, Blob>();

export function registerBlobUrl(blob: Blob): string {
  const url = URL.createObjectURL(blob);
  blobsByUrl.set(url, blob);
  return url;
}

/** Revokes a URL created by registerBlobUrl. Returns false when it was already revoked. */
export function revokeBlobUrl(url: string): boolean {
  if (!blobsByUrl.delete(url)) return false;
  try {
    URL.revokeObjectURL(url);
  } catch {
    // Nothing to do: the URL is gone from our books either way.
  }
  return true;
}

/** The Blob behind a live import URL, or undefined after release. */
export function blobForUrl(url: string): Blob | undefined {
  return blobsByUrl.get(url);
}

/** The URLs one post references: its pictures, videos, posters and the author's avatar. */
export function postUrls(post: ImportedPost): string[] {
  const urls: string[] = [];
  for (const item of post?.media ?? []) {
    if (typeof item?.url === 'string') urls.push(item.url);
    if (typeof item?.posterUrl === 'string') urls.push(item.posterUrl);
  }
  const avatar = post?.user?.avatarUrl;
  if (typeof avatar === 'string') urls.push(avatar);
  return urls;
}

/** Every distinct URL referenced by a result (media, posters and avatars). */
export function collectResultUrls(result: ImportResult): string[] {
  const urls = new Set<string>();
  for (const post of result?.posts ?? []) {
    for (const url of postUrls(post)) urls.add(url);
  }
  return [...urls];
}
