import type { ConnectionState } from "../types";
import { Mark } from "./Mark";

type Route = "feed" | "leaderboard" | "methodology";

export function TopNav({
  route,
  onNavigate,
  connection,
}: {
  route: Route;
  onNavigate: (r: Route) => void;
  connection: ConnectionState;
}) {
  return (
    <header className="topnav">
      <div className="topnav-inner">
        <a
          className="brand"
          href="/"
          onClick={(e) => {
            e.preventDefault();
            onNavigate("feed");
          }}
          style={{ color: "var(--accent)" }}
        >
          <Mark size={16} />
          <span className="wordmark">sandwich.rs</span>
        </a>

        <nav className="nav-inline" aria-label="Primary">
          <NavItem label="Live feed" active={route === "feed"} onClick={() => onNavigate("feed")} />
          <NavItem label="Leaderboard" active={route === "leaderboard"} onClick={() => onNavigate("leaderboard")} />
          <NavItem label="Methodology" active={route === "methodology"} onClick={() => onNavigate("methodology")} />
        </nav>

        <div className="topnav-right">
          <div
            className={`connection-status ${connection}`}
            aria-live="polite"
            title={`Realtime: ${connection}`}
          >
            <span className="dot" />
            <span>{connectionLabel(connection)}</span>
          </div>
          <a
            className="github-link"
            href="https://github.com/claygeo/sandwich-rs"
            target="_blank"
            rel="noopener noreferrer"
          >
            github ↗
          </a>
        </div>
      </div>
    </header>
  );
}

function NavItem({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`nav-inline-item ${active ? "active" : ""}`}
      onClick={onClick}
      aria-current={active ? "page" : undefined}
    >
      {label}
    </button>
  );
}

function connectionLabel(c: ConnectionState): string {
  switch (c) {
    case "connecting":
      return "connecting…";
    case "healthy":
      return "live · mainnet";
    case "degraded":
      return "reconnecting…";
    case "disconnected":
      return "offline";
  }
}
