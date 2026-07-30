/**
 * Doc-finder path helpers.
 *
 * The case that motivated these: PR #906 made `TerminalPage` pass the active
 * terminal's `workingDir` as the scan root, replacing a hardcoded
 * `C:\Users\<account>\...` literal. The scanner still forced backslashes, so a
 * POSIX root round-tripped to `\home\x\proj` and the scan found nothing. The
 * Windows expectations below are the pre-#906 behavior, pinned so the fix
 * cannot regress it.
 */

import { describe, it, expect } from "vitest";
import {
  pathSeparatorFor,
  normalizeRoot,
  joinPath,
  rootPrefixLength,
  relativeToRoot,
} from "./docFinderPaths";

describe("pathSeparatorFor", () => {
  it("treats drive-qualified roots as Windows, in either slash style", () => {
    expect(pathSeparatorFor("C:\\Users\\x\\proj")).toBe("\\");
    // transcript `cwd` values arrive forward-slashed about as often
    expect(pathSeparatorFor("C:/Users/x/proj")).toBe("\\");
    expect(pathSeparatorFor("d:/repo")).toBe("\\");
  });

  it("treats UNC shares as Windows", () => {
    expect(pathSeparatorFor("\\\\server\\share\\docs")).toBe("\\");
  });

  it("treats any backslash-bearing root as Windows", () => {
    expect(pathSeparatorFor("relative\\sub")).toBe("\\");
  });

  it("treats POSIX absolute and plain relative roots as forward-slash", () => {
    expect(pathSeparatorFor("/home/x/proj")).toBe("/");
    expect(pathSeparatorFor("/")).toBe("/");
    expect(pathSeparatorFor("sub/dir")).toBe("/");
    expect(pathSeparatorFor("")).toBe("/");
  });
});

describe("normalizeRoot", () => {
  it("unifies forward slashes to backslashes on Windows roots", () => {
    expect(normalizeRoot("C:/Users/x/proj")).toBe("C:\\Users\\x\\proj");
  });

  it("leaves POSIX roots alone rather than mangling them (the #906 gap)", () => {
    expect(normalizeRoot("/home/x/proj")).toBe("/home/x/proj");
  });

  it("drops a trailing separator", () => {
    expect(normalizeRoot("C:\\Users\\x\\proj\\")).toBe("C:\\Users\\x\\proj");
    expect(normalizeRoot("/home/x/proj/")).toBe("/home/x/proj");
  });

  it("keeps the separator on a filesystem root, which needs it to name a directory", () => {
    expect(normalizeRoot("/")).toBe("/");
    expect(normalizeRoot("C:\\")).toBe("C:\\");
    expect(normalizeRoot("C:/")).toBe("C:\\");
  });
});

describe("joinPath", () => {
  it("joins with the given separator", () => {
    expect(joinPath("C:\\Users\\x", "README.md", "\\")).toBe("C:\\Users\\x\\README.md");
    expect(joinPath("/home/x", "README.md", "/")).toBe("/home/x/README.md");
  });

  it("does not double a separator the directory already ends with", () => {
    expect(joinPath("C:\\", "README.md", "\\")).toBe("C:\\README.md");
    expect(joinPath("/", "README.md", "/")).toBe("/README.md");
  });
});

describe("relativeToRoot", () => {
  it("strips the root and its separator", () => {
    expect(relativeToRoot("C:\\Users\\x\\proj", "C:\\Users\\x\\proj\\docs\\a.md", "\\")).toBe(
      "docs\\a.md",
    );
    expect(relativeToRoot("/home/x/proj", "/home/x/proj/docs/a.md", "/")).toBe("docs/a.md");
  });

  it("does not eat a leading character when the root carries its own separator", () => {
    // The old `normalizedRoot.length + 1` slice turned `/a.md` into `.md` here.
    expect(rootPrefixLength("/", "/")).toBe(1);
    expect(relativeToRoot("/", "/a.md", "/")).toBe("a.md");
    expect(relativeToRoot("C:\\", "C:\\a.md", "\\")).toBe("a.md");
  });
});
