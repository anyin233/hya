import { cpSync, existsSync, mkdirSync, rmSync } from 'node:fs';
import path from 'node:path';
import { repoRoot } from '../src/lib/paths.mjs';

const docsDist = path.resolve('dist');
const pagesDist = path.resolve('dist-pages');
const publicRoot = path.join(repoRoot, 'public');

if (!existsSync(docsDist)) {
  throw new Error(`Missing Starlight build output at ${docsDist}`);
}
if (!existsSync(path.join(publicRoot, 'index.html'))) {
  throw new Error(`Missing landing page at ${path.join(publicRoot, 'index.html')}`);
}

rmSync(pagesDist, { recursive: true, force: true });
mkdirSync(pagesDist, { recursive: true });

cpSync(publicRoot, pagesDist, { recursive: true });
cpSync(docsDist, path.join(pagesDist, 'docs'), { recursive: true });

console.log(`Assembled GitHub Pages artifact in ${pagesDist} (/ -> public/, /docs/ -> Starlight).`);
