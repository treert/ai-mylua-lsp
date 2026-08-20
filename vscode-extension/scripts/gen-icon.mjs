// Rasterizes assets/icon.svg -> assets/icon.png (256x256, marketplace icon).
// The SVG is the single source of truth for the icon design; edit the SVG,
// then re-run: node scripts/gen-icon.mjs
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

const ASSETS = join(dirname(fileURLToPath(import.meta.url)), '..', 'assets');
const SVG = join(ASSETS, 'icon.svg');
const PNG = join(ASSETS, 'icon.png');

const resvg = new Resvg(readFileSync(SVG, 'utf8'), {
  fitTo: { mode: 'width', value: 256 },
  background: 'rgba(0, 0, 0, 0)',
});

writeFileSync(PNG, resvg.render().asPng());
console.log(`wrote ${PNG}`);
