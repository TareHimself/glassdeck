import { css } from "@linaria/core";
import { styled } from "@linaria/react";
import { NavLink, Outlet } from "react-router";
import { describeError, useGlassdeckd, type Connection } from "./glassdeckd";

const Shell = styled.div`
  display: flex;
  height: 100%;
`;

const Sidebar = styled.nav`
  flex: none;
  width: 240px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 20px 12px;
  background: var(--sidebar);
  border-right: 1px solid var(--line);
`;

const Brand = styled.div`
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 4px 10px 20px;
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.01em;
`;

const item = css`
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 40px;
  padding: 0 12px;
  border-radius: var(--radius-small);
  color: var(--ink-soft);
  text-decoration: none;

  &.active {
    background: var(--raised);
    color: var(--ink);
    font-weight: 600;
  }
`;

const Spacer = styled.div`
  flex: 1;
`;

const LinkCard = styled.div`
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  border-radius: 10px;
  background: var(--surface);
  border: 1px solid var(--line);
`;

const LinkState = styled.div`
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
`;

const Dot = styled.span`
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--muted);

  &[data-on] {
    background: var(--ok);
  }
`;

const Detail = styled.div`
  font-family: var(--mono);
  font-size: 12px;
  color: var(--muted);
  overflow-wrap: anywhere;
`;

const Main = styled.main`
  flex: 1;
  min-width: 0;
  overflow: auto;
  padding: 24px 28px;
`;

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
    <Shell>
      <Sidebar aria-label="Main">
        <Brand>
          <Logo />
          Glassdeck
        </Brand>
        <NavLink className={item} to="/" end>
          Overview
        </NavLink>
        <NavLink className={item} to="/widgets">
          Widgets
        </NavLink>
        <NavLink className={item} to="/backgrounds">
          Backgrounds
        </NavLink>
        <Spacer />
        <LinkCard>
          <LinkState>
            <Dot data-on={link.on || undefined} />
            {link.label}
          </LinkState>
          <Detail>{link.detail}</Detail>
        </LinkCard>
      </Sidebar>
      <Main>
        <Outlet />
      </Main>
    </Shell>
  );
}
