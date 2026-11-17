import { currentUser, footerLinks, languages, suggestions } from '../data';
import { Avatar } from './common';

export interface RightRailProps {
  following: Set<string>;
  onToggleFollow: (username: string) => void;
  onOpenProfile?: (username: string) => void;
}

export function RightRail({ following, onToggleFollow, onOpenProfile }: RightRailProps) {
  return (
    <aside className="right-rail">
      <div className="right-rail__me">
        <Avatar username={currentUser.username} avatar={currentUser.avatar} size={44} onClick={() => onOpenProfile?.(currentUser.username)} />
        <div className="right-rail__me-meta">
          <button className="link-strong" type="button" onClick={() => onOpenProfile?.(currentUser.username)}>{currentUser.username}</button>
          <span>{currentUser.fullName}</span>
        </div>
        <button className="link-blue right-rail__switch" type="button">Switch</button>
      </div>

      <div className="right-rail__section-head">
        <span className="right-rail__section-title">Suggested for you</span>
        <button className="link-strong link-small" type="button">See All</button>
      </div>

      <div className="right-rail__suggestions">
        {suggestions.map((s) => {
          const isFollowing = following.has(s.username);
          return (
            <div className="suggestion" key={s.username}>
              <Avatar username={s.username} avatar={s.avatar} size={44} onClick={() => onOpenProfile?.(s.username)} />
              <div className="suggestion__meta">
                <button className="link-strong" type="button" onClick={() => onOpenProfile?.(s.username)}>{s.username}</button>
                <span>Followed by nuitcourte + 3 more</span>
              </div>
              <button
                className={`link-blue link-small ${isFollowing ? 'suggestion__following' : ''}`}
                type="button"
                onClick={() => onToggleFollow(s.username)}
              >
                {isFollowing ? 'Following' : 'Follow'}
              </button>
            </div>
          );
        })}
      </div>

      <footer className="footer">
        <div className="footer__links">
          {footerLinks.map((l, i) => <a href="#" key={`${l}-${i}`} onClick={(e) => e.preventDefault()}>{l}</a>)}
        </div>
        <div className="footer__bottom">
          <select className="footer__lang" aria-label="Language" defaultValue="English">
            {languages.map((l) => <option key={l}>{l}</option>)}
          </select>
          <span>© 2026 INSTAGRAM FROM META</span>
        </div>
      </footer>
    </aside>
  );
}
