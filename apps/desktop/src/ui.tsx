import { css } from "@linaria/core";
import { styled } from "@linaria/react";

export const Title = styled.h1`
  margin: 0;
  font-size: 32px;
  line-height: 1.1;
  letter-spacing: -0.02em;
  font-weight: 700;
`;

export const Header = styled.header`
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  margin-bottom: 20px;

  & > :first-child {
    flex: 1 1 240px;
  }
`;

export const Card = styled.section`
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  padding: 16px;
`;

export const CardTitle = styled.h2`
  margin: 0 0 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--muted);
`;

const buttonBase = `
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 44px;
  padding: 0 16px;
  border-radius: 10px;
  font-weight: 500;
  text-decoration: none;
  cursor: pointer;

  &:disabled {
    cursor: default;
    opacity: 0.5;
  }
`;

export const primary = css`
  ${buttonBase}
  padding: 0 18px;
  border: 0;
  background: var(--accent);
  color: var(--accent-ink);
  font-weight: 600;
`;

export const secondary = css`
  ${buttonBase}
  border: 1px solid var(--line-strong);
  background: var(--surface);
  color: var(--ink);
`;

export const quiet = css`
  ${buttonBase}
  border: 1px solid var(--line-strong);
  background: transparent;
  color: var(--ink);
`;

export const Input = styled.input`
  min-height: 44px;
  padding: 0 12px;
  border-radius: var(--radius-small);
  border: 1px solid var(--line-strong);
  background: var(--sunken);
  color: var(--ink);
  font-size: 16px;

  &[data-variant="mono"] {
    font-family: var(--mono);
    font-size: 14px;
  }

  &[data-variant="search"] {
    width: 220px;
    padding: 0 14px;
    border-radius: 10px;
  }

  &[data-variant="inline"] {
    min-height: 32px;
    flex: 1;
    padding: 0 8px;
    font-size: 15px;
    font-weight: 600;
  }
`;

export const Label = styled.label`
  font-weight: 500;
`;

export const Mono = styled.span`
  font-family: var(--mono);
  font-size: 12px;
  color: var(--muted);
  overflow-wrap: anywhere;
`;

export const Problem = styled.div`
  padding: 10px 12px;
  border-radius: 10px;
  background: #2e1a1a;
  color: var(--danger);
`;

export const Pill = styled.span`
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--raised);
  font-size: 12px;
  white-space: nowrap;

  &[data-tone="ok"] {
    background: #17301f;
    color: #7fe0a8;
  }

  &[data-tone="host"] {
    background: #1b2b3a;
    color: #8dbcff;
  }
`;
