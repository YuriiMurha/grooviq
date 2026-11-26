// Small authored glyphs for the mixer's buttons, drawn in the device windows' stroke style.

const PATHS = {
  route: "M4 13V5H12M9 2L12 5L9 8M4 10H8",
  chevron: "M3 6L8 11L13 6",
  arrow: "M2 8H13M9 4L13 8L9 12",
  up: "M8 13V3M4 7L8 3L12 7",
  down: "M8 3V13M4 9L8 13L12 9",
  remove: "M4 4L12 12M12 4L4 12",
  open: "M6 3H3V13H13V10M9 3H13V7M13 3L7 9",
} as const;

export function Icon({ name }: { name: keyof typeof PATHS }) {
  return (
    <svg className="mx-icon" viewBox="0 0 16 16" aria-hidden="true">
      <path d={PATHS[name]} />
    </svg>
  );
}
