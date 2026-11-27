import { useState } from "react";
import { NavLink, Outlet } from "react-router";
import { describeError, keyboardLabel, useGlassdeckd, type Connection } from "./glassdeckd";
import { useKeyboard } from "./keyboard";
import styles from "./Layout.module.css";

/** The line under the link state: the keyboard's address, hidden until asked for, or a reason. */
type Detail = { kind: "address"; address: string } | { kind: "text"; text: string };

function linkSummary(connection: Connection): { on: boolean; label: string; detail: Detail } {
  switch (connection.state) {
    case "connecting":
    case "syncing":
      return { on: false, label: "Connecting", detail: { kind: "text", text: "glassdeckd" } };
    case "lost":
      return { on: false, label: "No glassdeckd", detail: { kind: "text", text: describeError(connection.error) } };
    case "connected": {
      const { link } = connection.status;
      return { on: link.connection.state === "connected", label: keyboardLabel(link.connection), detail: { kind: "address", address: link.keyboard } };
    }
  }
}

const HIDDEN_ADDRESS = "•••••••••••••";

function EyeIcon({ crossed }: { crossed: boolean }) {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.6">
      <path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8s-2.4 4.5-6.5 4.5S1.5 8 1.5 8z" />
      <circle cx="8" cy="8" r="2" />
      {crossed && <path d="M2.5 2.5l11 11" />}
    </svg>
  );
}

function Address({ address }: { address: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className={styles.addressRow}>
      <span className={styles.detail}>{shown ? address : HIDDEN_ADDRESS}</span>
      <button type="button" className={styles.eye} aria-label={shown ? "Hide address" : "Show address"} aria-pressed={shown} onClick={() => setShown(!shown)}>
        <EyeIcon crossed={shown} />
      </button>
    </div>
  );
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
  const needsSetup = useKeyboard((s) => s.keyboard?.software === "flux");
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
        <NavLink className={styles.item} to="/keyboard">
          <span className={styles.itemName}>Keyboard</span>
          {needsSetup && (
            <span role="img" aria-label="Needs setup" className={styles.badge}>
              !
            </span>
          )}
        </NavLink>
        <div className={styles.spacer} />
        <div className={styles.link}>
          <div className={styles.linkState}>
            <span className={styles.dot} data-on={(!needsSetup && link.on) || undefined} data-warn={needsSetup || undefined} />
            {needsSetup ? "Keyboard needs setup" : link.label}
          </div>
          {link.detail.kind === "address" ? <Address address={link.detail.address} /> : <div className={styles.detail}>{link.detail.text}</div>}
        </div>
      </nav>
      <main className={styles.main}>
        <Outlet />
      </main>
    </div>
  );
}

/** A screen of its own, without the sidebar: the libraries in select mode, which end in going back or a pick. */
export function BareLayout() {
  return (
    <main className={styles.bare}>
      <Outlet />
    </main>
  );
}
