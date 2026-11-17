import { useCallback, useEffect, useRef, useState, type ChangeEvent } from 'react';
import { ARCHIVE_ACCEPT, type ImportProgress, type ImportWarning } from '../import';
import {
  countLabel, failedSources, failedSourcesLine, formatBytes, importedFrom, savedLine, sourceLine, unsavedLine,
  type ImportReport,
} from '../importReport';
import { Icon } from '../icons';
import { useDialogFocus } from './common';

/** State of the one import the app can run at a time. */
export type ImportJob =
  | { status: 'idle' }
  | { status: 'running'; progress: ImportProgress | null }
  | { status: 'error'; message: string }
  | { status: 'success'; report: ImportReport };

/**
 * Where imports go. library: the backend's cache folder (each import is merged into it);
 * browser: this browser only (IndexedDB), because the library cannot be used.
 */
export type StorageMode =
  | { kind: 'checking' }
  | { kind: 'library'; directory: string }
  /**
   * `reason`: why the library cannot be used, when known. `afterSaveFailure`: the library was
   * found at startup, but saving to it failed; this page keeps everything in the browser until it
   * is reloaded.
   */
  | { kind: 'browser'; reason?: string; afterSaveFailure?: boolean };

/** Browser mode, with an import on screen: what a new import does with it. */
export type MergeChoice = 'add' | 'replace';

export interface ImportModalProps {
  job: ImportJob;
  /** Files are being dragged over the window (the window-level handler performs the drop). */
  dragging: boolean;
  storage: StorageMode;
  /** The Add / Replace choice; null when there is nothing to add to (the library always adds). */
  merge: MergeChoice | null;
  onMergeChange: (merge: MergeChoice) => void;
  /** One or several archive files (several are merged into one import). */
  onPickArchives: (files: File[]) => void;
  /** The files of one picked folder. */
  onPickFolder: (files: File[]) => void;
  /** Abort the running import. */
  onCancel: () => void;
  /** Leave the error state and show the pickers again. */
  onRetry: () => void;
  /** Close the dialog (a running import keeps going in the background). */
  onClose: () => void;
  /** Close the dialog after a successful import. */
  onViewFeed: () => void;
}

/** Warnings listed at first; the rest is behind "Show all". */
const MAX_WARNINGS = 5;

const FORMAT_TREE = `my-export/            (a folder, or my-export.zip / .tar / .tar.gz)
├─ post.json
└─ posts/
   ├─ post1.jpg
   ├─ post2.png
   ├─ post3.webp
   ├─ post4.heic        (HEIC / HEIF is converted in the browser)
   ├─ post5.mp4         (videos: MP4, MOV or WebM)
   └─ post5_poster.jpg  (optional poster of the video)`;

const FORMAT_JSON = `{
  "format": "instagram-export",
  "version": 1,
  "posts": [
    {
      "url": "https://www.instagram.com/p/DBx1abcDEF/",
      "image": "posts/post1.jpg",
      "user": { "username": "natgeo" },
      "caption": "The post description",
      "comment_count": 1234,
      "like_count": 56789,
      "taken_at": "2026-11-01T15:04:05Z"
    },
    {
      "url": "https://www.instagram.com/reel/C5xYzAbCdEf/",
      "media_type": "video",
      "media": [
        { "type": "video", "path": "posts/post5.mp4", "poster": "posts/post5_poster.jpg" }
      ],
      "user": { "username": "natgeo" },
      "caption": "A video post",
      "comment_count": 12,
      "like_count": 340,
      "taken_at": "2026-10-01T12:00:00Z"
    }
  ]
}`;

/** The problems of a finished import: the first few, and all of them on request. */
function ImportWarnings({ warnings }: { warnings: ImportWarning[] }) {
  const [showAll, setShowAll] = useState(false);
  const shown = showAll ? warnings : warnings.slice(0, MAX_WARNINGS);
  return (
    <div className="import-warnings" data-testid="import-warnings">
      <p className="import-warnings__title">
        <Icon name="warning" size={16} /> {countLabel(warnings.length, 'warning')}
      </p>
      <ul id="import-warnings-list">
        {shown.map((warning, i) => <li key={i}>{warning.message}</li>)}
      </ul>
      {warnings.length > MAX_WARNINGS && (
        <button
          className="link-blue link-small import-warnings__more"
          type="button"
          aria-expanded={showAll}
          aria-controls="import-warnings-list"
          onClick={() => setShowAll((all) => !all)}
        >
          {showAll ? 'Show fewer' : `Show all ${countLabel(warnings.length, 'warning')}`}
        </button>
      )}
    </div>
  );
}

/** The archives / folders of a merged import, one line each. */
function ImportSources({ sources }: { sources: ImportReport['sources'] }) {
  return (
    <div className="import-sources" data-testid="import-sources">
      <p className="import-sources__title">{countLabel(sources.length, 'source')}</p>
      <ul>
        {sources.map((source, i) => (
          <li key={i} className={source.error ? 'import-sources__failed' : undefined}>{sourceLine(source)}</li>
        ))}
      </ul>
    </div>
  );
}

