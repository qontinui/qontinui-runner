/**
 * Apollo Client default-option declarations.
 *
 * From `@apollo/client` v4.2 onward the client-wide `defaultOptions` passed to
 * `new ApolloClient({ ... })` are type-gated: any option listed as a "required
 * declaration" (`errorPolicy` for watchQuery/query/mutate, plus
 * `returnPartialData` for watchQuery) must first be declared here via
 * declaration merging on `ApolloClient.DeclareDefaultOptions`. Without it the
 * literal is rejected with the self-describing error
 *
 *   Type '"all"' is not assignable to type 'A default option for
 *   watchQuery.errorPolicy must be declared in
 *   ApolloClient.DeclareDefaultOptions before usage. …'
 *
 * See `@apollo/client/core/defaultOptions.d.ts` →
 * `RequireDefaultOptionDeclarations`, and
 * https://www.apollographql.com/docs/react/data/typescript#declaring-default-options-for-type-safety
 *
 * The declared shapes here must match the `defaultOptions` object actually
 * passed in `src/lib/graphql-client.ts` — Apollo folds them into the return
 * types of `client.query` / `client.mutate` / `useQuery` etc. so that `data`
 * is correctly typed as possibly-`undefined` under `errorPolicy: "all"`.
 *
 * The properties are declared OPTIONAL on purpose. Declaring any of them as
 * non-optional flips Apollo's method/hook signatures from "classic" to
 * "modern" globally (see the v4.2.0 CHANGELOG entry for #13132), which forbids
 * manually-specified generics such as `useQuery<TData>(...)` across the whole
 * codebase. Optional declarations satisfy the gate (the check is
 * `Exclude<RequiredDeclarations, keyof DeclarationInterface>`, which is
 * key-presence based, not optionality based) while leaving the signature style
 * untouched.
 *
 * This file is inert on v4.1.x — that version has no `DeclareDefaultOptions`
 * and no gate — so it is safe both before and after the floating `^4.1.6`
 * range resolves up to 4.2.x.
 */

import "@apollo/client";

export {};

declare module "@apollo/client" {
  namespace ApolloClient {
    namespace DeclareDefaultOptions {
      interface WatchQuery {
        errorPolicy?: "all";
      }
      interface Query {
        errorPolicy?: "all";
      }
      interface Mutate {
        errorPolicy?: "all";
      }
    }
  }
}
