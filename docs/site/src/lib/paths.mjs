import { existsSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const docsRoot = fileURLToPath(new URL('../../../', import.meta.url));
export const repoRoot = path.dirname(docsRoot);
export const repository = 'https://github.com/anyin233/hya';

export function isDocumentation(relativePath) {
  return relativePath.endsWith('.md') &&
    !relativePath.startsWith('../') &&
    !/^(changes|examples|site)\//.test(relativePath);
}

export function documentId(relativePath) {
  return relativePath.replace(/\.md$/, '').replace(/(^|\/)README$/, '$1index');
}

// Starlight's built-in autogenerate assumes src/content/docs file paths.
// Build sidebar entries from the same external source tree as the loader.
export function documentLinks(directory) {
  return readdirSync(path.join(docsRoot, directory), { recursive: true })
    .map((file) => `${directory}/${file.split(path.sep).join('/')}`)
    .filter(isDocumentation)
    .sort()
    .map((file) => ({ slug: documentId(file) }));
}

export function sitePath(relativePath, base = '/') {
  return `${base.replace(/\/$/, '')}/${relativePath.replace(/^\//, '')}`;
}

// Resolve against the Markdown source, not its rendered (trailing-slash) URL.
export function rewriteLink(url, sourcePath, base = '/') {
  if (!url || /^(?:[a-z][a-z\d+.-]*:|\/\/|#)/i.test(url)) return url;
  const [, pathname, suffix = ''] = url.match(/^([^?#]*)(.*)$/);
  if (!pathname) return url;
  const target = path.resolve(pathname.startsWith('/') ? repoRoot : path.dirname(sourcePath),
    decodeURI(pathname.replace(/^\//, '')));
  const relative = path.relative(docsRoot, target).split(path.sep).join('/');

  if (isDocumentation(relative) && existsSync(target)) {
    const route = documentId(relative).replace(/(^|\/)index$/, '');
    return sitePath(route ? `${route.replace(/\/$/, '')}/` : '', base) + suffix;
  }
  if (relative.startsWith('assets/')) {
    return sitePath(relative.slice('assets/'.length), base) + suffix;
  }
  if (relative === 'protocol/openapi.json') return sitePath(relative, base) + suffix;
  const repositoryPath = path.relative(repoRoot, target).split(path.sep).join('/');
  const kind = existsSync(target) && statSync(target).isDirectory() ? 'tree' : 'blob';
  return `${repository}/${kind}/main/${repositoryPath}${suffix}`;
}
