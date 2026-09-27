// "Is this file a real native executable image?" — the bundling path's check,
// mirroring `src-tauri/src/native_executable.rs` (keep the two magic sets in
// sync). `bundle-profile-sidecar.mjs` runs it on every sidecar it writes, so a
// zero-length or non-native file can never be handed to the bundler as a
// shipped `qontinui-pr` / `qontinui_profile` (plan
// 2026-09-27-qontinui-pr-zero-byte-sidecar-placeholder-published-as-session-cli:
// a 0-byte `qontinui-pr.exe` on PATH exits 0 under Git Bash having opened no PR).

import { closeSync, openSync, readSync, statSync } from "node:fs";

const MACHO_MAGICS = [
  [0xfe, 0xed, 0xfa, 0xce],
  [0xfe, 0xed, 0xfa, 0xcf],
  [0xce, 0xfa, 0xed, 0xfe],
  [0xcf, 0xfa, 0xed, 0xfe],
  [0xca, 0xfe, 0xba, 0xbe], // universal (fat)
  [0xbe, 0xba, 0xfe, 0xca],
];

/** The image format a target triple's loader runs: "pe" | "macho" | "elf". */
export function formatForTriple(triple) {
  if (triple.includes("windows")) return "pe";
  if (triple.includes("apple")) return "macho";
  return "elf";
}

/** Does `head` (the file's first bytes) carry `format`'s magic? */
export function matchesFormat(format, head) {
  const at = (i) => head[i];
  if (format === "pe") return head.length >= 2 && at(0) === 0x4d && at(1) === 0x5a;
  if (format === "elf")
    return head.length >= 4 && at(0) === 0x7f && at(1) === 0x45 && at(2) === 0x4c && at(3) === 0x46;
  return head.length >= 4 && MACHO_MAGICS.some((m) => m.every((b, i) => at(i) === b));
}

/**
 * Why `path` is not a runnable native executable for `triple`, or `null` when
 * it is. Never throws for a missing file — that is a reason like any other.
 */
export function nativeExecutableProblem(path, triple) {
  let st;
  try {
    st = statSync(path);
  } catch {
    return "no file at that path";
  }
  if (!st.isFile()) return "not a regular file";
  if (st.size === 0) return "zero-length file (a build placeholder, not a binary)";
  const head = Buffer.alloc(4);
  const fd = openSync(path, "r");
  let n;
  try {
    n = readSync(fd, head, 0, 4, 0);
  } finally {
    closeSync(fd);
  }
  const format = formatForTriple(triple);
  if (!matchesFormat(format, head.subarray(0, n))) {
    const name = { pe: "PE (MZ)", elf: "ELF", macho: "Mach-O" }[format];
    return `${st.size}-byte file that is not a valid ${name} executable for ${triple}`;
  }
  return null;
}
