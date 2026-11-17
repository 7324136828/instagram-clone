export type PhotoKey =
  | 'books'
  | 'finance'
  | 'moon'
  | 'floral'
  | 'sunset'
  | 'ocean'
  | 'city'
  | 'coffee'
  | 'mountains'
  | 'cat'
  | 'art'
  | 'forest';

export const PHOTO_KEYS: readonly PhotoKey[] = [
  'books', 'finance', 'moon', 'floral', 'sunset', 'ocean',
  'city', 'coffee', 'mountains', 'cat', 'art', 'forest',
];

export interface Photo {
  /** Gradient placeholder: the whole picture for seed posts, the loading/fallback background for real images. */
  key: PhotoKey;
  /** Decorative glyph drawn on the placeholder. */
  emoji: string;
  /**
   * Real image: an object URL owned by the current import, or a library file URL. For a video it
   * is the poster, which is optional.
   */
  src?: string;
  /** Alternative text for the real image or video. */
  alt?: string;
  /** Video to play instead of the picture (object URL of the import or library file URL). */
  video?: string;
  /** MIME type of `video`, e.g. video/mp4. */
  videoMime?: string;
}

export interface Comment {
  id: string;
  username: string;
  text: string;
  likes: number;
  /** Relative time label; empty when unknown. */
  time: string;
  replies?: number;
  liked?: boolean;
}

export interface Post {
  id: string;
  username: string;
  verified?: boolean;
  sponsored?: boolean;
  /** Relative time label ("35m", "2d"); empty when unknown. */
  time: string;
  context?: string;
  /** Cover photo. */
  photo: Photo;
  /** Null when the like count is unknown or hidden: never rendered as a number then. */
  likes: number | null;
  caption: string;
  hashtags?: string[];
  likedBy?: string;
  likedByCount?: number;
  comments: Comment[];
  commentCount: number;
  reposts: number;
  shares: number;
  date?: string;
  tags?: string[];
  liked?: boolean;
  saved?: boolean;

  // --- Set on posts that come from an imported Instagram export ---
  /** True for posts that come from an Instagram export. */
  imported?: boolean;
  /** Carousel slides, cover first. Absent for single-photo posts. */
  photos?: Photo[];
  /** Sanitised https link to the post on instagram.com. */
  permalink?: string;
  /** Sanitised https link to the author's profile on instagram.com. */
  profileUrl?: string;
  /** Real profile picture (an object URL owned by the current import). */
  avatarSrc?: string;
  fullName?: string;
  location?: string;
  /**
   * The post (or its first slide) is a video. When the cover has no `video`, only the poster
   * image was exported.
   */
  isVideo?: boolean;
  /** ISO 8601 timestamp of the original post. */
  takenAt?: string;
  /** Position of the post in the import's `posts` (ImportResult): the record it was made from. */
  importIndex?: number;
}

/** All photos of a post, cover first (one entry for single-photo posts). */
export function postPhotos(post: Post): Photo[] {
  return post.photos && post.photos.length > 0 ? post.photos : [post.photo];
}

export interface User {
  username: string;
  fullName: string;
  verified?: boolean;
  avatar: PhotoKey;
}

export const currentUser: User = {
  username: 'souls.in.blue',
  fullName: 'Alex Rivera',
  avatar: 'ocean',
};

export const photoBackgrounds: Record<PhotoKey, string> = {
  books:
    'radial-gradient(circle at 28% 22%, rgba(255,240,210,.55), transparent 45%), linear-gradient(135deg, #7b5e3c 0%, #b08b5e 42%, #dcc09a 100%)',
  finance:
    'radial-gradient(circle at 78% 18%, rgba(120,200,255,.35), transparent 40%), linear-gradient(160deg, #0a2a43 0%, #14507a 48%, #3a95b8 100%)',
  moon:
    'radial-gradient(circle at 63% 30%, #fdf6d8 0 5%, rgba(253,246,216,.3) 9%, transparent 18%), radial-gradient(circle at 18% 82%, rgba(120,80,170,.45), transparent 45%), linear-gradient(170deg, #0b1026 0%, #241a4a 46%, #5b3a86 76%, #b06a9e 100%)',
  floral:
    'radial-gradient(circle at 24% 28%, rgba(255,255,255,.45), transparent 38%), linear-gradient(135deg, #ffd9e8 0%, #f7a8c4 46%, #bd5f97 100%)',
  sunset:
    'radial-gradient(circle at 50% 78%, #ffd884 0 7%, transparent 28%), linear-gradient(180deg, #f6d365 0%, #f79b5b 40%, #ef476f 74%, #6a2c70 100%)',
  ocean:
    'radial-gradient(circle at 70% 20%, rgba(255,255,255,.25), transparent 35%), linear-gradient(180deg, #4facfe 0%, #1e6fb0 55%, #0b3d6b 100%)',
  city:
    'linear-gradient(180deg, #2b3a55 0%, #4b5d7a 58%, #d4a373 100%)',
  coffee:
    'radial-gradient(circle at 50% 46%, #d9b48f 0 29%, #a9774f 30% 33%, #6f4a2f 35%, transparent 36%), linear-gradient(135deg,#3b2a20,#7a5a42)',
  mountains:
    'linear-gradient(180deg,#90c4e4 0%, #cfe6f2 40%, #7fa07a 41%, #3f5e42 100%)',
  cat: 'linear-gradient(135deg,#f6d365,#fda085)',
  art: 'conic-gradient(from 90deg, #ff5f6d, #ffc371, #47e6a0, #4aa8ff, #a06bff, #ff5f6d)',
  forest:
    'radial-gradient(circle at 40% 18%, rgba(255,255,220,.35), transparent 42%), linear-gradient(160deg,#1e3d2f,#3a6b46,#8ab661)',
};

