import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  currentUser, posts as seedPosts, recentSearches, stories as seedStories,
  type Comment, type Photo, type Post, type Story,
} from './data';
import {
  ImportError, clearSavedImport, exportPosts, importFromArchive, importFromDataTransfer, importFromFiles, importMany,
  libraryFileUrl, loadSavedImport, mergeImportResults, prepareLibraryUploads, releaseImport, resultFromLibrary,
  saveImport,
  type ExportResult, type ImportOptions, type ImportProgress, type ImportResult, type ImportedPost, type LibraryResponse,
} from './import';
import {
  buildUserDirectory, importToPosts, safeLinks, siteName, storiesFromPosts, type UserDirectory,
} from './importedPosts';
import {
  commitCounts, commitMessage, countLabel, expectedPosts, reportOf, savedReport, unsavedLine, uploadMessage,
  type ImportReport,
} from './importReport';
import { LibraryError, libraryClient } from './library/client';
import {
  loadAutoViewSettings, parseAutoViewHash, saveAutoViewSettings, type AutoViewSettings,
} from './autoView';
import { AutoViewPlayer, AutoViewSetup } from './components/AutoView';
import { Sidebar, type Panel, type Theme, type View } from './components/Sidebar';
import { Feed } from './components/Feed';
import { RightRail } from './components/RightRail';
import { PostDetail } from './components/PostDetail';
import {
  CreateModal, ExploreGrid, MessagesPanel, NotificationsPanel,
  ProfileView, ReelsView, SearchPanel, importedSearchUsers, seedSearchUsers,
} from './components/Panels';
import {
  ImportModal, type ImportJob, type MergeChoice, type StorageMode,
} from './components/ImportModal';
import { PostOptionsSheet } from './components/ActionSheet';
import { ConfirmDialog } from './components/ConfirmDialog';
import { EditCaptionDialog } from './components/EditCaptionDialog';
import { clearImageCache, ExternalLink, UserDirectoryContext } from './components/common';
import { Icon } from './icons';

function readTheme(): Theme {
  const saved = localStorage.getItem('ig-theme');
  return saved === 'dark' ? 'dark' : 'light';
}

/** The library behind the posts on screen (library mode). */
interface LibraryInfo {
  directory: string;
  /** Import actions the library was built from. */
  imports: number;
}

/** The import on screen: the importer's result plus everything the UI derives from it. */
interface ActiveImport {
  /** Owns the object URLs behind every picture of `posts` (a library result holds server URLs). */
  result: ImportResult;
  /** The last import action of this session, or the summary of what was restored at startup. */
  report: ImportReport;
  /** Set in library mode: the posts are the library in the backend's cache folder. */
  library?: LibraryInfo;
  /** Saving the shown posts to the library failed: they live in this browser only. */
  saveError?: string;
  /** Feed posts, including local likes / saves and posts shared while the import is shown. */
  posts: Post[];
  directory: UserDirectory;
  stories: Story[];
  /** Changes with every import so list state (paging, seen stories) starts over. */
  seq: number;
  /** Comments the user wrote on these posts, by post id. They live and die with the import. */
  comments: Record<string, Comment[]>;
  /** Accounts of this import that were opened from the search panel, latest first. */
  recent: string[];
}

/** A finished import action, ready to be put on screen. */
interface ImportOutcome {
  result: ImportResult;
  report: ImportReport;
  /** The library after the commit: `result` is built from it. */
  library?: LibraryResponse;
  /** Saving to the library failed: `result` is the parsed import, shown in browser mode. */
  saveFailed?: boolean;
}

interface Toast {
  id: number;
  message: string;
}

type ImportRunner = (options: ImportOptions) => Promise<ImportResult>;

interface ImportRunOptions {
  /** The run saves the import kept in this browser (IndexedDB) to the library. */
  fromBrowserCopy?: boolean;
}

type AutoViewState =
  | { phase: 'setup'; startIndex: number }
  | { phase: 'running'; startIndex: number; settings: AutoViewSettings };

const PROGRESS_INTERVAL_MS = 60;

const EMPTY_DIRECTORY: UserDirectory = new Map();

const MAX_RECENT = 8;

function isAbortError(error: unknown): boolean {
  if (error instanceof ImportError) return error.code === 'aborted';
  if (error instanceof LibraryError) return error.aborted;
  return error instanceof DOMException && error.name === 'AbortError';
}

/** releaseImport must never take the UI down with it. */
function safeRelease(result: ImportResult): void {
  try {
    releaseImport(result);
  } catch {
    // Nothing useful can be done: the URLs die with the document at the latest.
  }
}

function libraryResult(library: LibraryResponse): ImportResult {
  return resultFromLibrary(library, (path) => libraryFileUrl(path, library.revision));
}

function libraryInfo(library: LibraryResponse): LibraryInfo {
  return { directory: library.directory, imports: library.imports.length };
}

/**
 * Saves a parsed import in the library: upload plans (one per part; most imports need a single
 * part, see prepareLibraryUploads), then for each part one PUT per file with progress and a commit.
 * The outcome shows the library after the last commit; when saving fails before anything was
 * committed it shows the parsed import in browser mode and says why. Cancelling rejects (the
 * session is dropped by the client) until the first commit is sent; a commit cannot be cancelled
 * (progress phase 'saving'). When a later part fails or is cancelled, the parts already committed
 * stay in the library: the outcome shows the library and says how many posts were not saved.
 */
async function saveToLibrary(
  parsed: ImportResult,
  signal: AbortSignal,
  onProgress: (progress: ImportProgress) => void,
): Promise<ImportOutcome> {
  const report = reportOf(parsed);
  let library: LibraryResponse | null = null;
  let savedPosts = 0;
  const counts = { added: 0, updated: 0 };
  try {
    onProgress({ phase: 'uploading', done: 0, total: 0, message: 'Preparing to save to your library…' });
    const plans = await prepareLibraryUploads(parsed);
    let totalBytes = 0;
    let totalFiles = 0;
    for (const plan of plans) {
      totalFiles += plan.files.length;
      for (const file of plan.files) totalBytes += file.blob.size;
    }
    let bytesBefore = 0;
    let filesBefore = 0;
    for (const [index, plan] of plans.entries()) {
      // Cancelled while the previous part was being committed: stop before the next part.
      if (library && signal.aborted) throw new ImportError('aborted', 'The rest of the import was cancelled.');
      const part = { part: index + 1, parts: plans.length };
      const committed: LibraryResponse = await libraryClient.upload(plan, {
        signal,
        onProgress: (progress) => {
          const overall = { ...progress, loaded: bytesBefore + progress.loaded, total: totalBytes, file: filesBefore + progress.file, files: totalFiles };
          onProgress({ phase: 'uploading', done: overall.loaded, total: overall.total, message: uploadMessage(overall, part) });
        },
        onCommit: () => onProgress({ phase: 'saving', done: 0, total: 0, message: commitMessage(part.part, part.parts) }),
      });
      library = committed;
      savedPosts += plan.commit.posts.length;
      const latest = commitCounts(committed);
      counts.added += latest.added;
      counts.updated += latest.updated;
      for (const file of plan.files) bytesBefore += file.blob.size;
      filesBefore += plan.files.length;
    }
    // The library serves its own copies now: the browser's copies are not shown any more.
    safeRelease(parsed);
    const done = library ?? await libraryClient.load();
    return { result: libraryResult(done), report: savedReport(report, done, counts), library: done };
  } catch (error) {
    const aborted = signal.aborted || isAbortError(error);
    const reason = aborted
      ? 'The rest of the import was cancelled.'
      : error instanceof LibraryError || error instanceof ImportError
        ? error.message
        : 'The files could not be sent to the server.';
    if (library) {
      // Earlier parts are in the library and cannot be taken back: show the library they built.
      safeRelease(parsed);
      return {
        result: libraryResult(library),
        report: { ...savedReport(report, library, counts), unsaved: { saved: savedPosts, posts: report.posts, reason } },
        library,
      };
    }
    if (aborted) {
      safeRelease(parsed);
      throw error;
    }
    return { result: parsed, report: { ...report, saveError: reason }, saveFailed: true };
  }
}

