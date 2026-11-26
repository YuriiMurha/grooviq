type Rgb = readonly [number, number, number];

function linear(channel: number): number {
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}

function luminance([red, green, blue]: Rgb): number {
  return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue);
}

/** WCAG contrast for two opaque sRGB colors, with each channel in 0..1. */
export function contrastRatio(first: Rgb, second: Rgb): number {
  const a = luminance(first);
  const b = luminance(second);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function toOklab(rgb: Rgb): readonly [number, number, number] {
  const [red, green, blue] = rgb.map(linear);
  // Linear sRGB to LMS, then the cube roots of LMS to OKLab.
  const l = Math.cbrt(0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue);
  const m = Math.cbrt(0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue);
  const s = Math.cbrt(0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue);
  return [
    0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
  ];
}

/** Euclidean distance in OKLab for opaque sRGB colors with channels in 0..1. */
export function oklabDeltaE(first: Rgb, second: Rgb): number {
  const [firstL, firstA, firstB] = toOklab(first);
  const [secondL, secondA, secondB] = toOklab(second);
  return Math.hypot(firstL - secondL, firstA - secondA, firstB - secondB);
}
