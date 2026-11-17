import { useEffect, useRef, useState } from 'react';
import { formatCount, photoBackgrounds, type Comment, type Post, type Story } from '../data';
import { Icon } from '../icons';
import { Avatar, isTopDialog, PhotoView, RichText, useDialogFocus, useIncremental, VerifiedBadge } from './common';
import { captionPreview, PostMedia, PostTime, ViewOnInstagram } from './Media';
import { FeedPlaybackContext } from './Video';

interface StoriesProps {
  /** Seed stories, or one story per author of the current import. */
  stories: Story[];
  onOpenProfile?: (username: string) => void;
}

export function Stories({ stories, onOpenProfile }: StoriesProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const [seen, setSeen] = useState<Set<string>>(() => new Set());
  const [viewer, setViewer] = useState<number | null>(null);
  const [scrollable, setScrollable] = useState(false);
  const count = stories.length;

  // The arrows only make sense when the tray overflows (an import may have a single author).
  useEffect(() => {
    const track = scroller.current;
    if (!track) return undefined;
    const measure = () => setScrollable(track.scrollWidth > track.clientWidth + 1);
    measure();
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [count]);

  const items = stories.map((s) => (seen.has(s.username) ? { ...s, seen: true } : s));
  const active = viewer !== null ? items[viewer] : undefined;

  const scroll = (dir: number) => {
    scroller.current?.scrollBy({ left: dir * 260, behavior: 'smooth' });
  };

  const markSeen = (index: number) => {
    const story = stories[index];
    if (story) setSeen((prev) => new Set(prev).add(story.username));
  };

  const open = (index: number) => {
    setViewer(index);
    markSeen(index);
  };

  const move = (dir: number) => {
    if (viewer === null) return;
    const next = viewer + dir;
    if (next < 0 || next >= items.length) {
      setViewer(null);
      return;
    }
    setViewer(next);
    markSeen(next);
  };

  if (items.length === 0) return null;

  return (
    <section className="stories" aria-label="Stories">
      {scrollable && (
        <button className="stories__arrow stories__arrow--left" type="button" onClick={() => scroll(-1)} aria-label="Previous stories">
          <Icon name="chevronLeft" size={18} />
        </button>
      )}
      <div className="stories__track" ref={scroller}>
        {items.map((story, i) => (
          <button
            className="story"
            key={story.username}
            type="button"
            onClick={() => open(i)}
            onDoubleClick={() => onOpenProfile?.(story.username)}
          >
            <Avatar
              username={story.username}
              avatar={story.avatar}
              src={story.avatarSrc}
              size={62}
              ring={story.live ? 'live' : story.seen ? 'seen' : 'story'}
            />
            <span className="story__name">{story.username}</span>
          </button>
        ))}
      </div>
      {scrollable && (
        <button className="stories__arrow stories__arrow--right" type="button" onClick={() => scroll(1)} aria-label="Next stories">
          <Icon name="chevronRight" size={18} />
        </button>
      )}

      {viewer !== null && active && (
        <StoryViewer items={items} index={viewer} onMove={move} onClose={() => setViewer(null)} />
      )}
    </section>
  );
}

interface StoryViewerProps {
  items: Story[];
  /** Index of the story on screen. */
  index: number;
  onMove: (dir: number) => void;
  onClose: () => void;
}

/** The full-screen story: a modal dialog of its own, opened from the tray. */
function StoryViewer({ items, index, onMove, onClose }: StoryViewerProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();
  const active = items[index];

  // Escape closes the viewer itself (it is not one of the app-level layers), unless another
  // dialog is open on top of it. Nothing below the viewer gets to see that key press.
  const close = useRef(onClose);
  useEffect(() => {
    close.current = onClose;
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || !isTopDialog(dialogRef.current)) return;
      e.stopPropagation();
      close.current();
    };
    document.addEventListener('keydown', onKey, true);
    return () => document.removeEventListener('keydown', onKey, true);
  }, [dialogRef]);

  return (
    <div
      className="story-viewer"
      role="dialog"
      aria-modal="true"
      aria-label={`Story by ${active.username}`}
      tabIndex={-1}
      ref={dialogRef}
      onClick={onClose}
    >
      <button className="story-viewer__close" type="button" aria-label="Close story" onClick={onClose}>
        <Icon name="close" size={26} />
      </button>
      <button className="story-viewer__nav story-viewer__nav--prev" type="button" aria-label="Previous" onClick={(e) => { e.stopPropagation(); onMove(-1); }}>
        <Icon name="chevronLeft" size={24} />
      </button>
      <div className="story-viewer__card" style={{ background: photoBackgrounds[active.avatar] }} onClick={(e) => e.stopPropagation()}>
        {active.image && <PhotoView photo={active.image} fill className="story-viewer__photo" />}
        <div className="story-viewer__progress">
          {items.map((_, i) => <span key={i} className={i < index ? 'done' : i === index ? 'active' : ''} />)}
        </div>
        <div className="story-viewer__header">
          <Avatar username={active.username} avatar={active.avatar} src={active.avatarSrc} size={34} ring="story" />
          <strong>{active.username}</strong>
          {active.image ? active.time && <span>{active.time}</span> : <span>2h</span>}
        </div>
        {!active.image && (
          <span className="story-viewer__subject" aria-hidden="true">
            {active.live ? '🔴' : '✨'}
          </span>
        )}
      </div>
      <button className="story-viewer__nav story-viewer__nav--next" type="button" aria-label="Next" onClick={(e) => { e.stopPropagation(); onMove(1); }}>
        <Icon name="chevronRight" size={24} />
      </button>
    </div>
  );
}

