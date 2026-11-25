import type { OutputKind } from "./kind";

/** Decorative output pictograms, using the transport controls' 16px stroke geometry. */
export function OutputIcon({ kind }: { kind: OutputKind | "none" | "default" }) {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {kind === "headphones" ? <>
        <path d="M2 9V8a6 6 0 0 1 12 0v1" />
        <rect x="1.5" y="8" width="3" height="5" rx="1" />
        <rect x="11.5" y="8" width="3" height="5" rx="1" />
      </> : kind === "default" ? <>
        <rect x="1.5" y="2.5" width="13" height="9" rx="1.5" />
        <path d="M8 11.5v3M5 14.5h6" />
      </> : <>
        <path d="M7 3 3.5 6h-2v4h2L7 13z" />
        {kind === "none"
          ? <path d="m10.5 6 4 4m0-4-4 4" />
          : <path d={kind === "speakers" ? "M10 5a4.2 4.2 0 0 1 0 6M12 2.5a7.5 7.5 0 0 1 0 11" : "M10 6a3 3 0 0 1 0 4"} />}
      </>}
    </svg>
  );
}
