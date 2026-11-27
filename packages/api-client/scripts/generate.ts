// Generates the API client from an OpenAPI document.
//
// Usage: pnpm --filter telemetry-web-api-client run generate -- --input <openapi.json> --output <dir>
//
// Relative paths resolve against the current directory. pnpm runs the
// script in packages/api-client. Node runs this file directly (type
// stripping), so the file uses only erasable TypeScript syntax.
//
// The output must not depend on where the generator writes it, because
// scripts/contract-check.sh generates into a scratch directory and compares
// the tree with the committed packages/api-client/src/generated:
// - `tsConfigPath: null` stops the lookup of a tsconfig above the output.
// - `module.extension` fixes the import extension.
// - `logs.file: false` writes no log file into the current directory.
import path from 'node:path';
import process from 'node:process';
import { createClient } from '@hey-api/openapi-ts';

type Arguments = { readonly input: string; readonly output: string };

function parseArguments(argv: readonly string[]): Arguments {
  let input: string | undefined;
  let output: string | undefined;
  for (let index = 0; index < argv.length; index += 1) {
    const name = argv[index];
    // pnpm can pass the `--` separator through to the script.
    if (name === '--') {
      continue;
    }
    const value = argv[index + 1];
    if ((name === '--input' || name === '--output') && value !== undefined) {
      if (name === '--input') {
        input = value;
      } else {
        output = value;
      }
      index += 1;
      continue;
    }
    throw new Error(`generate: unknown argument ${String(name)}`);
  }
  if (input === undefined || output === undefined) {
    throw new Error('generate: give --input <openapi.json> and --output <dir>');
  }
  return { input: path.resolve(input), output: path.resolve(output) };
}

const { input, output } = parseArguments(process.argv.slice(2));

await createClient({
  input: { path: input },
  output: {
    path: output,
    clean: true,
    tsConfigPath: null,
    module: { extension: '.js' },
    postProcess: [],
  },
  logs: { file: false, level: 'warn' },
  plugins: [
    '@hey-api/client-fetch',
    '@hey-api/typescript',
    'zod',
    // The SDK runs the Zod validators of the request and of the 2xx
    // response body (https://heyapi.dev/docs/openapi/typescript/plugins/sdk#validators).
    // The validator discards the parse result, so the transformer parses
    // again and the SDK returns the parsed value without unknown keys.
    // The SDK does not validate an error body; src/index.ts validates it
    // with the generated Problem schema.
    {
      name: '@hey-api/sdk',
      validator: { request: 'zod', response: 'zod' },
      transformer: { response: 'zod' },
    },
  ],
});
