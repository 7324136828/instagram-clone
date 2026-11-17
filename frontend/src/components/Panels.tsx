import { useEffect, useState, type ReactNode } from 'react';
import {
  currentUser, explorePhotos, notifications, photoBackgrounds, stories,
  suggestions, type Photo, type PhotoKey, type Post,
} from '../data';
import { Icon } from '../icons';
import { siteName, type DirectoryUser } from '../importedPosts';
import { Avatar, ExternalLink, useDialogFocus, useIncremental, VerifiedBadge } from './common';
import { PostTile } from './Media';

function Drawer({ title, header, children, className = '' }: { title?: string; header?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={`drawer ${className}`}>
      <div className="drawer__head">
        {header ?? <h2>{title}</h2>}
      </div>
      <div className="drawer__body">{children}</div>
    </div>
  );
}

export interface SearchUser {
  username: string;
  fullName: string;
  avatar: PhotoKey;
  /** Real profile picture (imported users). */
  avatarSrc?: string;
  verified?: boolean;
}

export interface SearchPanelProps {
  query: string;
  recent: string[];
  /** The accounts that can be found: seed accounts, or the authors of the current import. */
  users: SearchUser[];
  onQueryChange: (q: string) => void;
  onClearRecent: () => void;
  onOpenProfile: (username: string) => void;
}

/** Searchable accounts of the demo data: suggestions, stories and the authors of the given posts. */
export function seedSearchUsers(posts: readonly Post[]): SearchUser[] {
  const candidates: SearchUser[] = [
    ...suggestions.map((u) => ({ username: u.username, fullName: u.fullName, avatar: u.avatar })),
    ...stories.map((s) => ({ username: s.username, fullName: s.username, avatar: s.avatar })),
    ...posts.map((p) => ({ username: p.username, fullName: p.username, avatar: p.photo.key })),
  ];
  return Array.from(
    new Map<string, SearchUser>(candidates.map((u): [string, SearchUser] => [u.username, u])).values(),
  );
}

/** Searchable accounts of an import: its authors. */
export function importedSearchUsers(users: Iterable<DirectoryUser>): SearchUser[] {
  return Array.from(users, (u) => ({
    username: u.username,
    fullName: u.fullName ?? u.username,
    avatar: u.avatar,
    avatarSrc: u.avatarSrc,
    verified: u.verified,
  }));
}

export function SearchPanel({ query, recent, users, onQueryChange, onClearRecent, onOpenProfile }: SearchPanelProps) {
  const q = query.trim().toLowerCase();
  const results = q
    ? users.filter((u) => u.username.toLowerCase().includes(q) || u.fullName.toLowerCase().includes(q)).slice(0, 12)
    : [];
  return (
    <Drawer
      className="drawer--search"
      header={(
        <>
          <h2>Search</h2>
          <div className="drawer__searchbox">
            <Icon name="search" size={16} />
            <input
              autoFocus
              value={query}
              onChange={(e) => onQueryChange(e.target.value)}
              placeholder="Search"
              aria-label="Search"
            />
            {query && <button type="button" aria-label="Clear" onClick={() => onQueryChange('')}><Icon name="close" size={14} /></button>}
          </div>
        </>
      )}
    >
      <div className="drawer__section-head">
        <strong>{q ? 'Results' : 'Recent'}</strong>
        {!q && <button className="link-blue link-small" type="button" onClick={onClearRecent}>Clear all</button>}
      </div>
      {q ? (
        results.length ? results.map((u) => (
          <button className="result-row" key={u.username} type="button" onClick={() => onOpenProfile(u.username)}>
            <Avatar username={u.username} avatar={u.avatar} src={u.avatarSrc} size={44} />
            <span className="result-row__meta">
              <strong>{u.username} {u.verified && <VerifiedBadge size={12} />}</strong>
              <span>{u.fullName}</span>
            </span>
          </button>
        )) : <div className="drawer__empty">No results found.</div>
      ) : recent.length ? (
        recent.map((name) => {
          const u: SearchUser = users.find((x) => x.username === name) ?? { username: name, fullName: name, avatar: 'ocean' };
          return (
            <button className="result-row" key={name} type="button" onClick={() => onOpenProfile(name)}>
              <Avatar username={u.username} avatar={u.avatar} src={u.avatarSrc} size={44} />
              <span className="result-row__meta">
                <strong>{u.username}</strong>
                <span>{u.fullName}</span>
              </span>
            </button>
          );
        })
      ) : (
        <div className="drawer__empty">No recent searches.</div>
      )}
    </Drawer>
  );
}

