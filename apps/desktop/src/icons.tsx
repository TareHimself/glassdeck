type Props = { size?: number };

export function KeyboardIcon({ size = 20 }: Props) {
  return (
    <svg width={size} height={size} viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.6">
      <rect x="1.5" y="5" width="19" height="12" rx="2.5" />
      <path d="M5 9h1M8 9h1M11 9h1M14 9h1M17 9h1M7 13h8" />
    </svg>
  );
}

export function MonitorIcon({ size = 20 }: Props) {
  return (
    <svg width={size} height={size} viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.6">
      <rect x="2" y="3" width="18" height="12" rx="2" />
      <path d="M8 19h6M11 15v4" />
    </svg>
  );
}

export function SlidersIcon({ size = 20 }: Props) {
  return (
    <svg width={size} height={size} viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.6">
      <path d="M3 6h16M3 11h16M3 16h16" />
      <circle cx="8" cy="6" r="2" fill="var(--sunken)" />
      <circle cx="14" cy="11" r="2" fill="var(--sunken)" />
      <circle cx="6" cy="16" r="2" fill="var(--sunken)" />
    </svg>
  );
}

export function PlusIcon({ size = 16 }: Props) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
      <path d="M8 3v10M3 8h10" />
    </svg>
  );
}