/** Comments of the post itself that a feed card shows; the rest is in the post detail. */
const PREVIEW_COMMENTS = 2;

export interface PostCardProps {
  post: Post;
  extraComments: Comment[];
  onToggleLike: (id: string) => void;
  onToggleSave: (id: string) => void;
  onOpenDetail: (post: Post) => void;
  onAddComment: (id: string, text: string) => void;
  onOpenProfile?: (username: string) => void;
  /** Open the "More options" action sheet for this post. */
  onOpenOptions?: (post: Post) => void;
}

export function PostCard({
  post,
  extraComments,
  onToggleLike,
  onToggleSave,
  onOpenDetail,
  onAddComment,
  onOpenProfile,
  onOpenOptions,
}: PostCardProps) {
  const [expanded, setExpanded] = useState(false);
  const [draft, setDraft] = useState('');
  const [burst, setBurst] = useState(false);
  const [slide, setSlide] = useState(0);
  // A preview of the post's comments, then everything the user wrote here: a comment that was
  // just posted must never be the one that is cut off.
  const shownComments = [...post.comments.slice(0, PREVIEW_COMMENTS), ...extraComments];
  const commentTotal = post.commentCount + extraComments.length;

  const like = () => {
    onToggleLike(post.id);
    triggerBurst();
  };

  const triggerBurst = () => {
    setBurst(true);
    window.setTimeout(() => setBurst(false), 650);
  };

  const submitComment = () => {
    const text = draft.trim();
    if (!text) return;
    onAddComment(post.id, text);
    setDraft('');
  };

  const preview = expanded ? null : captionPreview(post.caption);
  const subline = post.context ?? post.location;
  const hasCaption = post.caption.trim().length > 0 || (post.hashtags?.length ?? 0) > 0;

  return (
    <article className={`post ${post.imported ? 'post--imported' : ''}`} data-post-id={post.id}>
      <header className="post__header">
        <Avatar
          username={post.username}
          avatar={post.photo.key}
          src={post.avatarSrc}
          size={34}
          ring="story"
          onClick={() => onOpenProfile?.(post.username)}
        />
        <div className="post__meta">
          <div className="post__authorline">
            <button className="post__username" type="button" onClick={() => onOpenProfile?.(post.username)}>{post.username}</button>
            {post.verified && <VerifiedBadge />}
            {post.sponsored && <span className="post__sponsored">Sponsored</span>}
            {post.time && <span className="post__sep">·</span>}
            <PostTime post={post} />
          </div>
          {subline && <span className="post__context">{subline}</span>}
        </div>
        {post.permalink && <ViewOnInstagram href={post.permalink} />}
        <button
          className="post__more"
          type="button"
          aria-label="More options"
          aria-haspopup="dialog"
          onClick={() => onOpenOptions?.(post)}
        >
          <Icon name="ellipsis" size={24} />
        </button>
      </header>

      <div className="post__media" onDoubleClick={() => { if (!post.liked) like(); else triggerBurst(); }}>
        <PostMedia post={post} variant="feed" index={slide} onIndexChange={setSlide}>
          {post.tags && <span className="post__media-badge"><Icon name="tag" size={18} /></span>}
          {burst && <span className="post__heart-burst" aria-hidden="true"><Icon name="heart" size={90} filled /></span>}
        </PostMedia>
      </div>

      <div className="post__actions">
        <div className="post__actions-left">
          <button className={`icon-btn ${post.liked ? 'icon-btn--liked' : ''}`} type="button" aria-label={post.liked ? 'Unlike' : 'Like'} onClick={like}>
            <Icon name="heart" size={26} filled={post.liked} strokeWidth={post.liked ? 0 : 1.9} />
          </button>
          <button className="icon-btn" type="button" aria-label="Comment" onClick={() => onOpenDetail(post)}>
            <Icon name="comment" size={26} />
          </button>
          <button className="icon-btn" type="button" aria-label="Share" onClick={() => onOpenDetail(post)}>
            <Icon name="share" size={26} />
          </button>
          <span className="post__action-spacer" />
          {!post.imported && (
            <span className="post__reposts">Repost {formatCount(post.reposts)} · Share {formatCount(post.shares)}</span>
          )}
        </div>
        <button className={`icon-btn ${post.saved ? 'icon-btn--saved' : ''}`} type="button" aria-label={post.saved ? 'Remove from saved' : 'Save'} onClick={() => onToggleSave(post.id)}>
          <Icon name="bookmark" size={26} filled={post.saved} strokeWidth={post.saved ? 0 : 1.9} />
        </button>
      </div>

      <div className="post__body">
        <PostLikes post={post} className="post__likes" />

        {hasCaption && (
          <div className="post__caption">
            <button className="post__username" type="button" onClick={() => onOpenProfile?.(post.username)}>{post.username}</button>{' '}
            {post.verified && <VerifiedBadge size={12} />}{' '}
            {preview !== null ? (
              <>
                <span className="post__caption-text"><RichText text={preview} linkify={post.imported} /></span>
                {'… '}
                <button className="post__caption-more" type="button" onClick={() => setExpanded(true)}>more</button>
              </>
            ) : (
              <span className="post__caption-text"><RichText text={post.caption} linkify={post.imported} /></span>
            )}
            {post.hashtags && (
              <span className="post__hashtags"> {post.hashtags.map((h) => `#${h}`).join(' ')}</span>
            )}
          </div>
        )}

        {commentTotal > 0 && (
          <button className="post__viewcomments" type="button" onClick={() => onOpenDetail(post)}>
            {commentTotal === 1 ? 'View 1 comment' : `View all ${formatCount(commentTotal)} comments`}
          </button>
        )}
        {shownComments.map((c) => (
          <div className="post__comment" key={c.id}>
            <button className="post__username" type="button" onClick={() => onOpenProfile?.(c.username)}>{c.username}</button>{' '}
            <span className="post__comment-text"><RichText text={c.text} linkify={post.imported} /></span>
          </div>
        ))}
        {post.date && <div className="post__date">{post.date}</div>}
      </div>

      <div className="post__addcomment">
        <button className="icon-btn" type="button" aria-label="Emoji"><Icon name="emoji" size={24} /></button>
        <input
          className="post__comment-input"
          placeholder="Add a comment…"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') submitComment(); }}
          aria-label="Add a comment"
        />
        <button className="post__post-btn" type="button" onClick={submitComment} disabled={!draft.trim()}>Post</button>
      </div>
    </article>
  );
}

