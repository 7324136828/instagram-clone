import type { ReactNode } from 'react';
import { useDialogFocus } from './common';

export interface ConfirmDialogProps {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  /** The action is running: both buttons are disabled. */
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/** A question before a destructive action, in the style of the post action sheet. */
export function ConfirmDialog({ title, children, confirmLabel, busy = false, onConfirm, onCancel }: ConfirmDialogProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();
  return (
    <div className="sheet-overlay" onClick={busy ? undefined : onCancel} role="presentation">
      <div
        className="sheet confirm"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-message"
        aria-busy={busy}
        data-testid="confirm-dialog"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="confirm__text">
          <h2 id="confirm-title" className="confirm__title">{title}</h2>
          <div id="confirm-message" className="confirm__message">{children}</div>
        </div>
        <button className="sheet__item sheet__item--danger" type="button" disabled={busy} onClick={onConfirm}>
          {busy ? 'Working…' : confirmLabel}
        </button>
        <button className="sheet__item" type="button" disabled={busy} onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}