export function NotificationsPanel({ onOpenProfile }: { onOpenProfile: (username: string) => void }) {
  return (
    <Drawer title="Notifications" className="drawer--notifications">
      <div className="drawer__section-head"><strong>This week</strong></div>
      {notifications.map((n) => (
        <button className="notif-row" key={n.id} type="button" onClick={() => onOpenProfile(n.username)}>
          <Avatar username={n.username} avatar={n.avatar} size={44} />
          <span className="notif-row__text">
            <span><strong>{n.username}</strong> {n.text} <em>{n.time}</em></span>
          </span>
          {n.action === 'follow' && <span className="notif-follow">Follow</span>}
          {n.preview && <span className="notif-preview" style={{ background: photoBackgrounds[n.preview] }}>{findEmoji(n.preview)}</span>}
        </button>
      ))}
      <div className="drawer__section-head"><strong>This month</strong></div>
      {notifications.slice(0, 3).map((n) => (
        <button className="notif-row" key={`m-${n.id}`} type="button" onClick={() => onOpenProfile(n.username)}>
          <Avatar username={n.username} avatar={n.avatar} size={44} />
          <span className="notif-row__text"><span><strong>{n.username}</strong> {n.text} <em>{n.time}</em></span></span>
          {n.preview && <span className="notif-preview" style={{ background: photoBackgrounds[n.preview] }}>{findEmoji(n.preview)}</span>}
        </button>
      ))}
    </Drawer>
  );
}

function findEmoji(key: PhotoKey): string {
  return explorePhotos.find((p) => p.key === key)?.emoji ?? '📷';
}

interface ChatMessage { from: string; text: string; mine: boolean }

const chatSeeds: { username: string; avatar: PhotoKey; preview: string; time: string }[] = [
  { username: 'noelreads', avatar: 'books', preview: 'Have you read the new one yet? 📖', time: '2h' },
  { username: 'nature_aroundclock', avatar: 'moon', preview: 'Sent a photo', time: '5h' },
  { username: 'floralpoetics', avatar: 'floral', preview: 'thank you so much 🌸', time: '1d' },
  { username: 'wanderlens', avatar: 'mountains', preview: 'Let’s plan the next trip!', time: '3d' },
  { username: 'stefansbooks', avatar: 'coffee', preview: '☕', time: '1w' },
];

export function MessagesPanel() {
  const [active, setActive] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [threads, setThreads] = useState<Record<string, ChatMessage[]>>({
    noelreads: [{ from: 'noelreads', text: 'Have you read the new one yet? 📖', mine: false }],
    nature_aroundclock: [{ from: 'nature_aroundclock', text: 'Sent a photo', mine: false }],
  });

  const send = () => {
    if (!active || !draft.trim()) return;
    setThreads((prev) => ({
      ...prev,
      [active]: [...(prev[active] ?? []), { from: 'me', text: draft.trim(), mine: true }],
    }));
    setDraft('');
  };

  return (
    <Drawer
      className="drawer--messages"
      header={(
        <>
          <h2>{active ?? 'Messages'}</h2>
          <button className="drawer__icon-btn" type="button" aria-label="New message"><Icon name="create" size={22} /></button>
        </>
      )}
    >
      {active ? (
        <div className="chat">
          <button className="chat__back" type="button" onClick={() => setActive(null)}><Icon name="back" size={20} /> Back</button>
          <div className="chat__thread">
            {(threads[active] ?? []).map((m, i) => (
              <div key={i} className={`chat__bubble ${m.mine ? 'chat__bubble--mine' : ''}`}>{m.text}</div>
            ))}
          </div>
          <div className="chat__composer">
            <input value={draft} onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') send(); }} placeholder="Message…" aria-label="Message" />
            <button className="link-blue" type="button" onClick={send} disabled={!draft.trim()}>Send</button>
          </div>
        </div>
      ) : (
        <>
          <div className="drawer__searchbox drawer__searchbox--plain"><Icon name="search" size={16} /><input placeholder="Search" aria-label="Search messages" /></div>
          <div className="drawer__section-head"><strong>Your messages</strong></div>
          {chatSeeds.map((c) => (
            <button className="result-row" key={c.username} type="button" onClick={() => setActive(c.username)}>
              <Avatar username={c.username} avatar={c.avatar} size={52} />
              <span className="result-row__meta">
                <strong>{c.username}</strong>
                <span>{c.preview} · {c.time}</span>
              </span>
            </button>
          ))}
        </>
      )}
    </Drawer>
  );
}