/** The description editor: which post, and how saving it went. */
interface CaptionEdit {
  id: string;
  busy: boolean;
  error?: string;
}

/** A download being prepared (see exportPosts). */
interface DownloadJob {
  /** Every post on screen ("Download all"), or a single post. */
  all: boolean;
  done: number;
  total: number;
}

/** Hands a file to the browser's download manager. */
function saveFile(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.rel = 'noopener';
  link.hidden = true;
  document.body.appendChild(link);
  link.click();
  link.remove();
  // The browser reads the file after click() returns: the URL is revoked much later.
  window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** What a finished download tells the user. */
function downloadMessage(result: ExportResult): string {
  const { archives, problems } = result;
  if (archives.length === 0) return `Nothing could be downloaded. ${problems[0] ?? ''}`.trim();
  const saved = archives.length === 1
    ? `Downloaded ${countLabel(result.posts, 'post')} as ${archives[0].name}.`
    : `Downloaded ${countLabel(result.posts, 'post')} in ${archives.length} archives (${archives[0].name} …).`;
  if (problems.length === 0) return saved;
  const more = problems.length > 1 ? ` (and ${countLabel(problems.length - 1, 'more problem')})` : '';
  return `${saved} Left out: ${problems[0]}${more}`;
}

/** Removes "#autoview=..." from the address without adding a history entry. */
function dropAutoViewHash(): void {
  if (parseAutoViewHash(window.location.hash) === null) return;
  window.history.replaceState(window.history.state, '', `${window.location.pathname}${window.location.search}`);
}

export default function App() {
  const [view, setView] = useState<View>('home');
  const [panel, setPanel] = useState<Panel | null>(null);
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [moreOpen, setMoreOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [selectedPostId, setSelectedPostId] = useState<string | null>(null);
  const [seedState, setSeedState] = useState<Post[]>(seedPosts);
  const [activeImport, setActiveImport] = useState<ActiveImport | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [job, setJob] = useState<ImportJob>({ status: 'idle' });
  const [dragging, setDragging] = useState(false);
  const [optionsFor, setOptionsFor] = useState<{ id: string; fromDetail: boolean } | null>(null);
  /** A finished import the user has not looked at yet (see settleImport). */
  const [parked, setParked] = useState<ImportOutcome | null>(null);
  /** Comments written on the seed posts; the ones on imported posts are in `activeImport`. */
  const [seedComments, setSeedComments] = useState<Record<string, Comment[]>>({});
  const [following, setFollowing] = useState<Set<string>>(new Set(['nuitcourte']));
  const [query, setQuery] = useState('');
  /** Recent searches of the seed data; the ones of an import are in `activeImport`. */
  const [seedRecent, setSeedRecent] = useState<string[]>(recentSearches);
  const [profileUsername, setProfileUsername] = useState<string>(currentUser.username);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [storage, setStorage] = useState<StorageMode>({ kind: 'checking' });
  /** Browser mode: whether a new import is added to the one on screen or replaces it. */
  const [merge, setMerge] = useState<MergeChoice>('add');
  /** The library check and the restore of a saved import are done (the #autoview hash waits for it). */
  const [startupDone, setStartupDone] = useState(false);
  const [autoView, setAutoView] = useState<AutoViewState | null>(null);
  const [autoSettings, setAutoSettings] = useState<AutoViewSettings>(loadAutoViewSettings);
  const [clearState, setClearState] = useState<'idle' | 'confirm' | 'busy'>('idle');
  /**
   * Library mode: an import kept in this browser (IndexedDB) that is not in the library, e.g.
   * after saving to the library failed on an earlier visit. The user can save it or discard it.
   */
  const [browserCopy, setBrowserCopy] = useState<ImportResult | null>(null);
  const [captionEdit, setCaptionEdit] = useState<CaptionEdit | null>(null);
  const [download, setDownload] = useState<DownloadJob | null>(null);
  /** Cancels the download being prepared; null when there is none. */
  const downloadAbortRef = useRef<AbortController | null>(null);

  // --- Import bookkeeping that must not depend on render timing ---
  const mountedRef = useRef(false);
  /** The result whose object URLs are (about to be) on screen. */
  const currentResultRef = useRef<ImportResult | null>(null);
  /** Replaced / cleared results: released only after the commit that stopped rendering them. */
  const retiredRef = useRef<ImportResult[]>([]);
  const importSeqRef = useRef(0);
  /** Identifies the running import; anything that arrives for an older run is discarded. */
  const runIdRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const restoreStartedRef = useRef(false);
  /** False once the user imported or cleared: a late restore must not overwrite that. */
  const restoreAllowedRef = useRef(true);
  /** Serialises saveImport / clearSavedImport so a slow save cannot resurrect a cleared import. */
  const persistRef = useRef<Promise<unknown>>(Promise.resolve());
  const importOpenRef = useRef(false);
  const parkedRef = useRef<ImportOutcome | null>(null);
  /** True while a post, the Create dialog, an action sheet or a panel is open. */
  const layerOpenRef = useRef(false);
  const toastIdRef = useRef(0);
  /** Resolves with the storage mode once the startup check of the library is done. */
  const storageReadyRef = useRef<Promise<StorageMode> | null>(null);
  /** The storage mode imports use now (the startup check, or browser mode after a failed save). */
  const storageRef = useRef<StorageMode>({ kind: 'checking' });
  const mergeRef = useRef<MergeChoice>('add');
  /** See `browserCopy`: owns the object URLs of that result until it is saved, discarded or shown. */
  const browserCopyRef = useRef<ImportResult | null>(null);
  /** The run whose commit was sent: it cannot be cancelled any more. */
  const committingRunRef = useRef(0);
  /**
   * The run that saved part of its import in the library (an import saved in several parts):
   * cancelling it stops the next parts, and the parts already saved are shown.
   */
  const partSavedRunRef = useRef(0);

  const posts = activeImport ? activeImport.posts : seedState;
  const selectedPost = posts.find((p) => p.id === selectedPostId) ?? null;
  const optionsPost = optionsFor ? posts.find((p) => p.id === optionsFor.id) ?? null : null;
  const editedPost = captionEdit ? posts.find((p) => p.id === captionEdit.id) ?? null : null;
  const shownResult = activeImport?.result ?? null;
  const directory: UserDirectory = activeImport?.directory ?? EMPTY_DIRECTORY;
  const importDirectory = activeImport?.directory;
  const searchUsers = useMemo(
    () => (importDirectory ? importedSearchUsers(importDirectory.values()) : seedSearchUsers(seedPosts)),
    [importDirectory],
  );
  const extraComments = activeImport ? activeImport.comments : seedComments;
  const recent = activeImport ? activeImport.recent : seedRecent;
  const layerOpen = !!selectedPost || !!optionsPost || !!editedPost || createOpen || panel !== null || autoView !== null;

  useEffect(() => {
    layerOpenRef.current = layerOpen;
    mergeRef.current = merge;
  });

  useEffect(() => {
    document.documentElement.classList.toggle('__fb-dark-mode', theme === 'dark');
    localStorage.setItem('ig-theme', theme);
  }, [theme]);

  const showToast = (message: string, duration = 2600) => {
    const id = ++toastIdRef.current;
    // At most three at a time; a repeated message replaces its older copy.
    setToasts((prev) => [...prev.filter((t) => t.message !== message).slice(-2), { id, message }]);
    window.setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), duration);
  };

  const setImportDialog = (open: boolean) => {
    importOpenRef.current = open;
    setImportOpen(open);
  };

  /**
   * Puts a result on screen. The result it replaces is released after the next commit, unless
   * `absorbed`: a merged result took over the previous result's object URLs.
   */
  const applyImport = (
    result: ImportResult,
    shown: { report: ImportReport; library?: LibraryInfo; saveError?: string; absorbed?: boolean },
  ) => {
    const previous = currentResultRef.current;
    if (previous && previous !== result && !shown.absorbed) retiredRef.current.push(previous);
    currentResultRef.current = result;
    restoreAllowedRef.current = false;
    const imported = importToPosts(result);
    const users = buildUserDirectory(imported);
    setActiveImport({
      result,
      report: shown.report,
      library: shown.library,
      saveError: shown.saveError,
      posts: imported,
      directory: users,
      stories: storiesFromPosts(imported),
      seq: ++importSeqRef.current,
      comments: {},
      recent: [],
    });
    setSelectedPostId(null);
    setOptionsFor(null);
    // The profile on screen may be an author of the import that was just replaced.
    setProfileUsername((name) => (users.has(name) ? name : currentUser.username));
  };

  /** Nothing imported on screen any more: the seed feed. */
  const showSeedFeed = () => {
    const previous = currentResultRef.current;
    if (previous) retiredRef.current.push(previous);
    currentResultRef.current = null;
    restoreAllowedRef.current = false;
    setActiveImport(null);
    setSelectedPostId(null);
    setOptionsFor(null);
    // Nobody from the import is left to look at.
    setProfileUsername(currentUser.username);
  };

  // Object URLs are revoked here, after React committed a tree that no longer shows them. This is
  // an effect body, not a cleanup: StrictMode's extra unmount / remount never revokes anything.
  useEffect(() => {
    if (retiredRef.current.length === 0) return;
    const retired = retiredRef.current;
    retiredRef.current = [];
    for (const result of retired) {
      if (result === shownResult) retiredRef.current.push(result);
      else safeRelease(result);
    }
    clearImageCache();
  }, [shownResult]);

  const persist = (task: () => Promise<unknown>) => {
    const next = persistRef.current.then(task, task);
    persistRef.current = next.catch(() => undefined);
    return next;
  };

  /**
   * Where the next import goes. The visible note of the import dialog, and what startImport does,
   * must always agree: both follow this.
   */
  const changeStorage = (mode: StorageMode) => {
    storageRef.current = mode;
    storageReadyRef.current = Promise.resolve(mode);
    setStorage(mode);
  };

  /** Takes the browser copy away from the notice: the caller owns (and shows or releases) it now. */
  const takeBrowserCopy = (): ImportResult | null => {
    const copy = browserCopyRef.current;
    browserCopyRef.current = null;
    setBrowserCopy(null);
    return copy;
  };

  /**
   * Library mode: looks for an import kept in this browser (an earlier failed save, or an import
   * of the version before the library). It is offered for saving instead of being lost from view.
   */
  const findBrowserCopy = () => loadSavedImport().then((saved) => {
    if (!saved) return;
    if (!mountedRef.current || storageRef.current.kind !== 'library' || saved.posts.length === 0) {
      safeRelease(saved);
      return;
    }
    const previous = browserCopyRef.current;
    browserCopyRef.current = saved;
    setBrowserCopy(saved);
    if (previous) safeRelease(previous);
  }).catch(() => undefined);

  const finishImport = (outcome: ImportOutcome) => {
    const { report } = outcome;
    // With its dialog open the dialog is the report; a toast would only cover it on small screens.
    const reportByToast = !importOpenRef.current;
    if (outcome.library) {
      applyImport(outcome.result, { report, library: libraryInfo(outcome.library) });
      if (reportByToast) {
        showToast(report.unsaved
          ? unsavedLine(report.unsaved)
          : `Saved ${countLabel(report.posts, 'post')} in your library`, report.unsaved ? 8000 : undefined);
      }
    } else {
      // Saving failed: until the page is reloaded, imports stay in this browser (the dialog says so,
      // and startImport does so), instead of reaching the library behind the user's back.
      if (outcome.saveFailed) {
        changeStorage({ kind: 'browser', reason: report.saveError, afterSaveFailure: true });
      }
      // Browser mode adds to the import on screen unless the user chose to replace it. A library on
      // screen is not added to: it is not in this browser.
      const current = currentResultRef.current;
      const adding = mergeRef.current === 'add' && current !== null && current.sourceKind !== 'library';
      // An import kept in this browser from an earlier visit stays with the posts kept here: the
      // save below replaces what IndexedDB holds.
      const copy = outcome.saveFailed ? takeBrowserCopy() : null;
      const parts = [...(copy ? [copy] : []), ...(adding ? [current] : []), outcome.result];
      const result = parts.length > 1 ? mergeImportResults(parts) : outcome.result;
      // The banner describes what is on screen: after adding, the merged import.
      applyImport(result, { report: parts.length > 1 ? reportOf(result) : report, saveError: report.saveError, absorbed: adding });

      const warnUnsaved = () => {
        if (mountedRef.current && currentResultRef.current === result) {
          showToast('This import could not be kept for your next visit.', 5200);
        }
      };
      persist(() => saveImport(result)).then((saved) => { if (saved !== true) warnUnsaved(); }, warnUnsaved);
      if (reportByToast) {
        showToast(outcome.saveFailed
          ? `Not saved in the library. ${report.saveError ?? ''}`.trim()
          : `Imported ${countLabel(report.posts, 'post')}`, outcome.saveFailed ? 5200 : undefined);
      }
    }
    setJob(importOpenRef.current ? { status: 'success', report } : { status: 'idle' });
    setView('home');
    setPanel(null);
    setMoreOpen(false);
    setCreateOpen(false);
    window.scrollTo(0, 0);
  };

  /** Forgets a finished import that was never shown (a library import stays saved). */
  const dropParked = () => {
    const outcome = parkedRef.current;
    if (!outcome) return;
    parkedRef.current = null;
    setParked(null);
    if (outcome.result !== currentResultRef.current) safeRelease(outcome.result);
  };

  /** Shows the finished import that was waiting for the user. */
  const showParked = () => {
    const outcome = parkedRef.current;
    if (!outcome) return;
    parkedRef.current = null;
    setParked(null);
    finishImport(outcome);
  };

  /**
   * An import finished. With its dialog open, or with nothing else in the way, it is shown right
   * away. If the user closed the dialog and is busy elsewhere (an open post, the Create dialog, a
   * panel, auto view), replacing the feed would take that away from them: the import waits until
   * they ask.
   */
  const settleImport = (outcome: ImportOutcome) => {
    if (importOpenRef.current || !layerOpenRef.current) {
      finishImport(outcome);
      return;
    }
    parkedRef.current = outcome;
    setParked(outcome);
    setJob({ status: 'idle' });
  };

  /**
   * Runs one import action: parsing in the browser, then (library mode) saving in the library.
   * `run` is invoked synchronously (a drop's DataTransfer is only readable inside the event
   * handler); a previous run is aborted and its late result discarded.
   */
  const startImport = (run: ImportRunner, runOptions: ImportRunOptions = {}) => {
    abortRef.current?.abort();
    dropParked();
    const controller = new AbortController();
    abortRef.current = controller;
    const runId = ++runIdRef.current;
    // A run that saved part of its import stays current when it is cancelled: it shows those parts.
    const isCurrent = () => mountedRef.current && runIdRef.current === runId
      && (!controller.signal.aborted || partSavedRunRef.current === runId);

    setJob({ status: 'running', progress: null });
    setImportDialog(true);
    setMoreOpen(false);
    setDragging(false);

    // Progress can arrive once per image or upload chunk: repaint at most every PROGRESS_INTERVAL_MS.
    let latest: ImportProgress | null = null;
    let timer: number | null = null;
    let settled = false;
    const onProgress = (progress: ImportProgress) => {
      latest = progress;
      if (progress.phase === 'saving') {
        // The commit was sent: Cancel goes away right now, not with the next repaint.
        committingRunRef.current = runId;
        if (!settled && isCurrent()) setJob({ status: 'running', progress });
        return;
      }
      if (committingRunRef.current === runId) {
        // The next part of an import saved in several parts: Cancel stops the rest from here on.
        committingRunRef.current = 0;
        partSavedRunRef.current = runId;
      }
      if (timer !== null || settled) return;
      timer = window.setTimeout(() => {
        timer = null;
        if (!settled && isCurrent() && latest) setJob({ status: 'running', progress: latest });
      }, PROGRESS_INTERVAL_MS);
    };

    const pipeline = async (): Promise<ImportOutcome> => {
      // Called before the first await: see above.
      const parsed = await run({ signal: controller.signal, onProgress });
      const mode = await (storageReadyRef.current ?? Promise.resolve<StorageMode>({ kind: 'browser' }));
      if (controller.signal.aborted) {
        safeRelease(parsed);
        throw new ImportError('aborted', 'The import was cancelled.');
      }
      if (mode.kind !== 'library') return { result: parsed, report: reportOf(parsed) };
      return saveToLibrary(parsed, controller.signal, onProgress);
    };

    pipeline().then((outcome) => {
      settled = true;
      if (runOptions.fromBrowserCopy) {
        // Saved: the copy is in the library now. Not saved: IndexedDB still holds it (or, when it
        // is shown, finishImport keeps it there).
        if (outcome.library) void persist(() => clearSavedImport()).catch(() => undefined);
        else if (!isCurrent()) void findBrowserCopy();
      }
      if (!isCurrent()) {
        // Cancelled, superseded or unmounted: this result will never be rendered.
        safeRelease(outcome.result);
        return;
      }
      abortRef.current = null;
      settleImport(outcome);
    }, (error: unknown) => {
      settled = true;
      // Cancelled or failed: the copy is still in IndexedDB, offer it again.
      if (runOptions.fromBrowserCopy) void findBrowserCopy();
      if (!mountedRef.current || runIdRef.current !== runId) return;
      abortRef.current = null;
      if (controller.signal.aborted || isAbortError(error)) {
        setJob({ status: 'idle' });
        return;
      }
      const message = error instanceof ImportError && error.message
        ? error.message
        : 'The export could not be read. Check the file or folder and try again.';
      // With the dialog closed the toast is the report: no stale error waits in the dialog.
      if (importOpenRef.current) setJob({ status: 'error', message });
      else {
        setJob({ status: 'idle' });
        showToast(`Import failed: ${message}`, 5200);
      }
    });
  };

  const cancelImport = () => {
    // Too late: the library is merging the import. It finishes and is shown as saved.
    if (committingRunRef.current === runIdRef.current && abortRef.current) return;
    if (partSavedRunRef.current === runIdRef.current && abortRef.current) {
      // Some parts are in the library already: stop the rest, and show what was saved.
      abortRef.current.abort();
      return;
    }
    runIdRef.current += 1;
    abortRef.current?.abort();
    abortRef.current = null;
    setJob({ status: 'idle' });
  };

  const openImport = () => {
    setImportDialog(true);
    setMoreOpen(false);
    setPanel(null);
    // An import that finished out of sight is what the user is asking about: show its result.
    showParked();
  };

  /** The outcome of the last import again: how many entries made it, and every warning. */
  const openImportReport = () => {
    if (!activeImport) return;
    if (job.status !== 'running') setJob({ status: 'success', report: activeImport.report });
    setImportDialog(true);
  };

  /**
   * Closing never cancels: a running import carries on, stays visible in the import chip (with
   * Show and Cancel) and reports there or by toast when it is done.
   */
  const closeImport = () => {
    setImportDialog(false);
    setJob((current) => (current.status === 'running' ? current : { status: 'idle' }));
  };

  /** Browser mode: forget the import kept in this browser. */
  const clearImport = () => {
    if (!currentResultRef.current) return;
    showSeedFeed();
    void persist(() => clearSavedImport()).catch(() => undefined);
    showToast('Import cleared');
  };

  /** Library mode, after the confirmation: delete every imported post and file in the cache folder. */
  const clearLibrary = () => {
    setClearState('busy');
    libraryClient.clear().then(() => {
      if (!mountedRef.current) return;
      setClearState('idle');
      showSeedFeed();
      showToast('Library cleared');
    }, (error: unknown) => {
      if (!mountedRef.current) return;
      setClearState('idle');
      showToast(error instanceof LibraryError ? error.message : 'The library could not be cleared.', 5200);
    });
  };

  /** Library mode: save the import kept in this browser to the library. */
  const saveBrowserCopy = () => {
    const copy = takeBrowserCopy();
    if (!copy) return;
    startImport(() => Promise.resolve(copy), { fromBrowserCopy: true });
  };

  /** Library mode: forget the import kept in this browser (the archives it came from are not touched). */
  const discardBrowserCopy = () => {
    const copy = takeBrowserCopy();
    if (!copy) return;
    safeRelease(copy);
    void persist(() => clearSavedImport()).catch(() => undefined);
    showToast('Removed the posts kept in this browser');
  };

  // Mount / unmount: which storage applies (GET /api/library), then what is shown at startup:
  // the library (plus an offer to save an import kept in IndexedDB), or (browser mode) the import
  // kept in IndexedDB.
  useEffect(() => {
    mountedRef.current = true;
    if (!restoreStartedRef.current) {
      // Started once: StrictMode re-runs this effect, and two loads would create two sets of URLs.
      restoreStartedRef.current = true;
      const restoreSaved = () => loadSavedImport().then((saved) => {
        if (!saved) return;
        if (!mountedRef.current || !restoreAllowedRef.current || saved.posts.length === 0) {
          // Unmounted, or the user already imported / cleared in the meantime.
          safeRelease(saved);
          return;
        }
        applyImport(saved, { report: reportOf(saved) });
      }).catch(() => undefined);

      const ready = libraryClient.load().then((library): StorageMode => {
        if (mountedRef.current && restoreAllowedRef.current && library.posts.length > 0) {
          const result = libraryResult(library);
          applyImport(result, { report: reportOf(result), library: libraryInfo(library) });
        }
        return { kind: 'library', directory: library.directory };
      }, (error: unknown): StorageMode => ({
        kind: 'browser',
        reason: error instanceof LibraryError ? error.message : undefined,
      }));
      storageReadyRef.current = ready;
      void ready
        .then((mode) => {
          // A failed save may have switched to browser mode already: that stays.
          if (storageRef.current.kind === 'checking') {
            storageRef.current = mode;
            if (mountedRef.current) setStorage(mode);
          }
          if (storageRef.current.kind === 'browser') return restoreSaved();
          return findBrowserCopy();
        })
        .finally(() => { if (mountedRef.current) setStartupDone(true); });
    }
    return () => {
      mountedRef.current = false;
      abortRef.current?.abort();
      downloadAbortRef.current?.abort();
      // A browser copy that was never saved or shown: nothing renders its URLs any more.
      const copy = browserCopyRef.current;
      browserCopyRef.current = null;
      if (copy) safeRelease(copy);
    };
    // Mount-only on purpose: applyImport touches nothing but refs and state setters.
  }, []);

  const navigate = (v: View) => {
    setView(v);
    setPanel(null);
    setMoreOpen(false);
  };

  const openPanel = (p: Panel) => {
    setPanel((prev) => (prev === p ? null : p));
    setMoreOpen(false);
    setSelectedPostId(null);
  };

  const openProfile = (username: string) => {
    setProfileUsername(username);
    navigate('profile');
    setSelectedPostId(null);
  };

  /** The "Profile" item is the user's own profile, whoever was looked at last. */
  const navigateFromSidebar = (v: View) => {
    if (v === 'profile') openProfile(currentUser.username);
    else navigate(v);
  };

  /** Updates the recent searches of whatever is on screen: the import or the seed data. */
  const updateRecent = (update: (prev: string[]) => string[]) => {
    if (activeImport) setActiveImport((prev) => (prev ? { ...prev, recent: update(prev.recent) } : prev));
    else setSeedRecent(update);
  };

  const openProfileFromSearch = (username: string) => {
    updateRecent((prev) => [username, ...prev.filter((name) => name !== username)].slice(0, MAX_RECENT));
    openProfile(username);
  };

  const toggleTheme = () => {
    setTheme((t) => (t === 'light' ? 'dark' : 'light'));
    setMoreOpen(false);
  };

  /** Updates whichever list is on screen: the import's posts or the seed posts. */
  const updatePosts = (update: (prev: Post[]) => Post[]) => {
    if (activeImport) setActiveImport((prev) => (prev ? { ...prev, posts: update(prev.posts) } : prev));
    else setSeedState(update);
  };

  const toggleLike = (id: string) => {
    updatePosts((prev) => prev.map((p) => (p.id === id
      ? { ...p, liked: !p.liked, likes: p.likes === null ? null : Math.max(0, p.likes + (p.liked ? -1 : 1)) }
      : p)));
  };

  const toggleSave = (id: string) => {
    updatePosts((prev) => prev.map((p) => (p.id === id ? { ...p, saved: !p.saved } : p)));
  };

  const toggleFollow = (username: string) => {
    setFollowing((prev) => {
      const next = new Set(prev);
      if (next.has(username)) next.delete(username); else next.add(username);
      return next;
    });
  };

  const addComment = (id: string, text: string) => {
    const comment: Comment = {
      id: `local-${Date.now()}`,
      username: currentUser.username,
      text,
      likes: 0,
      time: 'now',
    };
    const add = (prev: Record<string, Comment[]>) => ({ ...prev, [id]: [...(prev[id] ?? []), comment] });
    // Like likes and saves, a comment belongs to the list it was written on.
    if (activeImport) setActiveImport((prev) => (prev ? { ...prev, comments: add(prev.comments) } : prev));
    else setSeedComments(add);
  };

  const openDetail = (p: Post) => setSelectedPostId(p.id);

  /** The import record a feed post was made from; undefined for demo posts and posts shared here. */
  const sourceOf = (post: Post): ImportedPost | undefined => (
    activeImport && post.importIndex !== undefined ? activeImport.result.posts[post.importIndex] : undefined
  );

  /**
   * Puts an edited description into the import on screen: into its result (so a download, the copy
   * kept in this browser and a later merge carry it) and into its feed post. The new result shares
   * the old one's object URLs, so the old one is not retired. Null when `result` is no longer shown.
   */
  const applyCaption = (result: ImportResult, index: number, caption: string): ImportResult | null => {
    if (currentResultRef.current !== result || !result.posts[index]) return null;
    const next: ImportResult = {
      ...result,
      posts: result.posts.map((source, i) => (i === index ? { ...source, caption } : source)),
    };
    currentResultRef.current = next;
    setActiveImport((prev) => (prev && prev.result === result
      ? { ...prev, result: next, posts: prev.posts.map((p) => (p.importIndex === index ? { ...p, caption } : p)) }
      : prev));
    return next;
  };

  /**
   * Saves the description typed into the editor: in the library (post.json in the cache folder),
   * in the import kept in this browser, or (a demo post) on screen only.
   */
  const saveCaption = (caption: string) => {
    const target = editedPost;
    if (!target) return;
    const current = activeImport;
    const index = target.importIndex;
    if (!current || index === undefined || !current.result.posts[index]) {
      updatePosts((prev) => prev.map((p) => (p.id === target.id ? { ...p, caption } : p)));
      setCaptionEdit(null);
      showToast('Description updated.');
      return;
    }
    const result = current.result;
    if (current.library) {
      const id = result.posts[index].id;
      setCaptionEdit({ id: target.id, busy: true });
      libraryClient.updateCaption(id, caption).then((library) => {
        if (!mountedRef.current) return;
        const saved = library.posts.find((entry) => entry.id === id)?.caption;
        applyCaption(result, index, typeof saved === 'string' ? saved : caption);
        setCaptionEdit((edit) => (edit?.id === target.id ? null : edit));
        showToast('Description saved in your library.');
      }, (error: unknown) => {
        if (!mountedRef.current) return;
        const message = error instanceof LibraryError ? error.message : 'The description could not be saved.';
        setCaptionEdit((edit) => (edit?.id === target.id ? { ...edit, busy: false, error: message } : edit));
      });
      return;
    }
    const next = applyCaption(result, index, caption);
    setCaptionEdit(null);
    showToast('Description updated.');
    if (!next) return;
    const warnUnsaved = () => {
      if (mountedRef.current) showToast('The new description could not be kept for your next visit.', 5200);
    };
    persist(() => saveImport(next)).then((saved) => { if (saved !== true) warnUnsaved(); }, warnUnsaved);
  };

  /** Builds the export archive(s) of `sources` and hands them to the browser's downloads. */
  const startDownload = (sources: ImportedPost[], name: string, all: boolean) => {
    if (downloadAbortRef.current) {
      showToast('A download is already being prepared.');
      return;
    }
    if (sources.length === 0) return;
    const controller = new AbortController();
    downloadAbortRef.current = controller;
    setDownload({ all, done: 0, total: sources.length });
    if (!all) showToast('Preparing the download…');
    exportPosts(sources, {
      name,
      signal: controller.signal,
      onProgress: ({ done, total }) => {
        if (mountedRef.current && downloadAbortRef.current === controller) setDownload({ all, done, total });
      },
    }).then((result) => {
      if (!mountedRef.current) return;
      // Several parts: one after the other, so the browser does not drop any of them.
      result.archives.forEach((archive, i) => window.setTimeout(() => saveFile(archive.blob, archive.name), i * 600));
      showToast(downloadMessage(result), result.problems.length > 0 ? 9000 : 4000);
    }, (error: unknown) => {
      if (!mountedRef.current) return;
      if (isAbortError(error)) showToast('Download cancelled.');
      else showToast(error instanceof ImportError ? error.message : 'The download could not be prepared.', 5200);
    }).finally(() => {
      if (downloadAbortRef.current === controller) downloadAbortRef.current = null;
      if (mountedRef.current) setDownload(null);
    });
  };

  const downloadPost = (post: Post) => {
    const source = sourceOf(post);
    if (!source) return;
    const stem = source.id.replace(/[^A-Za-z0-9._-]+/g, '_').slice(0, 64);
    startDownload([source], `instagram-export-${stem}`, false);
  };

  const downloadAll = () => {
    if (activeImport) startDownload(activeImport.result.posts, 'instagram-export', true);
  };

  const cancelDownload = () => downloadAbortRef.current?.abort();

  const onShare = (caption: string, photo: Photo) => {
    const newPost: Post = {
      id: `new-${Date.now()}`,
      username: currentUser.username,
      time: 'now',
      photo,
      likes: 0,
      caption: caption || 'a little moment from today ✨',
      comments: [],
      commentCount: 0,
      reposts: 0,
      shares: 0,
      date: 'JUST NOW',
    };
    updatePosts((prev) => [newPost, ...prev]);
    navigate('home');
    showToast('Your post has been shared.');
  };

  // --- Auto view: a slideshow of the posts on screen (the import / library, else the feed) ---
  const openAutoView = (startIndex = 0) => {
    setAutoView({ phase: 'setup', startIndex: Math.max(0, startIndex) });
    setMoreOpen(false);
    setPanel(null);
  };

  const startAutoView = (settings: AutoViewSettings) => {
    saveAutoViewSettings(settings);
    setAutoSettings(settings);
    setSelectedPostId(null);
    setOptionsFor(null);
    setAutoView((state) => ({ phase: 'running', startIndex: state?.startIndex ?? 0, settings }));
  };

  /**
   * The time per post was changed while running. Only that is remembered: the other options of a
   * run started from "#autoview=2&loop=0" were for that run, not for the next setup.
   */
  const changeAutoViewSettings = (settings: AutoViewSettings) => {
    const remembered = { ...autoSettings, intervalSec: settings.intervalSec };
    saveAutoViewSettings(remembered);
    setAutoSettings(remembered);
    setAutoView((state) => (state?.phase === 'running' ? { ...state, settings } : state));
  };

  const exitAutoView = () => {
    setAutoView(null);
    dropAutoViewHash();
  };

  // "#autoview=15" starts auto view once the feed has loaded (also when the hash changes later).
  useEffect(() => {
    if (!startupDone) return undefined;
    const startFromHash = () => {
      const requested = parseAutoViewHash(window.location.hash);
      if (!requested) return;
      setSelectedPostId(null);
      setOptionsFor(null);
      setAutoView({ phase: 'running', startIndex: 0, settings: { ...loadAutoViewSettings(), ...requested } });
    };
    startFromHash();
    window.addEventListener('hashchange', startFromHash);
    return () => window.removeEventListener('hashchange', startFromHash);
  }, [startupDone]);

  /** Escape closes one layer at a time, innermost first. A running auto view handles its own keys. */
  const closeTopmost = () => {
    // While the library is being cleared there is nothing to cancel.
    if (clearState === 'busy' || autoView?.phase === 'running' || captionEdit?.busy) return;
    if (clearState === 'confirm') setClearState('idle');
    else if (captionEdit) setCaptionEdit(null);
    else if (autoView) setAutoView(null);
    else if (importOpen) closeImport();
    else if (optionsPost) setOptionsFor(null);
    else if (createOpen) setCreateOpen(false);
    else if (selectedPost) setSelectedPostId(null);
    else if (panel) setPanel(null);
    else if (moreOpen) setMoreOpen(false);
  };

  // Window-level listeners are registered once and always call the handlers of the latest render.
  const latest = useRef({ closeTopmost, startImport, ownsDrop: createOpen || autoView !== null });
  useEffect(() => {
    latest.current = { closeTopmost, startImport, ownsDrop: createOpen || autoView !== null };
  });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') latest.current.closeTopmost();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // Drag and drop anywhere in the window: archives, export folders, or the loose files of one.
  useEffect(() => {
    let depth = 0;
    const hasFiles = (e: DragEvent) => !!e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files');
    const reset = () => {
      depth = 0;
      setDragging(false);
    };
    // The Create dialog has a drop zone of its own, and auto view covers everything: no import then.
    const dropTaken = () => latest.current.ownsDrop;
    const onDragEnter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (dropTaken()) return;
      depth += 1;
      setDragging(true);
    };
    const onDragOver = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      // Without this the browser would navigate to the dropped file.
      e.preventDefault();
      if (dropTaken()) return;
      if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
      if (depth === 0) {
        depth = 1;
        setDragging(true);
      }
    };
    const onDragLeave = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      depth = Math.max(0, depth - 1);
      const outside = e.clientX <= 0 || e.clientY <= 0 || e.clientX >= window.innerWidth || e.clientY >= window.innerHeight;
      if (depth === 0 || outside) reset();
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      reset();
      if (dropTaken()) return;
      const data = e.dataTransfer;
      if (!data) return;
      // The DataTransfer is only readable right now: the importer captures its entries synchronously.
      // Several archives and / or folders are imported one by one and merged.
      latest.current.startImport((options) => importFromDataTransfer(data, options));
    };
    window.addEventListener('dragenter', onDragEnter);
    window.addEventListener('dragover', onDragOver);
    window.addEventListener('dragleave', onDragLeave);
    window.addEventListener('drop', onDrop);
    window.addEventListener('dragend', reset);
    return () => {
      window.removeEventListener('dragenter', onDragEnter);
      window.removeEventListener('dragover', onDragOver);
      window.removeEventListener('dragleave', onDragLeave);
      window.removeEventListener('drop', onDrop);
      window.removeEventListener('dragend', reset);
    };
  }, []);

  const pickArchives = (files: File[]) => {
    if (files.length === 1) startImport((options) => importFromArchive(files[0], options));
    else startImport((options) => importMany(files.map((file) => ({ kind: 'archive' as const, file })), options));
  };

  const library = activeImport?.library;
  const sourceUrl = safeLinks.source(activeImport?.result.sourceUrl);
  const warningCount = activeImport?.report.warnings.length ?? 0;
  /** Entries of post.json that did not become a post (their reasons are among the warnings). */
  const expectedCount = activeImport ? expectedPosts(activeImport.result) : 0;
  const skippedEntries = activeImport && !library ? expectedCount > activeImport.result.posts.length : false;
  const runningProgress = job.status === 'running' ? job.progress : null;
  /** The Add / Replace choice only exists in browser mode, with something to add to. */
  const mergeChoice: MergeChoice | null = storage.kind === 'browser' && activeImport && !library ? merge : null;

  let content: ReactNode;
  if (view === 'home') {
    content = (
      <>
        {browserCopy && storage.kind === 'library' && (
          <div className="import-banner import-banner--copy" role="region" aria-label="Posts kept in this browser" data-testid="browser-copy">
            <p className="import-banner__text">
              <Icon name="warning" size={16} />
              <span>
                {countLabel(browserCopy.posts.length, 'post')} from <strong>{browserCopy.sourceName}</strong>
                {browserCopy.posts.length === 1 ? ' is' : ' are'} kept in this browser only: they are not in your library.
              </span>
            </p>
            <div className="import-banner__actions">
              <button
                className="link-blue link-small"
                type="button"
                data-testid="browser-copy-save"
                disabled={job.status === 'running'}
                onClick={saveBrowserCopy}
              >
                Save to library
              </button>
              <button className="link-strong link-small" type="button" data-testid="browser-copy-discard" onClick={discardBrowserCopy}>
                Discard
              </button>
            </div>
          </div>
        )}
        {activeImport && (
          <div className="import-banner" role="region" aria-label="Imported posts" data-testid="import-banner">
            <p className="import-banner__text">
              <Icon name="import" size={16} />
              {library ? (
                <span data-testid="library-summary">
                  Showing {countLabel(activeImport.result.posts.length, 'post')} from your library ({countLabel(library.imports, 'import')})
                  {' '}· saved in <span className="import-banner__path">{library.directory}</span>
                </span>
              ) : (
                <span>
                  Showing{' '}
                  {skippedEntries
                    ? `${activeImport.result.posts.length.toLocaleString('en-US')} of ${countLabel(expectedCount, 'post')}`
                    : countLabel(activeImport.result.posts.length, 'post')}
                  {' '}imported from{' '}
                  <strong>{activeImport.result.sourceName}</strong>
                </span>
              )}
            </p>
            {activeImport.saveError && (
              <p className="import-banner__notice" data-testid="library-save-error">
                <Icon name="warning" size={14} /> Not saved in the library: these posts are kept in this browser only.
              </p>
            )}
            <div className="import-banner__actions">
              {sourceUrl && (
                <ExternalLink className="link-strong link-small" href={sourceUrl} data-testid="import-source-link">
                  View source on {siteName(sourceUrl)}
                </ExternalLink>
              )}
              {warningCount > 0 && (
                <button
                  className="link-strong link-small import-banner__warnings"
                  type="button"
                  aria-haspopup="dialog"
                  data-testid="import-warnings-open"
                  onClick={openImportReport}
                >
                  <Icon name="warning" size={14} /> {countLabel(warningCount, 'warning')}
                </button>
              )}
              <button
                className="link-blue link-small import-banner__auto"
                type="button"
                aria-haspopup="dialog"
                data-testid="auto-view-open"
                onClick={() => openAutoView(0)}
              >
                <Icon name="play" size={12} filled /> Auto view
              </button>
              {download?.all ? (
                <button
                  className="link-strong link-small import-banner__download"
                  type="button"
                  data-testid="download-all-cancel"
                  onClick={cancelDownload}
                >
                  <span className="spinner import-banner__spinner" aria-hidden="true" />
                  <span aria-live="polite">
                    Preparing {download.done.toLocaleString('en-US')} of {download.total.toLocaleString('en-US')}
                  </span>
                  {' '}· Cancel
                </button>
              ) : (
                <button
                  className="link-blue link-small import-banner__download"
                  type="button"
                  data-testid="download-all"
                  disabled={download !== null || activeImport.result.posts.length === 0}
                  title="Download every post with its description as an Instagram export (.zip)"
                  onClick={downloadAll}
                >
                  <Icon name="download" size={12} /> Download all
                </button>
              )}
              <button className="link-blue link-small" type="button" onClick={openImport}>
                {library ? 'Import more' : 'Import another'}
              </button>
              {library ? (
                <button
                  className="link-strong link-small"
                  type="button"
                  aria-haspopup="dialog"
                  onClick={() => setClearState('confirm')}
                >
                  Clear library
                </button>
              ) : (
                <button className="link-strong link-small" type="button" onClick={clearImport}>Clear import</button>
              )}
            </div>
          </div>
        )}
        <Feed
          key={activeImport ? `import-${activeImport.seq}` : 'seed'}
          posts={posts}
          stories={activeImport ? activeImport.stories : seedStories}
          extraComments={extraComments}
          onToggleLike={toggleLike}
          onToggleSave={toggleSave}
          onOpenDetail={openDetail}
          onAddComment={addComment}
          onOpenProfile={openProfile}
          onOpenOptions={(p) => setOptionsFor({ id: p.id, fromDetail: false })}
          videosPaused={layerOpen || importOpen || clearState !== 'idle'}
        />
      </>
    );
  } else if (view === 'explore') {
    content = <ExploreGrid posts={posts} imported={!!activeImport} onOpenDetail={openDetail} />;
  } else if (view === 'reels') {
    content = <ReelsView />;
  } else {
    content = (
      <ProfileView
        username={profileUsername}
        posts={posts}
        imported={!!activeImport}
        importedUser={directory.get(profileUsername)}
        onOpenDetail={openDetail}
        following={following}
        onToggleFollow={toggleFollow}
      />
    );
  }

  return (
    <UserDirectoryContext.Provider value={directory}>
      <div className="app">
        <header className="mobile-header">
          <button className="ig-wordmark" type="button" onClick={() => navigate('home')}>Instagram</button>
          <div className="mobile-header__actions">
            <button type="button" aria-label="Import Instagram export" aria-haspopup="dialog" data-testid="import-open-mobile" onClick={openImport}>
              <Icon name="import" size={24} />
            </button>
            <button type="button" aria-label="Toggle theme" onClick={toggleTheme}>
              <Icon name={theme === 'light' ? 'moon' : 'sun'} size={24} />
            </button>
            <button type="button" aria-label="Notifications" onClick={() => openPanel('notifications')}><Icon name="heart" size={26} /></button>
            <button type="button" aria-label="Messages" onClick={() => openPanel('messages')}><Icon name="messages" size={26} /></button>
          </div>
        </header>

        <Sidebar
          activeView={view}
          activePanel={panel}
          ownProfile={profileUsername === currentUser.username}
          theme={theme}
          onNavigate={navigateFromSidebar}
          onOpenPanel={openPanel}
          onOpenCreate={() => { setCreateOpen(true); setMoreOpen(false); }}
          onOpenImport={openImport}
          onOpenAutoView={() => openAutoView(0)}
          onToggleTheme={toggleTheme}
          moreOpen={moreOpen}
          onToggleMore={() => setMoreOpen((o) => !o)}
        />

        {panel && (
          <>
            <div className="scrim" onClick={() => setPanel(null)} role="presentation" />
            {panel === 'search' && (
              <SearchPanel
                query={query}
                recent={recent}
                users={searchUsers}
                onQueryChange={setQuery}
                onClearRecent={() => updateRecent(() => [])}
                onOpenProfile={openProfileFromSearch}
              />
            )}
            {panel === 'notifications' && <NotificationsPanel onOpenProfile={openProfile} />}
            {panel === 'messages' && <MessagesPanel />}
          </>
        )}

        <main className="main">
          <div className={`main__inner main__inner--${view}`}>
            <div className="main__content">{content}</div>
            {view === 'home' && (
              <RightRail following={following} onToggleFollow={toggleFollow} onOpenProfile={openProfile} />
            )}
          </div>
        </main>

        {selectedPost && (
          <PostDetail
            key={selectedPost.id}
            post={selectedPost}
            posts={posts}
            extraComments={extraComments[selectedPost.id] ?? []}
            onClose={() => setSelectedPostId(null)}
            onToggleLike={toggleLike}
            onToggleSave={toggleSave}
            onAddComment={addComment}
            onOpenProfile={openProfile}
            onOpenDetail={openDetail}
            onOpenOptions={(p) => setOptionsFor({ id: p.id, fromDetail: true })}
          />
        )}

        {createOpen && <CreateModal onClose={() => setCreateOpen(false)} onShare={onShare} />}

        {optionsFor && optionsPost && (
          <PostOptionsSheet
            post={optionsPost}
            canGoToPost={!optionsFor.fromDetail}
            onClose={() => setOptionsFor(null)}
            onGoToPost={openDetail}
            onOpenProfile={openProfile}
            onAutoView={(p) => {
              setSelectedPostId(null);
              openAutoView(posts.findIndex((candidate) => candidate.id === p.id));
            }}
            onEditCaption={(p) => setCaptionEdit({ id: p.id, busy: false })}
            onDownload={sourceOf(optionsPost) ? downloadPost : undefined}
            onToast={showToast}
          />
        )}

        {captionEdit && editedPost && (
          <EditCaptionDialog
            // Not the bare post id: PostDetail, a sibling, is keyed by it.
            key={`edit:${editedPost.id}`}
            post={editedPost}
            busy={captionEdit.busy}
            error={captionEdit.error}
            note={!sourceOf(editedPost)
              ? 'Demo post: the change lasts until you reload the page.'
              : library ? 'Saved in post.json of your library.' : 'Kept in this browser with the import.'}
            onSave={saveCaption}
            onCancel={() => setCaptionEdit(null)}
          />
        )}

        {importOpen && (
          <ImportModal
            job={job}
            dragging={dragging}
            storage={storage}
            merge={mergeChoice}
            onMergeChange={setMerge}
            onPickArchives={pickArchives}
            onPickFolder={(files) => startImport((options) => importFromFiles(files, options))}
            onCancel={cancelImport}
            onRetry={() => setJob({ status: 'idle' })}
            onClose={closeImport}
            onViewFeed={closeImport}
          />
        )}

        {clearState !== 'idle' && library && (
          <ConfirmDialog
            title="Clear library?"
            confirmLabel="Clear library"
            busy={clearState === 'busy'}
            onConfirm={clearLibrary}
            onCancel={() => setClearState('idle')}
          >
            <p>
              This deletes the {countLabel(activeImport?.result.posts.length ?? 0, 'imported post')} and their pictures and videos
              from <span className="import-modal__path">{library.directory}</span>. The archives you imported them from are
              not touched.
            </p>
          </ConfirmDialog>
        )}

        <AutoViewSetup
          open={autoView?.phase === 'setup'}
          settings={autoSettings}
          postCount={posts.length}
          onStart={startAutoView}
          onClose={() => setAutoView(null)}
        />

        {autoView?.phase === 'running' && (
          <AutoViewPlayer
            posts={posts}
            startIndex={Math.min(autoView.startIndex, Math.max(0, posts.length - 1))}
            settings={autoView.settings}
            onSettingsChange={changeAutoViewSettings}
            onExit={exitAutoView}
            onOpenPost={(p) => {
              exitAutoView();
              openDetail(p);
            }}
          />
        )}

        {dragging && (
          <div className="drop-overlay" data-testid="import-drop-overlay" aria-hidden="true">
            <div className="drop-overlay__box">
              <Icon name="import" size={64} strokeWidth={1.4} />
              <p className="drop-overlay__title">Drop your Instagram export</p>
              <p className="drop-overlay__hint">.zip, .tar or .tar.gz archives, or folders with post.json and posts/. Several are merged.</p>
            </div>
          </div>
        )}

        {/* An import whose dialog is closed: still running, or finished and waiting to be shown. */}
        {!importOpen && (job.status === 'running' || parked) && (
          <div className="import-chip" role="group" aria-label="Import" data-testid="import-chip">
            {parked ? (
              <>
                <span className="import-chip__icon"><Icon name="check" size={18} strokeWidth={2.2} /></span>
                <span className="import-chip__text" role="status">
                  {parked.library ? (
                    <>{countLabel(parked.report.posts, 'post')} saved in your library</>
                  ) : (
                    <>{countLabel(parked.report.posts, 'post')} ready from <strong>{parked.report.sourceName}</strong></>
                  )}
                </span>
                <button className="link-blue" type="button" onClick={showParked}>View feed</button>
                <button className="link-strong" type="button" onClick={dropParked}>{parked.library ? 'Later' : 'Discard'}</button>
              </>
            ) : (
              <>
                <span className="spinner import-chip__spinner" aria-hidden="true" />
                <span className="import-chip__text">
                  {runningProgress?.phase === 'uploading' || runningProgress?.phase === 'saving' ? 'Saving…' : 'Importing…'}
                  {runningProgress && runningProgress.total > 0 && runningProgress.phase !== 'uploading'
                    && ` ${runningProgress.done.toLocaleString('en-US')} of ${runningProgress.total.toLocaleString('en-US')}`}
                  {runningProgress && runningProgress.total > 0 && runningProgress.phase === 'uploading'
                    && ` ${Math.round((runningProgress.done / runningProgress.total) * 100)}%`}
                </span>
                <button className="link-blue" type="button" aria-haspopup="dialog" onClick={() => setImportDialog(true)}>Show</button>
                {runningProgress?.phase !== 'saving' && (
                  <button className="link-strong" type="button" onClick={cancelImport}>Cancel</button>
                )}
              </>
            )}
          </div>
        )}

        <div className="toasts" role="status" aria-live="polite">
          {toasts.map((toast) => <div className="toast" data-testid="toast" key={toast.id}>{toast.message}</div>)}
        </div>
      </div>
    </UserDirectoryContext.Provider>
  );
}
