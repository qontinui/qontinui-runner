/**
 * GraphQL Code Generator configuration.
 *
 * Generates TypeScript types from the backend's SDL schema.
 * Run: npx graphql-codegen
 *
 * The schema source is the SDL file exported by the Rust test:
 *   src-tauri/src/graphql/schema.graphql
 *
 * Output goes to:
 *   src/generated/graphql.ts
 */

import type { CodegenConfig } from "@graphql-codegen/cli";

const config: CodegenConfig = {
  schema: "src-tauri/src/graphql/schema.graphql",
  documents: ["src/hooks/graphql/documents.ts"],
  generates: {
    "src/generated/graphql.ts": {
      plugins: [
        "typescript",
        "typescript-operations",
      ],
      config: {
        // Use exact optional properties
        avoidOptionals: false,
        // Enum as const for better tree-shaking
        enumsAsConst: true,
        // Skip __typename in input types
        skipTypename: true,
        // Scalar mappings matching async-graphql's JSON type
        scalars: {
          JSON: "unknown",
        },
      },
    },
  },
  ignoreNoDocuments: false,
};

export default config;
