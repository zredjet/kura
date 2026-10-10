/** 音楽のタイルの波形（Main.dc.html の飾り。読み上げない）。 */
export function Waveform({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 120 24"
      preserveAspectRatio="none"
      aria-hidden="true"
      focusable="false"
    >
      <path
        d="M2 9v6M6 7v10M10 5v14M14 8v8M18 4v16M22 2v20M26 6v12M30 3v18M34 1v22M38 5v14M42 7v10M46 4v16M50 2v20M54 1v22M58 3v18M62 6v12M66 8v8M70 5v14M74 3v18M78 1v22M82 4v16M86 7v10M90 9v6M94 6v12M98 4v16M102 2v20M106 5v14M110 7v10M114 8v8M118 9v6"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
