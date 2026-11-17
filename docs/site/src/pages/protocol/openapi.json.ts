import { readFile } from 'node:fs/promises';
import type { APIRoute } from 'astro';

export const GET: APIRoute = async () => new Response(
  await readFile(new URL('../../../../protocol/openapi.json', import.meta.url), 'utf8'),
  { headers: { 'Content-Type': 'application/json' } },
);
