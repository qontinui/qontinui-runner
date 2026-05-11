/**
 * Apollo Client configuration for GraphQL API.
 *
 * Coexists alongside React Query — Apollo handles GraphQL queries/mutations/subscriptions,
 * while React Query continues to handle REST endpoints.
 *
 * Features:
 * - HTTP link for queries and mutations (POST /graphql)
 * - WebSocket link for subscriptions (ws://…/graphql/ws)
 * - Automatic link splitting: subscriptions go over WS, everything else over HTTP
 * - Error link with logging, retry hints, and network failure handling
 * - Trace ID propagation via X-Trace-Id header
 * - Dynamic port support (port set at runtime via setApiPort)
 * - Lazy WebSocket connection (only connects when first subscription is created)
 */

import { ApolloClient, InMemoryCache, HttpLink, split, from } from "@apollo/client/core";
import { ErrorLink } from "@apollo/client/link/error";
import { CombinedGraphQLErrors } from "@apollo/client/errors";
import { GraphQLWsLink } from "@apollo/client/link/subscriptions";
import { getMainDefinition } from "@apollo/client/utilities";
import { createClient } from "graphql-ws";

import { getApiBase, getWsBase } from "./runner-api";
import { getCurrentTraceId } from "./traced-fetch";

let apolloClient: ApolloClient | null = null;
let wsClient: ReturnType<typeof createClient> | null = null;

/**
 * Create (or return existing) Apollo Client instance.
 *
 * The client is lazily created on first call and reused thereafter.
 * If the API port changes, call `resetGraphQLClient()` to rebuild.
 */
export function getGraphQLClient(): ApolloClient {
  if (apolloClient) return apolloClient;

  // --- Error link for logging and recovery hints ---
  const errorLink = new ErrorLink(({ error, operation }) => {
    if (CombinedGraphQLErrors.is(error)) {
      for (const err of error.errors) {
        const loc = err.locations
          ?.map((l: { line: number; column: number }) => `${l.line}:${l.column}`)
          .join(", ");
        console.warn(
          `[GraphQL Error] ${err.message}` +
            (err.path ? ` (path: ${err.path.join(".")})` : "") +
            (loc ? ` (loc: ${loc})` : "") +
            ` [op: ${operation.operationName}]`,
        );

        // Detect circuit breaker / rate limit errors for recovery hints
        const msg = err.message.toLowerCase();
        if (msg.includes("circuit breaker") || msg.includes("concurrency")) {
          console.warn("[GraphQL] Circuit breaker or concurrency limit hit — retry after delay");
        }
      }
    } else {
      // Network error or other non-GraphQL error
      console.error(`[GraphQL Network Error] ${error.message} [op: ${operation.operationName}]`);
    }
  });

  // --- HTTP link for queries & mutations ---
  const httpLink = new HttpLink({
    uri: () => `${getApiBase()}/graphql`,
    headers: {
      "Content-Type": "application/json",
    },
    // Inject trace ID on every request
    fetch: (input, init) => {
      const headers = new Headers(init?.headers);
      const traceId = getCurrentTraceId();
      if (traceId) {
        headers.set("X-Trace-Id", traceId);
      }
      return fetch(input, { ...init, headers });
    },
  });

  // --- WebSocket link for subscriptions ---
  wsClient = createClient({
    url: () => `${getWsBase()}/graphql/ws`,
    lazy: true, // Only connect when a subscription is created
    retryAttempts: 5,
    connectionParams: () => {
      const traceId = getCurrentTraceId();
      return traceId ? { traceId } : {};
    },
    on: {
      connected: () => {
        console.info("[GraphQL WS] Connected");
      },
      closed: () => {
        console.info("[GraphQL WS] Closed");
      },
      error: (err) => {
        console.warn("[GraphQL WS] Error:", err);
      },
    },
  });

  const wsLink = new GraphQLWsLink(wsClient);

  // --- Split link: subscriptions -> WS, everything else -> error+HTTP ---
  const splitLink = split(
    ({ query }) => {
      const definition = getMainDefinition(query);
      return definition.kind === "OperationDefinition" && definition.operation === "subscription";
    },
    wsLink,
    from([errorLink, httpLink]),
  );

  // --- Cache configuration ---
  const cache = new InMemoryCache({
    typePolicies: {
      GqlTaskRun: {
        keyFields: ["id"],
      },
      GqlWorkflowSummary: {
        keyFields: ["id"],
      },
      GqlWorkflow: {
        keyFields: ["id"],
      },
      GqlFinding: {
        keyFields: ["id"],
      },
      GqlTaskRunOutput: {
        keyFields: ["taskRunId"],
      },
      UiBridgeHealth: {
        // Singleton — always merge into the same cache entry
        keyFields: [],
      },
    },
  });

  apolloClient = new ApolloClient({
    link: splitLink,
    cache,
    defaultOptions: {
      watchQuery: {
        fetchPolicy: "cache-and-network",
        errorPolicy: "all",
      },
      query: {
        fetchPolicy: "cache-first",
        errorPolicy: "all",
      },
      mutate: {
        errorPolicy: "all",
      },
    },
    devtools: {
      enabled: import.meta.env.DEV,
    },
  });

  return apolloClient;
}

/**
 * Reset the Apollo Client (e.g., when API port changes).
 * The next call to `getGraphQLClient()` will create a fresh instance.
 */
export async function resetGraphQLClient(): Promise<void> {
  if (wsClient) {
    await wsClient.dispose();
    wsClient = null;
  }
  if (apolloClient) {
    apolloClient.stop();
    await apolloClient.clearStore();
    apolloClient = null;
  }
}
