import { NavLink, Outlet } from "react-router";
import { describeError, useGlassdeckd, type Connection } from "./glassdeckd";
import styles from "./Layout.module.css";

function linkSummary(connection: Connection): { on: boolean; label: string; detail: string } {
  switch (connection.state) {
    case "connecting":
    case "syncing":
      return { on: false, label: "Connecting", detail: "glassdeckd" };
    case "lost":
      return { on: false, label: "No glassdeckd", detail: describeError(connection.error) };
    case "connected": {
      const { link } = connection.status;
      return { on: link.connected, label: link.connected ? "Keyboard connected" : "Keyboard not connected", detail: link.keyboard };
    }
  }
}

function Logo() {
  return (
    <svg width="26" height="26" viewBox="0 0 26 26" aria-hidden="true">
      <rect x="1" y="5" width="24" height="16" rx="4" fill="none" stroke="var(--accent)" strokeWidth="2" />
      <rect x="5" y="9" width="16" height="3" rx="1.5" fill="var(--accent)" />
      <rect x="5" y="14" width="10" height="3" rx="1.5" fill="var(--accent)" opacity=".5" />
    </svg>
  );
}

export function Layout() {
  const connection = useGlassdeckd((s) => s.connection);
  const link = linkSummary(connection);
  return (
    <div className={styles.shell}>
      <nav aria-label="Main" className={styles.sidebar}>
        <div className={styles.brand}>
          <Logo />
          Glassdeck
        </div>
        <NavLink className={styles.item} to="/" end>
          Overview
        </NavLink>
        <NavLink className={styles.item} to="/widgets">
          Widgets
        </NavLink>
        <NavLink className={styles.item} to="/backgrounds">
          Backgrounds
        </NavLink>
        <div className={styles.spacer} />
        <div className={styles.link}>
          <div className={styles.linkState}>
            <span className={styles.dot} data-on={link.on || undefined} />
            {link.label}
          </div>
          <div className={styles.detail}>{link.detail}</div>
        </div>
      </nav>
      <main className={styles.main}>
        <Outlet />
      </main>
    </div>
  );
}
