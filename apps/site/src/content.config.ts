import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';
import { fileURLToPath } from 'node:url';

// Subpages live as plain .mdx files outside src/ (content/), so product
// docs stay editable without touching a component.
const pages = defineCollection({
  loader: glob({
    pattern: '**/*.mdx',
    base: fileURLToPath(new URL('../content/', import.meta.url)),
    generateId: ({ entry }) => entry.replace(/\.mdx$/, '').replace(/\/index$/, ''),
  }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    order: z.number().default(0),
  }),
});

// Resolve against this file, not the shell's cwd: apps/site is two levels
// below the repository whose Markdown remains the single source of truth.
const docs = defineCollection({
  loader: glob({ pattern: '**/*.md', base: fileURLToPath(new URL('../../../docs/', import.meta.url)), generateId: ({ entry }) => entry.replace(/\.md$/, '') }),
});
const book = defineCollection({
  loader: glob({ pattern: '**/*.md', base: fileURLToPath(new URL('../../../book/', import.meta.url)), generateId: ({ entry }) => entry.replace(/\.md$/, '') }),
});
const blog = defineCollection({
  loader: glob({ pattern: '**/*.md', base: fileURLToPath(new URL('../../../blog/', import.meta.url)), generateId: ({ entry }) => entry.replace(/\.md$/, '') }),
});

export const collections = { pages, docs, book, blog };
