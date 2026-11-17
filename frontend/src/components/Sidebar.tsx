import { useRef } from 'react';
import { currentUser, type User } from '../data';
import { Icon, type IconName } from '../icons';
import { Avatar } from './common';

export type View = 'home' | 'explore' | 'reels' | 'profile';
export type Panel = 'search' | 'notifications' | 'messages';
export type Theme = 'light' | 'dark';

export interface SidebarProps {
  activeView: View;
  activePanel: Panel | null;
  /** False while the profile view shows somebody else: "Profile" is the user's own profile only. */
  ownProfile?: boolean;
  theme: Theme;
  /** 'profile' means the user's own profile. */
  onNavigate: (view: View) => void;
  onOpenPanel: (panel: Panel) => void;
  onOpenCreate: () => void;
  /** Open the "Import from Instagram export" dialog. */
  onOpenImport: () => void;
  /** Open the auto view setup (a slideshow of the posts on screen). */
  onOpenAutoView: () => void;
  onToggleTheme: () => void;
  moreOpen: boolean;
  onToggleMore: () => void;
  user?: User;
}

interface NavItem {
  key: string;
  label: string;
  icon: IconName;
  filled?: boolean;
  onClick: () => void;
  active: boolean;
}

export function InstagramGlyph({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <rect x="2.5" y="2.5" width="19" height="19" rx="5.5" />
      <circle cx="12" cy="12" r="4.4" />
      <circle cx="17.6" cy="6.4" r="1.2" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function Sidebar({
  activeView,
  activePanel,
  ownProfile = true,
  theme,
  onNavigate,
  onOpenPanel,
  onOpenCreate,
  onOpenImport,
  onOpenAutoView,
  onToggleTheme,
  moreOpen,
  onToggleMore,
  user = currentUser,
}: SidebarProps) {
  const moreButton = useRef<HTMLButtonElement>(null);
  const items: NavItem[] = [
    { key: 'home', label: 'Home', icon: 'home', filled: activeView === 'home' && !activePanel, onClick: () => onNavigate('home'), active: activeView === 'home' && !activePanel },
    { key: 'search', label: 'Search', icon: 'search', onClick: () => onOpenPanel('search'), active: activePanel === 'search' },
    { key: 'explore', label: 'Explore', icon: 'compass', onClick: () => onNavigate('explore'), active: activeView === 'explore' },
    { key: 'reels', label: 'Reels', icon: 'reels', onClick: () => onNavigate('reels'), active: activeView === 'reels' },
    { key: 'messages', label: 'Messages', icon: 'messages', onClick: () => onOpenPanel('messages'), active: activePanel === 'messages' },
    { key: 'notifications', label: 'Notifications', icon: 'heart', onClick: () => onOpenPanel('notifications'), active: activePanel === 'notifications' },
    { key: 'create', label: 'Create', icon: 'create', onClick: onOpenCreate, active: false },
    // Hidden in the mobile bottom bar (styles.css); the mobile header has its own import button.
    { key: 'import', label: 'Import', icon: 'import', onClick: onOpenImport, active: false },
    { key: 'profile', label: 'Profile', icon: 'grid', onClick: () => onNavigate('profile'), active: activeView === 'profile' && ownProfile },
  ];

  return (
    <>
      <aside className="sidebar">
        <div className="sidebar__logo">
          <button className="sidebar__logo-full ig-wordmark" type="button" onClick={() => onNavigate('home')}>Instagram</button>
          <button className="sidebar__logo-glyph" type="button" aria-label="Instagram" onClick={() => onNavigate('home')}><InstagramGlyph /></button>
        </div>

        <nav className="sidebar__nav" aria-label="Main navigation">
          {items.map((item) => (
            <button
              key={item.key}
              type="button"
              className={`nav-item nav-item--${item.key} ${item.active ? 'nav-item--active' : ''}`}
              onClick={item.onClick}
              aria-label={item.label}
              aria-haspopup={item.key === 'import' ? 'dialog' : undefined}
              data-testid={item.key === 'import' ? 'import-open' : undefined}
            >
              <span className="nav-item__icon">
                {item.key === 'profile' ? (
                  <Avatar username={user.username} avatar={user.avatar} size={26} className={item.active ? 'nav-avatar--active' : ''} />
                ) : (
                  <>
                    <Icon name={item.icon} size={26} filled={item.filled} strokeWidth={item.filled ? 0 : 1.9} />
                    {item.key === 'notifications' && <span className="nav-item__dot" />}
                  </>
                )}
              </span>
              <span className="nav-item__label">{item.label}</span>
            </button>
          ))}
        </nav>

        <div className="sidebar__bottom">
          <button
            type="button"
            ref={moreButton}
            className={`nav-item ${moreOpen ? 'nav-item--active' : ''}`}
            onClick={onToggleMore}
            aria-label="More"
          >
            <span className="nav-item__icon"><Icon name="more" size={26} /></span>
            <span className="nav-item__label">More</span>
          </button>
        </div>

        {moreOpen && (
          <div className="more-menu" role="menu">
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Settings</span><Icon name="settings" size={20} />
            </button>
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Your activity</span><Icon name="clock" size={20} />
            </button>
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Saved</span><Icon name="bookmark" size={20} />
            </button>
            <button
              className="more-menu__item"
              role="menuitem"
              data-testid="import-open-menu"
              onClick={() => {
                // The menu item is about to unmount: park focus on its trigger so the dialog can restore it.
                moreButton.current?.focus();
                onOpenImport();
              }}
            >
              <span>Import Instagram export</span><Icon name="import" size={20} />
            </button>
            <button
              className="more-menu__item"
              role="menuitem"
              data-testid="auto-view-open-menu"
              onClick={() => {
                moreButton.current?.focus();
                onOpenAutoView();
              }}
            >
              <span>Auto view</span><Icon name="play" size={20} />
            </button>
            <button className="more-menu__item" role="menuitem" onClick={onToggleTheme}>
              <span>Switch appearance</span>
              <Icon name={theme === 'light' ? 'moon' : 'sun'} size={20} />
            </button>
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Report a problem</span><Icon name="flag" size={20} />
            </button>
            <div className="more-menu__divider" />
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Switch accounts</span>
            </button>
            <button className="more-menu__item" role="menuitem" onClick={onToggleMore}>
              <span>Log out</span><Icon name="logout" size={20} />
            </button>
          </div>
        )}
      </aside>
    </>
  );
}
