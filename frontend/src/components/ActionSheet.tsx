import { copyText } from '../clipboard';
import type { Post } from '../data';
import { siteName } from '../importedPosts';
import { ExternalLink, useDialogFocus } from './common';

export interface PostOptionsSheetProps {
  post: Post;
  /** Offer "Go to post" (the sheet was opened from the feed, not from the post itself). */
  canGoToPost: boolean;
  onClose: () => void;
  onGoToPost: (post: Post) => void;
  onOpenProfile: (username: string) => void;
  /** Start auto view at this post. */
  onAutoView: (post: Post) => void;
  /** Open the editor of the post's description. */
  onEditCaption: (post: Post) => void;
  /** Download the post as an export archive; absent when the post has no files to download. */
  onDownload?: (post: Post) => void;
  onToast: (message: string) => void;
}

/** The Instagram-style action sheet behind a post's "More options" button. */
export function PostOptionsSheet({
  post, canGoToPost, onClose, onGoToPost, onOpenProfile, onAutoView, onEditCaption, onDownload, onToast,
}: PostOptionsSheetProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();

  // A link must still be in the document when the browser follows it, so the sheet closes a tick later.
  const closeSoon = () => { window.setTimeout(onClose, 0); };

  const copyLink = async () => {
    if (!post.permalink) return;
    const ok = await copyText(post.permalink);
    onToast(ok ? 'Link copied to clipboard.' : 'Could not copy the link.');
    onClose();
  };

  return (
    <div className="sheet-overlay" onClick={onClose} role="presentation">
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-label="Post options"
        data-testid="post-options"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        {post.permalink && (
          <ExternalLink className="sheet__item sheet__item--accent" href={post.permalink} onClick={closeSoon}>
            View on {siteName(post.permalink)}
          </ExternalLink>
        )}
        {post.profileUrl && (
          <ExternalLink className="sheet__item" href={post.profileUrl} onClick={closeSoon}>
            View profile on {siteName(post.profileUrl)}
          </ExternalLink>
        )}
        {post.permalink && (
          <button className="sheet__item" type="button" onClick={() => { void copyLink(); }}>Copy link</button>
        )}
        {canGoToPost && (
          <button className="sheet__item" type="button" onClick={() => { onClose(); onGoToPost(post); }}>Go to post</button>
        )}
        <button
          className="sheet__item"
          type="button"
          data-testid="post-edit-caption"
          onClick={() => { onClose(); onEditCaption(post); }}
        >
          Edit description
        </button>
        {onDownload && (
          <button
            className="sheet__item"
            type="button"
            data-testid="post-download"
            onClick={() => { onClose(); onDownload(post); }}
          >
            Download post
          </button>
        )}
        <button className="sheet__item" type="button" onClick={() => { onClose(); onOpenProfile(post.username); }}>
          About this account
        </button>
        <button className="sheet__item" type="button" onClick={() => { onClose(); onAutoView(post); }}>
          Auto view from here
        </button>
        <button className="sheet__item" type="button" onClick={onClose}>Cancel</button>
      </div>
    </div>
  );
}