export interface CreateModalProps {
  onClose: () => void;
  onShare: (caption: string, photo: Photo) => void;
}

export function CreateModal({ onClose, onShare }: CreateModalProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();
  const [preview, setPreview] = useState<Photo | null>(null);
  const [caption, setCaption] = useState('');

  // The demo has no upload: "selecting" a picture, by click or by drop, picks a placeholder photo.
  const pickPhoto = () => setPreview(explorePhotos[Math.floor(Math.random() * explorePhotos.length)]);

  // The control that led to the other step is gone after the switch: keep focus inside the dialog.
  useEffect(() => {
    const node = dialogRef.current;
    if (node && !node.contains(document.activeElement)) node.focus({ preventScroll: true });
  }, [preview, dialogRef]);

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className={`create-modal ${preview ? 'create-modal--wide' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-label="Create new post"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <button className="pd-close" type="button" aria-label="Close" onClick={onClose}><Icon name="close" size={28} /></button>
        {preview ? (
          <>
            <div className="create-modal__head">
              <button className="drawer__icon-btn" type="button" aria-label="Back" onClick={() => setPreview(null)}><Icon name="back" size={20} /></button>
              <span>Create new post</span>
              <button className="link-blue" type="button" onClick={() => { onShare(caption, preview); onClose(); }}>Share</button>
            </div>
            <div className="create-modal__edit">
              <div className="create-modal__preview" style={{ background: photoBackgrounds[preview.key] }}>
                <span className="pd-media__subject" aria-hidden="true">{preview.emoji}</span>
              </div>
              <div className="create-modal__caption">
                <div className="create-modal__user"><Avatar username="souls.in.blue" avatar="ocean" size={30} /><strong>souls.in.blue</strong></div>
                <textarea value={caption} onChange={(e) => setCaption(e.target.value)} placeholder="Write a caption…" aria-label="Caption" />
                <div className="create-modal__extras"><Icon name="emoji" size={20} /><span>{caption.length}/2,200</span></div>
              </div>
            </div>
          </>
        ) : (
          <>
            <div className="create-modal__head">Create new post<button className="link-blue link-small" type="button" onClick={() => setPreview(explorePhotos[0])}>Next</button></div>
            {/* While this dialog is open, dropped files are meant for it: the app does not import them. */}
            <div
              className="create-modal__drop"
              onClick={pickPhoto}
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => { e.preventDefault(); pickPhoto(); }}
            >
              <span className="create-modal__glyph"><Icon name="carousel" size={64} /></span>
              <p>Drag photos and videos here</p>
              <button className="btn-blue" type="button">Select from computer</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export interface ExploreGridProps {
  /** The posts currently shown by the app: seed data or the import. */
  posts: Post[];
  /** True when `posts` come from an import: every post becomes a square tile with its real picture. */
  imported: boolean;
  onOpenDetail: (post: Post) => void;
}

/** Grid tiles rendered at first and added per step while scrolling. */
const GRID_PAGE = 30;

function EmptyGrid({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="grid-empty">
      <span className="grid-empty__glyph"><Icon name="grid" size={30} /></span>
      <p>{title}</p>
      {hint && <span>{hint}</span>}
    </div>
  );
}

/** Square tiles for real posts, loaded page by page. */
function PostGrid({ posts, className, onOpenDetail }: { posts: Post[]; className: string; onOpenDetail: (post: Post) => void }) {
  const { visible, hasMore, sentinel } = useIncremental(posts.length, GRID_PAGE);
  return (
    <>
      <div className={className}>
        {posts.slice(0, visible).map((post) => (
          <PostTile key={post.id} post={post} className="explore__tile" onOpen={onOpenDetail} />
        ))}
      </div>
      {hasMore && <div className="load-sentinel" ref={sentinel} aria-hidden="true"><span className="spinner" /></div>}
    </>
  );
}

/** The decorative mosaic of the demo data: placeholder pictures that open the matching seed post. */
function PlaceholderGrid({ posts, className, mosaic, onOpenDetail }: {
  posts: Post[];
  className: string;
  mosaic: boolean;
  onOpenDetail: (post: Post) => void;
}) {
  if (posts.length === 0) return <EmptyGrid title="No posts yet" />;
  return (
    <div className={className}>
      {explorePhotos.map((photo, i) => {
        const tall = mosaic && i % 7 === 3;
        const wide = mosaic && i % 11 === 5;
        return (
          <PostTile
            key={`${photo.key}-${i}`}
            post={posts.find((x) => x.photo.key === photo.key) ?? posts[0]}
            photo={photo}
            stretch={tall}
            stats={mosaic}
            className={`explore__tile ${tall ? 'explore__tile--tall' : ''} ${wide ? 'explore__tile--wide' : ''}`}
            onOpen={onOpenDetail}
          />
        );
      })}
    </div>
  );
}

export function ExploreGrid({ posts, imported, onOpenDetail }: ExploreGridProps) {
  return (
    <div className="explore">
      <h2 className="explore__title">Explore</h2>
      {imported
        ? <PostGrid posts={posts} className="explore__grid" onOpenDetail={onOpenDetail} />
        : <PlaceholderGrid posts={posts} className="explore__grid" mosaic onOpenDetail={onOpenDetail} />}
    </div>
  );
}

export function ReelsView() {
  const [liked, setLiked] = useState<Set<number>>(new Set());
  const reels = explorePhotos.slice(0, 6);
  const toggle = (i: number) => setLiked((prev) => {
    const n = new Set(prev);
    if (n.has(i)) n.delete(i); else n.add(i);
    return n;
  });
  return (
    <div className="reels">
      {reels.map((p, i) => (
        <div className="reel" key={i}>
          <div className="reel__media" style={{ background: photoBackgrounds[p.key] }}>
            <span className="reel__subject" aria-hidden="true">{p.emoji}</span>
            <div className="reel__top"><Icon name="clip" size={22} /><span>Reels</span></div>
            <div className="reel__bottom">
              <div className="reel__user"><Avatar username={stories[i].username} avatar={p.key} size={32} ring="story" /><strong>{stories[i].username}</strong><button className="reel__follow" type="button">Follow</button></div>
              <p>when the vibes are just right ✨ #{p.key}</p>
              <div className="reel__audio"><Icon name="at" size={13} /> original audio · {stories[i].username}</div>
            </div>
          </div>
          <div className="reel__actions">
            <button className={`reel__action ${liked.has(i) ? 'icon-btn--liked' : ''}`} type="button" onClick={() => toggle(i)}>
              <Icon name="heart" size={28} filled={liked.has(i)} strokeWidth={liked.has(i) ? 0 : 1.9} /><span>{(i + 2) * 124}K</span>
            </button>
            <button className="reel__action" type="button"><Icon name="comment" size={28} /><span>1,204</span></button>
            <button className="reel__action" type="button"><Icon name="share" size={28} /><span>Share</span></button>
            <button className="reel__action" type="button"><Icon name="bookmark" size={28} /></button>
            <button className="reel__action" type="button"><Icon name="ellipsis" size={28} /></button>
            <Avatar username={stories[i].username} avatar={p.key} size={28} />
          </div>
        </div>
      ))}
    </div>
  );
}

export interface ProfileViewProps {
  username: string;
  /** The posts currently shown by the app: seed data or the import. */
  posts: Post[];
  /** True when `posts` come from an import. */
  imported: boolean;
  /** Set when `username` is an author of the current import. */
  importedUser?: DirectoryUser;
  onOpenDetail: (post: Post) => void;
  following: Set<string>;
  onToggleFollow: (username: string) => void;
}

export function ProfileView({ username, posts, imported, importedUser, onOpenDetail, following, onToggleFollow }: ProfileViewProps) {
  const ownPosts = posts.filter((p) => p.username === username);
  const tabs = (
    <div className="profile__tabs"><span className="profile__tab profile__tab--active"><Icon name="grid" size={13} /> POSTS</span><span className="profile__tab"><Icon name="clip" size={13} /> REELS</span><span className="profile__tab"><Icon name="tag" size={13} /> TAGGED</span></div>
  );

  // An account from the import: only what the export really knows, plus the way to the real profile.
  if (importedUser) {
    return (
      <div className="profile profile--imported" data-testid="profile-imported">
        <header className="profile__header">
          <div className="profile__avatar"><Avatar username={username} avatar={importedUser.avatar} src={importedUser.avatarSrc} size={150} /></div>
          <div className="profile__info">
            <div className="profile__topline">
              <h2>{username} {importedUser.verified && <VerifiedBadge size={18} />}</h2>
              {importedUser.profileUrl && (
                <ExternalLink className="btn-blue profile__instagram" href={importedUser.profileUrl} data-testid="profile-instagram-link">
                  View on {siteName(importedUser.profileUrl)} <Icon name="external" size={15} />
                </ExternalLink>
              )}
            </div>
            <div className="profile__stats">
              <span data-testid="profile-post-count"><strong>{ownPosts.length.toLocaleString('en-US')}</strong> {ownPosts.length === 1 ? 'post' : 'posts'}</span>
            </div>
            {importedUser.fullName && <div className="profile__bio"><strong>{importedUser.fullName}</strong></div>}
          </div>
        </header>
        {tabs}
        {ownPosts.length > 0
          ? <PostGrid posts={ownPosts} className="profile__grid" onOpenDetail={onOpenDetail} />
          : <EmptyGrid title="No posts" />}
      </div>
    );
  }

  // While an import is shown, nothing is known about any other account: no invented profile.
  if (imported && username !== currentUser.username) {
    return (
      <div className="profile profile--unknown">
        <header className="profile__header">
          <div className="profile__avatar"><Avatar username={username} avatar="ocean" src={null} size={150} /></div>
          <div className="profile__info">
            <div className="profile__topline"><h2>{username}</h2></div>
            <div className="profile__stats"><span><strong>0</strong> posts</span></div>
          </div>
        </header>
        {tabs}
        <EmptyGrid title="No imported posts" hint="This account is not an author in the current import." />
      </div>
    );
  }

  const isFollowing = following.has(username);
  return (
    <div className="profile">
      <header className="profile__header">
        <div className="profile__avatar"><Avatar username={username} avatar="floral" size={150} ring="story" /></div>
        <div className="profile__info">
          <div className="profile__topline">
            <h2>{username}</h2>
            <button className={`btn-blue ${isFollowing ? 'btn-blue--ghost' : ''}`} type="button" onClick={() => onToggleFollow(username)}>{isFollowing ? 'Following' : 'Follow'}</button>
            <button className="profile__msg" type="button">Message</button>
            <button className="drawer__icon-btn" type="button" aria-label="More"><Icon name="more" size={22} /></button>
          </div>
          <div className="profile__stats"><span><strong>{(imported ? ownPosts : posts).length}</strong> posts</span><span><strong>18.4K</strong> followers</span><span><strong>312</strong> following</span></div>
          <div className="profile__bio">
            <strong>curating little moments ✨</strong>
            <span>photographer / daydreamer</span>
            <span>📍 somewhere between the pages and the sky</span>
            <a href="#" onClick={(e) => e.preventDefault()}>linktr.ee/{username}</a>
          </div>
        </div>
      </header>
      <div className="profile__highlights">
        {['Travel', 'Books', 'Bloom', 'Night', 'Studio'].map((h, i) => (
          <button className="highlight" key={h} type="button"><Avatar username={h} avatar={explorePhotos[i].key} src={null} size={77} ring="seen" /><span>{h}</span></button>
        ))}
      </div>
      {tabs}
      {imported ? (
        // The signed-in demo account while an import is shown: only what it shared itself.
        ownPosts.length > 0
          ? <PostGrid posts={ownPosts} className="profile__grid" onOpenDetail={onOpenDetail} />
          : <EmptyGrid title="No posts yet" hint="Imported posts are shown on the profiles of their authors." />
      ) : (
        <PlaceholderGrid posts={posts} className="profile__grid" mosaic={false} onOpenDetail={onOpenDetail} />
      )}
    </div>
  );
}
