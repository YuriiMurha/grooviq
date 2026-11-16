// Writes icon.svg: the placeholder app icon, a "G" monogram in paper white on the Riso Ink
// background. Our own geometry, no font or third-party artwork. Regenerate the platform icons
// from it with `npm --prefix app/ui run tauri icon icons/icon.svg` (from the repo root), then
// delete the mobile and Store icons it adds: the app is desktop-only.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const size = 1024;
const c = size / 2;
const r = 300; // radius of the G's bowl, to the middle of its stroke
const stroke = 120;
const rad = (deg) => (deg * Math.PI) / 180;
const at = (deg) => [c + r * Math.cos(rad(deg)), c - r * Math.sin(rad(deg))];

// The bowl runs counter-clockwise from 40 degrees round to 0 degrees (an opening on the upper
// right); the bar hangs below the bowl's cut end and reaches in towards the centre.
const [sx, sy] = at(40);
const [ex, ey] = at(0);
const bowl = `M ${sx.toFixed(1)} ${sy.toFixed(1)} A ${r} ${r} 0 1 0 ${ex.toFixed(1)} ${ey.toFixed(1)}`;
const bar = `M ${(ex + stroke / 2).toFixed(1)} ${(ey + stroke / 2).toFixed(1)} H ${c + 20}`;

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">
  <rect width="${size}" height="${size}" rx="200" fill="#0e1127"/>
  <path d="${bowl}" fill="none" stroke="#f3eee4" stroke-width="${stroke}" stroke-linecap="butt"/>
  <path d="${bar}" fill="none" stroke="#ff48b0" stroke-width="${stroke}" stroke-linecap="butt"/>
</svg>
`;
writeFileSync(fileURLToPath(new URL("icon.svg", import.meta.url)), svg);