export function ImportModal({
  job, dragging, storage, merge, onMergeChange, onPickArchives, onPickFolder, onCancel, onRetry, onClose, onViewFeed,
}: ImportModalProps) {
  const dialogRef = useDialogFocus<HTMLDivElement>();
  const archiveInput = useRef<HTMLInputElement>(null);
  const folderInput = useRef<HTMLInputElement | null>(null);
  const primaryAction = useRef<HTMLButtonElement>(null);

  // React has no typed prop for directory pickers, so the attributes are set on the element itself.
  const setFolderInput = useCallback((node: HTMLInputElement | null) => {
    folderInput.current = node;
    if (node) {
      node.setAttribute('webkitdirectory', '');
      node.setAttribute('directory', '');
    }
  }, []);

  // The focused control disappears when the state changes: keep focus inside the dialog.
  const { status } = job;
  const commitPhase = job.status === 'running' && job.progress?.phase === 'saving';
  useEffect(() => {
    const node = dialogRef.current;
    if (!node) return;
    if (status === 'success' || status === 'error') primaryAction.current?.focus();
    else if (!node.contains(document.activeElement)) node.focus({ preventScroll: true });
  }, [status, commitPhase, dialogRef]);

  // input.files is live: copy it before resetting the value (so the same pick can be repeated).
  const onArchiveChange = (e: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.currentTarget.files ?? []);
    e.currentTarget.value = '';
    if (files.length > 0) onPickArchives(files);
  };
  const onFolderChange = (e: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.currentTarget.files ?? []);
    e.currentTarget.value = '';
    if (files.length > 0) onPickFolder(files);
  };

  let statusText = '';
  if (job.status === 'running') {
    statusText = job.progress?.message || 'Reading the export…';
  } else if (job.status === 'error') {
    statusText = job.message;
  } else if (job.status === 'success') {
    const { report } = job;
    statusText = `Imported ${report.posts.toLocaleString('en-US')} of ${countLabel(report.totalEntries, 'post')} from ${importedFrom(report)}`;
  }

  const progress = job.status === 'running' ? job.progress : null;
  const determinate = !!progress && progress.total > 0;
  const percent = determinate ? Math.round(Math.min(1, Math.max(0, progress.done / progress.total)) * 100) : 0;
  const report = job.status === 'success' ? job.report : null;
  /** Some archives / folders of a merged import could not be imported: a partial success. */
  const partial = !!report && (failedSources(report) > 0 || !!report.unsaved);
  /** The library is merging the upload: too late to cancel. */
  const committing = progress?.phase === 'saving';

  return (
    <div className="import-overlay" onClick={onClose} role="presentation">
      <div
        className="import-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="import-modal-title"
        data-testid="import-modal"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <header className="import-modal__head">
          <h2 id="import-modal-title">Import from Instagram export</h2>
          <button className="import-modal__close icon-btn" type="button" aria-label="Close" data-testid="import-close" onClick={onClose}>
            <Icon name="close" size={20} />
          </button>
        </header>

        <input
          ref={archiveInput}
          className="visually-hidden"
          type="file"
          multiple
          accept={ARCHIVE_ACCEPT}
          aria-label="Archive files"
          data-testid="import-archive-input"
          tabIndex={-1}
          onChange={onArchiveChange}
        />
        <input
          ref={setFolderInput}
          className="visually-hidden"
          type="file"
          multiple
          aria-label="Export folder"
          data-testid="import-folder-input"
          tabIndex={-1}
          onChange={onFolderChange}
        />

        <div className={`import-modal__body import-modal__body--${status}`}>
          {status === 'idle' && (
            <div className={`import-drop ${dragging ? 'import-drop--active' : ''}`} data-testid="import-dropzone">
              <span className="import-drop__glyph"><Icon name="import" size={56} strokeWidth={1.4} /></span>
              <p className="import-drop__title">Drag your Instagram export here</p>
              <p className="import-drop__hint">
                One or more .zip, .tar or .tar.gz archives, or export folders. Several are merged into one import.
              </p>
              <div className="import-drop__actions">
                <button className="btn-blue import-btn" type="button" onClick={() => archiveInput.current?.click()}>
                  <Icon name="archive" size={18} /> Select archive files
                </button>
                <button className="btn-blue btn-blue--ghost import-btn" type="button" onClick={() => folderInput.current?.click()}>
                  <Icon name="folder" size={18} /> Select folder
                </button>
              </div>
            </div>
          )}

          {status === 'running' && <span className="spinner import-modal__spinner" aria-hidden="true" />}
          {status === 'error' && <span className="import-modal__glyph import-modal__glyph--error"><Icon name="warning" size={44} strokeWidth={1.5} /></span>}
          {status === 'success' && (partial ? (
            <span className="import-modal__glyph import-modal__glyph--partial" data-testid="import-partial">
              <Icon name="warning" size={44} strokeWidth={1.5} />
            </span>
          ) : (
            <span className="import-modal__glyph import-modal__glyph--success"><Icon name="check" size={44} strokeWidth={1.8} /></span>
          ))}

          <p
            className="import-modal__status"
            role={status === 'error' ? 'alert' : 'status'}
            aria-live={status === 'error' ? 'assertive' : 'polite'}
            data-testid="import-status"
            data-state={status}
          >
            {statusText}
          </p>

          {status === 'idle' && merge && (
            <fieldset className="import-merge" data-testid="import-merge">
              <legend>The posts on screen</legend>
              <label>
                <input type="radio" name="import-merge" value="add" checked={merge === 'add'} onChange={() => onMergeChange('add')} />
                Add to current import
              </label>
              <label>
                <input type="radio" name="import-merge" value="replace" checked={merge === 'replace'} onChange={() => onMergeChange('replace')} />
                Replace current import
              </label>
            </fieldset>
          )}

          {status === 'idle' && (
            <>
              <p className="import-modal__note" data-testid="import-storage-note">
                {storage.kind === 'library' ? (
                  <>
                    Imports are added to your library, which this app&apos;s server keeps in{' '}
                    <span className="import-modal__path">{storage.directory}</span>. Nothing is sent to Instagram.
                  </>
                ) : storage.kind === 'browser' ? (
                  <>
                    {storage.reason && <>{storage.reason} </>}
                    Everything stays in this browser. Nothing is uploaded.
                    {storage.afterSaveFailure && <> Reload the page to save to the library again.</>}
                  </>
                ) : 'Looking for your library…'}
              </p>
              <details className="import-format">
                <summary>Expected format</summary>
                <p>
                  A folder (or an archive of it) with a <code>post.json</code> manifest next to a <code>posts/</code> folder
                  that holds the pictures and videos:
                </p>
                <pre className="import-format__code" aria-label="Folder layout">{FORMAT_TREE}</pre>
                <p>
                  <code>post.json</code> lists the posts: the image or video paths, who the post is from, the total number
                  of comments, the caption (the author&apos;s first comment) and the link to the real post:
                </p>
                <pre className="import-format__code" aria-label="Example post.json">{FORMAT_JSON}</pre>
                <p>
                  Videos (<code>media</code> entries of type <code>video</code>), carousels (<code>images</code>, or
                  <code> media</code> mixing pictures and videos), profile pictures and sample comments are optional. The
                  guide <code>instagram.md</code> in the project root explains the full format and how a browser MCP client
                  can export Instagram posts into it.
                </p>
              </details>
            </>
          )}

          {status === 'running' && (
            <>
              <div
                className={`import-progress ${determinate ? '' : 'import-progress--indeterminate'}`}
                role="progressbar"
                aria-label={progress?.phase === 'uploading' || committing ? 'Saving progress' : 'Import progress'}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={determinate ? percent : undefined}
                data-testid="import-progress"
                data-phase={progress?.phase}
              >
                <span className="import-progress__bar" style={determinate ? { width: `${percent}%` } : undefined} />
              </div>
              {determinate && (
                <p className="import-modal__note">
                  {progress.phase === 'uploading'
                    ? `${formatBytes(progress.done)} of ${formatBytes(progress.total)}`
                    : `${progress.done.toLocaleString('en-US')} of ${progress.total.toLocaleString('en-US')}`}
                </p>
              )}
              {committing ? (
                <p className="import-modal__note" data-testid="import-committing">Almost done: this can&apos;t be cancelled any more.</p>
              ) : (
                <button className="btn-blue btn-blue--ghost import-btn" type="button" onClick={onCancel}>Cancel</button>
              )}
            </>
          )}

          {status === 'error' && (
            <button className="btn-blue import-btn" type="button" ref={primaryAction} onClick={onRetry}>Try again</button>
          )}

          {report && (
            <>
              {report.saved && (
                <p className="import-modal__note" data-testid="import-saved">
                  Saved in your library ({savedLine(report.saved)}):{' '}
                  <span className="import-modal__path">{report.saved.directory}</span>
                </p>
              )}
              {report.saveError && (
                <p className="import-save-error" role="alert" data-testid="import-save-error">
                  <Icon name="warning" size={16} />
                  <span>Not saved in the library. {report.saveError} These posts are shown from this browser only.</span>
                </p>
              )}
              {report.unsaved && (
                <p className="import-save-error" role="alert" data-testid="import-unsaved">
                  <Icon name="warning" size={16} />
                  <span>{unsavedLine(report.unsaved)}</span>
                </p>
              )}
              {failedSources(report) > 0 && (
                <p className="import-modal__note import-modal__note--warning" data-testid="import-partial-note">
                  <Icon name="warning" size={14} /> {failedSourcesLine(report)}
                </p>
              )}
              {report.sources.length > 1 && <ImportSources sources={report.sources} />}
              {report.warnings.length > 0 && <ImportWarnings warnings={report.warnings} />}
              <button className="btn-blue import-btn" type="button" ref={primaryAction} onClick={onViewFeed}>View feed</button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
