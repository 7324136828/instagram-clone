import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react';
import type { Post } from '../data';
import { MAX_CAPTION_CHARS } from '../import';
import { useDialogFocus } from './common';

export interface EditCaptionDialogProps {
  post: Post;
  /** The change is being saved: the field and the buttons are disabled. */
  busy?: boolean;
  /** Why the last attempt to save failed. */
  error?: string;
  /** Where the change is kept, shown under the field. */
  note?: string;
  onSave: (caption: string) => void;
  onCancel: () => void;
}

/** Edits a post's description (its caption), in the style of the post action sheet. */
export function EditCaptionDialog({ post, busy = false, error, note, onSave, onCancel }: EditCaptionDialogProps) {
  const dialogRef = useDialogFocus<HTMLFormElement>();
  const fieldRef = useRef<HTMLTextAreaElement>(null);
  const [text, setText] = useState(post.caption);
  const caption = text.trim();
  const changed = caption !== post.caption.trim();

  // After useDialogFocus has noted where focus came from: start typing at the end of the text.
  useEffect(() => {
    const field = fieldRef.current;
    if (!field) return;
    field.focus({ preventScroll: true });
    field.setSelectionRange(field.value.length, field.value.length);
  }, []);

  const save = () => {
    if (!busy && changed) onSave(caption);
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    save();
  };
  // Enter makes a new line, as in a caption; Ctrl+Enter (Cmd+Enter) saves.
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      save();
    }
  };

  return (
    <div className="sheet-overlay" onClick={busy ? undefined : onCancel} role="presentation">
      <form
        className="sheet edit-caption"
        role="dialog"
        aria-modal="true"
        aria-labelledby="edit-caption-title"
        aria-busy={busy}
        data-testid="edit-caption"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
        onSubmit={submit}
      >
        <div className="edit-caption__body">
          <h2 id="edit-caption-title" className="confirm__title">Edit description</h2>
          <p className="edit-caption__author">Post by <strong>{post.username}</strong></p>
          <textarea
            ref={fieldRef}
            className="edit-caption__field"
            aria-label="Description"
            data-testid="edit-caption-field"
            value={text}
            maxLength={MAX_CAPTION_CHARS}
            rows={7}
            disabled={busy}
            placeholder="Write a description…"
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKeyDown}
          />
          <div className="edit-caption__meta">
            <span>{note}</span>
            <span className="edit-caption__count" aria-live="polite">
              {text.length.toLocaleString('en-US')} / {MAX_CAPTION_CHARS.toLocaleString('en-US')}
            </span>
          </div>
          {error && <p className="edit-caption__error" role="alert" data-testid="edit-caption-error">{error}</p>}
        </div>
        <button className="sheet__item sheet__item--accent" type="submit" disabled={busy || !changed} data-testid="edit-caption-save">
          {busy ? 'Saving…' : 'Save'}
        </button>
        <button className="sheet__item" type="button" disabled={busy} onClick={onCancel}>Cancel</button>
      </form>
    </div>
  );
}
