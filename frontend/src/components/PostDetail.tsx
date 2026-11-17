import { useEffect, useState } from 'react';
import { formatCount, postPhotos, type Comment, type Post } from '../data';
import { Icon } from '../icons';
import { placeholderKey } from '../importedPosts';
import { Avatar, ExternalLink, RichText, useDialogFocus, VerifiedBadge } from './common';
import { PostLikes } from './Feed';
import { PostMedia, PostTile, PostTime, ViewOnInstagram } from './Media';

export interface PostDetailProps {
  post: Post;
  /** The posts currently shown by the app (seed data or the import), for "More posts from". */
  posts: Post[];
  extraComments: Comment[];
  onClose: () => void;
  onToggleLike: (id: string) => void;
  onToggleSave: (id: string) => void;
  onAddComment: (id: string, text: string) => void;
  onOpenProfile?: (username: string) => void;
  onOpenDetail?: (post: Post) => void;
  /** Open the "More options" action sheet for this post. */
  onOpenOptions?: (post: Post) => void;
}

const MORE_POSTS = 4;

export function PostDetail({
  post,
  posts,
  extraComments,
  onClose,
  onToggleLike,
  onToggleSave,
  onAddComment,
  onOpenProfile,
  onOpenDetail,
  onOpenOptions,
}: PostDetailProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();
  const [draft, setDraft] = useState('');
  const [slide, setSlide] = useState(0);
  const comments = [...post.comments, ...extraComments];
  const slideCount = postPhotos(post).length;
  // An import knows who posted what; the seed data only has one post per account, so it shows the others.
  const morePosts = posts
    .filter((p) => p.id !== post.id && (!post.imported || p.username === post.username))
    .slice(0, MORE_POSTS);
  const hasCaption = post.caption.trim().length > 0 || (post.hashtags?.length ?? 0) > 0;
  const hiddenComments = post.commentCount > post.comments.length;

  // ArrowLeft / ArrowRight page through a carousel, unless the user is typing or another dialog is on top.
  useEffect(() => {
    if (slideCount < 2) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest('input, textarea, select, [contenteditable="true"]')) return;
      const dialog = target?.closest('[aria-modal="true"]');
      if (dialog && dialog !== dialogRef.current) return;
      const step = e.key === 'ArrowRight' ? 1 : -1;
      setSlide((current) => Math.min(Math.max(current + step, 0), slideCount - 1));
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [slideCount, dialogRef]);

  const submit = () => {
    const text = draft.trim();
    if (!text) return;
    onAddComment(post.id, text);
    setDraft('');
  };

  return (
    <div className="pd-overlay" onClick={onClose} role="presentation">
      {/* The dialog is the whole post: Close / Back (fixed to the window), the card and "More posts". */}
      <div
        className="pd-wrap"
        role="dialog"
        aria-modal="true"
        aria-label={`Post by ${post.username}`}
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <button className="pd-close" type="button" aria-label="Close" onClick={onClose}><Icon name="close" size={30} /></button>
        <button className="pd-topback" type="button" aria-label="Back" onClick={onClose}><Icon name="back" size={24} /></button>

        <div className="pd-card" data-post-id={post.id}>
          <div className="pd-media">
            <PostMedia post={post} variant="detail" index={slide} onIndexChange={setSlide}>
              {post.tags && <span className="pd-media__badge"><Icon name="tag" size={18} /></span>}
            </PostMedia>
          </div>

          <div className="pd-side">
            <header className="pd-header">
              <Avatar username={post.username} avatar={post.photo.key} src={post.avatarSrc} size={34} ring="story" onClick={() => onOpenProfile?.(post.username)} />
              <div className="pd-header__meta">
                <button className="post__username" type="button" onClick={() => onOpenProfile?.(post.username)}>{post.username}</button>
                {post.verified && <VerifiedBadge />}
                {post.time && <span className="post__sep">·</span>}
                <PostTime post={post} />
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

            {post.location && <div className="pd-location">{post.location}</div>}

            {post.tags && (
              <div className="pd-tags">
                <span className="pd-tags__label"><Icon name="tag" size={15} /> Tags</span>
                <span>{post.tags.join(' ')}</span>
              </div>
            )}

            <div className="pd-comments">
              {hasCaption && (
                <div className="pd-comment pd-comment--caption">
                  <Avatar username={post.username} avatar={post.photo.key} src={post.avatarSrc} size={32} />
                  <div className="pd-comment__body">
                    <span>
                      <button className="post__username" type="button" onClick={() => onOpenProfile?.(post.username)}>{post.username}</button>
                      {' '}{post.verified && <VerifiedBadge size={12} />}{' '}
                      <span className="post__caption-text"><RichText text={post.caption} linkify={post.imported} /></span>
                    </span>
                    {post.hashtags && <span className="post__hashtags"> {post.hashtags.map((h) => `#${h}`).join(' ')}</span>}
                    {(post.time || !post.imported) && (
                      <div className="pd-comment__sub">
                        {post.time}
                        {!post.imported && <> · <button type="button">See translation</button></>}
                      </div>
                    )}
                  </div>
                </div>
              )}

              {comments.map((c) => (
                <div className="pd-comment" key={c.id}>
                  <Avatar username={c.username} avatar={post.imported ? placeholderKey(c.username) : post.photo.key} size={32} />
                  <div className="pd-comment__body">
                    <span>
                      <button className="post__username" type="button" onClick={() => onOpenProfile?.(c.username)}>{c.username}</button>
                      {' '}
                      <span className="post__comment-text"><RichText text={c.text} linkify={post.imported} /></span>
                    </span>
                    <div className="pd-comment__sub">
                      {c.time && <span>{c.time}</span>}
                      {c.likes > 0 && <span>{formatCount(c.likes)} {c.likes === 1 ? 'like' : 'likes'}</span>}
                      <button type="button">Reply</button>
                      {!post.imported && /[^\x00-\x7F]/.test(c.text) && <button type="button">See translation</button>}
                    </div>
                    {c.replies && <button className="pd-comment__replies" type="button">View all {c.replies} {c.replies === 1 ? 'reply' : 'replies'}</button>}
                  </div>
                  <button className="pd-comment__like" type="button" aria-label="Like comment"><Icon name="heart" size={13} /></button>
                </div>
              ))}

              {post.permalink && hiddenComments && (
                <ExternalLink className="pd-viewall" href={post.permalink} data-testid="post-comments-link">
                  {post.commentCount === 1
                    ? 'View 1 comment on Instagram'
                    : `View all ${post.commentCount.toLocaleString('en-US')} comments on Instagram`}
                </ExternalLink>
              )}
            </div>

            <div className="pd-actions">
              <div className="post__actions-left">
                <button className={`icon-btn ${post.liked ? 'icon-btn--liked' : ''}`} type="button" aria-label={post.liked ? 'Unlike' : 'Like'} onClick={() => onToggleLike(post.id)}>
                  <Icon name="heart" size={26} filled={post.liked} strokeWidth={post.liked ? 0 : 1.9} />
                </button>
                <button className="icon-btn" type="button" aria-label="Comment"><Icon name="comment" size={26} /></button>
                <button className="icon-btn" type="button" aria-label="Share"><Icon name="share" size={26} /></button>
              </div>
              <button className={`icon-btn ${post.saved ? 'icon-btn--saved' : ''}`} type="button" aria-label={post.saved ? 'Remove from saved' : 'Save'} onClick={() => onToggleSave(post.id)}>
                <Icon name="bookmark" size={26} filled={post.saved} strokeWidth={post.saved ? 0 : 1.9} />
              </button>
            </div>

            <PostLikes post={post} className="pd-likes" />
            {(post.date ?? post.time) && <div className="pd-date">{post.date ?? post.time}</div>}

            <div className="post__addcomment pd-addcomment">
              <button className="icon-btn" type="button" aria-label="Emoji"><Icon name="emoji" size={24} /></button>
              <input
                className="post__comment-input"
                placeholder="Add a comment…"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
                aria-label="Add a comment"
              />
              <button className="post__post-btn" type="button" onClick={submit} disabled={!draft.trim()}>Post</button>
            </div>
          </div>
        </div>

        {morePosts.length > 0 && (
          <div className="pd-more">
            <div className="pd-more__head">More posts from <strong>{post.username}</strong></div>
            <div className="pd-more__grid">
              {morePosts.map((p) => (
                <PostTile key={p.id} post={p} className="pd-more__tile" stats={false} onOpen={(next) => onOpenDetail?.(next)} />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