export interface Story {
  username: string;
  avatar: PhotoKey;
  seen: boolean;
  live?: boolean;
  /** Real profile picture, for stories derived from an import. */
  avatarSrc?: string;
  /** Picture shown in the story viewer, for stories derived from an import. */
  image?: Photo;
  /** Relative time label shown in the story viewer. */
  time?: string;
}

export const stories: Story[] = [
  { username: 'pretty_hormonal', avatar: 'floral', seen: false },
  { username: 'itsmejunagen', avatar: 'sunset', seen: false },
  { username: 'lerennis', avatar: 'forest', seen: false },
  { username: '1vrachpiesidia', avatar: 'ocean', seen: true },
  { username: 'noelreads', avatar: 'books', seen: false },
  { username: 'malikbenelfoul', avatar: 'city', seen: false },
  { username: 'stefansbooks', avatar: 'coffee', seen: true },
  { username: 'elspethdetouraine', avatar: 'art', seen: false },
  { username: 'nature_aroundclock', avatar: 'moon', seen: false, live: true },
  { username: 'floralpoetics', avatar: 'floral', seen: false },
  { username: 'wanderlens', avatar: 'mountains', seen: true },
  { username: 'quietmornings', avatar: 'cat', seen: false },
];

export const posts: Post[] = [
  {
    id: 'p1',
    username: 'pocketdiarybookclub',
    verified: true,
    time: '35m',
    photo: { key: 'books', emoji: '📚' },
    likes: 24,
    caption: 'Readers carry whole worlds in a tote bag. What is in yours this week?',
    hashtags: ['bookstagram', 'tote'],
    comments: [
      { id: 'c1', username: 'noelreads', text: 'Currently three books deep 😅', likes: 8, time: '12m' },
      { id: 'c2', username: 'stefansbooks', text: 'This is so me', likes: 3, time: '5m' },
    ],
    commentCount: 5,
    reposts: 2,
    shares: 1,
    date: 'NOVEMBER 9',
  },
  {
    id: 'p2',
    username: 'ubs',
    verified: true,
    sponsored: true,
    time: 'Sponsored',
    photo: { key: 'finance', emoji: '🏦' },
    likes: 72,
    caption:
      'As geopolitics and trade policies shift, staying close to the numbers matters more than ever. See the full outlook.',
    comments: [],
    commentCount: 0,
    reposts: 4,
    shares: 6,
    date: 'NOVEMBER 9',
  },
  {
    id: 'p3',
    username: 'nature_aroundclock',
    time: '2d',
    context: 'Suggested for you',
    photo: { key: 'moon', emoji: '🌙' },
    likes: 8300,
    caption: 'WHEN THE MOON MEETS THE COLORS OF THE SKY.',
    hashtags: ['nightsky', 'moon'],
    comments: [
      { id: 'c3', username: 'wanderlens', text: 'Absolutely unreal 🌙', likes: 41, time: '1d', replies: 2 },
      { id: 'c4', username: 'quietmornings', text: 'The colors!!', likes: 12, time: '20h' },
    ],
    commentCount: 63,
    reposts: 973,
    shares: 595,
    date: 'NOVEMBER 7',
  },
  {
    id: 'p4',
    username: 'floralpoetics',
    time: '1w',
    photo: { key: 'floral', emoji: '🌸' },
    likes: 263300,
    caption: 'a small poem made of petals for your feed today 🌸',
    hashtags: ['flowers', 'poetry'],
    likedBy: 'souls.in.blue',
    likedByCount: 263000,
    comments: [
      { id: 'c5', username: 'elspethdetouraine', text: 'Stunning as always 💐', likes: 210, time: '6d' },
    ],
    commentCount: 857,
    reposts: 1200,
    shares: 934,
    date: 'NOVEMBER 2',
  },
  {
    id: 'p5',
    username: 'hashtag21208',
    time: '1w',
    photo: { key: 'cat', emoji: '🐱' },
    likes: 378600,
    caption: 'meow 🐱',
    hashtags: ['cats', 'meow'],
    comments: [
      { id: 'c6', username: 'jameswebb2.0', text: 'Soy yo cada vez que tengo que salir 😂', likes: 1200, time: '1w', replies: 1 },
      { id: 'c7', username: 'bozena_ewa33', text: '❤️❤️❤️', likes: 22, time: '1w' },
      { id: 'c8', username: 'lilly.klzr', text: 'the zoomies are real', likes: 9, time: '6d' },
    ],
    commentCount: 68,
    reposts: 43400,
    shares: 12800,
    date: 'NOVEMBER 1',
    tags: ['wiiiikoo', 'gutosiaxx', 'evapruski', 'r0stee', 'hashtag21208'],
  },
  {
    id: 'p6',
    username: 'wanderlens',
    time: '2w',
    photo: { key: 'mountains', emoji: '🏔️' },
    likes: 9421,
    caption: 'Somewhere above the clouds, everything gets quieter.',
    hashtags: ['mountains', 'wanderlust'],
    comments: [
      { id: 'c9', username: 'lerennis', text: 'Take me back 🙌', likes: 30, time: '12d' },
    ],
    commentCount: 41,
    reposts: 88,
    shares: 120,
    date: 'OCTOBER 26',
  },
];

