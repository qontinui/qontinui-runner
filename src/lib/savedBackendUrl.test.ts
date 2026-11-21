import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  resolveSignInBackendUrl,
  signInBackendUrlFrom,
  signInTargetExplanation,
  type SavedBackendStatus,
  type SignInSurface,
} from "./savedBackendUrl";

// Literal, never imported: the test must fail if the constant drifts.
const PROD = "https://api.qontinui.io";

const SURFACES: SignInSurface[] = ["gate", "operator"];

const URL_CLASSES: Array<[string, string]> = [
  ["remote https", "https://backend.example.test"],
  ["remote http", "http://backend.example.test:8080"],
  ["loopback v4", "http://127.0.0.1:8000"],
  ["localhost", "http://localhost:8000"],
  ["[::1]", "http://[::1]:8000"],
  ["0.0.0.0", "http://0.0.0.0:8000"],
  ["[::]", "http://[::]:8000"],
];

type Verdict = boolean | "missing";
const VERDICTS: Verdict[] = [true, false, "missing"];

/**
 * Independent oracle, written from the rule text (plan
 * 2026-11-21-unify-runner-sign-in-backend-url-helper, Design decision), for a
 * readable status carrying an absolute http(s) URL — conditions (1) and (2)
 * hold, so only (3) and (4) decide:
 *   (3) the runner would dial it: `backendUrlRefused` is exactly `false`;
 *   (4) if `backendUrlMachineLocal` is `true`, the surface is "operator";
 *       a missing verdict is UNKNOWN and takes production.
 */
function oracle(surface: SignInSurface, url: string, refused: Verdict, local: Verdict): string {
  if (refused !== false) return PROD;
  if (local === "missing") return PROD;
  if (local === true && surface !== "operator") return PROD;
  return url;
}

function statusOf(url: string, refused: Verdict, local: Verdict): SavedBackendStatus {
  const st: SavedBackendStatus = { backendUrl: url, settingsFault: null };
  if (refused !== "missing") st.backendUrlRefused = refused;
  if (local !== "missing") st.backendUrlMachineLocal = local;
  return st;
}

const TABLE: Array<[SignInSurface, string, string, Verdict, Verdict, string]> = [];
for (const surface of SURFACES)
  for (const [cls, url] of URL_CLASSES)
    for (const refused of VERDICTS)
      for (const local of VERDICTS)
        TABLE.push([surface, cls, url, refused, local, oracle(surface, url, refused, local)]);

describe("signInBackendUrlFrom — surface × URL class × refused × machine-local", () => {
  it("enumerates every combination", () => {
    expect(TABLE).toHaveLength(2 * 7 * 3 * 3);
  });

  it.each(TABLE)(
    "%s / %s (%s) refused=%s machineLocal=%s → %s",
    (surface, _cls, url, refused, local, expected) => {
      expect(signInBackendUrlFrom(statusOf(url, refused, local), surface)).toBe(expected);
      // The explanation is null exactly when the saved URL is the target.
      expect(signInTargetExplanation(statusOf(url, refused, local), surface) === null).toBe(
        expected === url,
      );
    },
  );

  // The shapes the runner actually serves, spelled out as literals.
  it("release build, saved loopback: production on both surfaces", () => {
    const st = statusOf("http://127.0.0.1:8000", true, true);
    expect(signInBackendUrlFrom(st, "gate")).toBe("https://api.qontinui.io");
    expect(signInBackendUrlFrom(st, "operator")).toBe("https://api.qontinui.io");
  });

  it("debug build, saved loopback: honoured on operator, production on gate", () => {
    const st = statusOf("http://127.0.0.1:8000", false, true);
    expect(signInBackendUrlFrom(st, "operator")).toBe("http://127.0.0.1:8000");
    expect(signInBackendUrlFrom(st, "gate")).toBe("https://api.qontinui.io");
  });

  it("saved remote backend: honoured on both surfaces", () => {
    const st = statusOf("https://backend.example.test", false, false);
    expect(signInBackendUrlFrom(st, "gate")).toBe("https://backend.example.test");
    expect(signInBackendUrlFrom(st, "operator")).toBe("https://backend.example.test");
  });

  it.each(SURFACES)("trims the honoured saved URL (%s)", (surface) => {
    expect(
      signInBackendUrlFrom(
        {
          backendUrl: "  https://backend.example.test/  ",
          backendUrlRefused: false,
          backendUrlMachineLocal: false,
        },
        surface,
      ),
    ).toBe("https://backend.example.test/");
  });

  it.each(SURFACES)("a non-boolean verdict is UNKNOWN → production (%s)", (surface) => {
    const st = {
      backendUrl: "https://backend.example.test",
      backendUrlRefused: "false",
      backendUrlMachineLocal: 0,
    } as unknown as SavedBackendStatus;
    expect(signInBackendUrlFrom(st, surface)).toBe(PROD);
  });
});

