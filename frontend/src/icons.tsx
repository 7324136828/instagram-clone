import type { ReactNode } from 'react';

export type IconName =
  | 'home'
  | 'search'
  | 'compass'
  | 'reels'
  | 'messages'
  | 'heart'
  | 'create'
  | 'more'
  | 'settings'
  | 'bookmark'
  | 'comment'
  | 'share'
  | 'ellipsis'
  | 'emoji'
  | 'chevronLeft'
  | 'chevronRight'
  | 'close'
  | 'sun'
  | 'moon'
  | 'grid'
  | 'clip'
  | 'carousel'
  | 'at'
  | 'back'
  | 'tag'
  | 'flag'
  | 'clock'
  | 'logout'
  | 'plus'
  | 'import'
  | 'external'
  | 'folder'
  | 'archive'
  | 'link'
  | 'warning'
  | 'check'
  | 'play'
  | 'volume'
  | 'volumeOff'
  | 'download';

const FILLED = new Set<IconName>(['ellipsis']);

const glyphs: Record<IconName, ReactNode> = {
  home: (
    <>
      <path d="M3 9.6 12 2.8l9 6.8V20a1 1 0 0 1-1 1h-5v-6.5H9V21H4a1 1 0 0 1-1-1V9.6Z" />
    </>
  ),
  search: (
    <>
      <circle cx="10.8" cy="10.8" r="6.8" />
      <path d="m16 16 5 5" />
    </>
  ),
  compass: (
    <>
      <circle cx="12" cy="12" r="9.2" />
      <path d="m15.6 8.4-2.1 5.1-5.1 2.1 2.1-5.1 5.1-2.1Z" strokeLinejoin="round" />
    </>
  ),
  reels: (
    <>
      <rect x="2.8" y="2.8" width="18.4" height="18.4" rx="4.5" />
      <path d="M2.8 8.6h18.4" />
      <path d="m8 2.8 1.7 5.8M14 2.8l1.7 5.8" />
      <path d="m10.6 12.2 4 2.6-4 2.6v-5.2Z" strokeLinejoin="round" />
    </>
  ),
  messages: (
    <>
      <path d="M21.6 2.4 11 13" />
      <path d="M21.6 2.4 15 21.6l-4-8.6-8.6-4 19.2-6.6Z" strokeLinejoin="round" />
    </>
  ),
  heart: (
    <>
      <path d="M12 20.6 10.8 19.5C5.2 14.4 2.4 11.8 2.4 8.4 2.4 5 5 2.8 8.2 2.8c1.8 0 3.6.9 4.6 2.3 1-1.4 2.8-2.3 4.6-2.3 3.2 0 5.8 2.2 5.8 5.6 0 3.4-2.8 6-8.4 11.1L12 20.6Z" strokeLinejoin="round" />
    </>
  ),
  create: (
    <>
      <rect x="2.8" y="2.8" width="18.4" height="18.4" rx="5" />
      <path d="M12 8v8M8 12h8" />
    </>
  ),
  more: (
    <>
      <path d="M4 6.5h16M4 12h16M4 17.5h16" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.1 14.5a1.6 1.6 0 0 0 .32 1.77l.06.06a1.9 1.9 0 1 1-2.69 2.69l-.06-.06a1.6 1.6 0 0 0-1.77-.32 1.6 1.6 0 0 0-.97 1.47v.17a1.9 1.9 0 1 1-3.8 0v-.09a1.6 1.6 0 0 0-1.05-1.47 1.6 1.6 0 0 0-1.77.32l-.06.06a1.9 1.9 0 1 1-2.69-2.69l.06-.06a1.6 1.6 0 0 0 .32-1.77 1.6 1.6 0 0 0-1.47-.97H3.3a1.9 1.9 0 1 1 0-3.8h.09a1.6 1.6 0 0 0 1.47-1.05 1.6 1.6 0 0 0-.32-1.77l-.06-.06a1.9 1.9 0 1 1 2.69-2.69l.06.06a1.6 1.6 0 0 0 1.77.32H9a1.6 1.6 0 0 0 .97-1.47V3.3a1.9 1.9 0 1 1 3.8 0v.09a1.6 1.6 0 0 0 .97 1.47 1.6 1.6 0 0 0 1.77-.32l.06-.06a1.9 1.9 0 1 1 2.69 2.69l-.06.06a1.6 1.6 0 0 0-.32 1.77V9a1.6 1.6 0 0 0 1.47.97h.17a1.9 1.9 0 1 1 0 3.8h-.09a1.6 1.6 0 0 0-1.47.73Z" strokeLinejoin="round" />
    </>
  ),
  bookmark: (
    <>
      <path d="M6.5 2.8h11a1 1 0 0 1 1 1v17.4l-6.5-3.9-6.5 3.9V3.8a1 1 0 0 1 1-1Z" strokeLinejoin="round" />
    </>
  ),
  comment: (
    <>
      <path d="M20.8 11.6a8.7 8.7 0 0 1-9.3 8.7 9.8 9.8 0 0 1-3.6-.7L3 21l1.5-4.6a8.5 8.5 0 0 1-1.3-4.6 8.7 8.7 0 0 1 9.3-8.6 8.7 8.7 0 0 1 8.3 8.4Z" strokeLinejoin="round" />
    </>
  ),
  share: (
    <>
      <path d="M21.6 2.4 11 13" />
      <path d="M21.6 2.4 15 21.6l-4-8.6-8.6-4 19.2-6.6Z" strokeLinejoin="round" />
    </>
  ),
  ellipsis: (
    <>
      <circle cx="5" cy="12" r="1.5" />
      <circle cx="12" cy="12" r="1.5" />
      <circle cx="19" cy="12" r="1.5" />
    </>
  ),
  emoji: (
    <>
      <circle cx="12" cy="12" r="9.2" />
      <path d="M8.2 14.2a4.6 4.6 0 0 0 7.6 0" />
      <circle cx="9" cy="9.6" r="1.05" fill="currentColor" stroke="none" />
      <circle cx="15" cy="9.6" r="1.05" fill="currentColor" stroke="none" />
    </>
  ),
  chevronLeft: <path d="m15 5-7 7 7 7" />,
  chevronRight: <path d="m9 5 7 7-7 7" />,
  close: <path d="M5 5 19 19M19 5 5 19" />,
  back: <path d="M15 4 7 12l8 8" />,
  sun: (
    <>
      <circle cx="12" cy="12" r="4.2" />
      <path d="M12 2v2.2M12 19.8V22M4.9 4.9l1.6 1.6M17.5 17.5l1.6 1.6M2 12h2.2M19.8 12H22M4.9 19.1l1.6-1.6M17.5 6.5l1.6-1.6" />
    </>
  ),
  moon: <path d="M20.5 14.2A8.6 8.6 0 0 1 9.8 3.5 8.8 8.8 0 1 0 20.5 14.2Z" strokeLinejoin="round" />,
  grid: (
    <>
      <rect x="3" y="3" width="7" height="7" rx="1" />
      <rect x="14" y="3" width="7" height="7" rx="1" />
      <rect x="3" y="14" width="7" height="7" rx="1" />
      <rect x="14" y="14" width="7" height="7" rx="1" />
    </>
  ),
  clip: (
    <>
      <rect x="2.8" y="4.5" width="18.4" height="15" rx="3" />
      <path d="m2.8 8.5 4-3.9M9 4.6l3.5 3.9M16.8 4.6l4.4 3.9" />
      <path d="m10.4 10.5 4.2 2.7-4.2 2.7v-5.4Z" strokeLinejoin="round" />
    </>
  ),
  carousel: (
    <>
      <rect x="7.5" y="3" width="13" height="13" rx="2.5" />
      <path d="M4.5 7.5v13a1 1 0 0 0 1 1h11" />
    </>
  ),
  at: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-3.9 7.9" />
    </>
  ),
  tag: (
    <>
      <path d="M20.8 13.3 13.3 20.8a2 2 0 0 1-2.8 0L3 13.3V3.5a.5.5 0 0 1 .5-.5h9.8l7.5 7.5a2 2 0 0 1 0 2.8Z" strokeLinejoin="round" />
      <circle cx="7.6" cy="7.6" r="1.3" />
    </>
  ),
  flag: <path d="M5 21V4M5 4.5h11l-2 3.6 2 3.9H5" strokeLinejoin="round" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5.3l3.4 2" />
    </>
  ),
  logout: (
    <>
      <path d="M10 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h5" />
      <path d="M15 8.5 18.5 12 15 15.5M18.5 12H9" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  import: (
    <>
      <path d="M12 3v11.5" />
      <path d="m7.5 10.5 4.5 4.5 4.5-4.5" />
      <path d="M3.5 14.5v3.7a2.8 2.8 0 0 0 2.8 2.8h11.4a2.8 2.8 0 0 0 2.8-2.8v-3.7" />
    </>
  ),
  external: (
    <>
      <path d="M18.5 13.5v4.7a2.3 2.3 0 0 1-2.3 2.3H5.8a2.3 2.3 0 0 1-2.3-2.3V7.8a2.3 2.3 0 0 1 2.3-2.3h4.7" />
      <path d="M14.5 3.5h6v6" />
      <path d="M20.5 3.5 11 13" />
    </>
  ),
  folder: (
    <>
      <path d="M3 7.2A2.2 2.2 0 0 1 5.2 5h3.6a2 2 0 0 1 1.5.7L11.6 7.2h7.2A2.2 2.2 0 0 1 21 9.4v7.9a2.2 2.2 0 0 1-2.2 2.2H5.2A2.2 2.2 0 0 1 3 17.3V7.2Z" />
    </>
  ),
  archive: (
    <>
      <rect x="3" y="3.5" width="18" height="5" rx="1.5" />
      <path d="M4.8 8.5v9.8a2.2 2.2 0 0 0 2.2 2.2h10a2.2 2.2 0 0 0 2.2-2.2V8.5" />
      <path d="M9.8 12.5h4.4" />
    </>
  ),
  link: (
    <>
      <path d="M10.2 13.8a4.2 4.2 0 0 0 5.9 0l3.2-3.2a4.2 4.2 0 0 0-5.9-5.9l-1.2 1.2" />
      <path d="M13.8 10.2a4.2 4.2 0 0 0-5.9 0l-3.2 3.2a4.2 4.2 0 0 0 5.9 5.9l1.2-1.2" />
    </>
  ),
  warning: (
    <>
      <path d="M12 3.5 2.8 19.5h18.4L12 3.5Z" />
      <path d="M12 10v4.5" />
      <path d="M12 17.2v.1" />
    </>
  ),
  check: <path d="m5 12.5 4.5 4.5L19 7.5" />,
  play: <path d="M7 4.8v14.4a.8.8 0 0 0 1.2.7l11.3-7.2a.8.8 0 0 0 0-1.4L8.2 4.1a.8.8 0 0 0-1.2.7Z" />,
  volume: (
    <>
      <path d="M4 9.5h3.2L12 5.5v13l-4.8-4H4a1 1 0 0 1-1-1v-3a1 1 0 0 1 1-1Z" />
      <path d="M15.5 9a4.2 4.2 0 0 1 0 6M18.2 6.5a7.8 7.8 0 0 1 0 11" />
    </>
  ),
  volumeOff: (
    <>
      <path d="M4 9.5h3.2L12 5.5v13l-4.8-4H4a1 1 0 0 1-1-1v-3a1 1 0 0 1 1-1Z" />
      <path d="m15.5 9.5 5 5M20.5 9.5l-5 5" />
    </>
  ),
  download: (
    <>
      <path d="M12 3.5v11" />
      <path d="m7.5 10 4.5 4.5 4.5-4.5" />
      <path d="M4.5 19.5h15" />
    </>
  ),
};

export interface IconProps {
  name: IconName;
  size?: number;
  filled?: boolean;
  className?: string;
  strokeWidth?: number;
}

export function Icon({ name, size = 24, filled = false, className, strokeWidth = 1.9 }: IconProps) {
  const isFilled = filled || FILLED.has(name);
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={isFilled ? 'currentColor' : 'none'}
      stroke="currentColor"
      strokeWidth={isFilled ? 0 : strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {glyphs[name]}
    </svg>
  );
}
