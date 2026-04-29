export function Mark({ size = 16 }: { size?: number }) {
  return (
    <svg
      className="mark"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <rect x="0" y="2" width="16" height="3" rx="1" fill="currentColor" />
      <rect x="2" y="7" width="12" height="2" rx="0.5" fill="currentColor" />
      <rect x="0" y="11" width="16" height="3" rx="1" fill="currentColor" />
    </svg>
  );
}