describe("signInBackendUrlFrom — unusable saved values", () => {
  const VERDICTS_OK = { backendUrlRefused: false, backendUrlMachineLocal: false };
  const UNUSABLE: Array<[string, SavedBackendStatus | null]> = [
    ["null status (read failed)", null],
    [
      "settings fault",
      {
        backendUrl: "https://backend.example.test",
        settingsFault: { path: "settings.json", error: "bad" },
        ...VERDICTS_OK,
      },
    ],
    ["blank", { backendUrl: "", ...VERDICTS_OK }],
    ["whitespace", { backendUrl: "   ", ...VERDICTS_OK }],
    ["null url", { backendUrl: null, ...VERDICTS_OK }],
    ["absent url", { ...VERDICTS_OK }],
    ["non-URL", { backendUrl: "not a url", ...VERDICTS_OK }],
    ["ftp:", { backendUrl: "ftp://backend.example.test", ...VERDICTS_OK }],
  ];
  const CASES: Array<[SignInSurface, string, SavedBackendStatus | null]> = [];
  for (const surface of SURFACES) for (const [n, st] of UNUSABLE) CASES.push([surface, n, st]);

  it.each(CASES)("%s / %s → production", (surface, _name, st) => {
    expect(signInBackendUrlFrom(st, surface)).toBe(PROD);
  });
});

describe("signInTargetExplanation", () => {
  const ok = { backendUrlRefused: false, backendUrlMachineLocal: false };

  it.each(SURFACES)("is null when the saved remote URL is honoured (%s)", (surface) => {
    expect(
      signInTargetExplanation({ backendUrl: "https://backend.example.test", ...ok }, surface),
    ).toBeNull();
  });

  it("is null on operator for a dialable machine-local URL", () => {
    expect(
      signInTargetExplanation(
        {
          backendUrl: "http://127.0.0.1:8000",
          backendUrlRefused: false,
          backendUrlMachineLocal: true,
        },
        "operator",
      ),
    ).toBeNull();
  });

  const REASONS: Array<[string, SavedBackendStatus | null, SignInSurface, RegExp]> = [
    ["null status", null, "operator", /could not be read/],
    [
      "settings fault",
      { backendUrl: "https://backend.example.test", settingsFault: { x: 1 }, ...ok },
      "operator",
      /could not be read/,
    ],
    ["blank", { backendUrl: "  ", ...ok }, "operator", /No backend URL/],
    ["ftp", { backendUrl: "ftp://backend.example.test", ...ok }, "operator", /absolute http\(s\)/],
    [
      "release refusal",
      {
        backendUrl: "http://127.0.0.1:8000",
        backendUrlRefused: true,
        backendUrlMachineLocal: true,
      },
      "operator",
      /release build/,
    ],
    [
      "first-run screen",
      {
        backendUrl: "http://127.0.0.1:8000",
        backendUrlRefused: false,
        backendUrlMachineLocal: true,
      },
      "gate",
      /first-run/,
    ],
    [
      "refused verdict missing",
      { backendUrl: "https://backend.example.test", backendUrlMachineLocal: false },
      "operator",
      /build verdict/,
    ],
    [
      "machine-local verdict missing",
      { backendUrl: "https://backend.example.test", backendUrlRefused: false },
      "operator",
      /whether the saved URL is machine-local/,
    ],
  ];

  it.each(REASONS)("names the reason: %s", (_name, st, surface, word) => {
    expect(signInTargetExplanation(st, surface)).toMatch(word);
  });
});

describe("resolveSignInBackendUrl", () => {
  beforeEach(() => mockInvoke.mockReset());

  it.each(SURFACES)("reads get_web_integration_status and honours a remote URL (%s)", async (s) => {
    mockInvoke.mockResolvedValueOnce({
      backendUrl: "https://backend.example.test",
      settingsFault: null,
      backendUrlMachineLocal: false,
      backendUrlRefused: false,
    });
    await expect(resolveSignInBackendUrl(s)).resolves.toBe("https://backend.example.test");
    expect(mockInvoke).toHaveBeenCalledWith("get_web_integration_status");
  });

  it("debug loopback: operator honours it, gate takes production", async () => {
    const st = {
      backendUrl: "http://127.0.0.1:8000",
      settingsFault: null,
      backendUrlMachineLocal: true,
      backendUrlRefused: false,
    };
    mockInvoke.mockResolvedValueOnce(st);
    await expect(resolveSignInBackendUrl("operator")).resolves.toBe("http://127.0.0.1:8000");
    mockInvoke.mockResolvedValueOnce(st);
    await expect(resolveSignInBackendUrl("gate")).resolves.toBe(PROD);
  });

  it.each(SURFACES)("falls back to production when the read rejects (%s)", async (s) => {
    mockInvoke.mockRejectedValueOnce("ipc down");
    await expect(resolveSignInBackendUrl(s)).resolves.toBe(PROD);
  });
});
