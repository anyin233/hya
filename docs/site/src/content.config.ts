import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { defineCollection } from 'astro:content';
import { glob, type Loader } from 'astro/loaders';
import { docsSchema } from '@astrojs/starlight/schema';
import { docsRoot, documentId, repository } from './lib/paths.mjs';

// Adapt existing GitHub-friendly Markdown without copying it or adding frontmatter.
const source = glob({
  base: docsRoot,
  pattern: ['**/*.md', '!changes/**', '!examples/**', '!site/**'],
  generateId: ({ entry }) => documentId(entry),
});

const loader: Loader = {
  name: 'hya-documentation',
  async load(context) {
    await source.load({
      ...context,
      async parseData(entry) {
        const filePath = entry.filePath!;
        const markdown = await readFile(filePath, 'utf8');
        const title = /^#\s+(.+)$/m.exec(markdown)?.[1]?.replace(/[`*]/g, '') ?? entry.id;
        const relativePath = path.relative(docsRoot, filePath).split(path.sep).join('/');
        return context.parseData({
          ...entry,
          data: {
            title,
            editUrl: `${repository}/edit/main/docs/${relativePath}`,
            ...entry.data,
          },
        });
      },
    });
  },
};

export const collections = {
  docs: defineCollection({ loader, schema: docsSchema() }),
};
