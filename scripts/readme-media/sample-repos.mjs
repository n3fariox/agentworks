// Small fictional projects shown in README media. heroEdits are written after the scripted
// approval so the Diff and Files views show real working-tree changes.
const authRoutes=limited=>`import { Router } from 'express';
import { z } from 'zod';
${limited?"import { rateLimit } from '../middleware/rate-limit';\n":''}import { verifyPassword } from '../services/passwords';
import { issueSession } from '../services/sessions';
import { users } from '../db';

const router = Router();
const credentials = z.object({ email: z.string().email(), password: z.string().min(8) });
${limited?'\nexport const loginLimiter = rateLimit({ windowMs: 60_000, max: 5 });\n':''}
async function login(req, res) {
  const parsed = credentials.safeParse(req.body);
  if (!parsed.success) return res.status(400).json({ error: 'invalid_request' });

  const user = await users.findByEmail(parsed.data.email);
  if (!user || !(await verifyPassword(user, parsed.data.password))) {
    return res.status(401).json({ error: 'invalid_credentials' });
  }
  res.json(await issueSession(user));
}

${limited?"router.post('/login', loginLimiter, login);":"router.post('/login', login);"}
router.post('/logout', (req, res) => res.status(204).end());

export default router;
`;

export const repos={
  'acme-api':{
    'package.json':JSON.stringify({name:'acme-api',version:'1.4.0',type:'module',scripts:{dev:'tsx watch src/server.ts',test:'vitest run'},dependencies:{express:'^5.1.0',zod:'^4.1.0'},devDependencies:{tsx:'^4.20.0',typescript:'^5.9.0',vitest:'^3.2.0'}},null,2)+'\n',
    'README.md':'# acme-api\n\nAccounts and billing API for Acme.\n\n```bash\nnpm install\nnpm run dev\n```\n',
    'src/server.ts':"import express from 'express';\nimport auth from './routes/auth';\nimport users from './routes/users';\n\nconst app = express();\napp.set('trust proxy', 1);\napp.use(express.json());\napp.use('/auth', auth);\napp.use('/users', users);\napp.get('/health', (_req, res) => res.json({ ok: true }));\n\napp.listen(Number(process.env.PORT ?? 3000));\n",
    'src/routes/auth.ts':authRoutes(false),
    'src/routes/users.ts':"import { Router } from 'express';\nimport { users } from '../db';\n\nconst router = Router();\nrouter.get('/', async (req, res) => res.json(await users.page(String(req.query.cursor ?? ''))));\nexport default router;\n",
    'src/db.ts':"export const users = {\n  async findByEmail(email: string) { return null as any; },\n  async page(cursor: string) { return { items: [], next: null }; },\n};\n",
    'src/services/passwords.ts':"export async function verifyPassword(user: any, password: string) { return false; }\n",
    'src/services/sessions.ts':"export async function issueSession(user: any) { return { token: 'todo' }; }\n",
    'test/auth.test.ts':"import { describe, it, expect } from 'vitest';\n\ndescribe('POST /auth/login', () => {\n  it('rejects malformed bodies', () => expect(true).toBe(true));\n});\n",
    'test/webhooks.test.ts':"import { it } from 'vitest';\nit.todo('retries with backoff');\n",
  },
  'acme-web':{
    'package.json':JSON.stringify({name:'acme-web',version:'2.8.1',private:true,scripts:{dev:'vite',build:'vite build'}},null,2)+'\n',
    'README.md':'# acme-web\n\nCustomer dashboard for Acme.\n',
    'src/pages/Settings.tsx':"export function Settings() {\n  return <main style={{ background: '#fff' }}>Settings</main>;\n}\n",
    'src/lib/format.ts':"import * as dateFns from 'date-fns';\nexport const day = (d: Date) => dateFns.format(d, 'PP');\n",
  },
};

export const heroEdits={
  'src/routes/auth.ts':authRoutes(true),
  'src/middleware/rate-limit.ts':`import type { Request, Response, NextFunction } from 'express';

type Options = { windowMs: number; max: number };

/** Sliding-window limiter keyed by client IP. In memory: use a shared store when scaling out. */
export function rateLimit({ windowMs, max }: Options) {
  const hits = new Map<string, number[]>();

  return (req: Request, res: Response, next: NextFunction) => {
    const key = req.ip ?? 'unknown';
    const now = Date.now();
    const recent = (hits.get(key) ?? []).filter((t) => now - t < windowMs);

    if (recent.length >= max) {
      const retryAfter = Math.ceil((recent[0] + windowMs - now) / 1000);
      res.set('Retry-After', String(retryAfter));
      return res.status(429).json({ error: 'too_many_requests', retryAfter });
    }

    recent.push(now);
    hits.set(key, recent);
    next();
  };
}
`,
  'test/rate-limit.test.ts':`import { describe, it, expect, vi, beforeEach } from 'vitest';
import { rateLimit } from '../src/middleware/rate-limit';

const call = (mw, ip = '1.2.3.4') => {
  const res: any = { statusCode: 200, headers: {}, set(k, v) { this.headers[k] = v; }, status(c) { this.statusCode = c; return this; }, json() { return this; } };
  let passed = false;
  mw({ ip }, res, () => { passed = true; });
  return { res, passed };
};

describe('rateLimit', () => {
  beforeEach(() => vi.useFakeTimers());

  it('allows up to max requests in the window', () => {
    const mw = rateLimit({ windowMs: 60_000, max: 5 });
    for (let i = 0; i < 5; i++) expect(call(mw).passed).toBe(true);
  });

  it('returns 429 with Retry-After once exceeded', () => {
    const mw = rateLimit({ windowMs: 60_000, max: 5 });
    for (let i = 0; i < 5; i++) call(mw);
    const { res, passed } = call(mw);
    expect(passed).toBe(false);
    expect(res.statusCode).toBe(429);
    expect(res.headers['Retry-After']).toBe('60');
  });

  it('frees capacity as the window slides', () => {
    const mw = rateLimit({ windowMs: 60_000, max: 5 });
    for (let i = 0; i < 5; i++) call(mw);
    vi.advanceTimersByTime(60_001);
    expect(call(mw).passed).toBe(true);
  });

  it('tracks each IP separately', () => {
    const mw = rateLimit({ windowMs: 60_000, max: 1 });
    call(mw, 'a');
    expect(call(mw, 'b').passed).toBe(true);
  });
});
`,
};