/** The like count line. An unknown count (imported, hidden likes) is never shown as a number. */
export function PostLikes({ post, className }: { post: Post; className: string }) {
  if (post.likes === null) {
    return post.liked ? <div className={className}>Liked by <strong>you</strong></div> : null;
  }
  return (
    <div className={className}>
      {post.likedBy ? (
        <>Liked by <strong>{post.likedBy}</strong> and <strong>{formatCount(post.likedByCount ?? post.likes)} others</strong></>
      ) : (
        <><strong>{formatCount(post.likes)}</strong> {post.likes === 1 ? 'like' : 'likes'}</>
      )}
    </div>
  );
}

export interface FeedProps {
  posts: Post[];
  stories: Story[];
  extraComments: Record<string, Comment[]>;
  onToggleLike: (id: string) => void;
  onToggleSave: (id: string) => void;
  onOpenDetail: (post: Post) => void;
  onAddComment: (id: string, text: string) => void;
  onOpenProfile?: (username: string) => void;
  onOpenOptions?: (post: Post) => void;
  /** Something covers the feed (a post, a dialog, auto view): its videos stay paused. */
  videosPaused?: boolean;
}

/** Posts rendered at first and added per step while scrolling (an import can hold thousands). */
const FEED_PAGE = 8;

export function Feed({
  posts, stories, extraComments, onToggleLike, onToggleSave, onOpenDetail, onAddComment, onOpenProfile, onOpenOptions,
  videosPaused = false,
}: FeedProps) {
  const { visible, hasMore, sentinel } = useIncremental(posts.length, FEED_PAGE);
  return (
    <FeedPlaybackContext.Provider value={!videosPaused}>
      <div className="feed">
        <Stories stories={stories} onOpenProfile={onOpenProfile} />
        <div className="feed__posts">
          {posts.slice(0, visible).map((p) => (
            <PostCard
              key={p.id}
              post={p}
              extraComments={extraComments[p.id] ?? []}
              onToggleLike={onToggleLike}
              onToggleSave={onToggleSave}
              onOpenDetail={onOpenDetail}
              onAddComment={onAddComment}
              onOpenProfile={onOpenProfile}
              onOpenOptions={onOpenOptions}
            />
          ))}
          {hasMore ? (
            <div className="load-sentinel" ref={sentinel} aria-hidden="true"><span className="spinner" /></div>
          ) : (
            <div className="feed__end">
              <Icon name="compass" size={40} />
              <p>You&apos;re all caught up</p>
              <span>{posts.some((p) => p.imported)
                ? 'You’ve seen every post of this import.'
                : 'You’ve seen all new posts from the past few days.'}
              </span>
            </div>
          )}
        </div>
      </div>
    </FeedPlaybackContext.Provider>
  );
}
