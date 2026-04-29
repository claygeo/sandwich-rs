import type { ConnectionState } from "../types";
import { Mark } from "./Mark";

type Route = "feed" | "leaderboard" | "methodology";

export function Sidebar({
  route,
  onNavigate,
  connection,
}: {
  route: Route;
  onNavigate: (r: Route) => void;
  connection: ConnectionState;
}) {
  return (
    <aside className="sidebar">
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

      <nav className="nav" aria-label="Primary">
        <NavItem
          label="Live feed"
          active={route === "feed"}
          onClick={() => onNavigate("feed")}
        />
        <NavItem
          label="Leaderboard"
          active={route === "leaderboard"}
          onClick={() => onNavigate("leaderboard")}
        />
        <NavItem
          label="Methodology"
          active={route === "methodology"}
          onClick={() => onNavigate("methodology")}
        />
      </nav>

      <div className="sidebar-footer">
        <div
          className={`connection-status ${connection}`}
          aria-live="polite"
          title={`Realtime: ${connection}`}
        >
          <span className="dot" />
          <span>{connectionLabel(connection)}</span>
        </div>
        <a
          className="sidebar-link"
          href="https://github.com/claygeo/sandwich-rs"
          target="_blank"
          rel="noopener noreferrer"
        >
          github →
        </a>
      </div>
    </aside>
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
      className={`nav-item ${active ? "active" : ""}`}
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
