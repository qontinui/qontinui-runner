import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { callsOf } from "../lib/__test-helpers__/assertAnchored";
import {
  carriesNotes,
  credentialsAfterReread,
  latestReadGate,
  type TenantCandidateCredential,
} from "./TenantContext";

const usable = (tenant: string): TenantCandidateCredential => ({
  tenant,
  can_act: true,
  slot: "usable",
  via_default_slot: false,
});

describe("credentialsAfterReread", () => {
  it("takes a read that carries credential state", () => {
    const fresh = [usable("b")];
    expect(credentialsAfterReread([usable("a")], fresh)).toBe(fresh);
  });

  it("takes a read in which only one note moved", () => {
    const fresh = [usable("a"), { ...usable("b"), can_act: false, slot: "present-but-dead" }];
    expect(credentialsAfterReread([usable("a"), usable("b")], fresh)).toBe(fresh);
  });

  it("keeps what it had when the read carries none", () => {
    const previous = [usable("a")];
    expect(credentialsAfterReread(previous, [])).toBe(previous);
    expect(credentialsAfterReread(previous, undefined)).toBe(previous);
  });

  it("keeps the same array when the read changed nothing, so consumers do not re-render", () => {
    const previous = [usable("a"), usable("b")];
    expect(credentialsAfterReread(previous, [usable("a"), usable("b")])).toBe(previous);
  });
});

describe("carriesNotes", () => {
  it("is true only for a read with at least one note", () => {
    expect(carriesNotes([usable("a")])).toBe(true);
    expect(carriesNotes([])).toBe(false);
    expect(carriesNotes(undefined)).toBe(false);
  });
});

describe("latestReadGate", () => {
  it("makes an older answer stale once a newer one has landed", () => {
    const gate = latestReadGate();
    const older = gate.begin();
    const newer = gate.begin();
    expect(newer.fresh()).toBe(true);
    newer.apply();
    expect(older.fresh()).toBe(false);
  });

  it("leaves an older answer free when the newer read applied nothing", () => {
    // The newer read failed, or answered with nothing to apply.
    const gate = latestReadGate();
    const older = gate.begin();
    gate.begin();
    expect(older.fresh()).toBe(true);
  });

  it("lets a newer answer land after an older one", () => {
    const gate = latestReadGate();
    const older = gate.begin();
    const newer = gate.begin();
    older.apply();
    expect(newer.fresh()).toBe(true);
  });

  it("is per gate: another gate's answers do not make this one stale", () => {
    const a = latestReadGate();
    const b = latestReadGate();
    const read = a.begin();
    b.begin().apply();
    expect(read.fresh()).toBe(true);
  });
});

const source = readFileSync(new URL("./TenantContext.tsx", import.meta.url), "utf8");

describe("TenantProvider bindings", () => {
  it("re-reads on the event Rust emits for every in-process binding change", () => {
    const rust = readFileSync(
      new URL("../../src-tauri/src/commands/web_integration.rs", import.meta.url),
      "utf8",
    );
    expect(rust).toContain(
      'pub const TENANT_BINDINGS_CHANGED_EVENT: &str = "tenant-bindings-changed";',
    );
    const listeners = callsOf(source, "listen(").filter((c) =>
      c.includes('"tenant-bindings-changed"'),
    );
    expect(listeners).toHaveLength(1);
    expect(listeners[0]).toContain("void refresh();");
  });

  it("reads once more when the listener is live, so an early announcement is not lost", () => {
    const [effect] = callsOf(source, "useEffect(").filter((e) =>
      e.includes('"tenant-bindings-changed"'),
    );
    expect(effect).toMatch(/unlisten\s*\.then\(\(\) => refresh\(\)\)/);
  });
});

describe("TenantProvider read ordering", () => {
  it("applies a full read's bindings and notes only while each is still fresh", () => {
    const [body] = callsOf(source, "const refresh = useCallback(");
    expect(body).toContain("const bindingsRead = bindingReads.begin();");
    expect(body).toContain("const notesRead = credentialReads.begin();");
    const bindings = body.slice(body.indexOf("if (bindingsRead.fresh()) {"));
    expect(bindings).toMatch(/^if \(bindingsRead\.fresh\(\)\) \{\s+bindingsRead\.apply\(\);/);
    expect(bindings).toContain("setCandidates(");
    const notes = body.slice(body.indexOf("if (notesRead.fresh()) {"));
    expect(notes).toMatch(/^if \(notesRead\.fresh\(\)\) \{\s+notesRead\.apply\(\);/);
    expect(notes).toContain("setCredentials(");
    expect(notes).not.toContain("setCandidates(");
  });

  it("keeps the candidates array when a full read changed none of them", () => {
    const [body] = callsOf(source, "const refresh = useCallback(");
    expect(body).toContain(
      "setCandidates((previous) => (sameIds(previous, ids) ? previous : ids));",
    );
  });

  it("orders a notes-only read against the notes alone, applying only real notes", () => {
    const [body] = callsOf(source, "const refreshCredentials = useCallback(");
    expect(body).toContain("const notesRead = credentialReads.begin();");
    expect(body).toContain("if (carriesNotes(read) && notesRead.fresh()) {");
    expect(body).toContain("notesRead.apply();");
    expect(body).not.toContain("bindingReads");
  });
});

describe("TenantProvider.refreshCredentials", () => {
  it("moves only the credential state", () => {
    const [body] = callsOf(source, "const refreshCredentials = useCallback(");
    expect(body).toContain("credentialsAfterReread(previous, read)");
    for (const setter of ["setCandidates", "setPin", "setSource", "setDefaultTenantId"]) {
      expect(body).not.toContain(setter);
    }
  });
});
