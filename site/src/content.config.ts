import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { z } from 'astro/zod';

// Subpages live as plain .mdx files outside src/ (content/), so product
// docs stay editable without touching a component.
const pages = defineCollection({
  loader: glob({ pattern: '**/*.mdx', base: './content' }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    order: z.number().default(0),
  }),
});

export const collections = { pages };
