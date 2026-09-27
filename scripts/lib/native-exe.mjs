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

/**
 * Does `head` (the file's first bytes) carry `format`'s magic? Like the Rust
 * `check`, fewer than four bytes never pass: PE's own magic is only `MZ`, and
 * no real image is shorter than four bytes.
 */
export function matchesFormat(format, head) {
  const at = (i) => head[i];
  if (head.length < 4) return false;
  if (format === "pe") return at(0) === 0x4d && at(1) === 0x5a;
  if (format === "elf") return at(0) === 0x7f && at(1) === 0x45 && at(2) === 0x4c && at(3) === 0x46;
  return MACHO_MAGICS.some((m) => m.every((b, i) => at(i) === b));
}

/**
 * Why `path` is not a runnable native executable for `triple`, or `null` when
 * it is. Never throws: a missing or unreadable file is a reason like any other.
 * On a POSIX host an image without any execute bit is refused too, as the Rust
 * `check` refuses it (`NoExecutePermission`); Windows has no such bit.
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
  if (process.platform !== "win32" && (st.mode & 0o111) === 0) return "no execute permission";
  const head = Buffer.alloc(4);
  let n;
  try {
    const fd = openSync(path, "r");
    try {
      n = readSync(fd, head, 0, 4, 0);
    } finally {
      closeSync(fd);
    }
  } catch (e) {
    return `unreadable: ${e.message}`;
  }
  const format = formatForTriple(triple);
  if (!matchesFormat(format, head.subarray(0, n))) {
    const name = { pe: "PE (MZ)", elf: "ELF", macho: "Mach-O" }[format];
    return `${st.size}-byte file that is not a valid ${name} executable for ${triple}`;
  }
  return null;
}
