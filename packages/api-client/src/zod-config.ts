// Zod configuration. This module must run before any module that builds a
// Zod schema, so src/index.ts imports it first.
//
// The content security policy of the server forbids eval. Zod probes
// `new Function` when it builds an object schema, and the browser reports
// that probe as a `securitypolicyviolation` although Zod catches the error.
// With `jitless`, Zod skips the probe and parses without its compiled fast
// path.
import { config } from 'zod';

config({ jitless: true });