export const suggestions: User[] = [
  { username: 'shah_arhaan147', fullName: 'Arhaan', avatar: 'city' },
  { username: 'nuitcourte', fullName: 'Nuit Courte', verified: true, avatar: 'moon' },
  { username: 'souls.and.pages', fullName: 'Pages', avatar: 'books' },
  { username: 'marigold.studio', fullName: 'Marigold Studio', avatar: 'art' },
  { username: 'the.daily.bloom', fullName: 'Daily Bloom', avatar: 'floral' },
];

export interface Notification {
  id: string;
  username: string;
  avatar: PhotoKey;
  text: string;
  time: string;
  action?: 'follow' | 'like';
  preview?: PhotoKey;
}

export const notifications: Notification[] = [
  { id: 'n1', username: 'noelreads', avatar: 'books', text: 'liked your photo.', time: '2h', action: 'like', preview: 'books' },
  { id: 'n2', username: 'nuitcourte', avatar: 'moon', text: 'started following you.', time: '5h', action: 'follow' },
  { id: 'n3', username: 'floralpoetics', avatar: 'floral', text: 'liked your comment.', time: '1d', action: 'like', preview: 'floral' },
  { id: 'n4', username: 'wanderlens', avatar: 'mountains', text: 'mentioned you in a comment.', time: '2d', preview: 'mountains' },
  { id: 'n5', username: 'stefansbooks', avatar: 'coffee', text: 'started following you.', time: '3d', action: 'follow' },
];

export const recentSearches = [
  'floralpoetics',
  'nature_aroundclock',
  'bookstagram',
  'pocketdiarybookclub',
  'wanderlens',
];

export const explorePhotos: Photo[] = [
  { key: 'floral', emoji: '🌸' },
  { key: 'moon', emoji: '🌙' },
  { key: 'books', emoji: '📚' },
  { key: 'ocean', emoji: '🌊' },
  { key: 'sunset', emoji: '🌅' },
  { key: 'forest', emoji: '🌲' },
  { key: 'cat', emoji: '🐱' },
  { key: 'art', emoji: '🎨' },
  { key: 'mountains', emoji: '🏔️' },
  { key: 'coffee', emoji: '☕' },
  { key: 'city', emoji: '🏙️' },
  { key: 'finance', emoji: '🏦' },
];

export const footerLinks = [
  'About', 'Help', 'Press', 'API', 'Jobs', 'Privacy', 'Terms', 'Locations', 'Locations',
  'Instagram Lite', 'Threads', 'Contact Uploading & Non-Users', 'Meta Verified',
];

export const languages = [
  'English', 'Afrikaans', 'Čeština', 'Deutsch', 'Español', 'Français', 'Italiano',
  '日本語', '한국어', 'Português', 'Русский', 'Türkçe', '简体中文',
];

export function formatCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
  return n.toLocaleString('en-US');
}
